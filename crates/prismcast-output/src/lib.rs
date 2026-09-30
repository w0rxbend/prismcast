//! # prismcast-output
//!
//! Output graph: recording, streaming, and native multistreaming —
//! encoder graph, muxers, and output lifecycle.
//!
//! **Layer: Media Core.** Domain-level runtime model only (PLAN.md §10–11,
//! ADR-0007): this crate computes the encoder sharing plan and owns the
//! per-output state machines, reconnect policies, and statistics. It contains
//! **no GStreamer code** — the media engine executes what this graph plans.
//!
//! - [`OutputGraph`]: N independent outputs, encoder registry, replanning.
//! - [`EncoderPlan`]/[`EncoderGroup`]: which declared encoders share one
//!   physical instance feeding an encoded-packet tee.
//! - [`EncoderSpec`]: the share-identity (resolution, FPS, codec, profile,
//!   bitrate, GOP, color format).
//! - [`OutputRuntime`]: per-output PLAN §61 state machine, reconnect backoff
//!   ([`backoff_ms`]), and [`OutputStats`].
//!
//! Failure isolation is structural: runtimes share no mutable state, so one
//! broken output can never stop another (PLAN.md §11, §50).

pub mod error;
pub mod graph;
pub mod plan;
pub mod runtime;
pub mod spec;

pub use error::{OutputGraphError, Result};
pub use graph::OutputGraph;
pub use plan::{EncoderGroup, EncoderPlan, PlanEntry};
pub use runtime::{backoff_ms, OutputRuntime, OutputStats, ReconnectStep};
pub use spec::{EncoderSpec, VideoSpec};
