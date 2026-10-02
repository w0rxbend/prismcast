//! Domain event → obs `Event` (op 5) translation (OBSWS-001 event slice;
//! RES-007 §Domain surface; ADR-0020 §c/§e).
//!
//! Every translated event carries the obs `eventIntent` bit a client would
//! filter on upstream (see [`super::bitmask`]); gating itself happens in the
//! session by native category *before* this translation runs, so the intent
//! is informational and may be narrower than the bit that admitted the event
//! (e.g. `SceneItemEnableStateChanged` reports `SceneItems` but is admitted
//! by `Scenes` too, since both map to the native `Scene` category).
//!
//! ## Mapping table
//!
//! | domain event | obs event | intent |
//! |---|---|---|
//! | `SceneEvent::Added` | `SceneCreated` | `Scenes` |
//! | `SceneEvent::Removed` | `SceneRemoved` | `Scenes` |
//! | `SceneEvent::Renamed` | `SceneNameChanged` | `Scenes` |
//! | `SceneEvent::CurrentChanged` | `CurrentProgramSceneChanged` | `Scenes` |
//! | `SceneEvent::ItemAdded` | `SceneItemCreated` | `SceneItems` |
//! | `SceneEvent::ItemRemoved` | `SceneItemRemoved` | `SceneItems` |
//! | `SceneEvent::ItemUpdated` (visibility flip only) | `SceneItemEnableStateChanged` | `SceneItems` |
//! | `SourceEvent::Added` | `InputCreated` | `Inputs` |
//! | `SourceEvent::Removed` | `InputRemoved` | `Inputs` |
//! | `SourceEvent::Renamed` | `InputNameChanged` | `Inputs` |
//! | `AudioEvent::MixerChanged` (mute flip) | `InputMuteStateChanged` | `Inputs` |
//! | `AudioEvent::MixerChanged` (volume change) | `InputVolumeChanged` | `Inputs` |
//! | `OutputEvent::StateChanged` | `OutputStateChanged`* | `Outputs` |
//! | + emitting output is the stream primary | `StreamStateChanged` | `Outputs` |
//! | + emitting output is the record primary | `RecordStateChanged` | `Outputs` |
//! | `SystemEvent::StudioModeChanged` | `StudioModeStateChanged` | `Ui` |
//! | `SystemEvent::PreviewSceneChanged` | `CurrentPreviewSceneChanged` | `Scenes` |
//!
//! *`OutputStateChanged` is a **Prismcast extension**: upstream obs-websocket
//! has no per-output state event (its outputs are singletons); the payload
//! mirrors `GetOutputStatus` addressing (`outputName`/`outputUuid`) with the
//! upstream `OBS_WEBSOCKET_OUTPUT_*` state vocabulary.
//!
//! Everything else (`SceneEvent::Reordered`, source settings/enabled/runtime,
//! audio bus/route changes, output add/remove/policy, transitions, profiles,
//! collections) has no obs-websocket 5.x counterpart in the MVP scope and is
//! never emitted.
//!
//! ## Why a translator state (not a registry)
//!
//! obs `eventData` addresses entities by *name*, but domain events carry
//! typed IDs — and removal events fire after the snapshot no longer contains
//! the entity. Name resolution is therefore snapshot-first (stateless scan,
//! ADR-0020 §c) with a per-session memo of last-known names as the fallback
//! for already-removed entities. The same memo records prior mute/volume and
//! item-visibility values, because `MixerChanged`/`ItemUpdated` carry the
//! full state and the *kind* of change must be detected by diffing. Numeric
//! `sceneItemId`s come from the server-wide [`ItemIdMap`] (ADR-0020 §c), so
//! events and request responses agree; the translator memoizes the minted
//! number per item so `SceneItemRemoved` still reports it even when the
//! map's eviction listener (or an eager request-side eviction) processed the
//! removal first.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::json;
use tracing::debug;
use uuid::Uuid;

use prismcast_app::snapshot::AppSnapshot;
use prismcast_core::event::{AudioEvent, Event, OutputEvent, SceneEvent, SourceEvent, SystemEvent};
use prismcast_core::id::{OutputId, SceneId, SceneItemId, SourceId};
use prismcast_core::output::{OutputKind, OutputState};
use prismcast_core::source::SourceKind;

use super::names::ItemIdMap;
use super::proto::{self, subscription};

/// Per-session event translation state: last-known names (for entities the
/// post-event snapshot can no longer name) and prior values for diff-based
/// change detection. Seeded from the snapshot at session start and on every
/// `Reidentify`; maintained incrementally from the events that pass the
/// subscription gate.
#[derive(Debug)]
pub(crate) struct EventTranslator {
    /// Server-wide numeric `sceneItemId` registry (shared with requests).
    item_ids: Arc<ItemIdMap>,
    /// Last-known name per scene/source/output UUID.
    names: HashMap<Uuid, String>,
    /// Last-known mute/volume per source UUID (MixerChanged diffing).
    mixer: HashMap<Uuid, MixerMemo>,
    /// Last-known visibility per scene-item UUID (ItemUpdated diffing).
    item_visible: HashMap<Uuid, bool>,
    /// Scene membership per scene-item UUID, for eviction on scene removal.
    item_scene: HashMap<Uuid, Uuid>,
    /// The minted `sceneItemId` per scene-item UUID, as emitted to this
    /// session; memoized so removal events survive eviction-listener races.
    item_numbers: HashMap<Uuid, u64>,
}

/// Last-known mixer values relevant to obs events.
#[derive(Debug, Clone, Copy)]
struct MixerMemo {
    muted: bool,
    volume_db: f32,
}

impl EventTranslator {
    pub(crate) fn new(item_ids: Arc<ItemIdMap>) -> Self {
        Self {
            item_ids,
            names: HashMap::new(),
            mixer: HashMap::new(),
            item_visible: HashMap::new(),
            item_scene: HashMap::new(),
            item_numbers: HashMap::new(),
        }
    }

    /// Records the current names/values of every entity in the snapshot, so
    /// renames and removals of pre-existing entities resolve correctly.
    pub(crate) fn seed(&mut self, snapshot: &AppSnapshot) {
        for scene in snapshot.scenes() {
            self.names.insert(*scene.id.as_uuid(), scene.name.clone());
            for item in &scene.items {
                self.item_visible.insert(*item.id.as_uuid(), item.visible);
                self.item_scene
                    .insert(*item.id.as_uuid(), *scene.id.as_uuid());
                // Mint (idempotently) so removals of pre-existing items
                // report a stable number even if no event named one yet.
                let number = self.item_ids.mint(scene.id, item.id);
                self.item_numbers.insert(*item.id.as_uuid(), number);
            }
        }
        for source in snapshot.sources() {
            let mixer = snapshot.state().audio.mixer_state(source.id);
            self.names.insert(*source.id.as_uuid(), source.name.clone());
            self.mixer.insert(
                *source.id.as_uuid(),
                MixerMemo {
                    muted: mixer.muted,
                    volume_db: mixer.volume_db,
                },
            );
        }
        for output in snapshot.outputs() {
            self.names.insert(*output.id.as_uuid(), output.name.clone());
        }
    }

    /// Translates one committed domain event into the obs events it implies
    /// (0..=2: e.g. a primary stream output state change yields
    /// `OutputStateChanged` + `StreamStateChanged`; a mixer change can yield
    /// both a mute and a volume event). Events with no obs-websocket 5.x
    /// counterpart yield nothing.
    pub(crate) fn event_to_obs(
        &mut self,
        event: &Event,
        snapshot: &AppSnapshot,
    ) -> Vec<proto::Event> {
        match event {
            Event::Scene(event) => self.scene_event(event, snapshot),
            Event::Source(event) => self.source_event(event, snapshot),
            Event::Audio(event) => self.audio_event(event, snapshot),
            Event::Output(event) => self.output_event(event, snapshot),
            Event::System(event) => self.system_event(event, snapshot),
        }
    }

    /// Remembers a freshly observed name.
    fn remember(&mut self, uuid: Uuid, name: &str) {
        self.names.insert(uuid, name.to_string());
    }

    /// Resolves an entity name: the snapshot is authoritative for entities
    /// that still exist (and refreshes the memo); the memo covers removed
    /// ones. When neither knows the entity (events lost to lag before any
    /// seeding), the UUID string is a stable, unique fallback.
    fn name_or(&mut self, uuid: Uuid, current: Option<&str>) -> String {
        if let Some(name) = current {
            self.remember(uuid, name);
            return name.to_string();
        }
        match self.names.get(&uuid) {
            Some(name) => name.clone(),
            None => {
                debug!(%uuid, "entity name unknown (events lost to lag?); falling back to the UUID");
                uuid.to_string()
            }
        }
    }

    fn scene_name(&mut self, snapshot: &AppSnapshot, scene_id: SceneId) -> String {
        let current = snapshot.scene(scene_id).map(|scene| scene.name.clone());
        self.name_or(*scene_id.as_uuid(), current.as_deref())
    }

    fn source_name(&mut self, snapshot: &AppSnapshot, source_id: SourceId) -> String {
        let current = snapshot.source(source_id).map(|source| source.name.clone());
        self.name_or(*source_id.as_uuid(), current.as_deref())
    }

    /// The obs `sceneItemId` for an item: the memoized number this session
    /// already emitted, else an idempotent `mint` on the shared map (so the
    /// number agrees with the request path even when this session never
    /// emitted the item's creation).
    fn item_number(&mut self, scene_id: SceneId, item_id: SceneItemId) -> u64 {
        let uuid = *item_id.as_uuid();
        if let Some(number) = self.item_numbers.get(&uuid) {
            return *number;
        }
        let number = self.item_ids.mint(scene_id, item_id);
        self.item_numbers.insert(uuid, number);
        number
    }

    fn scene_event(&mut self, event: &SceneEvent, snapshot: &AppSnapshot) -> Vec<proto::Event> {
        match event {
            SceneEvent::Added { scene_id, name } => {
                self.remember(*scene_id.as_uuid(), name);
                vec![obs_event(
                    "SceneCreated",
                    subscription::SCENES,
                    json!({
                        "sceneName": name,
                        "sceneUuid": scene_id.as_uuid().to_string(),
                        "isGroup": false,
                    }),
                )]
            }
            SceneEvent::Removed { scene_id } => {
                let uuid = *scene_id.as_uuid();
                // The snapshot no longer contains the scene; the memo does.
                let name = self.name_or(uuid, None);
                self.names.remove(&uuid);
                let items: Vec<Uuid> = self
                    .item_scene
                    .iter()
                    .filter(|(_, scene)| **scene == uuid)
                    .map(|(item, _)| *item)
                    .collect();
                for item in items {
                    self.item_scene.remove(&item);
                    self.item_visible.remove(&item);
                    self.item_numbers.remove(&item);
                }
                vec![obs_event(
                    "SceneRemoved",
                    subscription::SCENES,
                    json!({
                        "sceneName": name,
                        "sceneUuid": uuid.to_string(),
                        "isGroup": false,
                    }),
                )]
            }
            SceneEvent::Renamed { scene_id, name } => {
                let old = self
                    .names
                    .insert(*scene_id.as_uuid(), name.clone())
                    // Lag can cost us the old name; reporting the new one is
                    // the least surprising fallback.
                    .unwrap_or_else(|| name.clone());
                vec![obs_event(
                    "SceneNameChanged",
                    subscription::SCENES,
                    json!({
                        "sceneUuid": scene_id.as_uuid().to_string(),
                        "oldSceneName": old,
                        "sceneName": name,
                    }),
                )]
            }
            SceneEvent::CurrentChanged { scene_id } => {
                let name = self.scene_name(snapshot, *scene_id);
                vec![obs_event(
                    "CurrentProgramSceneChanged",
                    subscription::SCENES,
                    json!({
                        "sceneName": name,
                        "sceneUuid": scene_id.as_uuid().to_string(),
                    }),
                )]
            }
            // Upstream has no scene-reorder event (SceneListChanged is never
            // fired for reordering upstream either).
            SceneEvent::Reordered => Vec::new(),
            SceneEvent::ItemAdded { scene_id, item } => {
                let scene_uuid = *scene_id.as_uuid();
                let item_uuid = *item.id.as_uuid();
                self.item_visible.insert(item_uuid, item.visible);
                self.item_scene.insert(item_uuid, scene_uuid);
                let number = self.item_number(*scene_id, item.id);
                let scene_name = self.scene_name(snapshot, *scene_id);
                let source_name = self.source_name(snapshot, item.source_id);
                let index = snapshot
                    .scene(*scene_id)
                    .and_then(|scene| scene.items.iter().position(|i| i.id == item.id))
                    .map_or_else(|| i64::from(item.z_index), |index| index as i64);
                vec![obs_event(
                    "SceneItemCreated",
                    subscription::SCENE_ITEMS,
                    json!({
                        "sceneName": scene_name,
                        "sceneUuid": scene_uuid.to_string(),
                        "sourceName": source_name,
                        "sourceUuid": item.source_id.as_uuid().to_string(),
                        "sceneItemId": number,
                        "sceneItemIndex": index,
                    }),
                )]
            }
            SceneEvent::ItemRemoved {
                scene_id,
                item_id,
                source_id,
            } => {
                let item_uuid = *item_id.as_uuid();
                self.item_visible.remove(&item_uuid);
                self.item_scene.remove(&item_uuid);
                // The memoized number survives the shared map's eviction
                // (the server-wide listener or an eager request-side
                // `evict_item` may have processed this removal first).
                let number = match self.item_numbers.remove(&item_uuid) {
                    Some(number) => number,
                    // Never emitted to this session (lagged/unsubscribed at
                    // creation): mint is the graceful fallback — idempotent
                    // while the number is still registered.
                    None => self.item_ids.mint(*scene_id, *item_id),
                };
                let scene_name = self.scene_name(snapshot, *scene_id);
                let source_name = self.source_name(snapshot, *source_id);
                vec![obs_event(
                    "SceneItemRemoved",
                    subscription::SCENE_ITEMS,
                    json!({
                        "sceneName": scene_name,
                        "sceneUuid": scene_id.as_uuid().to_string(),
                        "sourceName": source_name,
                        "sourceUuid": source_id.as_uuid().to_string(),
                        "sceneItemId": number,
                    }),
                )]
            }
            SceneEvent::ItemUpdated { scene_id, item } => {
                let item_uuid = *item.id.as_uuid();
                self.item_scene.insert(item_uuid, *scene_id.as_uuid());
                let prior = self.item_visible.insert(item_uuid, item.visible);
                if prior == Some(item.visible) {
                    // Transform/crop/opacity/lock/bounds/z-index change: no
                    // obs event in MVP scope (SceneItemTransformChanged is a
                    // high-volume opt-in upstream, deferred to OBSWS-002+).
                    return Vec::new();
                }
                let number = self.item_number(*scene_id, item.id);
                let scene_name = self.scene_name(snapshot, *scene_id);
                vec![obs_event(
                    "SceneItemEnableStateChanged",
                    subscription::SCENE_ITEMS,
                    json!({
                        "sceneName": scene_name,
                        "sceneUuid": scene_id.as_uuid().to_string(),
                        "sceneItemId": number,
                        "sceneItemEnabled": item.visible,
                    }),
                )]
            }
        }
    }

    fn source_event(&mut self, event: &SourceEvent, snapshot: &AppSnapshot) -> Vec<proto::Event> {
        match event {
            SourceEvent::Added { source } => {
                let uuid = *source.id.as_uuid();
                self.remember(uuid, &source.name);
                let mixer = snapshot.state().audio.mixer_state(source.id);
                self.mixer.insert(
                    uuid,
                    MixerMemo {
                        muted: mixer.muted,
                        volume_db: mixer.volume_db,
                    },
                );
                let kind = obs_input_kind(source.kind);
                vec![obs_event(
                    "InputCreated",
                    subscription::INPUTS,
                    json!({
                        "inputName": source.name,
                        "inputUuid": uuid.to_string(),
                        "inputKind": kind,
                        // Prismcast kinds carry no `_vN` versioning.
                        "unversionedInputKind": kind,
                        "inputSettings": object_or_empty(&source.settings),
                        // The domain does not model per-kind default settings.
                        "defaultInputSettings": json!({}),
                    }),
                )]
            }
            SourceEvent::Removed { source_id } => {
                let uuid = *source_id.as_uuid();
                let name = self.name_or(uuid, None);
                self.names.remove(&uuid);
                self.mixer.remove(&uuid);
                vec![obs_event(
                    "InputRemoved",
                    subscription::INPUTS,
                    json!({
                        "inputName": name,
                        "inputUuid": uuid.to_string(),
                    }),
                )]
            }
            SourceEvent::Renamed { source_id, name } => {
                let old = self
                    .names
                    .insert(*source_id.as_uuid(), name.clone())
                    .unwrap_or_else(|| name.clone());
                vec![obs_event(
                    "InputNameChanged",
                    subscription::INPUTS,
                    json!({
                        "inputUuid": source_id.as_uuid().to_string(),
                        "oldInputName": old,
                        "inputName": name,
                    }),
                )]
            }
            // `SettingsChanged` → `InputSettingsChanged` and
            // `EnabledChanged` → `InputActiveStateChanged` (high-volume
            // opt-in upstream) are deferred to OBSWS-002+; capture
            // authorization/runtime have no obs counterpart.
            SourceEvent::SettingsChanged { .. }
            | SourceEvent::EnabledChanged { .. }
            | SourceEvent::CaptureAuthorizationRequested { .. }
            | SourceEvent::RuntimeChanged { .. } => Vec::new(),
        }
    }

    fn audio_event(&mut self, event: &AudioEvent, snapshot: &AppSnapshot) -> Vec<proto::Event> {
        match event {
            AudioEvent::MixerChanged { source_id, state } => {
                let uuid = *source_id.as_uuid();
                if snapshot.source(*source_id).is_none() {
                    // The source is gone: this is the mixer-entry reset that
                    // `RemoveSource` cascades. Upstream emits no mute/volume
                    // events for a removed input.
                    self.mixer.remove(&uuid);
                    return Vec::new();
                }
                let prior = self.mixer.insert(
                    uuid,
                    MixerMemo {
                        muted: state.muted,
                        volume_db: state.volume_db,
                    },
                );
                let mute_changed = prior.is_none_or(|p| p.muted != state.muted);
                let volume_changed = prior.is_none_or(|p| p.volume_db != state.volume_db);
                if !mute_changed && !volume_changed {
                    // Solo/monitor/balance/sync-offset change.
                    return Vec::new();
                }
                let name = self.source_name(snapshot, *source_id);
                let input_uuid = uuid.to_string();
                let mut events = Vec::with_capacity(2);
                if mute_changed {
                    events.push(obs_event(
                        "InputMuteStateChanged",
                        subscription::INPUTS,
                        json!({
                            "inputName": name,
                            "inputUuid": input_uuid,
                            "inputMuted": state.muted,
                        }),
                    ));
                }
                if volume_changed {
                    let volume_db = f64::from(state.volume_db);
                    events.push(obs_event(
                        "InputVolumeChanged",
                        subscription::INPUTS,
                        json!({
                            "inputName": name,
                            "inputUuid": input_uuid,
                            "inputVolumeMul": volume_mul(state.volume_db),
                            "inputVolumeDb": volume_db,
                        }),
                    ));
                }
                events
            }
            // Buses and routing have no obs-websocket counterpart (upstream's
            // InputAudioTracksChanged is per-input track assignment, not bus
            // routing).
            AudioEvent::BusAdded { .. }
            | AudioEvent::BusRemoved { .. }
            | AudioEvent::RouteChanged { .. }
            | AudioEvent::RouteRemoved { .. } => Vec::new(),
        }
    }

    fn output_event(&mut self, event: &OutputEvent, snapshot: &AppSnapshot) -> Vec<proto::Event> {
        match event {
            OutputEvent::Added { output_id, name } => {
                self.remember(*output_id.as_uuid(), name);
                Vec::new()
            }
            OutputEvent::Removed { output_id } => {
                self.names.remove(output_id.as_uuid());
                Vec::new()
            }
            OutputEvent::ReconnectPolicyChanged { .. } => Vec::new(),
            OutputEvent::StateChanged { output_id, state } => {
                // `Degraded` has no obs state; everything else maps onto the
                // upstream vocabulary (`Failed` is terminal-not-running, which
                // upstream reports as STOPPED).
                let Some((output_state, output_active)) = obs_output_state(*state) else {
                    return Vec::new();
                };
                let uuid = *output_id.as_uuid();
                let name = {
                    let current = snapshot.output(*output_id).map(|o| o.name.clone());
                    self.name_or(uuid, current.as_deref())
                };
                let mut events = vec![obs_event(
                    "OutputStateChanged",
                    subscription::OUTPUTS,
                    json!({
                        "outputName": name,
                        "outputUuid": uuid.to_string(),
                        "outputState": output_state,
                    }),
                )];
                if primary_stream_output(snapshot) == Some(*output_id) {
                    events.push(obs_event(
                        "StreamStateChanged",
                        subscription::OUTPUTS,
                        json!({
                            "outputActive": output_active,
                            "outputState": output_state,
                        }),
                    ));
                }
                if primary_record_output(snapshot) == Some(*output_id) {
                    events.push(obs_event(
                        "RecordStateChanged",
                        subscription::OUTPUTS,
                        json!({
                            "outputActive": output_active,
                            "outputState": output_state,
                            // The domain does not model the record path yet.
                            "outputPath": serde_json::Value::Null,
                        }),
                    ));
                }
                events
            }
        }
    }

    fn system_event(&mut self, event: &SystemEvent, snapshot: &AppSnapshot) -> Vec<proto::Event> {
        match event {
            SystemEvent::StudioModeChanged { enabled } => vec![obs_event(
                "StudioModeStateChanged",
                subscription::UI,
                json!({ "studioModeEnabled": enabled }),
            )],
            SystemEvent::PreviewSceneChanged { scene_id } => {
                let name = self.scene_name(snapshot, *scene_id);
                vec![obs_event(
                    "CurrentPreviewSceneChanged",
                    subscription::SCENES,
                    json!({
                        "sceneName": name,
                        "sceneUuid": scene_id.as_uuid().to_string(),
                    }),
                )]
            }
            // Transition/profile/collection events are deferred to OBSWS-002+.
            _ => Vec::new(),
        }
    }
}

/// Builds one obs event with a payload.
fn obs_event(event_type: &str, intent: u32, data: serde_json::Value) -> proto::Event {
    proto::Event {
        event_type: event_type.to_string(),
        event_intent: intent,
        event_data: Some(data),
    }
}

/// Maps a domain output state onto the upstream `OBS_WEBSOCKET_OUTPUT_*`
/// vocabulary plus the `outputActive` flag of the singleton events. `None`
/// for `Degraded`, which has no obs counterpart.
fn obs_output_state(state: OutputState) -> Option<(&'static str, bool)> {
    Some(match state {
        OutputState::Stopped | OutputState::Failed => ("OBS_WEBSOCKET_OUTPUT_STOPPED", false),
        OutputState::Starting => ("OBS_WEBSOCKET_OUTPUT_STARTING", false),
        OutputState::Running => ("OBS_WEBSOCKET_OUTPUT_STARTED", true),
        OutputState::Reconnecting { .. } => ("OBS_WEBSOCKET_OUTPUT_RECONNECTING", true),
        OutputState::Stopping => ("OBS_WEBSOCKET_OUTPUT_STOPPING", true),
        OutputState::Degraded => return None,
    })
}

/// The designated primary stream output (ADR-0020 §e): the first `Rtmp`
/// output, falling back to the first `Srt`/`Whip` output, in insertion order.
pub(crate) fn primary_stream_output(snapshot: &AppSnapshot) -> Option<OutputId> {
    snapshot
        .outputs()
        .find(|output| output.kind == OutputKind::Rtmp)
        .or_else(|| {
            snapshot
                .outputs()
                .find(|output| matches!(output.kind, OutputKind::Srt | OutputKind::Whip))
        })
        .map(|output| output.id)
}

/// The designated primary record output (ADR-0020 §e): the first `Recording`.
pub(crate) fn primary_record_output(snapshot: &AppSnapshot) -> Option<OutputId> {
    snapshot
        .outputs()
        .find(|output| output.kind == OutputKind::Recording)
        .map(|output| output.id)
}

/// Multiplier form of a dB gain (obs carries both; the core stores dB).
/// `volumeDb = 20 * log10(volumeMul)`.
fn volume_mul(volume_db: f32) -> f64 {
    10_f64.powf(f64::from(volume_db) / 20.0)
}

/// obs requires `inputSettings` to be an object; sources with unset settings
/// (`Null`) report an empty object.
fn object_or_empty(settings: &serde_json::Value) -> serde_json::Value {
    if settings.is_object() {
        settings.clone()
    } else {
        json!({})
    }
}

/// obs source-kind strings for Prismcast source kinds. Real OBS source IDs
/// are used where a counterpart exists; `prismcast_*` marks kinds with none.
fn obs_input_kind(kind: SourceKind) -> &'static str {
    match kind {
        SourceKind::PipeWireDisplay => "pipewire_screen_capture",
        SourceKind::PipeWireWindow => "pipewire_window_capture",
        SourceKind::V4l2Camera => "v4l2_input",
        SourceKind::PipeWireAudioInput => "pulse_input_capture",
        SourceKind::PipeWireAppAudio => "pulse_output_capture",
        SourceKind::MediaFile => "ffmpeg_source",
        SourceKind::Image => "image_source",
        SourceKind::ImageSlideshow => "slideshow",
        SourceKind::Color => "color_source",
        SourceKind::Text => "text_ft2_source",
        SourceKind::Browser => "browser_source",
        SourceKind::Scene(_) => "scene",
        SourceKind::TestPattern => "prismcast_test_pattern",
        SourceKind::NetworkStream => "prismcast_network_stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_mul_is_db_to_linear() {
        assert!((volume_mul(0.0) - 1.0).abs() < f64::EPSILON);
        assert!((volume_mul(-6.0) - 0.501_187_233_627_272_2).abs() < 1e-12);
        assert!((volume_mul(6.0) - 1.995_262_314_968_879_5).abs() < 1e-12);
        // Very negative gains approach zero without going negative or NaN.
        assert!(volume_mul(-1000.0) > 0.0);
    }

    #[test]
    fn output_state_vocabulary() {
        assert_eq!(
            obs_output_state(OutputState::Stopped),
            Some(("OBS_WEBSOCKET_OUTPUT_STOPPED", false))
        );
        assert_eq!(
            obs_output_state(OutputState::Starting),
            Some(("OBS_WEBSOCKET_OUTPUT_STARTING", false))
        );
        assert_eq!(
            obs_output_state(OutputState::Running),
            Some(("OBS_WEBSOCKET_OUTPUT_STARTED", true))
        );
        assert_eq!(
            obs_output_state(OutputState::Reconnecting { attempt: 2 }),
            Some(("OBS_WEBSOCKET_OUTPUT_RECONNECTING", true))
        );
        assert_eq!(
            obs_output_state(OutputState::Stopping),
            Some(("OBS_WEBSOCKET_OUTPUT_STOPPING", true))
        );
        assert_eq!(
            obs_output_state(OutputState::Failed),
            Some(("OBS_WEBSOCKET_OUTPUT_STOPPED", false))
        );
        assert_eq!(obs_output_state(OutputState::Degraded), None);
    }

    #[test]
    fn input_kinds_are_stable_strings() {
        assert_eq!(obs_input_kind(SourceKind::Color), "color_source");
        assert_eq!(obs_input_kind(SourceKind::V4l2Camera), "v4l2_input");
        assert_eq!(obs_input_kind(SourceKind::Scene(SceneId::new())), "scene");
    }

    /// The removal event must report the number the session already
    /// emitted, even when the shared map's eviction ran first (the
    /// server-wide eviction listener and request-side eager eviction are
    /// independent of the session's event pipe, so either ordering occurs).
    #[tokio::test]
    async fn removal_event_keeps_the_minted_number_after_eviction() {
        use prismcast_app::{AppHandle, CoreConfig};
        use prismcast_core::Command;

        let app = AppHandle::spawn(CoreConfig::default());
        let response = app
            .dispatch(Command::AddScene {
                name: "Main".into(),
            })
            .await
            .expect("add scene");
        let scene_id = match &response.events[0] {
            Event::Scene(SceneEvent::Added { scene_id, .. }) => *scene_id,
            other => panic!("unexpected {other:?}"),
        };
        let response = app
            .dispatch(Command::AddSource {
                kind: SourceKind::Color,
                name: "Mic".into(),
            })
            .await
            .expect("add source");
        let source_id = match &response.events[0] {
            Event::Source(SourceEvent::Added { source }) => source.id,
            other => panic!("unexpected {other:?}"),
        };

        let map = ItemIdMap::shared();
        let mut translator = EventTranslator::new(map.clone());
        translator.seed(&app.snapshot());

        let response = app
            .dispatch(Command::AddSceneItem {
                scene_id,
                source_id,
            })
            .await
            .expect("add item");
        let added = response.events[0].clone();
        let item_id = match &added {
            Event::Scene(SceneEvent::ItemAdded { item, .. }) => item.id,
            other => panic!("unexpected {other:?}"),
        };
        let created = translator.event_to_obs(&added, &app.snapshot());
        let number = created[0]
            .event_data
            .as_ref()
            .and_then(|data| data["sceneItemId"].as_u64())
            .expect("SceneItemCreated carries sceneItemId");
        assert_eq!(number, map.mint(scene_id, item_id), "event mints the map");

        // Eviction wins the race: the map no longer knows the number.
        let response = app
            .dispatch(Command::RemoveSceneItem { scene_id, item_id })
            .await
            .expect("remove item");
        let removed = response.events[0].clone();
        map.apply_event(&removed);
        assert_eq!(map.resolve(scene_id, number), None, "evicted");

        let events = translator.event_to_obs(&removed, &app.snapshot());
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "SceneItemRemoved");
        assert_eq!(
            events[0]
                .event_data
                .as_ref()
                .and_then(|data| data["sceneItemId"].as_u64()),
            Some(number),
            "removal event still carries the minted number"
        );
        // The translator does not re-mint into the shared map.
        assert_eq!(map.resolve(scene_id, number), None);
        app.shutdown().await;
    }
}
