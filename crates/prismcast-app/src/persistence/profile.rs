//! The `profile.toml` envelope: retained-document round-tripping
//! (`docs/architecture/persistence-model.md` §2–§4).
//!
//! Profiles are hand-editable TOML, so instead of a flatten-capture map (which
//! loses comments and key ordering) the loader retains the parsed
//! [`toml_edit::DocumentMut`] and the saver **patches the retained tree** with
//! the in-memory state: known top-level keys (`schemaVersion`, `id`, `name`,
//! `video`, `encoders`, `services`, `outputs`, `settings`) are replaced with
//! freshly serialized values, while unknown sections and keys — and user
//! comments adjacent to them — survive byte-for-byte.
//!
//! Documented trade-off: known sections are rewritten wholesale, so comments
//! or unknown keys *inside* them (e.g. inside an `[[outputs]]` table) are
//! lost on save; unknown **top-level** keys and sections survive byte-for-byte.
//!
//! `Profile.settings` (recording settings and other extra config) is written
//! under a single `[settings]` table — an explicit choice over scattering its
//! members across the top level, so the domain mapping stays unambiguous.
//! TOML cannot represent JSON `null`, so null-valued settings keys are
//! stripped on save (they carry no information in TOML).
//!
//! `Output.state` is runtime-only and never persisted: the envelope writes
//! outputs without lifecycle state, and a loaded output always starts as
//! [`OutputState::Stopped`].

use serde::{Deserialize, Serialize};
use serde_json::Value;
use toml_edit::DocumentMut;

use prismcast_core::id::{EncoderId, OutputId, ServiceId};
use prismcast_core::output::{
    EncoderSettings, Output, OutputKind, OutputState, ReconnectPolicy, Service,
};
use prismcast_core::project::{Profile, VideoConfig};

use super::error::{PersistenceError, Result};
use super::migrate::{self, CURRENT_PROFILE_SCHEMA};

/// Top-level keys the saver owns; everything else in a retained document is
/// preserved verbatim.
const KNOWN_KEYS: &[&str] = &[
    "schemaVersion",
    "id",
    "name",
    "video",
    "encoders",
    "services",
    "outputs",
    "settings",
];

/// The domain-side aggregate the persistence actor saves: a profile plus the
/// output graph that belongs to it (persistence-model §3).
///
/// Note: `AppState` currently holds outputs in a flat top-level map, so the
/// caller fills `outputs` with the whole working set; nesting outputs under
/// the profile in the domain is a tracked prismcast-core follow-up.
#[derive(Debug, Clone, PartialEq)]
pub struct ProfileSnapshot {
    /// The profile (video config, extra settings).
    pub profile: Profile,
    /// Encoder descriptors referenced by outputs.
    pub encoders: Vec<EncoderSettings>,
    /// Streaming service descriptors (stream keys — the file is `0600`).
    pub services: Vec<Service>,
    /// Configured outputs; `state` is stripped on save.
    pub outputs: Vec<Output>,
}

/// Persisted form of an [`Output`] — identical to the domain struct minus the
/// runtime-only lifecycle state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutputFileV1 {
    /// Output ID.
    pub id: OutputId,
    /// Destination kind.
    pub kind: OutputKind,
    /// User-facing name.
    pub name: String,
    /// Video encoder feeding this output.
    pub video_encoder: EncoderId,
    /// Audio encoders feeding this output.
    #[serde(default)]
    pub audio_encoders: Vec<EncoderId>,
    /// Streaming service, for network outputs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service: Option<ServiceId>,
    /// Reconnect/backoff policy.
    pub reconnect_policy: ReconnectPolicy,
}

impl OutputFileV1 {
    fn from_output(output: &Output) -> Self {
        Self {
            id: output.id,
            kind: output.kind,
            name: output.name.clone(),
            video_encoder: output.video_encoder,
            audio_encoders: output.audio_encoders.clone(),
            service: output.service,
            reconnect_policy: output.reconnect_policy,
        }
    }

    fn to_output(&self) -> Output {
        Output {
            id: self.id,
            kind: self.kind,
            name: self.name.clone(),
            video_encoder: self.video_encoder,
            audio_encoders: self.audio_encoders.clone(),
            service: self.service,
            reconnect_policy: self.reconnect_policy,
            // Runtime state is never persisted (persistence-model §3).
            state: OutputState::Stopped,
        }
    }
}

/// Persisted form of an [`EncoderSettings`]. A wrapper (not the domain
/// struct) because TOML cannot express JSON null: `settings = null` is
/// written as an absent key and defaults back to null on load. Unknown keys
/// inside `[[encoders]]` tables are *not* captured — the retained-document
/// saver rewrites known sections wholesale (module docs).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EncoderFileV1 {
    /// Encoder ID.
    pub id: EncoderId,
    /// Codec identifier.
    pub codec: String,
    /// Target bitrate in kbit/s.
    pub bitrate_kbps: u32,
    /// Optional keyframe/GOP interval in frames.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keyframe_interval: Option<u32>,
    /// Codec-specific settings blob (nulls stripped; see module docs).
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub settings: Value,
}

impl EncoderFileV1 {
    fn from_encoder(encoder: &EncoderSettings) -> Self {
        Self {
            id: encoder.id,
            codec: encoder.codec.clone(),
            bitrate_kbps: encoder.bitrate_kbps,
            keyframe_interval: encoder.keyframe_interval,
            settings: toml_safe(&encoder.settings),
        }
    }

    fn to_encoder(&self) -> EncoderSettings {
        EncoderSettings {
            id: self.id,
            codec: self.codec.clone(),
            bitrate_kbps: self.bitrate_kbps,
            keyframe_interval: self.keyframe_interval,
            settings: self.settings.clone(),
        }
    }
}

/// Persisted form of a [`Service`] (see [`EncoderFileV1`] for why).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServiceFileV1 {
    /// Service ID.
    pub id: ServiceId,
    /// User-facing name.
    pub name: String,
    /// Server/ingest URL.
    pub url: String,
    /// Stream key; serialized transparently (the file is `0600`).
    pub key: prismcast_core::output::SecretString,
    /// Service-specific settings blob (nulls stripped; see module docs).
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub settings: Value,
}

impl ServiceFileV1 {
    fn from_service(service: &Service) -> Self {
        Self {
            id: service.id,
            name: service.name.clone(),
            url: service.url.clone(),
            key: service.key.clone(),
            settings: toml_safe(&service.settings),
        }
    }

    fn to_service(&self) -> Service {
        Service {
            id: self.id,
            name: self.name.clone(),
            url: self.url.clone(),
            key: self.key.clone(),
            settings: self.settings.clone(),
        }
    }
}

/// Typed view of the known keys of a V1 `profile.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileFileV1 {
    /// Schema version; always [`CURRENT_PROFILE_SCHEMA`] when written.
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    /// Profile ID (authoritative identity).
    pub id: prismcast_core::id::ProfileId,
    /// User-facing name.
    pub name: String,
    /// Base/output video configuration.
    pub video: VideoConfig,
    /// Encoder descriptors.
    #[serde(default)]
    pub encoders: Vec<EncoderFileV1>,
    /// Streaming services.
    #[serde(default)]
    pub services: Vec<ServiceFileV1>,
    /// Outputs (without runtime state).
    #[serde(default)]
    pub outputs: Vec<OutputFileV1>,
    /// Extra profile settings (recording settings, output defaults).
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub settings: Value,
}

impl ProfileFileV1 {
    fn from_snapshot(snapshot: &ProfileSnapshot) -> Self {
        Self {
            schema_version: CURRENT_PROFILE_SCHEMA,
            id: snapshot.profile.id,
            name: snapshot.profile.name.clone(),
            video: snapshot.profile.video,
            encoders: snapshot
                .encoders
                .iter()
                .map(EncoderFileV1::from_encoder)
                .collect(),
            services: snapshot
                .services
                .iter()
                .map(ServiceFileV1::from_service)
                .collect(),
            outputs: snapshot
                .outputs
                .iter()
                .map(OutputFileV1::from_output)
                .collect(),
            settings: toml_safe(&snapshot.profile.settings),
        }
    }

    fn to_snapshot(&self) -> ProfileSnapshot {
        ProfileSnapshot {
            profile: Profile {
                id: self.id,
                name: self.name.clone(),
                video: self.video,
                settings: self.settings.clone(),
            },
            encoders: self
                .encoders
                .iter()
                .map(EncoderFileV1::to_encoder)
                .collect(),
            services: self
                .services
                .iter()
                .map(ServiceFileV1::to_service)
                .collect(),
            outputs: self.outputs.iter().map(OutputFileV1::to_output).collect(),
        }
    }
}

/// A loaded `profile.toml`: the retained document tree plus the typed view.
///
/// Retaining the document is what lets unknown keys, unknown sections, and
/// comments survive a load → save cycle (persistence-model §4).
#[derive(Debug, Clone)]
pub struct ProfileDocument {
    doc: DocumentMut,
    typed: ProfileFileV1,
    migrated: bool,
}

impl ProfileDocument {
    /// Parses, version-checks, and migrates a `profile.toml` document.
    pub fn parse(bytes: &[u8], path: &std::path::Path) -> Result<Self> {
        let text = std::str::from_utf8(bytes).map_err(|err| PersistenceError::Parse {
            path: path.into(),
            reason: format!("not valid UTF-8: {err}"),
        })?;
        let mut doc: DocumentMut =
            text.parse()
                .map_err(|err: toml_edit::TomlError| PersistenceError::Parse {
                    path: path.into(),
                    reason: err.to_string(),
                })?;
        let version = migrate::toml_version(&doc, path)?;
        let migrated = migrate::migrate_toml(
            &mut doc,
            version,
            migrate::PROFILE_MIGRATIONS,
            CURRENT_PROFILE_SCHEMA,
            path,
        )?;
        let typed: ProfileFileV1 =
            toml_edit::de::from_document(doc.clone()).map_err(|err| PersistenceError::Parse {
                path: path.into(),
                reason: err.to_string(),
            })?;
        Ok(Self {
            doc,
            typed,
            migrated,
        })
    }

    /// The domain aggregate view.
    pub fn snapshot(&self) -> ProfileSnapshot {
        self.typed.to_snapshot()
    }

    /// Whether loading ran a migration (callers re-save at the current
    /// version so migration cost is paid once).
    pub fn migrated(&self) -> bool {
        self.migrated
    }

    /// Serializes a profile snapshot to bytes, patching the retained document
    /// when one is given so unknown keys and comments survive.
    pub fn to_bytes(snapshot: &ProfileSnapshot, retained: Option<&Self>) -> Result<Vec<u8>> {
        let typed = ProfileFileV1::from_snapshot(snapshot);
        // Serialize with the `toml` crate (renders hand-editable `[video]` /
        // `[[outputs]]` sections), then parse into a document tree so the
        // known keys can be patched into the retained document.
        let rendered = toml::to_string(&typed).map_err(|err| PersistenceError::Serialize {
            what: "profile.toml",
            reason: err.to_string(),
        })?;
        let fresh: DocumentMut =
            rendered
                .parse()
                .map_err(|err: toml_edit::TomlError| PersistenceError::Serialize {
                    what: "profile.toml",
                    reason: err.to_string(),
                })?;
        let mut doc = match retained {
            Some(retained) => {
                let mut doc = retained.doc.clone();
                // Known keys are owned by us: replace them with the fresh
                // serialization, keeping every unknown key/section in place.
                for key in KNOWN_KEYS {
                    doc.remove(key);
                }
                doc
            }
            None => DocumentMut::new(),
        };
        for (key, item) in fresh.iter() {
            doc[key] = item.clone();
        }
        let mut text = doc.to_string();
        if !text.ends_with('\n') {
            text.push('\n');
        }
        Ok(text.into_bytes())
    }
}

/// Recursively strips `null`-valued object members: TOML cannot represent
/// JSON null, and a null member carries no information.
fn toml_safe(value: &Value) -> Value {
    match value {
        Value::Object(map) => map
            .iter()
            .filter(|(_, v)| !v.is_null())
            .map(|(k, v)| (k.clone(), toml_safe(v)))
            .collect(),
        Value::Array(items) => Value::Array(items.iter().map(toml_safe).collect()),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use prismcast_core::output::SecretString;

    fn sample_snapshot() -> ProfileSnapshot {
        let mut profile = Profile::new("twitch-1080p", VideoConfig::hd_1080p60());
        profile.settings = serde_json::json!({"recording": {"format": "mkv", "path": "~/Videos"}});
        let video_encoder = EncoderId::new();
        let service_id = ServiceId::new();
        let mut output = Output::new(OutputKind::Rtmp, "Twitch main", video_encoder);
        output.service = Some(service_id);
        output.state = OutputState::Running; // must not persist
        ProfileSnapshot {
            profile,
            encoders: vec![EncoderSettings {
                id: video_encoder,
                codec: "h264".into(),
                bitrate_kbps: 6000,
                keyframe_interval: Some(120),
                settings: serde_json::json!({"preset": "veryfast"}),
            }],
            services: vec![Service {
                id: service_id,
                name: "Twitch".into(),
                url: "rtmps://live.twitch.tv/app".into(),
                key: SecretString::new("live_secret_key"),
                settings: Value::Null,
            }],
            outputs: vec![output],
        }
    }

    #[test]
    fn profile_roundtrip_preserves_domain_and_strips_state() {
        let snapshot = sample_snapshot();
        let bytes = ProfileDocument::to_bytes(&snapshot, None).unwrap();
        let text = String::from_utf8(bytes.clone()).unwrap();
        assert!(text.contains("schemaVersion = 1"));
        assert!(text.contains("live_secret_key"), "secrets must persist");
        let doc = ProfileDocument::parse(&bytes, std::path::Path::new("profile.toml")).unwrap();
        assert!(!doc.migrated());
        let loaded = doc.snapshot();
        assert_eq!(loaded.profile, snapshot.profile);
        assert_eq!(loaded.encoders, snapshot.encoders);
        assert_eq!(loaded.services, snapshot.services);
        assert_eq!(loaded.outputs.len(), 1);
        let mut expected_output = snapshot.outputs[0].clone();
        expected_output.state = OutputState::Stopped;
        assert_eq!(loaded.outputs[0], expected_output);
    }

    #[test]
    fn unknown_keys_and_comments_survive() {
        let snapshot = sample_snapshot();
        let bytes = ProfileDocument::to_bytes(&snapshot, None).unwrap();
        let mut text = String::from_utf8(bytes).unwrap();
        // A hand-added top-level section (a bare `key = value` at the end of
        // the file would belong to the last `[table]` in TOML).
        text.push_str("\n# my hand-written comment\n[futureSection]\nx = 1\n");
        let doc =
            ProfileDocument::parse(text.as_bytes(), std::path::Path::new("profile.toml")).unwrap();
        // Save with modified state; unknowns and comments must survive.
        let mut updated = doc.snapshot();
        updated.profile.name = "renamed".into();
        let resaved = ProfileDocument::to_bytes(&updated, Some(&doc)).unwrap();
        let resaved_text = String::from_utf8_lossy(&resaved).to_string();
        assert!(resaved_text.contains("# my hand-written comment"));
        assert!(resaved_text.contains("[futureSection]"));
        assert!(resaved_text.contains("x = 1"));
        assert!(resaved_text.contains("name = \"renamed\""));
        let reparsed =
            ProfileDocument::parse(&resaved, std::path::Path::new("profile.toml")).unwrap();
        assert_eq!(reparsed.snapshot().profile.name, "renamed");
    }

    #[test]
    fn newer_schema_is_typed_error() {
        let bytes = b"schemaVersion = 99\nid = \"x\"\n";
        let err = ProfileDocument::parse(bytes, std::path::Path::new("profile.toml")).unwrap_err();
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
    fn null_settings_keys_are_stripped_for_toml() {
        let mut snapshot = sample_snapshot();
        snapshot.profile.settings = serde_json::json!({"keep": 1, "drop": null});
        let bytes = ProfileDocument::to_bytes(&snapshot, None).unwrap();
        let doc = ProfileDocument::parse(&bytes, std::path::Path::new("p.toml")).unwrap();
        assert_eq!(
            doc.snapshot().profile.settings,
            serde_json::json!({"keep": 1})
        );
    }
}
