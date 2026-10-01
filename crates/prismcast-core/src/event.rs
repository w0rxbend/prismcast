//! The [`Event`] enum: every state change, strongly typed (PLAN.md §58).
//!
//! Events are notifications of committed changes produced by
//! [`crate::state::apply`], not a storage format (ADR-0005). There is exactly
//! one event hierarchy; GTK, WebSocket, IPC, CLI, and the web UI all consume
//! the same variants.

use serde::{Deserialize, Serialize};

use crate::id::{
    AudioBusId, OutputId, ProfileId, SceneCollectionId, SceneId, SceneItemId, SourceId,
};
use crate::output::OutputState;
use crate::scene::SceneItem;
use crate::source::Source;
use crate::transition::Transition;

/// A committed state change.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "domain", rename_all = "snake_case")]
pub enum Event {
    /// Scene and scene-item changes.
    Scene(SceneEvent),
    /// Source changes.
    Source(SourceEvent),
    /// Audio mixer/routing changes.
    Audio(AudioEvent),
    /// Output graph changes.
    Output(OutputEvent),
    /// Studio mode, transitions, profiles, collections.
    System(SystemEvent),
}

/// Scene-domain events.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum SceneEvent {
    /// A scene was added.
    Added {
        /// New scene ID.
        scene_id: SceneId,
        /// Scene name.
        name: String,
    },
    /// A scene was removed.
    Removed {
        /// Removed scene ID.
        scene_id: SceneId,
    },
    /// A scene was renamed.
    Renamed {
        /// Scene ID.
        scene_id: SceneId,
        /// New name.
        name: String,
    },
    /// The scene list order changed.
    Reordered,
    /// The current (program) scene changed.
    CurrentChanged {
        /// New current scene ID.
        scene_id: SceneId,
    },
    /// An item was added to a scene.
    ItemAdded {
        /// Scene containing the item.
        scene_id: SceneId,
        /// The added item (full snapshot for controllers).
        item: Box<SceneItem>,
    },
    /// An item was removed from a scene.
    ItemRemoved {
        /// Scene the item was removed from.
        scene_id: SceneId,
        /// Removed item ID.
        item_id: SceneItemId,
        /// Source the item referenced.
        source_id: SourceId,
    },
    /// An item's transform, crop, opacity, visibility, lock, bounds, or
    /// z-index changed. Carries the full updated item.
    ItemUpdated {
        /// Scene containing the item.
        scene_id: SceneId,
        /// The updated item (full snapshot for controllers).
        item: Box<SceneItem>,
    },
}

/// Source-domain events.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum SourceEvent {
    /// An explicit authorization command was admitted by the application owner.
    CaptureAuthorizationRequested {
        /// Capture source.
        source_id: SourceId,
    },
    /// Transient lifecycle/caps changed; None invalidates prior capture runtime.
    RuntimeChanged {
        /// Capture source.
        source_id: SourceId,
        /// New transient observation, never a portal grant.
        runtime: Option<crate::SourceRuntime>,
    },
    /// A source was added.
    Added {
        /// The added source (full snapshot for controllers).
        source: Box<Source>,
    },
    /// A source was removed.
    Removed {
        /// Removed source ID.
        source_id: SourceId,
    },
    /// A source was renamed.
    Renamed {
        /// Source ID.
        source_id: SourceId,
        /// New name.
        name: String,
    },
    /// A source's kind-specific settings changed.
    SettingsChanged {
        /// Source ID.
        source_id: SourceId,
    },
    /// A source was enabled or disabled.
    EnabledChanged {
        /// Source ID.
        source_id: SourceId,
        /// New enabled state.
        enabled: bool,
    },
}

/// Audio-domain events.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum AudioEvent {
    /// A mixer parameter changed.
    MixerChanged {
        /// Source whose mixer state changed.
        source_id: SourceId,
        /// The full updated mixer state.
        state: crate::audio::AudioMixerState,
    },
    /// An audio bus was added.
    BusAdded {
        /// New bus ID.
        bus_id: AudioBusId,
        /// Bus name.
        name: String,
    },
    /// An audio bus was removed.
    BusRemoved {
        /// Removed bus ID.
        bus_id: AudioBusId,
    },
    /// A source's route into a bus was added or replaced.
    RouteChanged {
        /// Routed source.
        source_id: SourceId,
        /// Destination bus.
        bus_id: AudioBusId,
        /// New track assignment.
        tracks: crate::audio::TrackMask,
    },
    /// A source's route into a bus was removed.
    RouteRemoved {
        /// Unrouted source.
        source_id: SourceId,
        /// Bus unrouted from.
        bus_id: AudioBusId,
    },
}

/// Output-domain events.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum OutputEvent {
    /// An output was added to the graph.
    Added {
        /// New output ID.
        output_id: OutputId,
        /// Output name.
        name: String,
    },
    /// An output was removed from the graph.
    Removed {
        /// Removed output ID.
        output_id: OutputId,
    },
    /// An output's lifecycle state changed.
    StateChanged {
        /// Output ID.
        output_id: OutputId,
        /// New state.
        state: OutputState,
    },
    /// An output's reconnect policy changed.
    ReconnectPolicyChanged {
        /// Output ID.
        output_id: OutputId,
    },
}

/// System-domain events: studio mode, transitions, profiles, collections.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum SystemEvent {
    /// Studio mode was enabled or disabled.
    StudioModeChanged {
        /// New enabled state.
        enabled: bool,
    },
    /// The preview scene changed.
    PreviewSceneChanged {
        /// New preview scene ID.
        scene_id: SceneId,
    },
    /// The default transition configuration changed.
    TransitionChanged {
        /// New transition.
        transition: Transition,
    },
    /// A studio-mode transition started (not emitted for `Cut`).
    TransitionStarted {
        /// Transition effect in progress.
        kind: crate::transition::TransitionKind,
        /// Duration in milliseconds.
        duration_ms: u32,
    },
    /// A profile was added.
    ProfileAdded {
        /// New profile ID.
        profile_id: ProfileId,
    },
    /// A profile was removed.
    ProfileRemoved {
        /// Removed profile ID.
        profile_id: ProfileId,
    },
    /// The active profile changed.
    ProfileSelected {
        /// Newly active profile ID.
        profile_id: ProfileId,
    },
    /// A scene collection was added.
    CollectionAdded {
        /// New collection ID.
        collection_id: SceneCollectionId,
    },
    /// A scene collection was removed.
    CollectionRemoved {
        /// Removed collection ID.
        collection_id: SceneCollectionId,
    },
    /// The active scene collection changed.
    CollectionSelected {
        /// Newly active collection ID.
        collection_id: SceneCollectionId,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::{AudioMixerState, MonitorMode, TrackMask};
    use crate::source::SourceKind;

    fn sample_events() -> Vec<Event> {
        let scene = SceneId::new();
        let item = SceneItem::new(SourceId::new(), 0);
        let source = SourceId::new();
        let bus = AudioBusId::new();
        let output = OutputId::new();
        vec![
            Event::Scene(SceneEvent::Added {
                scene_id: scene,
                name: "s".into(),
            }),
            Event::Scene(SceneEvent::Removed { scene_id: scene }),
            Event::Scene(SceneEvent::Renamed {
                scene_id: scene,
                name: "r".into(),
            }),
            Event::Scene(SceneEvent::Reordered),
            Event::Scene(SceneEvent::CurrentChanged { scene_id: scene }),
            Event::Scene(SceneEvent::ItemAdded {
                scene_id: scene,
                item: Box::new(item.clone()),
            }),
            Event::Scene(SceneEvent::ItemRemoved {
                scene_id: scene,
                item_id: item.id,
                source_id: source,
            }),
            Event::Scene(SceneEvent::ItemUpdated {
                scene_id: scene,
                item: Box::new(item),
            }),
            Event::Source(SourceEvent::Added {
                source: Box::new(Source::new(SourceKind::Color, "c")),
            }),
            Event::Source(SourceEvent::Removed { source_id: source }),
            Event::Source(SourceEvent::Renamed {
                source_id: source,
                name: "n".into(),
            }),
            Event::Source(SourceEvent::SettingsChanged { source_id: source }),
            Event::Source(SourceEvent::EnabledChanged {
                source_id: source,
                enabled: false,
            }),
            Event::Audio(AudioEvent::MixerChanged {
                source_id: source,
                state: AudioMixerState {
                    volume_db: -1.0,
                    muted: false,
                    solo: true,
                    monitor: MonitorMode::MonitorAndOutput,
                    balance: 0.0,
                    sync_offset_ms: 0,
                },
            }),
            Event::Audio(AudioEvent::BusAdded {
                bus_id: bus,
                name: "b".into(),
            }),
            Event::Audio(AudioEvent::BusRemoved { bus_id: bus }),
            Event::Audio(AudioEvent::RouteChanged {
                source_id: source,
                bus_id: bus,
                tracks: TrackMask::ALL,
            }),
            Event::Audio(AudioEvent::RouteRemoved {
                source_id: source,
                bus_id: bus,
            }),
            Event::Output(OutputEvent::Added {
                output_id: output,
                name: "o".into(),
            }),
            Event::Output(OutputEvent::Removed { output_id: output }),
            Event::Output(OutputEvent::StateChanged {
                output_id: output,
                state: OutputState::Reconnecting { attempt: 2 },
            }),
            Event::Output(OutputEvent::ReconnectPolicyChanged { output_id: output }),
            Event::System(SystemEvent::StudioModeChanged { enabled: true }),
            Event::System(SystemEvent::PreviewSceneChanged { scene_id: scene }),
            Event::System(SystemEvent::TransitionChanged {
                transition: Transition::default(),
            }),
            Event::System(SystemEvent::TransitionStarted {
                kind: crate::transition::TransitionKind::Stinger,
                duration_ms: 900,
            }),
            Event::System(SystemEvent::ProfileAdded {
                profile_id: ProfileId::new(),
            }),
            Event::System(SystemEvent::ProfileRemoved {
                profile_id: ProfileId::new(),
            }),
            Event::System(SystemEvent::ProfileSelected {
                profile_id: ProfileId::new(),
            }),
            Event::System(SystemEvent::CollectionAdded {
                collection_id: SceneCollectionId::new(),
            }),
            Event::System(SystemEvent::CollectionRemoved {
                collection_id: SceneCollectionId::new(),
            }),
            Event::System(SystemEvent::CollectionSelected {
                collection_id: SceneCollectionId::new(),
            }),
        ]
    }

    #[test]
    fn event_serde_roundtrip_all_variants() {
        for event in sample_events() {
            let json = serde_json::to_string(&event).unwrap();
            let back: Event = serde_json::from_str(&json).unwrap();
            assert_eq!(event, back, "roundtrip {json}");
        }
    }
}
