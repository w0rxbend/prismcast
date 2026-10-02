//! Sources: capture devices, media, generators, and nested scenes.
//!
//! Per PLAN.md §7, a [`Source`] is a shared entity — scenes never own sources,
//! they reference them through `SceneItem`s (see [`crate::scene`]). The same
//! source may therefore appear in multiple scenes (and multiple times in one
//! scene).
//!
//! Per-kind configuration lives in `settings` as opaque JSON owned by the
//! source implementation; the domain validates structure, not per-kind keys.

use serde::{Deserialize, Serialize};

use crate::id::{FilterId, SceneId, SourceId};

/// A shared media source (capture device, media file, generator, ...).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Source {
    /// Unique source ID.
    pub id: SourceId,
    /// Source implementation kind.
    pub kind: SourceKind,
    /// User-facing name (unique-ified by the core, see `crate::state`).
    pub name: String,
    /// Whether the source produces media when referenced.
    pub enabled: bool,
    /// Kind-specific settings, owned by the source implementation.
    pub settings: serde_json::Value,
    /// Filters applied to this source, in application order.
    pub filters: Vec<FilterId>,
}

impl Source {
    /// Creates a source with a fresh ID and empty settings/filters.
    pub fn new(kind: SourceKind, name: impl Into<String>) -> Self {
        Self {
            id: SourceId::new(),
            kind,
            name: name.into(),
            enabled: true,
            settings: serde_json::Value::Null,
            filters: Vec::new(),
        }
    }

    /// Returns the scene this source renders, if it is a scene source.
    pub fn scene_reference(&self) -> Option<SceneId> {
        match self.kind {
            SourceKind::Scene(scene_id) => Some(scene_id),
            _ => None,
        }
    }
}

/// The kind of media a source produces (PLAN.md §7 initial list).
///
/// Adjacently tagged so the `Scene(SceneId)` payload can be any serde type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum SourceKind {
    /// PipeWire screen capture via xdg-desktop-portal.
    PipeWireDisplay,
    /// PipeWire single-window capture via xdg-desktop-portal.
    PipeWireWindow,
    /// V4L2 camera device.
    V4l2Camera,
    /// PipeWire audio input or selected sink monitor (`settings.mode`).
    PipeWireAudioInput,
    /// PipeWire per-application audio capture.
    PipeWireAppAudio,
    /// Local media file (video/audio).
    MediaFile,
    /// Still image.
    Image,
    /// Image slideshow.
    ImageSlideshow,
    /// Solid color generator.
    Color,
    /// Text renderer.
    Text,
    /// Web page (WebKit/CEF-class browser source).
    Browser,
    /// Nested scene rendered as a source (OBS-style scene recursion).
    Scene(SceneId),
    /// Test pattern generator.
    TestPattern,
    /// Network stream input (SRT/RIST/RTMP ingest, ...).
    NetworkStream,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_new_defaults() {
        let src = Source::new(SourceKind::TestPattern, "pattern");
        assert!(src.enabled);
        assert_eq!(src.name, "pattern");
        assert!(src.filters.is_empty());
        assert!(src.settings.is_null());
    }

    #[test]
    fn source_serde_roundtrip() {
        let mut src = Source::new(SourceKind::Browser, "web");
        src.settings = serde_json::json!({"url": "https://example.com"});
        src.filters.push(FilterId::new());
        let json = serde_json::to_string(&src).unwrap();
        let back: Source = serde_json::from_str(&json).unwrap();
        assert_eq!(src, back);
    }

    #[test]
    fn source_kind_serde_roundtrip_all_variants() {
        let kinds = [
            SourceKind::PipeWireDisplay,
            SourceKind::PipeWireWindow,
            SourceKind::V4l2Camera,
            SourceKind::PipeWireAudioInput,
            SourceKind::PipeWireAppAudio,
            SourceKind::MediaFile,
            SourceKind::Image,
            SourceKind::ImageSlideshow,
            SourceKind::Color,
            SourceKind::Text,
            SourceKind::Browser,
            SourceKind::Scene(SceneId::new()),
            SourceKind::TestPattern,
            SourceKind::NetworkStream,
        ];
        for kind in kinds {
            let json = serde_json::to_string(&kind).unwrap();
            let back: SourceKind = serde_json::from_str(&json).unwrap();
            assert_eq!(kind, back);
        }
    }

    #[test]
    fn scene_reference_only_for_scene_sources() {
        let scene_id = SceneId::new();
        let nested = Source::new(SourceKind::Scene(scene_id), "nested");
        assert_eq!(nested.scene_reference(), Some(scene_id));
        let cam = Source::new(SourceKind::V4l2Camera, "cam");
        assert_eq!(cam.scene_reference(), None);
    }
}
