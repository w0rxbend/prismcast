//! The [`Command`] enum: every user-visible mutation, one variant each
//! (PLAN.md §20, §76; ADR-0005).
//!
//! Commands are the single write path for all controllers (GTK, CLI,
//! WebSocket, IPC, web UI, plugins) and the unit of undo/redo: each variant
//! carries the data needed to apply the change, and
//! [`crate::state::AppState::inverse`] reconstructs the inverse command from
//! the pre-change state (PLAN.md §59). Irreversible variants (destructive
//! removals) return `None` there; destructive cascades return an explicit
//! transaction group instead.

use serde::{Deserialize, Serialize};

use crate::audio::{MonitorMode, TrackMask};
use crate::id::{
    AudioBusId, OutputId, ProfileId, SceneCollectionId, SceneId, SceneItemId, SourceId,
};
use crate::output::ReconnectPolicy;
use crate::scene::{Bounds, Crop, Transform};
use crate::source::SourceKind;
use crate::transition::Transition;

/// A mutating operation on the application state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum Command {
    /// Replays the latest undoable entry through the application history owner.
    Undo,
    /// Replays the latest undone entry through the application history owner.
    Redo,
    // --- Scenes ---
    /// Creates an empty scene.
    AddScene {
        /// User-facing name.
        name: String,
    },
    /// Removes a scene and all its items. Irreversible.
    RemoveScene {
        /// Scene to remove.
        scene_id: SceneId,
    },
    /// Renames a scene.
    RenameScene {
        /// Scene to rename.
        scene_id: SceneId,
        /// New name.
        name: String,
    },
    /// Moves a scene within the scene list.
    ReorderScene {
        /// Scene to move.
        scene_id: SceneId,
        /// Target index in the scene list (clamped).
        new_index: usize,
    },
    /// Switches the current (program) scene.
    SetCurrentScene {
        /// Scene to make current.
        scene_id: SceneId,
    },

    // --- Scene items ---
    /// Adds a source to a scene as a new item (on top of the z-order).
    AddSceneItem {
        /// Scene receiving the item.
        scene_id: SceneId,
        /// Shared source to place.
        source_id: SourceId,
    },
    /// Removes an item from a scene. Irreversible.
    RemoveSceneItem {
        /// Scene containing the item.
        scene_id: SceneId,
        /// Item to remove.
        item_id: SceneItemId,
    },
    /// Duplicates an item (new ID, placed directly above the original).
    DuplicateSceneItem {
        /// Scene containing the item.
        scene_id: SceneId,
        /// Item to duplicate.
        item_id: SceneItemId,
    },
    /// Replaces an item's transform.
    SetSceneItemTransform {
        /// Scene containing the item.
        scene_id: SceneId,
        /// Item to transform.
        item_id: SceneItemId,
        /// New transform.
        transform: Transform,
    },
    /// Replaces an item's crop.
    SetSceneItemCrop {
        /// Scene containing the item.
        scene_id: SceneId,
        /// Item to crop.
        item_id: SceneItemId,
        /// New crop.
        crop: Crop,
    },
    /// Shows or hides an item.
    SetSceneItemVisible {
        /// Scene containing the item.
        scene_id: SceneId,
        /// Item to show/hide.
        item_id: SceneItemId,
        /// New visibility.
        visible: bool,
    },
    /// Locks or unlocks an item.
    SetSceneItemLocked {
        /// Scene containing the item.
        scene_id: SceneId,
        /// Item to lock/unlock.
        item_id: SceneItemId,
        /// New lock state.
        locked: bool,
    },
    /// Sets an item's z-index directly.
    SetSceneItemZIndex {
        /// Scene containing the item.
        scene_id: SceneId,
        /// Item to reorder.
        item_id: SceneItemId,
        /// New z-index.
        z_index: i32,
    },
    /// Raises an item one step in the z-order.
    RaiseSceneItem {
        /// Scene containing the item.
        scene_id: SceneId,
        /// Item to raise.
        item_id: SceneItemId,
    },
    /// Lowers an item one step in the z-order.
    LowerSceneItem {
        /// Scene containing the item.
        scene_id: SceneId,
        /// Item to lower.
        item_id: SceneItemId,
    },
    /// Sets an item's opacity.
    SetSceneItemOpacity {
        /// Scene containing the item.
        scene_id: SceneId,
        /// Item to change.
        item_id: SceneItemId,
        /// Opacity in `[0.0, 1.0]`.
        opacity: f32,
    },
    /// Sets an item's bounds fitting.
    SetSceneItemBounds {
        /// Scene containing the item.
        scene_id: SceneId,
        /// Item to change.
        item_id: SceneItemId,
        /// New bounds.
        bounds: Bounds,
    },

    // --- Sources ---
    /// Creates a shared source.
    AddSource {
        /// Source implementation kind.
        kind: SourceKind,
        /// User-facing name.
        name: String,
    },
    /// Removes a source. Rejected while referenced (scene items, scene-source
    /// nesting, audio routes); see `docs` on `crate::state` for the
    /// delete policy.
    RemoveSource {
        /// Source to remove.
        source_id: SourceId,
    },
    /// Renames a source.
    RenameSource {
        /// Source to rename.
        source_id: SourceId,
        /// New name.
        name: String,
    },
    /// Replaces a source's kind-specific settings.
    SetSourceSettings {
        /// Source to configure.
        source_id: SourceId,
        /// New settings.
        settings: serde_json::Value,
    },
    /// Enables or disables a source.
    SetSourceEnabled {
        /// Source to toggle.
        source_id: SourceId,
        /// New enabled state.
        enabled: bool,
    },

    /// Explicitly requests or retries capture authorization for an enabled capture source.
    AuthorizeSourceCapture {
        /// Shared source to authorize.
        source_id: SourceId,
    },

    // --- Audio ---
    /// Sets a source's mixer volume.
    SetSourceVolume {
        /// Source to adjust.
        source_id: SourceId,
        /// Gain in decibels.
        volume_db: f32,
    },
    /// Mutes or unmutes a source.
    SetSourceMuted {
        /// Source to mute/unmute.
        source_id: SourceId,
        /// New mute state.
        muted: bool,
    },
    /// Solos or unsolos a source.
    SetSourceSolo {
        /// Source to solo/unsolo.
        source_id: SourceId,
        /// New solo state.
        solo: bool,
    },
    /// Sets a source's monitoring mode.
    SetSourceMonitor {
        /// Source to configure.
        source_id: SourceId,
        /// New monitor mode.
        monitor: MonitorMode,
    },
    /// Sets a source's stereo balance.
    SetSourceBalance {
        /// Source to adjust.
        source_id: SourceId,
        /// Balance in `[-1.0, 1.0]`.
        balance: f32,
    },
    /// Sets a source's A/V sync offset.
    SetSourceSyncOffset {
        /// Source to adjust.
        source_id: SourceId,
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
        bus_id: AudioBusId,
    },
    /// Assigns (or replaces) a source's route into a bus.
    SetAudioRoute {
        /// Source to route.
        source_id: SourceId,
        /// Destination bus.
        bus_id: AudioBusId,
        /// Output tracks of the bus this route feeds.
        tracks: TrackMask,
    },
    /// Removes a source's route into a bus.
    RemoveAudioRoute {
        /// Source to unroute.
        source_id: SourceId,
        /// Bus to unroute from.
        bus_id: AudioBusId,
    },

    // --- Outputs ---
    /// Adds a configured output to the graph (initially `Stopped`).
    AddOutput {
        /// The output to add.
        output: crate::output::Output,
    },
    /// Removes an output. Must be `Stopped`. Irreversible.
    RemoveOutput {
        /// Output to remove.
        output_id: OutputId,
    },
    /// Starts an output. Legal from `Stopped` or `Failed` only.
    StartOutput {
        /// Output to start.
        output_id: OutputId,
    },
    /// Stops an output. Legal from `Starting`, `Running`, `Degraded`,
    /// or `Reconnecting`; `Stopped`/`Stopping`/`Failed` are rejected.
    StopOutput {
        /// Output to stop.
        output_id: OutputId,
    },
    /// Replaces an output's reconnect policy.
    SetOutputReconnectPolicy {
        /// Output to configure.
        output_id: OutputId,
        /// New policy.
        policy: ReconnectPolicy,
    },

    // --- Studio mode ---
    /// Enables or disables studio (dual-scene) mode.
    SetStudioModeEnabled {
        /// New enabled state.
        enabled: bool,
    },
    /// Sets the preview scene (studio mode must be enabled).
    SetPreviewScene {
        /// Scene to preview; must exist and differ from program.
        scene_id: SceneId,
    },
    /// Runs the configured transition: preview becomes program.
    TransitionToProgram,
    /// Instantly swaps preview and program.
    SwapPreviewProgram,

    // --- Transitions ---
    /// Replaces the default transition configuration.
    SetTransition {
        /// New transition.
        transition: Transition,
    },

    // --- Profiles & collections ---
    /// Adds a profile.
    AddProfile {
        /// The profile to add.
        profile: crate::project::Profile,
    },
    /// Removes a profile. The active profile cannot be removed. Irreversible.
    RemoveProfile {
        /// Profile to remove.
        profile_id: ProfileId,
    },
    /// Selects the active profile.
    SelectProfile {
        /// Profile to activate.
        profile_id: ProfileId,
    },
    /// Adds a scene collection.
    AddSceneCollection {
        /// The collection to add.
        collection: crate::project::SceneCollection,
    },
    /// Removes a scene collection. The active collection cannot be removed.
    /// Irreversible.
    RemoveSceneCollection {
        /// Collection to remove.
        collection_id: SceneCollectionId,
    },
    /// Selects the active scene collection.
    SelectSceneCollection {
        /// Collection to activate.
        collection_id: SceneCollectionId,
    },

    /// Applies several commands atomically, in order: either all succeed or
    /// none are applied (PLAN.md §59 transaction groups). Inverse is the
    /// reversed group of per-command inverses.
    Transaction {
        /// Commands to apply as one unit.
        commands: Vec<Command>,
    },
}

impl Command {
    /// Returns a short, human-readable name for the operation (for undo
    /// labels, logs, and remote UIs).
    pub fn label(&self) -> &'static str {
        match self {
            Self::Undo => "undo",
            Self::Redo => "redo",
            Self::AddScene { .. } => "add scene",
            Self::RemoveScene { .. } => "remove scene",
            Self::RenameScene { .. } => "rename scene",
            Self::ReorderScene { .. } => "reorder scene",
            Self::SetCurrentScene { .. } => "set current scene",
            Self::AddSceneItem { .. } => "add scene item",
            Self::RemoveSceneItem { .. } => "remove scene item",
            Self::DuplicateSceneItem { .. } => "duplicate scene item",
            Self::SetSceneItemTransform { .. } => "transform scene item",
            Self::SetSceneItemCrop { .. } => "crop scene item",
            Self::SetSceneItemVisible { .. } => "set scene item visibility",
            Self::SetSceneItemLocked { .. } => "lock scene item",
            Self::SetSceneItemZIndex { .. } => "set scene item z-order",
            Self::RaiseSceneItem { .. } => "raise scene item",
            Self::LowerSceneItem { .. } => "lower scene item",
            Self::SetSceneItemOpacity { .. } => "set scene item opacity",
            Self::SetSceneItemBounds { .. } => "set scene item bounds",
            Self::AddSource { .. } => "add source",
            Self::RemoveSource { .. } => "remove source",
            Self::RenameSource { .. } => "rename source",
            Self::SetSourceSettings { .. } => "configure source",
            Self::AuthorizeSourceCapture { .. } => "authorize source capture",
            Self::SetSourceEnabled { .. } => "enable source",
            Self::SetSourceVolume { .. } => "set source volume",
            Self::SetSourceMuted { .. } => "mute source",
            Self::SetSourceSolo { .. } => "solo source",
            Self::SetSourceMonitor { .. } => "set source monitoring",
            Self::SetSourceBalance { .. } => "set source balance",
            Self::SetSourceSyncOffset { .. } => "set source sync offset",
            Self::AddAudioBus { .. } => "add audio bus",
            Self::RemoveAudioBus { .. } => "remove audio bus",
            Self::SetAudioRoute { .. } => "set audio route",
            Self::RemoveAudioRoute { .. } => "remove audio route",
            Self::AddOutput { .. } => "add output",
            Self::RemoveOutput { .. } => "remove output",
            Self::StartOutput { .. } => "start output",
            Self::StopOutput { .. } => "stop output",
            Self::SetOutputReconnectPolicy { .. } => "set reconnect policy",
            Self::SetStudioModeEnabled { .. } => "toggle studio mode",
            Self::SetPreviewScene { .. } => "set preview scene",
            Self::TransitionToProgram => "transition to program",
            Self::SwapPreviewProgram => "swap preview/program",
            Self::SetTransition { .. } => "set transition",
            Self::AddProfile { .. } => "add profile",
            Self::RemoveProfile { .. } => "remove profile",
            Self::SelectProfile { .. } => "select profile",
            Self::AddSceneCollection { .. } => "add scene collection",
            Self::RemoveSceneCollection { .. } => "remove scene collection",
            Self::SelectSceneCollection { .. } => "select scene collection",
            Self::Transaction { .. } => "transaction",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::{Output, OutputKind, ReconnectPolicy};
    use crate::project::{Profile, SceneCollection, VideoConfig};
    use crate::scene::{BlendMode, Vec2};
    use crate::transition::TransitionKind;

    fn sample_commands() -> Vec<Command> {
        let scene = SceneId::new();
        let item = SceneItemId::new();
        let source = SourceId::new();
        let bus = AudioBusId::new();
        let output = OutputId::new();
        vec![
            Command::Undo,
            Command::Redo,
            Command::AddScene {
                name: "Main".into(),
            },
            Command::RemoveScene { scene_id: scene },
            Command::RenameScene {
                scene_id: scene,
                name: "Renamed".into(),
            },
            Command::ReorderScene {
                scene_id: scene,
                new_index: 2,
            },
            Command::SetCurrentScene { scene_id: scene },
            Command::AddSceneItem {
                scene_id: scene,
                source_id: source,
            },
            Command::RemoveSceneItem {
                scene_id: scene,
                item_id: item,
            },
            Command::DuplicateSceneItem {
                scene_id: scene,
                item_id: item,
            },
            Command::SetSceneItemTransform {
                scene_id: scene,
                item_id: item,
                transform: Transform {
                    position: Vec2::new(1.0, 2.0),
                    scale: Vec2::new(1.5, 1.5),
                    rotation: 90.0,
                    anchor: crate::scene::Anchor::Center,
                },
            },
            Command::SetSceneItemCrop {
                scene_id: scene,
                item_id: item,
                crop: Crop {
                    left: 1,
                    top: 2,
                    right: 3,
                    bottom: 4,
                },
            },
            Command::SetSceneItemVisible {
                scene_id: scene,
                item_id: item,
                visible: false,
            },
            Command::SetSceneItemLocked {
                scene_id: scene,
                item_id: item,
                locked: true,
            },
            Command::SetSceneItemZIndex {
                scene_id: scene,
                item_id: item,
                z_index: 7,
            },
            Command::RaiseSceneItem {
                scene_id: scene,
                item_id: item,
            },
            Command::LowerSceneItem {
                scene_id: scene,
                item_id: item,
            },
            Command::SetSceneItemOpacity {
                scene_id: scene,
                item_id: item,
                opacity: 0.5,
            },
            Command::SetSceneItemBounds {
                scene_id: scene,
                item_id: item,
                bounds: crate::scene::Bounds::default(),
            },
            Command::AddSource {
                kind: SourceKind::V4l2Camera,
                name: "cam".into(),
            },
            Command::RemoveSource { source_id: source },
            Command::RenameSource {
                source_id: source,
                name: "cam2".into(),
            },
            Command::SetSourceSettings {
                source_id: source,
                settings: serde_json::json!({"device": "/dev/video0"}),
            },
            Command::AuthorizeSourceCapture { source_id: source },
            Command::SetSourceEnabled {
                source_id: source,
                enabled: false,
            },
            Command::SetSourceVolume {
                source_id: source,
                volume_db: -3.0,
            },
            Command::SetSourceMuted {
                source_id: source,
                muted: true,
            },
            Command::SetSourceSolo {
                source_id: source,
                solo: true,
            },
            Command::SetSourceMonitor {
                source_id: source,
                monitor: MonitorMode::MonitorOnly,
            },
            Command::SetSourceBalance {
                source_id: source,
                balance: -0.5,
            },
            Command::SetSourceSyncOffset {
                source_id: source,
                sync_offset_ms: 80,
            },
            Command::AddAudioBus { name: "VOD".into() },
            Command::RemoveAudioBus { bus_id: bus },
            Command::SetAudioRoute {
                source_id: source,
                bus_id: bus,
                tracks: TrackMask::stereo_pair(),
            },
            Command::RemoveAudioRoute {
                source_id: source,
                bus_id: bus,
            },
            Command::AddOutput {
                output: Output::new(OutputKind::Recording, "rec", crate::id::EncoderId::new()),
            },
            Command::RemoveOutput { output_id: output },
            Command::StartOutput { output_id: output },
            Command::StopOutput { output_id: output },
            Command::SetOutputReconnectPolicy {
                output_id: output,
                policy: ReconnectPolicy::default(),
            },
            Command::SetStudioModeEnabled { enabled: true },
            Command::SetPreviewScene { scene_id: scene },
            Command::TransitionToProgram,
            Command::SwapPreviewProgram,
            Command::SetTransition {
                transition: Transition {
                    kind: TransitionKind::Cut,
                    duration_ms: 0,
                    settings: serde_json::Value::Null,
                },
            },
            Command::AddProfile {
                profile: Profile::new("p", VideoConfig::default()),
            },
            Command::RemoveProfile {
                profile_id: ProfileId::new(),
            },
            Command::SelectProfile {
                profile_id: ProfileId::new(),
            },
            Command::AddSceneCollection {
                collection: SceneCollection::new("c"),
            },
            Command::RemoveSceneCollection {
                collection_id: SceneCollectionId::new(),
            },
            Command::SelectSceneCollection {
                collection_id: SceneCollectionId::new(),
            },
            Command::Transaction {
                commands: vec![
                    Command::SetSourceMuted {
                        source_id: source,
                        muted: true,
                    },
                    Command::RaiseSceneItem {
                        scene_id: scene,
                        item_id: item,
                    },
                ],
            },
        ]
    }

    #[test]
    fn command_serde_roundtrip_all_variants() {
        for command in sample_commands() {
            let json = serde_json::to_string(&command)
                .unwrap_or_else(|e| panic!("serialize {}: {e}", command.label()));
            let back: Command = serde_json::from_str(&json)
                .unwrap_or_else(|e| panic!("deserialize {}: {e}", command.label()));
            assert_eq!(command, back, "roundtrip {}", command.label());
        }
    }

    #[test]
    fn every_variant_has_a_label() {
        for command in sample_commands() {
            assert!(!command.label().is_empty());
        }
    }

    #[test]
    fn blend_mode_used_in_bounds_defaults() {
        // Guards against accidental removal of serde on small enums.
        let json = serde_json::to_string(&BlendMode::Normal).unwrap();
        assert_eq!(json, "\"normal\"");
    }
}
