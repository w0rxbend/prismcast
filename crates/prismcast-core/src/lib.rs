//! # prismcast-core
//!
//! Domain model of the Prismcast studio: strongly-typed IDs, the shared error
//! model, the domain entities (sources, scenes, audio, outputs, transitions,
//! profiles/collections), and the Command/Event API with a pure
//! [`state::apply`] function (ADR-0005).
//!
//! **Layer: Domain.** Per AGENTS.md this crate must not depend on GTK,
//! GStreamer, Tokio, or Axum. Dependency direction is
//! `domain <- core <- services <- UI/API` and is never reversed.
//!
//! Tracing convention (see PLAN.md §75): all logs use the `tracing` crate
//! with structured ID context fields (`source_id=`, `output_id=`, ...).

pub mod audio;
pub mod audio_capture;
pub mod capture;
pub mod command;
pub mod error;
pub mod event;
pub mod id;
pub mod output;
pub mod project;
pub mod scene;
pub mod source;
pub mod state;
pub mod transition;

pub use audio::{AudioBus, AudioMixerConfig, AudioMixerState, AudioRoute, MonitorMode, TrackMask};
pub use audio_capture::{PipeWireAudioMode, PipeWireAudioSettings};
pub use capture::{CaptureGeneration, CaptureStatus, SourceDimensions, SourceRuntime};
pub use command::Command;
pub use error::{Error, Result};
pub use event::{AudioEvent, Event, MeterEvent, OutputEvent, SceneEvent, SourceEvent, SystemEvent};
pub use id::{
    AudioBusId, CanvasId, EncoderId, FilterId, OutputId, ProfileId, SceneCollectionId, SceneId,
    SceneItemId, ServiceId, SourceId,
};
pub use output::{
    EncoderSettings, Output, OutputKind, OutputState, ReconnectPolicy, SecretString, Service,
};
pub use project::{Profile, SceneCollection, StudioMode, VideoConfig};
pub use scene::{
    Anchor, BlendMode, Bounds, BoundsKind, Canvas, Crop, Scene, SceneItem, Transform, Vec2,
};
pub use source::{Source, SourceKind};
pub use state::{apply, AppState};
pub use transition::{Transition, TransitionKind};
