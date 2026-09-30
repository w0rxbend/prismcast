//! Project-level aggregates: profiles, scene collections, studio mode
//! (PLAN.md §18–§19).
//!
//! OBS-style separation is preserved: a [`Profile`] carries video/output
//! configuration, a [`SceneCollection`] carries scenes/sources/transitions/
//! audio. Persisted forms of these types live in the application core with
//! explicit schema versioning; these are the pure domain aggregates.

use serde::{Deserialize, Serialize};

use crate::audio::AudioMixerConfig;
use crate::id::{ProfileId, SceneCollectionId, SceneId};
use crate::scene::Scene;
use crate::source::Source;
use crate::transition::Transition;

/// A settings profile: video configuration for the render/output pipeline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    /// Unique profile ID.
    pub id: ProfileId,
    /// User-facing name.
    pub name: String,
    /// Base/output video configuration.
    pub video: VideoConfig,
    /// Extra profile settings (output defaults, recording settings, ...).
    pub settings: serde_json::Value,
}

impl Profile {
    /// Creates a profile with a fresh ID and empty extra settings.
    pub fn new(name: impl Into<String>, video: VideoConfig) -> Self {
        Self {
            id: ProfileId::new(),
            name: name.into(),
            video,
            settings: serde_json::Value::Null,
        }
    }
}

/// Video resolution and frame rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoConfig {
    /// Canvas/output width in pixels.
    pub width: u32,
    /// Canvas/output height in pixels.
    pub height: u32,
    /// Frame rate numerator.
    pub fps_num: u32,
    /// Frame rate denominator (`fps_num / fps_den` = frames per second).
    pub fps_den: u32,
}

impl VideoConfig {
    /// 1920×1080 at 60 fps.
    pub fn hd_1080p60() -> Self {
        Self {
            width: 1920,
            height: 1080,
            fps_num: 60,
            fps_den: 1,
        }
    }
}

impl Default for VideoConfig {
    fn default() -> Self {
        Self::hd_1080p60()
    }
}

/// A scene collection: scenes, shared sources, transitions, and audio
/// configuration (PLAN.md §19).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SceneCollection {
    /// Unique collection ID.
    pub id: SceneCollectionId,
    /// User-facing name.
    pub name: String,
    /// Scenes in this collection.
    pub scenes: Vec<Scene>,
    /// Shared sources referenced by scene items.
    pub sources: Vec<Source>,
    /// The default transition (used by studio-mode transitions).
    pub transition: Transition,
    /// Audio buses, routes, and mixer state.
    pub audio: AudioMixerConfig,
}

impl SceneCollection {
    /// Creates an empty collection with a fresh ID and default audio config.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            id: SceneCollectionId::new(),
            name: name.into(),
            scenes: Vec::new(),
            sources: Vec::new(),
            transition: Transition::default(),
            audio: AudioMixerConfig::with_master_bus(),
        }
    }
}

/// Studio mode state (PLAN.md §18): program and preview are explicit and
/// independently controllable — never hidden in UI state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StudioMode {
    /// Whether studio (dual-scene) mode is active.
    pub enabled: bool,
    /// The live program scene.
    pub program: SceneId,
    /// The preview scene (must differ from `program` while `enabled`).
    pub preview: SceneId,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::SceneItem;
    use crate::source::SourceKind;

    #[test]
    fn profile_serde_roundtrip() {
        let mut profile = Profile::new("twitch-1080p", VideoConfig::hd_1080p60());
        profile.settings = serde_json::json!({"recording": {"format": "mkv"}});
        let json = serde_json::to_string(&profile).unwrap();
        assert_eq!(profile, serde_json::from_str(&json).unwrap());
    }

    #[test]
    fn video_config_serde_roundtrip() {
        let cfg = VideoConfig {
            width: 2560,
            height: 1440,
            fps_num: 30_000,
            fps_den: 1001,
        };
        let json = serde_json::to_string(&cfg).unwrap();
        assert_eq!(cfg, serde_json::from_str(&json).unwrap());
    }

    #[test]
    fn scene_collection_serde_roundtrip() {
        let mut collection = SceneCollection::new("dev-stream");
        let source = crate::source::Source::new(SourceKind::TestPattern, "pattern");
        let mut scene = Scene::new("Main");
        scene.add_item(SceneItem::new(source.id, 0));
        collection.sources.push(source);
        collection.scenes.push(scene);
        let json = serde_json::to_string(&collection).unwrap();
        let back: SceneCollection = serde_json::from_str(&json).unwrap();
        assert_eq!(collection, back);
    }

    #[test]
    fn studio_mode_serde_roundtrip() {
        let mode = StudioMode {
            enabled: true,
            program: SceneId::new(),
            preview: SceneId::new(),
        };
        let json = serde_json::to_string(&mode).unwrap();
        assert_eq!(mode, serde_json::from_str(&json).unwrap());
    }

    #[test]
    fn scene_collection_new_has_master_bus() {
        let collection = SceneCollection::new("c");
        assert_eq!(collection.audio.buses.len(), 1);
        assert_eq!(
            collection.audio.buses[0].name,
            crate::audio::MASTER_BUS_NAME
        );
    }
}
