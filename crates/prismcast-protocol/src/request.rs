//! Requests: every mutating command and read query a client can send.
//!
//! The command variants mirror `prismcast_core::Command` one-to-one (same
//! snake_case names, same fields with wire types, PLAN.md §76's single write
//! path exposed over the wire) but are distinct types — see [`crate::data`]
//! for why. The mirror is enforced by `tests/command_coverage.rs`, which
//! fails if a core command has no wire representation.
//!
//! Beyond commands, the protocol adds **queries** (read-only) and session
//! requests (`update_subscriptions`). Commands and queries share one enum so
//! batches and correlation work uniformly.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::data::{
    Bounds, Crop, MonitorMode, Output, PlacementExpectation, Profile, ReconnectPolicy,
    SceneCollection, SourceKind, TrackMask, Transform, Transition,
};
use crate::subscription::SubscriptionSet;

/// A single client request, correlated by a client-supplied ID.
///
/// Serializes flat: `{"request_id": "...", "request": "add_scene", ...}`.
/// The `request_id` is opaque to the server and echoed back verbatim in the
/// response (clients conventionally use UUIDs or monotonic counters).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    /// Client-chosen correlation ID, echoed in the response.
    pub request_id: String,
    /// The operation to perform.
    #[serde(flatten)]
    pub kind: RequestKind,
}

/// The operation of a [`Request`].
///
/// Command variants (including `undo`/`redo`) map 1:1 onto
/// `prismcast_core::Command`; query variants (`get_*`, `list_*`) are
/// read-only; `update_subscriptions` manages the session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "request", rename_all = "snake_case")]
pub enum RequestKind {
    // --- Scenes (Command mirror) ---
    /// Creates an empty scene.
    AddScene {
        /// User-facing name.
        name: String,
    },
    /// Removes a scene and all its items. Irreversible.
    RemoveScene {
        /// Scene to remove.
        scene_id: Uuid,
    },
    /// Renames a scene.
    RenameScene {
        /// Scene to rename.
        scene_id: Uuid,
        /// New name.
        name: String,
    },
    /// Moves a scene within the scene list.
    ReorderScene {
        /// Scene to move.
        scene_id: Uuid,
        /// Target index in the scene list (clamped).
        new_index: usize,
    },
    /// Switches the current (program) scene.
    SetCurrentScene {
        /// Scene to make current.
        scene_id: Uuid,
    },

    // --- Scene items (Command mirror) ---
    /// Adds a source to a scene as a new item (on top of the z-order).
    AddSceneItem {
        /// Scene receiving the item.
        scene_id: Uuid,
        /// Shared source to place.
        source_id: Uuid,
    },
    /// Removes an item from a scene. Irreversible.
    RemoveSceneItem {
        /// Scene containing the item.
        scene_id: Uuid,
        /// Item to remove.
        item_id: Uuid,
    },
    /// Duplicates an item (new ID, placed directly above the original).
    DuplicateSceneItem {
        /// Scene containing the item.
        scene_id: Uuid,
        /// Item to duplicate.
        item_id: Uuid,
    },
    /// Replaces an item's transform.
    SetSceneItemTransform {
        /// Scene containing the item.
        scene_id: Uuid,
        /// Item to transform.
        item_id: Uuid,
        /// New transform.
        transform: Transform,
    },
    /// Atomically replaces an item's transform when the placement context
    /// still matches `expect` (ADR-0026). Top-level only: rejected with
    /// `invalid_request` as a `transaction` member. A stale expectation is
    /// rejected with `state_conflict` 500 and `field` naming the mismatched
    /// expectation member (e.g. `expect.transform`); nothing is changed.
    SetSceneItemTransformIf {
        /// Scene containing the item.
        scene_id: Uuid,
        /// Item to transform.
        item_id: Uuid,
        /// New transform.
        transform: Transform,
        /// Placement context the edit was computed from.
        expect: PlacementExpectation,
    },
    /// Replaces an item's crop.
    SetSceneItemCrop {
        /// Scene containing the item.
        scene_id: Uuid,
        /// Item to crop.
        item_id: Uuid,
        /// New crop.
        crop: Crop,
    },
    /// Shows or hides an item.
    SetSceneItemVisible {
        /// Scene containing the item.
        scene_id: Uuid,
        /// Item to show/hide.
        item_id: Uuid,
        /// New visibility.
        visible: bool,
    },
    /// Locks or unlocks an item.
    SetSceneItemLocked {
        /// Scene containing the item.
        scene_id: Uuid,
        /// Item to lock/unlock.
        item_id: Uuid,
        /// New lock state.
        locked: bool,
    },
    /// Sets an item's z-index directly.
    SetSceneItemZIndex {
        /// Scene containing the item.
        scene_id: Uuid,
        /// Item to reorder.
        item_id: Uuid,
        /// New z-index.
        z_index: i32,
    },
    /// Raises an item one step in the z-order.
    RaiseSceneItem {
        /// Scene containing the item.
        scene_id: Uuid,
        /// Item to raise.
        item_id: Uuid,
    },
    /// Lowers an item one step in the z-order.
    LowerSceneItem {
        /// Scene containing the item.
        scene_id: Uuid,
        /// Item to lower.
        item_id: Uuid,
    },
    /// Sets an item's opacity.
    SetSceneItemOpacity {
        /// Scene containing the item.
        scene_id: Uuid,
        /// Item to change.
        item_id: Uuid,
        /// Opacity in `[0.0, 1.0]`.
        opacity: f32,
    },
    /// Sets an item's bounds fitting.
    SetSceneItemBounds {
        /// Scene containing the item.
        scene_id: Uuid,
        /// Item to change.
        item_id: Uuid,
        /// New bounds.
        bounds: Bounds,
    },

    // --- Sources (Command mirror) ---
    /// Creates a shared source.
    AddSource {
        /// Source implementation kind.
        kind: SourceKind,
        /// User-facing name.
        name: String,
    },
    /// Removes a source. Rejected while referenced.
    RemoveSource {
        /// Source to remove.
        source_id: Uuid,
    },
    /// Renames a source.
    RenameSource {
        /// Source to rename.
        source_id: Uuid,
        /// New name.
        name: String,
    },
    /// Replaces a source's kind-specific settings.
    SetSourceSettings {
        /// Source to configure.
        source_id: Uuid,
        /// New settings.
        settings: serde_json::Value,
    },
    /// Enables or disables a source.
    SetSourceEnabled {
        /// Source to toggle.
        source_id: Uuid,
        /// New enabled state.
        enabled: bool,
    },

    /// Explicitly initiates or retries display/window portal authorization.
    AuthorizeSourceCapture {
        /// Shared source to authorize; parent context stays local.
        source_id: Uuid,
    },

    // --- Audio (Command mirror) ---
    /// Sets a source's mixer volume.
    SetSourceVolume {
        /// Source to adjust.
        source_id: Uuid,
        /// Gain in decibels.
        volume_db: f32,
    },
    /// Mutes or unmutes a source.
    SetSourceMuted {
        /// Source to mute/unmute.
        source_id: Uuid,
        /// New mute state.
        muted: bool,
    },
    /// Solos or unsolos a source.
    SetSourceSolo {
        /// Source to solo/unsolo.
        source_id: Uuid,
        /// New solo state.
        solo: bool,
    },
    /// Sets a source's monitoring mode.
    SetSourceMonitor {
        /// Source to configure.
        source_id: Uuid,
        /// New monitor mode.
        monitor: MonitorMode,
    },
    /// Sets a source's stereo balance.
    SetSourceBalance {
        /// Source to adjust.
        source_id: Uuid,
        /// Balance in `[-1.0, 1.0]`.
        balance: f32,
    },
    /// Sets a source's A/V sync offset.
    SetSourceSyncOffset {
        /// Source to adjust.
        source_id: Uuid,
        /// Offset in milliseconds; positive delays the audio.
        sync_offset_ms: i32,
    },
    /// Adds a named audio bus.
    AddAudioBus {
        /// Bus name.
        name: String,
    },
    /// Removes an audio bus and its routes. Irreversible.
    RemoveAudioBus {
        /// Bus to remove.
        bus_id: Uuid,
    },
    /// Assigns (or replaces) a source's route into a bus.
    SetAudioRoute {
        /// Source to route.
        source_id: Uuid,
        /// Destination bus.
        bus_id: Uuid,
        /// Output tracks of the bus this route feeds.
        tracks: TrackMask,
    },
    /// Removes a source's route into a bus.
    RemoveAudioRoute {
        /// Source to unroute.
        source_id: Uuid,
        /// Bus to unroute from.
        bus_id: Uuid,
    },

    // --- Outputs (Command mirror) ---
    /// Adds a configured output to the graph (initially `Stopped`).
    AddOutput {
        /// The output to add. Its `id` and `state` fields are assigned by
        /// the server; client-supplied values are ignored.
        output: Output,
    },
    /// Removes an output. Must be `Stopped`. Irreversible.
    RemoveOutput {
        /// Output to remove.
        output_id: Uuid,
    },
    /// Starts an output. Legal from `Stopped` or `Failed` only.
    StartOutput {
        /// Output to start.
        output_id: Uuid,
    },
    /// Stops an output.
    StopOutput {
        /// Output to stop.
        output_id: Uuid,
    },
    /// Replaces an output's reconnect policy.
    SetOutputReconnectPolicy {
        /// Output to configure.
        output_id: Uuid,
        /// New policy.
        policy: ReconnectPolicy,
    },

    // --- Studio mode (Command mirror) ---
    /// Enables or disables studio (dual-scene) mode.
    SetStudioModeEnabled {
        /// New enabled state.
        enabled: bool,
    },
    /// Sets the preview scene (studio mode must be enabled).
    SetPreviewScene {
        /// Scene to preview; must exist and differ from program.
        scene_id: Uuid,
    },
    /// Runs the configured transition: preview becomes program.
    TransitionToProgram,
    /// Instantly swaps preview and program.
    SwapPreviewProgram,

    // --- Transitions (Command mirror) ---
    /// Replaces the default transition configuration.
    SetTransition {
        /// New transition.
        transition: Transition,
    },

    // --- Profiles & collections (Command mirror) ---
    /// Adds a profile. Its `id` field is assigned by the server.
    AddProfile {
        /// The profile to add.
        profile: Profile,
    },
    /// Removes a profile. The active profile cannot be removed.
    /// Irreversible.
    RemoveProfile {
        /// Profile to remove.
        profile_id: Uuid,
    },
    /// Selects the active profile.
    SelectProfile {
        /// Profile to activate.
        profile_id: Uuid,
    },
    /// Adds a scene collection. Its `id` field is assigned by the server.
    AddSceneCollection {
        /// The collection to add.
        collection: SceneCollection,
    },
    /// Removes a scene collection. The active collection cannot be removed.
    /// Irreversible.
    RemoveSceneCollection {
        /// Collection to remove.
        collection_id: Uuid,
    },
    /// Selects the active scene collection.
    SelectSceneCollection {
        /// Collection to activate.
        collection_id: Uuid,
    },

    /// Applies several commands atomically, in order: either all succeed or
    /// none are applied (maps to `prismcast_core::Command::Transaction`,
    /// PLAN.md §59). Members must be command variants, not queries;
    /// nesting `Transaction` or history commands inside `Transaction` is rejected.
    Transaction {
        /// Commands to apply as one unit.
        commands: Vec<RequestKind>,
    },

    /// Replays the latest inverse from the application's global history.
    /// Requires authorization for every operation replayed; not a transaction member.
    Undo,
    /// Replays the latest forward entry from the application's global history.
    /// Requires authorization for every operation replayed; not a transaction member.
    Redo,

    // --- Queries (read-only; no core Command counterpart) ---
    /// Server versions and the list of request types available at the
    /// negotiated protocol version (capability discovery).
    GetVersion,
    /// The full initial state snapshot (PLAN.md §23: snapshot + event
    /// stream; clients never re-fetch wholesale after this).
    GetSnapshot,
    /// Lists all scenes in working-set order.
    ListScenes,
    /// Fetches one scene.
    GetScene {
        /// Scene to fetch.
        scene_id: Uuid,
    },
    /// Lists all shared sources.
    ListSources,
    /// Fetches one source.
    GetSource {
        /// Source to fetch.
        source_id: Uuid,
    },
    /// Lists all outputs with their lifecycle state.
    ListOutputs,
    /// Fetches one output.
    GetOutput {
        /// Output to fetch.
        output_id: Uuid,
    },
    /// Lists audio buses, routes, and mixer state.
    GetAudioState,
    /// Lists profiles and the active profile.
    ListProfiles,
    /// Lists scene collections (summaries: full scenes/sources are served
    /// via `get_snapshot`) and the active collection.
    ListSceneCollections,

    // --- Session ---
    /// Replaces the session's event subscriptions (the native protocol's
    /// answer to obs-websocket's `Reidentify`, as a correlatable, errorable
    /// request). Also returns the applied set.
    UpdateSubscriptions {
        /// The new subscription set.
        subscriptions: SubscriptionSet,
    },
    /// Returns the session's current subscription set.
    GetSubscriptions,
}

impl RequestKind {
    /// Returns the wire tag (`request` field value) of this variant, for
    /// logging and for mirroring the request type in responses.
    ///
    /// Kept in sync with the serde tag by the
    /// `tag_matches_serde_tag_for_all_variants` test.
    pub fn tag(&self) -> &'static str {
        match self {
            Self::AddScene { .. } => "add_scene",
            Self::RemoveScene { .. } => "remove_scene",
            Self::RenameScene { .. } => "rename_scene",
            Self::ReorderScene { .. } => "reorder_scene",
            Self::SetCurrentScene { .. } => "set_current_scene",
            Self::AddSceneItem { .. } => "add_scene_item",
            Self::RemoveSceneItem { .. } => "remove_scene_item",
            Self::DuplicateSceneItem { .. } => "duplicate_scene_item",
            Self::SetSceneItemTransform { .. } => "set_scene_item_transform",
            Self::SetSceneItemTransformIf { .. } => "set_scene_item_transform_if",
            Self::SetSceneItemCrop { .. } => "set_scene_item_crop",
            Self::SetSceneItemVisible { .. } => "set_scene_item_visible",
            Self::SetSceneItemLocked { .. } => "set_scene_item_locked",
            Self::SetSceneItemZIndex { .. } => "set_scene_item_z_index",
            Self::RaiseSceneItem { .. } => "raise_scene_item",
            Self::LowerSceneItem { .. } => "lower_scene_item",
            Self::SetSceneItemOpacity { .. } => "set_scene_item_opacity",
            Self::SetSceneItemBounds { .. } => "set_scene_item_bounds",
            Self::AddSource { .. } => "add_source",
            Self::RemoveSource { .. } => "remove_source",
            Self::RenameSource { .. } => "rename_source",
            Self::SetSourceSettings { .. } => "set_source_settings",
            Self::AuthorizeSourceCapture { .. } => "authorize_source_capture",
            Self::SetSourceEnabled { .. } => "set_source_enabled",
            Self::SetSourceVolume { .. } => "set_source_volume",
            Self::SetSourceMuted { .. } => "set_source_muted",
            Self::SetSourceSolo { .. } => "set_source_solo",
            Self::SetSourceMonitor { .. } => "set_source_monitor",
            Self::SetSourceBalance { .. } => "set_source_balance",
            Self::SetSourceSyncOffset { .. } => "set_source_sync_offset",
            Self::AddAudioBus { .. } => "add_audio_bus",
            Self::RemoveAudioBus { .. } => "remove_audio_bus",
            Self::SetAudioRoute { .. } => "set_audio_route",
            Self::RemoveAudioRoute { .. } => "remove_audio_route",
            Self::AddOutput { .. } => "add_output",
            Self::RemoveOutput { .. } => "remove_output",
            Self::StartOutput { .. } => "start_output",
            Self::StopOutput { .. } => "stop_output",
            Self::SetOutputReconnectPolicy { .. } => "set_output_reconnect_policy",
            Self::SetStudioModeEnabled { .. } => "set_studio_mode_enabled",
            Self::SetPreviewScene { .. } => "set_preview_scene",
            Self::TransitionToProgram => "transition_to_program",
            Self::SwapPreviewProgram => "swap_preview_program",
            Self::SetTransition { .. } => "set_transition",
            Self::AddProfile { .. } => "add_profile",
            Self::RemoveProfile { .. } => "remove_profile",
            Self::SelectProfile { .. } => "select_profile",
            Self::AddSceneCollection { .. } => "add_scene_collection",
            Self::RemoveSceneCollection { .. } => "remove_scene_collection",
            Self::SelectSceneCollection { .. } => "select_scene_collection",
            Self::Transaction { .. } => "transaction",
            Self::Undo => "undo",
            Self::Redo => "redo",
            Self::GetVersion => "get_version",
            Self::GetSnapshot => "get_snapshot",
            Self::ListScenes => "list_scenes",
            Self::GetScene { .. } => "get_scene",
            Self::ListSources => "list_sources",
            Self::GetSource { .. } => "get_source",
            Self::ListOutputs => "list_outputs",
            Self::GetOutput { .. } => "get_output",
            Self::GetAudioState => "get_audio_state",
            Self::ListProfiles => "list_profiles",
            Self::ListSceneCollections => "list_scene_collections",
            Self::UpdateSubscriptions { .. } => "update_subscriptions",
            Self::GetSubscriptions => "get_subscriptions",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_requests() -> Vec<RequestKind> {
        let scene = Uuid::new_v4();
        let item = Uuid::new_v4();
        let source = Uuid::new_v4();
        let bus = Uuid::new_v4();
        let output = Uuid::new_v4();
        vec![
            RequestKind::Undo,
            RequestKind::Redo,
            RequestKind::AddScene {
                name: "Main".into(),
            },
            RequestKind::RemoveScene { scene_id: scene },
            RequestKind::RenameScene {
                scene_id: scene,
                name: "Renamed".into(),
            },
            RequestKind::ReorderScene {
                scene_id: scene,
                new_index: 2,
            },
            RequestKind::SetCurrentScene { scene_id: scene },
            RequestKind::AddSceneItem {
                scene_id: scene,
                source_id: source,
            },
            RequestKind::RemoveSceneItem {
                scene_id: scene,
                item_id: item,
            },
            RequestKind::DuplicateSceneItem {
                scene_id: scene,
                item_id: item,
            },
            RequestKind::SetSceneItemTransform {
                scene_id: scene,
                item_id: item,
                transform: Transform::default(),
            },
            RequestKind::SetSceneItemTransformIf {
                scene_id: scene,
                item_id: item,
                transform: Transform::default(),
                expect: PlacementExpectation {
                    current_scene: scene,
                    active_profile: Uuid::new_v4(),
                    video: crate::data::VideoConfig {
                        width: 1920,
                        height: 1080,
                        fps_num: 60,
                        fps_den: 1,
                    },
                    transform: Transform::default(),
                    crop: Crop::default(),
                    bounds: Bounds::default(),
                    locked: false,
                    source_dimensions: Some(crate::data::SourceDimensions {
                        width: 1920,
                        height: 1080,
                    }),
                },
            },
            RequestKind::SetSceneItemCrop {
                scene_id: scene,
                item_id: item,
                crop: Crop {
                    left: 1,
                    top: 2,
                    right: 3,
                    bottom: 4,
                },
            },
            RequestKind::SetSceneItemVisible {
                scene_id: scene,
                item_id: item,
                visible: false,
            },
            RequestKind::SetSceneItemLocked {
                scene_id: scene,
                item_id: item,
                locked: true,
            },
            RequestKind::SetSceneItemZIndex {
                scene_id: scene,
                item_id: item,
                z_index: 7,
            },
            RequestKind::RaiseSceneItem {
                scene_id: scene,
                item_id: item,
            },
            RequestKind::LowerSceneItem {
                scene_id: scene,
                item_id: item,
            },
            RequestKind::SetSceneItemOpacity {
                scene_id: scene,
                item_id: item,
                opacity: 0.5,
            },
            RequestKind::SetSceneItemBounds {
                scene_id: scene,
                item_id: item,
                bounds: Bounds::default(),
            },
            RequestKind::AddSource {
                kind: SourceKind::V4l2Camera,
                name: "cam".into(),
            },
            RequestKind::RemoveSource { source_id: source },
            RequestKind::RenameSource {
                source_id: source,
                name: "cam2".into(),
            },
            RequestKind::SetSourceSettings {
                source_id: source,
                settings: serde_json::json!({"device": "/dev/video0"}),
            },
            RequestKind::AuthorizeSourceCapture { source_id: source },
            RequestKind::SetSourceEnabled {
                source_id: source,
                enabled: false,
            },
            RequestKind::SetSourceVolume {
                source_id: source,
                volume_db: -3.0,
            },
            RequestKind::SetSourceMuted {
                source_id: source,
                muted: true,
            },
            RequestKind::SetSourceSolo {
                source_id: source,
                solo: true,
            },
            RequestKind::SetSourceMonitor {
                source_id: source,
                monitor: MonitorMode::MonitorOnly,
            },
            RequestKind::SetSourceBalance {
                source_id: source,
                balance: -0.5,
            },
            RequestKind::SetSourceSyncOffset {
                source_id: source,
                sync_offset_ms: 80,
            },
            RequestKind::AddAudioBus { name: "VOD".into() },
            RequestKind::RemoveAudioBus { bus_id: bus },
            RequestKind::SetAudioRoute {
                source_id: source,
                bus_id: bus,
                tracks: TrackMask::stereo_pair(),
            },
            RequestKind::RemoveAudioRoute {
                source_id: source,
                bus_id: bus,
            },
            RequestKind::AddOutput {
                output: Output {
                    id: Uuid::nil(),
                    kind: crate::data::OutputKind::Recording,
                    name: "rec".into(),
                    video_encoder: Uuid::new_v4(),
                    audio_encoders: Vec::new(),
                    service: None,
                    reconnect_policy: ReconnectPolicy {
                        max_retries: 10,
                        initial_backoff_ms: 1_000,
                        max_backoff_ms: 30_000,
                    },
                    state: crate::data::OutputState::Stopped,
                },
            },
            RequestKind::RemoveOutput { output_id: output },
            RequestKind::StartOutput { output_id: output },
            RequestKind::StopOutput { output_id: output },
            RequestKind::SetOutputReconnectPolicy {
                output_id: output,
                policy: ReconnectPolicy {
                    max_retries: 3,
                    initial_backoff_ms: 500,
                    max_backoff_ms: 5_000,
                },
            },
            RequestKind::SetStudioModeEnabled { enabled: true },
            RequestKind::SetPreviewScene { scene_id: scene },
            RequestKind::TransitionToProgram,
            RequestKind::SwapPreviewProgram,
            RequestKind::SetTransition {
                transition: Transition::default(),
            },
            RequestKind::AddProfile {
                profile: Profile {
                    id: Uuid::nil(),
                    name: "p".into(),
                    video: crate::data::VideoConfig {
                        width: 1920,
                        height: 1080,
                        fps_num: 60,
                        fps_den: 1,
                    },
                    settings: serde_json::Value::Null,
                },
            },
            RequestKind::RemoveProfile {
                profile_id: Uuid::new_v4(),
            },
            RequestKind::SelectProfile {
                profile_id: Uuid::new_v4(),
            },
            RequestKind::AddSceneCollection {
                collection: SceneCollection {
                    id: Uuid::nil(),
                    name: "c".into(),
                    scenes: Vec::new(),
                    sources: Vec::new(),
                    transition: Transition::default(),
                    audio: crate::data::AudioMixerConfig {
                        buses: Vec::new(),
                        routes: Vec::new(),
                        mixer: Vec::new(),
                    },
                },
            },
            RequestKind::RemoveSceneCollection {
                collection_id: Uuid::new_v4(),
            },
            RequestKind::SelectSceneCollection {
                collection_id: Uuid::new_v4(),
            },
            RequestKind::Transaction {
                commands: vec![
                    RequestKind::SetSourceMuted {
                        source_id: source,
                        muted: true,
                    },
                    RequestKind::RaiseSceneItem {
                        scene_id: scene,
                        item_id: item,
                    },
                ],
            },
            RequestKind::GetVersion,
            RequestKind::GetSnapshot,
            RequestKind::ListScenes,
            RequestKind::GetScene { scene_id: scene },
            RequestKind::ListSources,
            RequestKind::GetSource { source_id: source },
            RequestKind::ListOutputs,
            RequestKind::GetOutput { output_id: output },
            RequestKind::GetAudioState,
            RequestKind::ListProfiles,
            RequestKind::ListSceneCollections,
            RequestKind::UpdateSubscriptions {
                subscriptions: SubscriptionSet::default_all(),
            },
            RequestKind::GetSubscriptions,
        ]
    }

    #[test]
    fn request_serde_roundtrip_all_variants() {
        for kind in sample_requests() {
            let request = Request {
                request_id: "req-1".into(),
                kind,
            };
            let json = serde_json::to_string(&request).unwrap();
            let back: Request =
                serde_json::from_str(&json).unwrap_or_else(|e| panic!("deserialize {json}: {e}"));
            assert_eq!(request, back, "roundtrip {json}");
        }
    }

    #[test]
    fn tag_matches_serde_tag_for_all_variants() {
        for kind in sample_requests() {
            let value = serde_json::to_value(&kind).unwrap();
            let expected = value["request"].as_str().unwrap().to_string();
            assert_eq!(kind.tag(), expected, "tag() drifted for {expected}");
            assert_ne!(kind.tag(), "unknown", "unknown tag for {expected}");
        }
    }

    #[test]
    fn request_serializes_flat() {
        let request = Request {
            request_id: "abc".into(),
            kind: RequestKind::SetSourceMuted {
                source_id: Uuid::nil(),
                muted: true,
            },
        };
        let value = serde_json::to_value(&request).unwrap();
        assert_eq!(value["request_id"], "abc");
        assert_eq!(value["request"], "set_source_muted");
        assert_eq!(value["muted"], true);
        assert!(value.get("kind").is_none(), "no nesting on the wire");
    }
}
