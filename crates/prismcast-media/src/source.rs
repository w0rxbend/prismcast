//! [`SourceBackend`]: the control seam for live source instances.
//!
//! A backend instance corresponds 1:1 to a domain [`Source`] (shared entity,
//! PLAN.md §7). The trait only *controls* the instance; media frames flow
//! inside the engine graph and never cross this boundary, keeping the trait
//! free of GStreamer types (ADR-0004).

use serde::{Deserialize, Serialize};

use prismcast_core::{Result, SourceId, SourceKind};

use crate::component::BackendComponent;

/// Per-channel audio levels of a source, for meter rendering.
///
/// Polled by the media control actor and forwarded to controllers as part of
/// audio meter updates; never persisted.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AudioLevels {
    /// Per-channel peak level in dBFS (`f32::NEG_INFINITY` = silence).
    pub peak_db: Vec<f32>,
    /// Per-channel RMS level in dBFS.
    pub rms_db: Vec<f32>,
}

/// Controls one live source instance.
///
/// # Threading
///
/// Methods may block on media-graph operations and must only be called from
/// the media control actor's thread — never from the Tokio runtime or the GTK
/// main thread (PLAN.md §57). This is what "async-agnostic" means here: the
/// caller, not the trait, decides about executors.
///
/// # Failure model
///
/// Recoverable problems (device lost, format change — PLAN.md §61) are
/// reported via [`BackendComponent::drain_events`] and reflected in
/// [`BackendComponent::state`]; only immediate, synchronous misuses (invalid
/// settings, illegal transition) are returned as errors.
pub trait SourceBackend: BackendComponent {
    /// The domain source this instance implements.
    fn source_id(&self) -> SourceId;

    /// The source kind (fixed at creation; changing kind = recreate).
    fn kind(&self) -> SourceKind;

    /// JSON Schema (or equivalent document) describing the kind-specific
    /// `settings` payload, for property UIs and validation.
    fn settings_schema(&self) -> serde_json::Value;

    /// Starts producing media. Idempotent when already running.
    fn start(&mut self) -> Result<()>;

    /// Stops producing media and releases devices/handles. Idempotent when
    /// already stopped.
    fn stop(&mut self) -> Result<()>;

    /// Applies a new kind-specific settings payload (same shape as
    /// [`Source::settings`](prismcast_core::Source::settings)). Backends
    /// validate against their schema and return
    /// [`prismcast_core::Error::InvalidInput`] on mismatch.
    fn update_settings(&mut self, settings: serde_json::Value) -> Result<()>;

    /// Current per-channel audio levels, or `None` for sources without
    /// audio. Cheap to call at meter rate (~30 Hz).
    fn audio_levels(&self) -> Option<AudioLevels>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_levels_serde_roundtrip() {
        let levels = AudioLevels {
            peak_db: vec![-3.0, -6.5],
            rms_db: vec![-12.0, -15.25],
        };
        let json = serde_json::to_string(&levels).unwrap();
        assert_eq!(levels, serde_json::from_str(&json).unwrap());
    }
}
