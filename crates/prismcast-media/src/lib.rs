//! # prismcast-media
//!
//! Media engine abstraction traits and the media control actor: sources,
//! filters, and the boundary between the application core and the Linux
//! media platform (GStreamer, PipeWire).
//!
//! **Layer: Media Core.**
//!
//! ## Backend traits (PLAN.md §4, ADR-0004)
//!
//! All media functionality is accessed through object-safe backend traits so
//! GStreamer stays one replaceable implementation (`GstSourceBackend`,
//! `GstCompositorBackend`, ... in a later MEDIA task) and the domain never
//! sees media-framework types:
//!
//! - [`SourceBackend`] — live source instances (capture, media, generators)
//! - [`VideoFilterBackend`] / [`AudioFilterBackend`] — ordered filter chains
//!   (PLAN.md §16)
//! - [`CompositorBackend`] — scene composition onto canvases (PLAN.md §5, §8)
//! - [`EncoderBackend`] + [`EncoderRegistry`] — encoder instances and runtime
//!   capability probing (PLAN.md §12, RES-003 §5)
//! - [`OutputBackend`] — one instance per output: recording, N×RTMP, SRT,
//!   WHIP, virtual camera (PLAN.md §10–§14, ADR-0007)
//! - [`StreamingServiceBackend`] — service validation, ingest discovery,
//!   connectivity/auth probes, independent of any running output
//!
//! ## Threading contract (PLAN.md §57)
//!
//! The traits are deliberately **async-agnostic**: synchronous methods called
//! from the media control actor's thread, which owns the engine's streaming
//! threads. Callers must never invoke them on the Tokio runtime (methods may
//! block on graph operations) or the GTK main thread. Asynchronous happenings
//! (device loss, format changes, EOS — PLAN.md §61) surface as
//! [`BackendEvent`]s drained via [`BackendComponent::drain_events`]; the actor
//! translates them into core [`prismcast_core::Event`]s and maps
//! [`ComponentState`] onto persisted domain states such as
//! [`prismcast_core::OutputState`].
//!
//! ## Testing
//!
//! [`mock`] provides in-memory implementations of every trait with call
//! recording, event queueing, and failure injection.

pub mod component;
pub mod compositor;
pub mod encoder;
pub mod filter;
pub mod mock;
pub mod output;
pub mod service;
pub mod source;

pub use component::{BackendComponent, BackendEvent, ComponentState};
pub use compositor::CompositorBackend;
pub use encoder::{EncoderBackend, EncoderCapability, EncoderRegistry, HardwareAccel};
pub use filter::{
    AudioFilterBackend, FilterBackend, FilterDescriptor, FilterMedia, VideoFilterBackend,
};
pub use output::{OutputBackend, OutputCapabilities, OutputStats};
pub use service::{IngestEndpoint, ServiceProbe, StreamingServiceBackend};
pub use source::{AudioLevels, SourceBackend};
