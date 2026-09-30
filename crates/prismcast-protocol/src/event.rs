//! Wire events: committed state changes pushed to subscribed sessions.
//!
//! [`WireEvent`] mirrors `prismcast_core::Event` variant-for-variant (same
//! serde shape, distinct types — see [`crate::data`]) plus a `Meter` group
//! that originates in the media layer rather than the domain store.
//!
//! Every event is wrapped in an [`EventMessage`] carrying a per-session
//! sequence number and its subscription category. The sequence number lets
//! clients detect server-side drops under backpressure: on a gap, a client
//! must re-sync with `get_snapshot` (see `docs/protocols/native-protocol.md`
//! §Backpressure).

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::data::{
    AudioMixerState, OutputState, SceneItem, Source, TrackMask, Transition, TransitionKind,
};
use crate::subscription::EventCategory;

/// A domain event as delivered on the wire.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "domain", rename_all = "snake_case")]
pub enum WireEvent {
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
    /// High-volume media-layer telemetry (meters).
    Meter(MeterEvent),
}

impl WireEvent {
    /// The subscription category that gates this event.
    pub fn category(&self) -> EventCategory {
        match self {
            Self::Scene(_) => EventCategory::Scene,
            Self::Source(_) => EventCategory::Source,
            Self::Audio(_) => EventCategory::Audio,
            Self::Output(_) => EventCategory::Output,
            Self::System(_) => EventCategory::System,
            Self::Meter(_) => EventCategory::Meter,
        }
    }

    /// The primary entity this event is about, for per-entity subscription
    /// filtering. `None` for events without a filterable entity (e.g.
    /// `scene_reordered`); those pass only unfiltered category
    /// subscriptions.
    pub fn primary_entity(&self) -> Option<Uuid> {
        match self {
            Self::Scene(event) => match event {
                SceneEvent::Added { scene_id, .. }
                | SceneEvent::Removed { scene_id }
                | SceneEvent::Renamed { scene_id, .. }
                | SceneEvent::CurrentChanged { scene_id }
                | SceneEvent::ItemAdded { scene_id, .. }
                | SceneEvent::ItemRemoved { scene_id, .. }
                | SceneEvent::ItemUpdated { scene_id, .. } => Some(*scene_id),
                SceneEvent::Reordered => None,
            },
            Self::Source(event) => match event {
                SourceEvent::Added { source } => Some(source.id),
                SourceEvent::Removed { source_id }
                | SourceEvent::Renamed { source_id, .. }
                | SourceEvent::SettingsChanged { source_id }
                | SourceEvent::EnabledChanged { source_id, .. } => Some(*source_id),
            },
            Self::Audio(event) => match event {
                AudioEvent::MixerChanged { source_id, .. } => Some(*source_id),
                AudioEvent::BusAdded { bus_id, .. } | AudioEvent::BusRemoved { bus_id } => {
                    Some(*bus_id)
                }
                AudioEvent::RouteChanged { source_id, .. }
                | AudioEvent::RouteRemoved { source_id, .. } => Some(*source_id),
            },
            Self::Output(event) => match event {
                OutputEvent::Added { output_id, .. }
                | OutputEvent::Removed { output_id }
                | OutputEvent::StateChanged { output_id, .. }
                | OutputEvent::ReconnectPolicyChanged { output_id } => Some(*output_id),
            },
            Self::System(event) => match event {
                SystemEvent::PreviewSceneChanged { scene_id } => Some(*scene_id),
                SystemEvent::ProfileAdded { profile_id }
                | SystemEvent::ProfileRemoved { profile_id }
                | SystemEvent::ProfileSelected { profile_id } => Some(*profile_id),
                SystemEvent::CollectionAdded { collection_id }
                | SystemEvent::CollectionRemoved { collection_id }
                | SystemEvent::CollectionSelected { collection_id } => Some(*collection_id),
                SystemEvent::StudioModeChanged { .. }
                | SystemEvent::TransitionChanged { .. }
                | SystemEvent::TransitionStarted { .. } => None,
            },
            Self::Meter(MeterEvent::Levels { source_id, .. }) => Some(*source_id),
        }
    }
}

/// Scene-domain events (mirror of `prismcast_core::SceneEvent`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum SceneEvent {
    /// A scene was added.
    Added {
        /// New scene ID.
        scene_id: Uuid,
        /// Scene name.
        name: String,
    },
    /// A scene was removed.
    Removed {
        /// Removed scene ID.
        scene_id: Uuid,
    },
    /// A scene was renamed.
    Renamed {
        /// Scene ID.
        scene_id: Uuid,
        /// New name.
        name: String,
    },
    /// The scene list order changed.
    Reordered,
    /// The current (program) scene changed.
    CurrentChanged {
        /// New current scene ID.
        scene_id: Uuid,
    },
    /// An item was added to a scene.
    ItemAdded {
        /// Scene containing the item.
        scene_id: Uuid,
        /// The added item (full snapshot).
        item: Box<SceneItem>,
    },
    /// An item was removed from a scene.
    ItemRemoved {
        /// Scene the item was removed from.
        scene_id: Uuid,
        /// Removed item ID.
        item_id: Uuid,
        /// Source the item referenced.
        source_id: Uuid,
    },
    /// An item's transform, crop, opacity, visibility, lock, bounds, or
    /// z-index changed. Carries the full updated item.
    ItemUpdated {
        /// Scene containing the item.
        scene_id: Uuid,
        /// The updated item (full snapshot).
        item: Box<SceneItem>,
    },
}

/// Source-domain events (mirror of `prismcast_core::SourceEvent`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum SourceEvent {
    /// A source was added.
    Added {
        /// The added source (full snapshot).
        source: Box<Source>,
    },
    /// A source was removed.
    Removed {
        /// Removed source ID.
        source_id: Uuid,
    },
    /// A source was renamed.
    Renamed {
        /// Source ID.
        source_id: Uuid,
        /// New name.
        name: String,
    },
    /// A source's kind-specific settings changed.
    SettingsChanged {
        /// Source ID.
        source_id: Uuid,
    },
    /// A source was enabled or disabled.
    EnabledChanged {
        /// Source ID.
        source_id: Uuid,
        /// New enabled state.
        enabled: bool,
    },
}

/// Audio-domain events (mirror of `prismcast_core::AudioEvent`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum AudioEvent {
    /// A mixer parameter changed.
    MixerChanged {
        /// Source whose mixer state changed.
        source_id: Uuid,
        /// The full updated mixer state.
        state: AudioMixerState,
    },
    /// An audio bus was added.
    BusAdded {
        /// New bus ID.
        bus_id: Uuid,
        /// Bus name.
        name: String,
    },
    /// An audio bus was removed.
    BusRemoved {
        /// Removed bus ID.
        bus_id: Uuid,
    },
    /// A source's route into a bus was added or replaced.
    RouteChanged {
        /// Routed source.
        source_id: Uuid,
        /// Destination bus.
        bus_id: Uuid,
        /// New track assignment.
        tracks: TrackMask,
    },
    /// A source's route into a bus was removed.
    RouteRemoved {
        /// Unrouted source.
        source_id: Uuid,
        /// Bus unrouted from.
        bus_id: Uuid,
    },
}

/// Output-domain events (mirror of `prismcast_core::OutputEvent`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum OutputEvent {
    /// An output was added to the graph.
    Added {
        /// New output ID.
        output_id: Uuid,
        /// Output name.
        name: String,
    },
    /// An output was removed from the graph.
    Removed {
        /// Removed output ID.
        output_id: Uuid,
    },
    /// An output's lifecycle state changed.
    StateChanged {
        /// Output ID.
        output_id: Uuid,
        /// New state.
        state: OutputState,
    },
    /// An output's reconnect policy changed.
    ReconnectPolicyChanged {
        /// Output ID.
        output_id: Uuid,
    },
}

/// System-domain events (mirror of `prismcast_core::SystemEvent`).
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
        scene_id: Uuid,
    },
    /// The default transition configuration changed.
    TransitionChanged {
        /// New transition.
        transition: Transition,
    },
    /// A studio-mode transition started (not emitted for `Cut`).
    TransitionStarted {
        /// Transition effect in progress.
        kind: TransitionKind,
        /// Duration in milliseconds.
        duration_ms: u32,
    },
    /// A profile was added.
    ProfileAdded {
        /// New profile ID.
        profile_id: Uuid,
    },
    /// A profile was removed.
    ProfileRemoved {
        /// Removed profile ID.
        profile_id: Uuid,
    },
    /// The active profile changed.
    ProfileSelected {
        /// Newly active profile ID.
        profile_id: Uuid,
    },
    /// A scene collection was added.
    CollectionAdded {
        /// New collection ID.
        collection_id: Uuid,
    },
    /// A scene collection was removed.
    CollectionRemoved {
        /// Removed collection ID.
        collection_id: Uuid,
    },
    /// The active scene collection changed.
    CollectionSelected {
        /// Newly active collection ID.
        collection_id: Uuid,
    },
}

/// High-volume media-layer telemetry (no domain-store counterpart).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum MeterEvent {
    /// Audio levels of one source. Emitted at the subscription's meter
    /// interval while the source is active.
    Levels {
        /// The measured source.
        source_id: Uuid,
        /// Per-channel peak levels in dBFS.
        peak_dbfs: Vec<f32>,
        /// Per-channel RMS levels in dBFS.
        rms_dbfs: Vec<f32>,
    },
}

/// An event as delivered to a session: sequence number, gating category,
/// and the payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventMessage {
    /// Per-session monotonically increasing sequence number, starting at 0
    /// after `Identified`. A gap means the server dropped events under
    /// backpressure; the client must re-sync with `get_snapshot`.
    pub seq: u64,
    /// The subscription category that gated delivery (echoed so clients can
    /// filter locally too, like obs-websocket's `eventIntent`).
    pub category: EventCategory,
    /// The event payload.
    #[serde(flatten)]
    pub event: WireEvent,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::MonitorMode;

    fn sample_events() -> Vec<WireEvent> {
        let scene = Uuid::new_v4();
        let source = Uuid::new_v4();
        let bus = Uuid::new_v4();
        let output = Uuid::new_v4();
        let item = SceneItem {
            id: Uuid::new_v4(),
            source_id: source,
            transform: crate::data::Transform::default(),
            crop: crate::data::Crop::default(),
            opacity: 1.0,
            visible: true,
            locked: false,
            blend_mode: crate::data::BlendMode::Normal,
            bounds: crate::data::Bounds::default(),
            z_index: 0,
        };
        vec![
            WireEvent::Scene(SceneEvent::Added {
                scene_id: scene,
                name: "s".into(),
            }),
            WireEvent::Scene(SceneEvent::Removed { scene_id: scene }),
            WireEvent::Scene(SceneEvent::Renamed {
                scene_id: scene,
                name: "r".into(),
            }),
            WireEvent::Scene(SceneEvent::Reordered),
            WireEvent::Scene(SceneEvent::CurrentChanged { scene_id: scene }),
            WireEvent::Scene(SceneEvent::ItemAdded {
                scene_id: scene,
                item: Box::new(item.clone()),
            }),
            WireEvent::Scene(SceneEvent::ItemRemoved {
                scene_id: scene,
                item_id: item.id,
                source_id: source,
            }),
            WireEvent::Scene(SceneEvent::ItemUpdated {
                scene_id: scene,
                item: Box::new(item),
            }),
            WireEvent::Source(SourceEvent::Added {
                source: Box::new(Source {
                    id: source,
                    kind: crate::data::SourceKind::Color,
                    name: "c".into(),
                    enabled: true,
                    settings: serde_json::Value::Null,
                    filters: Vec::new(),
                }),
            }),
            WireEvent::Source(SourceEvent::Removed { source_id: source }),
            WireEvent::Source(SourceEvent::Renamed {
                source_id: source,
                name: "n".into(),
            }),
            WireEvent::Source(SourceEvent::SettingsChanged { source_id: source }),
            WireEvent::Source(SourceEvent::EnabledChanged {
                source_id: source,
                enabled: false,
            }),
            WireEvent::Audio(AudioEvent::MixerChanged {
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
            WireEvent::Audio(AudioEvent::BusAdded {
                bus_id: bus,
                name: "b".into(),
            }),
            WireEvent::Audio(AudioEvent::BusRemoved { bus_id: bus }),
            WireEvent::Audio(AudioEvent::RouteChanged {
                source_id: source,
                bus_id: bus,
                tracks: TrackMask::ALL,
            }),
            WireEvent::Audio(AudioEvent::RouteRemoved {
                source_id: source,
                bus_id: bus,
            }),
            WireEvent::Output(OutputEvent::Added {
                output_id: output,
                name: "o".into(),
            }),
            WireEvent::Output(OutputEvent::Removed { output_id: output }),
            WireEvent::Output(OutputEvent::StateChanged {
                output_id: output,
                state: OutputState::Reconnecting { attempt: 2 },
            }),
            WireEvent::Output(OutputEvent::ReconnectPolicyChanged { output_id: output }),
            WireEvent::System(SystemEvent::StudioModeChanged { enabled: true }),
            WireEvent::System(SystemEvent::PreviewSceneChanged { scene_id: scene }),
            WireEvent::System(SystemEvent::TransitionChanged {
                transition: Transition::default(),
            }),
            WireEvent::System(SystemEvent::TransitionStarted {
                kind: TransitionKind::Stinger,
                duration_ms: 900,
            }),
            WireEvent::System(SystemEvent::ProfileAdded {
                profile_id: Uuid::new_v4(),
            }),
            WireEvent::System(SystemEvent::ProfileRemoved {
                profile_id: Uuid::new_v4(),
            }),
            WireEvent::System(SystemEvent::ProfileSelected {
                profile_id: Uuid::new_v4(),
            }),
            WireEvent::System(SystemEvent::CollectionAdded {
                collection_id: Uuid::new_v4(),
            }),
            WireEvent::System(SystemEvent::CollectionRemoved {
                collection_id: Uuid::new_v4(),
            }),
            WireEvent::System(SystemEvent::CollectionSelected {
                collection_id: Uuid::new_v4(),
            }),
            WireEvent::Meter(MeterEvent::Levels {
                source_id: source,
                peak_dbfs: vec![-3.0, -3.5],
                rms_dbfs: vec![-12.0, -12.4],
            }),
        ]
    }

    #[test]
    fn event_serde_roundtrip_all_variants() {
        for event in sample_events() {
            let json = serde_json::to_string(&event).unwrap();
            let back: WireEvent =
                serde_json::from_str(&json).unwrap_or_else(|e| panic!("deserialize {json}: {e}"));
            assert_eq!(event, back, "roundtrip {json}");
        }
    }

    #[test]
    fn category_matches_domain_group() {
        for event in sample_events() {
            let expected = match &event {
                WireEvent::Scene(_) => EventCategory::Scene,
                WireEvent::Source(_) => EventCategory::Source,
                WireEvent::Audio(_) => EventCategory::Audio,
                WireEvent::Output(_) => EventCategory::Output,
                WireEvent::System(_) => EventCategory::System,
                WireEvent::Meter(_) => EventCategory::Meter,
            };
            assert_eq!(event.category(), expected);
        }
    }

    #[test]
    fn primary_entity_extracted_or_none() {
        let scene = Uuid::new_v4();
        let event = WireEvent::Scene(SceneEvent::Removed { scene_id: scene });
        assert_eq!(event.primary_entity(), Some(scene));
        assert_eq!(
            WireEvent::Scene(SceneEvent::Reordered).primary_entity(),
            None
        );
        assert_eq!(
            WireEvent::System(SystemEvent::StudioModeChanged { enabled: true }).primary_entity(),
            None
        );
    }

    #[test]
    fn event_message_flattens_payload() {
        let scene = Uuid::new_v4();
        let message = EventMessage {
            seq: 41,
            category: EventCategory::Scene,
            event: WireEvent::Scene(SceneEvent::CurrentChanged { scene_id: scene }),
        };
        let value = serde_json::to_value(&message).unwrap();
        assert_eq!(value["seq"], 41);
        assert_eq!(value["category"], "scene");
        assert_eq!(value["domain"], "scene");
        assert_eq!(value["event"], "current_changed");
        assert_eq!(value["scene_id"], scene.to_string());
        let back: EventMessage = serde_json::from_value(value).unwrap();
        assert_eq!(message, back);
    }
}
