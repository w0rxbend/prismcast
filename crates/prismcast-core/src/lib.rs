//! # prismcast-core
//!
//! Domain model of the Prismcast studio: strongly-typed IDs, the shared
//! error model, and (later) commands and events.
//!
//! **Layer: Domain.** Per AGENTS.md this crate must not depend on GTK,
//! GStreamer, Tokio, or Axum. Dependency direction is
//! `domain <- core <- services <- UI/API` and is never reversed.
//!
//! Tracing convention (see PLAN.md §75): all logs use the `tracing` crate
//! with structured ID context fields (`source_id=`, `output_id=`, ...).

pub mod error;
pub mod id;

pub use error::{Error, Result};
pub use id::{
    AudioBusId, EncoderId, FilterId, OutputId, ProfileId, SceneCollectionId, SceneId, SceneItemId,
    ServiceId, SourceId,
};
