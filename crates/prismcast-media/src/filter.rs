//! [`VideoFilterBackend`] / [`AudioFilterBackend`]: ordered filter chains
//! (PLAN.md §16).
//!
//! Per ADR-0004 the traits are a *control* seam only. PLAN.md §16 sketches a
//! generic `Filter` with a `process` method; `process` is deliberately absent
//! here because frame processing happens inside the engine's media graph and
//! exposing it would force media-framework frame types (e.g. `gst::Buffer`)
//! into the abstraction.

use serde::{Deserialize, Serialize};

use prismcast_core::{FilterId, Result};

use crate::component::BackendComponent;

/// Which media path a filter processes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilterMedia {
    /// Video frames.
    Video,
    /// Audio samples.
    Audio,
}

/// Static description of a filter implementation (PLAN.md §16 `descriptor`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilterDescriptor {
    /// Stable implementation slug (`"color_correction"`, `"compressor"`, ...).
    pub kind: String,
    /// Human-readable name for filter pickers.
    pub display_name: String,
    /// Which media path the filter processes.
    pub media: FilterMedia,
}

/// Shared control surface of a filter instance.
///
/// Filters are attached to a source's chain in application order
/// ([`Source::filters`](prismcast_core::Source::filters)); chain wiring is
/// owned by the engine, not by this trait.
///
/// Threading and failure semantics match [`crate::SourceBackend`].
pub trait FilterBackend: BackendComponent {
    /// The domain filter this instance implements.
    fn filter_id(&self) -> FilterId;

    /// Static description of this filter implementation.
    fn descriptor(&self) -> FilterDescriptor;

    /// JSON Schema (or equivalent document) describing the kind-specific
    /// `settings` payload.
    fn settings_schema(&self) -> serde_json::Value;

    /// Applies a new settings payload; invalid payloads yield
    /// [`prismcast_core::Error::InvalidInput`].
    fn update_settings(&mut self, settings: serde_json::Value) -> Result<()>;

    /// Enables or bypasses the filter without unwiring it from the chain.
    fn set_enabled(&mut self, enabled: bool) -> Result<()>;
}

/// A filter on a source's video path (crop/pad, scale, color correction,
/// chroma key, ... — PLAN.md §16 video MVP).
pub trait VideoFilterBackend: FilterBackend {}

/// A filter on a source's audio path (gain, compressor, limiter, gate,
/// noise suppression, ... — PLAN.md §16 audio MVP).
pub trait AudioFilterBackend: FilterBackend {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_descriptor_serde_roundtrip() {
        for media in [FilterMedia::Video, FilterMedia::Audio] {
            let descriptor = FilterDescriptor {
                kind: "chroma_key".to_string(),
                display_name: "Chroma Key".to_string(),
                media,
            };
            let json = serde_json::to_string(&descriptor).unwrap();
            assert_eq!(descriptor, serde_json::from_str(&json).unwrap());
        }
    }
}
