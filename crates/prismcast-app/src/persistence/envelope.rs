//! The `collection.json` envelope: `CollectionFileV1` and the per-type
//! wrappers that capture unknown fields (`docs/architecture/persistence-model.md`
//! §2–§4).
//!
//! These are **persisted structs**, not domain structs: they exist only in the
//! persistence layer, mirror the domain aggregates field-by-field, and each
//! carries a `#[serde(flatten)]` capture map so keys written by a newer build
//! survive a load → save cycle verbatim (PLAN §19: never silently discard
//! unknown fields).
//!
//! Round-trip property (golden-tested): `save(load(bytes)) == bytes` for any
//! well-formed V-current file, including files containing unknown fields.
//!
//! Open per-kind settings blobs (`Source.settings`, `Transition.settings`,
//! mixer entries, ...) pass through untouched as `serde_json::Value` — the
//! first line of unknown-field preservation.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use prismcast_core::audio::{AudioBus, AudioMixerConfig, AudioMixerState, AudioRoute};
use prismcast_core::id::{SceneCollectionId, SceneId, SourceId};
use prismcast_core::project::{SceneCollection, StudioMode};
use prismcast_core::scene::{Scene, SceneItem};
use prismcast_core::source::Source;
use prismcast_core::transition::Transition;

use super::error::{PersistenceError, Result};
use super::migrate::{self, CURRENT_COLLECTION_SCHEMA};

/// A domain struct plus every key this build does not know about.
///
/// Serializes flat: the inner struct's fields first, then the captured
/// extras. Deserialization buffers the map, feeds known keys to `T`, and
/// captures the rest.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WithExtras<T> {
    /// The known part.
    #[serde(flatten)]
    pub inner: T,
    /// Unknown keys, captured verbatim and re-emitted on save.
    #[serde(flatten)]
    pub extras: Map<String, Value>,
}

impl<T> WithExtras<T> {
    /// Wraps a value with no captured extras.
    pub fn new(inner: T) -> Self {
        Self {
            inner,
            extras: Map::new(),
        }
    }
}

/// Session state persisted inside `collection.json` (persistence-model §2):
/// collection-scoped UI semantics, mirroring OBS. Expendable — dangling
/// references are reset with a warning, never treated as corruption.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionState {
    /// The last current (program) scene.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_scene: Option<SceneId>,
    /// Studio mode state (`None` = disabled).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub studio_mode: Option<StudioMode>,
}

/// The domain-side aggregate the persistence actor saves: one whole
/// collection plus its session state. Snapshots are whole aggregates so a
/// superseded save is always safe to drop.
#[derive(Debug, Clone, PartialEq)]
pub struct CollectionSnapshot {
    /// The collection content (scenes, sources, transition, audio).
    pub collection: SceneCollection,
    /// Session state (current scene, studio mode).
    pub session: SessionState,
}

/// Persisted form of a [`Scene`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SceneFileV1 {
    /// Scene ID.
    pub id: SceneId,
    /// User-facing name.
    pub name: String,
    /// Placed sources, sorted ascending by z-index.
    #[serde(default)]
    pub items: Vec<WithExtras<SceneItem>>,
    /// Unknown keys captured verbatim.
    #[serde(flatten)]
    pub extras: Map<String, Value>,
}

/// Persisted form of a [`Source`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceFileV1 {
    /// Source ID.
    pub id: SourceId,
    /// Source implementation kind.
    pub kind: prismcast_core::source::SourceKind,
    /// User-facing name.
    pub name: String,
    /// Whether the source produces media when referenced.
    pub enabled: bool,
    /// Kind-specific settings blob, passed through untouched.
    #[serde(default)]
    pub settings: Value,
    /// Filters applied to this source (IDs; filters are stub-stage).
    #[serde(default)]
    pub filters: Vec<prismcast_core::id::FilterId>,
    /// Unknown keys captured verbatim.
    #[serde(flatten)]
    pub extras: Map<String, Value>,
}

/// Persisted form of an [`AudioMixerConfig`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioFileV1 {
    /// Named mix buses.
    #[serde(default)]
    pub buses: Vec<WithExtras<AudioBus>>,
    /// Source → bus routes.
    #[serde(default)]
    pub routes: Vec<WithExtras<AudioRoute>>,
    /// Per-source mixer parameters, keyed by source ID.
    #[serde(default)]
    pub mixer: BTreeMap<SourceId, WithExtras<AudioMixerState>>,
    /// Unknown keys captured verbatim.
    #[serde(flatten)]
    pub extras: Map<String, Value>,
}

/// The V1 `collection.json` envelope.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CollectionFileV1 {
    /// Schema version; always [`CURRENT_COLLECTION_SCHEMA`] when written.
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    /// Collection ID (authoritative identity; the directory name is only a
    /// human-facing handle).
    pub id: SceneCollectionId,
    /// User-facing name.
    pub name: String,
    /// Scenes in this collection.
    #[serde(default)]
    pub scenes: Vec<SceneFileV1>,
    /// Shared sources.
    #[serde(default)]
    pub sources: Vec<SourceFileV1>,
    /// The default transition.
    pub transition: WithExtras<Transition>,
    /// Audio buses, routes, and mixer state.
    pub audio: AudioFileV1,
    /// Session state.
    pub session: WithExtras<SessionState>,
    /// Unknown top-level keys captured verbatim.
    #[serde(flatten)]
    pub unknown: Map<String, Value>,
}

impl CollectionFileV1 {
    /// Builds the envelope from a domain snapshot, carrying unknown fields
    /// over from the retained envelope of the last load (matched per entity
    /// by ID, so renames/reorders keep their extras).
    pub fn from_snapshot(snapshot: &CollectionSnapshot, retained: Option<&Self>) -> Self {
        let collection = &snapshot.collection;
        let scenes = collection
            .scenes
            .iter()
            .map(|scene| {
                let retained_scene =
                    retained.and_then(|r| r.scenes.iter().find(|s| s.id == scene.id));
                SceneFileV1 {
                    id: scene.id,
                    name: scene.name.clone(),
                    items: carry_extras(
                        scene.items.iter().cloned().map(WithExtras::new).collect(),
                        retained_scene.map(|s| s.items.as_slice()).unwrap_or(&[]),
                        |item| item.id,
                    ),
                    extras: retained_scene.map_or_else(Map::new, |s| s.extras.clone()),
                }
            })
            .collect();
        let sources = collection
            .sources
            .iter()
            .map(|source| {
                let retained_source =
                    retained.and_then(|r| r.sources.iter().find(|s| s.id == source.id));
                SourceFileV1 {
                    id: source.id,
                    kind: source.kind,
                    name: source.name.clone(),
                    enabled: source.enabled,
                    settings: source.settings.clone(),
                    filters: source.filters.clone(),
                    extras: retained_source.map_or_else(Map::new, |s| s.extras.clone()),
                }
            })
            .collect();
        let audio = AudioFileV1 {
            buses: carry_extras(
                collection
                    .audio
                    .buses
                    .iter()
                    .cloned()
                    .map(WithExtras::new)
                    .collect(),
                retained.map(|r| r.audio.buses.as_slice()).unwrap_or(&[]),
                |bus| bus.id,
            ),
            routes: carry_extras(
                collection
                    .audio
                    .routes
                    .iter()
                    .cloned()
                    .map(WithExtras::new)
                    .collect(),
                retained.map(|r| r.audio.routes.as_slice()).unwrap_or(&[]),
                |route| (route.source_id, route.bus_id),
            ),
            mixer: collection
                .audio
                .mixer
                .iter()
                .map(|(source_id, state)| {
                    let extras = retained
                        .and_then(|r| r.audio.mixer.get(source_id))
                        .map_or_else(Map::new, |s| s.extras.clone());
                    (
                        *source_id,
                        WithExtras {
                            inner: state.clone(),
                            extras,
                        },
                    )
                })
                .collect(),
            extras: retained.map_or_else(Map::new, |r| r.audio.extras.clone()),
        };
        Self {
            schema_version: CURRENT_COLLECTION_SCHEMA,
            id: collection.id,
            name: collection.name.clone(),
            scenes,
            sources,
            transition: WithExtras {
                inner: collection.transition.clone(),
                extras: retained.map_or_else(Map::new, |r| r.transition.extras.clone()),
            },
            audio,
            session: WithExtras {
                inner: snapshot.session.clone(),
                extras: retained.map_or_else(Map::new, |r| r.session.extras.clone()),
            },
            unknown: retained.map_or_else(Map::new, |r| r.unknown.clone()),
        }
    }

    /// Extracts the domain aggregate, dropping the persistence-only
    /// wrapper structure (unknown fields stay with the retained envelope).
    pub fn to_snapshot(&self) -> CollectionSnapshot {
        CollectionSnapshot {
            collection: SceneCollection {
                id: self.id,
                name: self.name.clone(),
                scenes: self
                    .scenes
                    .iter()
                    .map(|scene| Scene {
                        id: scene.id,
                        name: scene.name.clone(),
                        items: scene.items.iter().map(|item| item.inner.clone()).collect(),
                    })
                    .collect(),
                sources: self
                    .sources
                    .iter()
                    .map(|source| Source {
                        id: source.id,
                        kind: source.kind,
                        name: source.name.clone(),
                        enabled: source.enabled,
                        settings: source.settings.clone(),
                        filters: source.filters.clone(),
                    })
                    .collect(),
                transition: self.transition.inner.clone(),
                audio: AudioMixerConfig {
                    buses: self.audio.buses.iter().map(|b| b.inner.clone()).collect(),
                    routes: self.audio.routes.iter().map(|r| r.inner.clone()).collect(),
                    mixer: self
                        .audio
                        .mixer
                        .iter()
                        .map(|(id, state)| (*id, state.inner.clone()))
                        .collect(),
                },
            },
            session: self.session.inner.clone(),
        }
    }

    /// Serializes to the canonical on-disk form: pretty-printed JSON,
    /// 2-space indent, trailing newline (byte-stable for golden tests).
    pub fn to_json_bytes(&self) -> Result<Vec<u8>> {
        let mut text =
            serde_json::to_string_pretty(self).map_err(|err| PersistenceError::Serialize {
                what: "collection.json",
                reason: err.to_string(),
            })?;
        text.push('\n');
        Ok(text.into_bytes())
    }

    /// Parses, version-checks, migrates, and deserializes a `collection.json`
    /// document. Returns the envelope and whether a migration ran.
    pub fn from_json_bytes(bytes: &[u8], path: &std::path::Path) -> Result<(Self, bool)> {
        let raw: Value = serde_json::from_slice(bytes).map_err(|err| PersistenceError::Parse {
            path: path.into(),
            reason: err.to_string(),
        })?;
        let version = migrate::json_version(&raw, path)?;
        let (raw, migrated) = migrate::migrate_json(
            raw,
            version,
            migrate::COLLECTION_MIGRATIONS,
            CURRENT_COLLECTION_SCHEMA,
            path,
        )?;
        let envelope: Self =
            serde_json::from_value(raw).map_err(|err| PersistenceError::Parse {
                path: path.into(),
                reason: err.to_string(),
            })?;
        Ok((envelope, migrated))
    }
}

/// Re-attaches captured extras from a retained list onto freshly built
/// wrappers, matched by key.
fn carry_extras<T, K: Eq>(
    mut fresh: Vec<WithExtras<T>>,
    retained: &[WithExtras<T>],
    key: impl Fn(&T) -> K,
) -> Vec<WithExtras<T>> {
    for wrapper in &mut fresh {
        if let Some(old) = retained
            .iter()
            .find(|old| key(&old.inner) == key(&wrapper.inner))
        {
            wrapper.extras = old.extras.clone();
        }
    }
    fresh
}

#[cfg(test)]
mod tests {
    use super::*;
    use prismcast_core::audio::TrackMask;
    use prismcast_core::source::SourceKind;

    fn sample_snapshot() -> CollectionSnapshot {
        let mut collection = SceneCollection::new("dev-stream");
        let source = Source::new(SourceKind::TestPattern, "pattern");
        let mut scene = Scene::new("Main");
        scene.add_item(SceneItem::new(source.id, 0));
        let bus = collection.audio.buses[0].id;
        collection.audio.routes.push(AudioRoute {
            source_id: source.id,
            bus_id: bus,
            tracks: TrackMask::stereo_pair(),
        });
        collection
            .audio
            .mixer
            .insert(source.id, AudioMixerState::default());
        let current = scene.id;
        collection.sources.push(source);
        collection.scenes.push(scene);
        CollectionSnapshot {
            collection,
            session: SessionState {
                current_scene: Some(current),
                studio_mode: None,
            },
        }
    }

    #[test]
    fn envelope_roundtrip_preserves_domain() {
        let snapshot = sample_snapshot();
        let envelope = CollectionFileV1::from_snapshot(&snapshot, None);
        let bytes = envelope.to_json_bytes().unwrap();
        assert!(bytes.ends_with(b"\n"));
        let (loaded, migrated) =
            CollectionFileV1::from_json_bytes(&bytes, std::path::Path::new("collection.json"))
                .unwrap();
        assert!(!migrated);
        assert_eq!(envelope, loaded);
        assert_eq!(loaded.to_snapshot(), snapshot);
    }

    #[test]
    fn byte_stable_golden_roundtrip() {
        let snapshot = sample_snapshot();
        let bytes = CollectionFileV1::from_snapshot(&snapshot, None)
            .to_json_bytes()
            .unwrap();
        let (loaded, _) =
            CollectionFileV1::from_json_bytes(&bytes, std::path::Path::new("c.json")).unwrap();
        let resaved = loaded.to_json_bytes().unwrap();
        assert_eq!(bytes, resaved, "save(load(bytes)) must be byte-identical");
    }

    #[test]
    fn unknown_fields_survive_load_save() {
        let snapshot = sample_snapshot();
        let bytes = CollectionFileV1::from_snapshot(&snapshot, None)
            .to_json_bytes()
            .unwrap();
        let mut raw: Value = serde_json::from_slice(&bytes).unwrap();
        // Inject unknown keys at every level.
        raw["futureTopLevel"] = serde_json::json!({"nested": [1, 2, 3]});
        raw["scenes"][0]["futureSceneKey"] = serde_json::json!("scene-extra");
        raw["scenes"][0]["items"][0]["futureItemKey"] = serde_json::json!(42);
        raw["sources"][0]["futureSourceKey"] = serde_json::json!(true);
        raw["audio"]["futureAudioKey"] = serde_json::json!(1.5);
        raw["audio"]["buses"][0]["futureBusKey"] = serde_json::json!("bus-extra");
        let source_id = raw["sources"][0]["id"].as_str().unwrap().to_string();
        raw["audio"]["mixer"][&source_id]["futureMixerKey"] = serde_json::json!("m");
        raw["transition"]["futureTransitionKey"] = serde_json::json!("t");
        raw["session"]["futureSessionKey"] = serde_json::json!("s");
        let injected = serde_json::to_vec_pretty(&raw).unwrap();

        let (loaded, _) =
            CollectionFileV1::from_json_bytes(&injected, std::path::Path::new("c.json")).unwrap();
        // Domain content is unaffected.
        assert_eq!(loaded.to_snapshot(), snapshot);
        // Save again: every unknown key is still present.
        let resaved: Value = serde_json::from_slice(&loaded.to_json_bytes().unwrap()).unwrap();
        assert_eq!(resaved["futureTopLevel"], raw["futureTopLevel"]);
        assert_eq!(
            resaved["scenes"][0]["futureSceneKey"],
            serde_json::json!("scene-extra")
        );
        assert_eq!(
            resaved["scenes"][0]["items"][0]["futureItemKey"],
            serde_json::json!(42)
        );
        assert_eq!(
            resaved["sources"][0]["futureSourceKey"],
            serde_json::json!(true)
        );
        assert_eq!(resaved["audio"]["futureAudioKey"], serde_json::json!(1.5));
        assert_eq!(
            resaved["audio"]["buses"][0]["futureBusKey"],
            serde_json::json!("bus-extra")
        );
        assert_eq!(
            resaved["audio"]["mixer"][&source_id]["futureMixerKey"],
            serde_json::json!("m")
        );
        assert_eq!(
            resaved["transition"]["futureTransitionKey"],
            serde_json::json!("t")
        );
        assert_eq!(
            resaved["session"]["futureSessionKey"],
            serde_json::json!("s")
        );
    }

    #[test]
    fn newer_schema_is_typed_error() {
        let bytes = br#"{"schemaVersion": 99, "id": "x"}"#;
        let err =
            CollectionFileV1::from_json_bytes(bytes, std::path::Path::new("c.json")).unwrap_err();
        assert!(matches!(
            err,
            PersistenceError::NewerSchema {
                found: 99,
                supported: 1,
                ..
            }
        ));
    }

    #[test]
    fn missing_schema_version_is_invalid() {
        let bytes = br#"{"id": "x"}"#;
        let err =
            CollectionFileV1::from_json_bytes(bytes, std::path::Path::new("c.json")).unwrap_err();
        assert!(matches!(err, PersistenceError::MissingSchemaVersion { .. }));
    }
}
