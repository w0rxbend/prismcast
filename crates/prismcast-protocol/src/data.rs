//! Wire data types: the governed serialization schema for entity payloads
//! carried inside requests, responses, events, and snapshots.
//!
//! These types intentionally *mirror* the `prismcast-core` domain model but
//! are distinct types (PLAN.md §75, ADR-0006 §4): the wire schema evolves on
//! its own schedule, and a domain refactor must never silently change what
//! clients see. Mapping to/from domain types happens at the interface
//! boundary (`prismcast-remote`, `prismcast-cli`), never inside the domain.
//!
//! Conventions:
//! - Entity IDs are plain [`uuid::Uuid`] on the wire (canonical hyphenated
//!   string in JSON). The boundary converts them to the domain's typed ID
//!   newtypes via `From<Uuid>`; per-class ID typing is a domain concern.
//! - Variant and field names match the domain's serde names, so the JSON
//!   shapes are aligned and golden tests stay readable — but the types are
//!   separate, so alignment is a choice, not a coupling.
//! - Maps keyed by entity ID (e.g. mixer state) are sequences of explicit
//!   entry structs, so ordering is deterministic and no codec-specific map
//!   key handling is required.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A 2D vector in canvas pixels (or scale factors, by context).
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Vec2 {
    /// Horizontal component.
    pub x: f32,
    /// Vertical component.
    pub y: f32,
}

/// The reference point within an item that its position applies to.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Anchor {
    /// Top-left corner.
    #[default]
    TopLeft,
    /// Top edge midpoint.
    Top,
    /// Top-right corner.
    TopRight,
    /// Left edge midpoint.
    Left,
    /// Item center.
    Center,
    /// Right edge midpoint.
    Right,
    /// Bottom-left corner.
    BottomLeft,
    /// Bottom edge midpoint.
    Bottom,
    /// Bottom-right corner.
    BottomRight,
}

/// Compositing blend mode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlendMode {
    /// Standard alpha-over compositing.
    #[default]
    Normal,
    /// Additive blending.
    Additive,
    /// Multiply blending.
    Multiply,
    /// Screen blending.
    Screen,
}

/// How an item fits into its [`Bounds`] rectangle.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BoundsKind {
    /// No bounds fitting; free transform applies.
    #[default]
    None,
    /// Stretch to fill the rectangle, ignoring aspect ratio.
    Stretch,
    /// Scale to fit inside the rectangle, preserving aspect ratio.
    FitInner,
    /// Scale to fill the rectangle, preserving aspect ratio (may overflow).
    FitOuter,
}

/// Bounds-based fitting: fit/stretch an item into a rectangle instead of
/// free scaling.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Bounds {
    /// Fitting mode.
    pub kind: BoundsKind,
    /// Target rectangle size in canvas pixels.
    pub size: Vec2,
    /// Alignment within the rectangle.
    pub alignment: Anchor,
}

/// Per-edge crop in source pixels.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Crop {
    /// Pixels cropped from the left edge.
    pub left: u32,
    /// Pixels cropped from the top edge.
    pub top: u32,
    /// Pixels cropped from the right edge.
    pub right: u32,
    /// Pixels cropped from the bottom edge.
    pub bottom: u32,
}

/// 2D position/scale/rotation of a scene item.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Transform {
    /// Position of the anchor point in canvas pixels.
    pub position: Vec2,
    /// Scale factor per axis (`1.0` = natural size).
    pub scale: Vec2,
    /// Rotation in degrees, clockwise.
    pub rotation: f32,
    /// The point of the item that `position` refers to.
    pub anchor: Anchor,
}

/// A placed source within a scene.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SceneItem {
    /// Unique item ID.
    pub id: Uuid,
    /// The shared source this item renders.
    pub source_id: Uuid,
    /// Position/scale/rotation/anchor.
    pub transform: Transform,
    /// Edge crop in pixels.
    pub crop: Crop,
    /// Opacity in `[0.0, 1.0]`.
    pub opacity: f32,
    /// Whether the item is rendered.
    pub visible: bool,
    /// Whether the item is protected from edits.
    pub locked: bool,
    /// Compositing blend mode.
    pub blend_mode: BlendMode,
    /// Bounds-based fitting.
    pub bounds: Bounds,
    /// Stacking order; items render ascending (higher = on top).
    pub z_index: i32,
}

/// A scene: an ordered set of placed sources.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Scene {
    /// Unique scene ID.
    pub id: Uuid,
    /// User-facing name.
    pub name: String,
    /// Placed sources, sorted ascending by `z_index`.
    pub items: Vec<SceneItem>,
}

/// The kind of media a source produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum SourceKind {
    /// PipeWire screen capture via xdg-desktop-portal.
    PipeWireDisplay,
    /// PipeWire single-window capture via xdg-desktop-portal.
    PipeWireWindow,
    /// V4L2 camera device.
    V4l2Camera,
    /// PipeWire audio input (microphone).
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
    /// Nested scene rendered as a source.
    Scene(Uuid),
    /// Test pattern generator.
    TestPattern,
    /// Network stream input (SRT/RIST/RTMP ingest, ...).
    NetworkStream,
}

/// A shared media source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Source {
    /// Unique source ID.
    pub id: Uuid,
    /// Source implementation kind.
    pub kind: SourceKind,
    /// User-facing name.
    pub name: String,
    /// Whether the source produces media when referenced.
    pub enabled: bool,
    /// Kind-specific settings, owned by the source implementation.
    pub settings: serde_json::Value,
    /// Filters applied to this source, in application order.
    pub filters: Vec<Uuid>,
}

/// Monitoring destination for a source.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MonitorMode {
    /// No monitoring.
    #[default]
    Off,
    /// Monitor only; the source does not reach output buses.
    MonitorOnly,
    /// Monitor and send to output buses.
    MonitorAndOutput,
}

/// Bitset over a `u32` selecting which output tracks of a bus a route feeds.
///
/// Bit `n` set = source contributes to track `n` (0-based).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TrackMask(u32);

impl TrackMask {
    /// Mask with no tracks selected.
    pub const NONE: Self = Self(0);
    /// Mask with all 32 tracks selected.
    pub const ALL: Self = Self(u32::MAX);

    /// Builds a mask from raw bits.
    pub fn from_bits(bits: u32) -> Self {
        Self(bits)
    }

    /// Stereo default: tracks 0 and 1.
    pub fn stereo_pair() -> Self {
        Self(0b11)
    }

    /// Returns the raw bitmask.
    pub fn bits(self) -> u32 {
        self.0
    }
}

/// Per-source mixer parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioMixerState {
    /// Gain in decibels (`0.0` = unity).
    pub volume_db: f32,
    /// Muted sources contribute silence to their buses.
    pub muted: bool,
    /// Soloed sources mute all non-soloed sources on the same buses.
    pub solo: bool,
    /// Monitoring destination.
    pub monitor: MonitorMode,
    /// Stereo balance in `[-1.0, 1.0]`.
    pub balance: f32,
    /// Sync offset in milliseconds; positive delays the audio.
    pub sync_offset_ms: i32,
}

/// One source's mixer parameters within an [`AudioMixerConfig`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MixerEntry {
    /// The source these parameters apply to.
    pub source_id: Uuid,
    /// Mixer parameters for the source.
    pub state: AudioMixerState,
}

/// Routes one source's audio into one bus on a set of output tracks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioRoute {
    /// The audio-producing source.
    pub source_id: Uuid,
    /// The destination bus.
    pub bus_id: Uuid,
    /// Output tracks of the bus this route feeds.
    pub tracks: TrackMask,
}

/// A named mix bus.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioBus {
    /// Unique bus ID.
    pub id: Uuid,
    /// User-facing name.
    pub name: String,
}

/// The audio configuration of a scene collection: buses, routes, and mixer
/// state.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AudioMixerConfig {
    /// Named mix buses (unbounded).
    pub buses: Vec<AudioBus>,
    /// Source → bus routing with track assignment.
    pub routes: Vec<AudioRoute>,
    /// Mixer parameters per source; sources without an entry use defaults.
    pub mixer: Vec<MixerEntry>,
}

/// Transition effect kinds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransitionKind {
    /// Instant switch.
    Cut,
    /// Crossfade.
    #[default]
    Fade,
    /// New scene swipes the old one away.
    Swipe,
    /// Both scenes slide.
    Slide,
    /// Video overlay with a cut point (stinger).
    Stinger,
}

/// A scene transition configuration.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Transition {
    /// Transition effect.
    pub kind: TransitionKind,
    /// Duration in milliseconds (ignored by `Cut`).
    pub duration_ms: u32,
    /// Kind-specific settings (direction, stinger media path, ...).
    pub settings: serde_json::Value,
}

/// Output destination kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputKind {
    /// Local file recording.
    Recording,
    /// RTMP/Enhanced-RTMP streaming.
    Rtmp,
    /// SRT streaming.
    Srt,
    /// WHIP (WebRTC ingest) streaming.
    Whip,
    /// Virtual camera output (v4l2loopback).
    VirtualCamera,
}

/// Output lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum OutputState {
    /// Not running.
    Stopped,
    /// Start requested; pipeline/connection not yet up.
    Starting,
    /// Actively producing.
    Running,
    /// Connection lost; retrying per the reconnect policy.
    Reconnecting {
        /// 1-based reconnect attempt number.
        attempt: u32,
    },
    /// Producing, but with problems (dropped frames, encoder lag, ...).
    Degraded,
    /// Unrecoverable failure; a new start is allowed.
    Failed,
    /// Stop requested; draining.
    Stopping,
}

/// Reconnect/backoff policy for network outputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReconnectPolicy {
    /// Maximum reconnect attempts before giving up (`0` = no retries).
    pub max_retries: u32,
    /// Backoff before the first retry, in milliseconds.
    pub initial_backoff_ms: u32,
    /// Cap for exponential backoff growth, in milliseconds.
    pub max_backoff_ms: u32,
}

/// A single output destination in the output graph.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Output {
    /// Unique output ID.
    pub id: Uuid,
    /// Output destination kind.
    pub kind: OutputKind,
    /// User-facing name.
    pub name: String,
    /// Video encoder feeding this output.
    pub video_encoder: Uuid,
    /// Audio encoders feeding this output (one per output track).
    pub audio_encoders: Vec<Uuid>,
    /// Streaming service configuration, for network outputs.
    ///
    /// The referenced service's credentials are never serialized onto the
    /// wire; clients address services by ID only.
    pub service: Option<Uuid>,
    /// Reconnect/backoff policy for network outputs.
    pub reconnect_policy: ReconnectPolicy,
    /// Lifecycle state.
    pub state: OutputState,
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

/// A settings profile: video configuration for the render/output pipeline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    /// Unique profile ID.
    pub id: Uuid,
    /// User-facing name.
    pub name: String,
    /// Base/output video configuration.
    pub video: VideoConfig,
    /// Extra profile settings.
    pub settings: serde_json::Value,
}

/// A scene collection: scenes, shared sources, transitions, and audio
/// configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SceneCollection {
    /// Unique collection ID.
    pub id: Uuid,
    /// User-facing name.
    pub name: String,
    /// Scenes in this collection.
    pub scenes: Vec<Scene>,
    /// Shared sources referenced by scene items.
    pub sources: Vec<Source>,
    /// The default transition.
    pub transition: Transition,
    /// Audio buses, routes, and mixer state.
    pub audio: AudioMixerConfig,
}

/// Studio mode state: program and preview are explicit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StudioMode {
    /// Whether studio (dual-scene) mode is active.
    pub enabled: bool,
    /// The live program scene.
    pub program: Uuid,
    /// The preview scene.
    pub preview: Uuid,
}

/// Full initial state snapshot (PLAN.md §23: `InitialStateSnapshot` + event
/// stream — clients never re-download the whole state after this).
///
/// Actual native pixel dimensions, not portal coordinate dimensions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceDimensions {
    /// Pixel width.
    pub width: u32,
    /// Pixel height.
    pub height: u32,
}
/// Capture lifecycle observation; contains no live permission grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureStatus {
    /// Request pending.
    Authorizing,
    /// Native producer ready.
    Active,
    /// User canceled.
    Cancelled,
    /// Permission denied.
    Denied,
    /// Grant revoked.
    Revoked,
    /// Recoverable failure.
    Failed,
}
/// Transient source runtime observation, independently mapped from domain types.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceRuntime {
    /// Monotonic request correlation, not a portal token.
    pub generation: u64,
    /// Current capture lifecycle.
    pub status: CaptureStatus,
    /// Actual native pixels when Active.
    pub dimensions: Option<SourceDimensions>,
    /// Bounded diagnostic.
    pub message: Option<String>,
}
/// Runtime map entry, separate from persisted source settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceRuntimeEntry {
    /// Capture source identity.
    pub source_id: Uuid,
    /// Transient observation.
    pub runtime: SourceRuntime,
}

/// Order of the `Vec` fields is presentation order (scene list order, output
/// insertion order), mirroring the server's ordered maps.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StateSnapshot {
    /// Known profiles; exactly one is active.
    pub profiles: Vec<Profile>,
    /// The active profile.
    pub active_profile: Option<Uuid>,
    /// Known scene collections; exactly one is active.
    pub collections: Vec<SceneCollection>,
    /// The active scene collection.
    pub active_collection: Option<Uuid>,
    /// Working-set scenes (ordered as in the UI list).
    pub scenes: Vec<Scene>,
    /// Shared sources referenced by scene items.
    pub sources: Vec<Source>,
    /// Transient capture observations; absent in older servers/snapshots.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_runtime: Vec<SourceRuntimeEntry>,
    /// The default transition configuration.
    pub transition: Transition,
    /// Audio buses, routes, and mixer state.
    pub audio: AudioMixerConfig,
    /// The output graph (N independent outputs, ADR-0007).
    pub outputs: Vec<Output>,
    /// The current (program) scene.
    pub current_scene: Option<Uuid>,
    /// Studio mode state (`None` = disabled).
    pub studio_mode: Option<StudioMode>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn track_mask_wire_shape_is_plain_integer() {
        let mask = TrackMask::stereo_pair();
        assert_eq!(serde_json::to_string(&mask).unwrap(), "3");
        assert_eq!(serde_json::from_str::<TrackMask>("3").unwrap(), mask);
        assert_eq!(TrackMask::from_bits(u32::MAX), TrackMask::ALL);
        assert_eq!(TrackMask::NONE.bits(), 0);
    }

    #[test]
    fn scene_item_roundtrip() {
        let item = SceneItem {
            id: Uuid::new_v4(),
            source_id: Uuid::new_v4(),
            transform: Transform {
                position: Vec2 { x: 1.5, y: -2.0 },
                scale: Vec2 { x: 1.0, y: 2.0 },
                rotation: 45.0,
                anchor: Anchor::Center,
            },
            crop: Crop {
                left: 1,
                top: 2,
                right: 3,
                bottom: 4,
            },
            opacity: 0.75,
            visible: true,
            locked: false,
            blend_mode: BlendMode::Additive,
            bounds: Bounds {
                kind: BoundsKind::FitInner,
                size: Vec2 {
                    x: 1920.0,
                    y: 1080.0,
                },
                alignment: Anchor::TopLeft,
            },
            z_index: 3,
        };
        let json = serde_json::to_string(&item).unwrap();
        assert_eq!(item, serde_json::from_str(&json).unwrap());
    }

    #[test]
    fn output_state_roundtrip_all_variants() {
        for state in [
            OutputState::Stopped,
            OutputState::Starting,
            OutputState::Running,
            OutputState::Reconnecting { attempt: 2 },
            OutputState::Degraded,
            OutputState::Failed,
            OutputState::Stopping,
        ] {
            let json = serde_json::to_string(&state).unwrap();
            assert_eq!(state, serde_json::from_str(&json).unwrap());
        }
    }

    #[test]
    fn source_kind_scene_carries_uuid() {
        let kind = SourceKind::Scene(Uuid::new_v4());
        let json = serde_json::to_string(&kind).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["type"], "scene");
        assert!(value["value"].is_string());
        assert_eq!(kind, serde_json::from_str(&json).unwrap());
    }

    #[test]
    fn state_snapshot_roundtrip_empty_and_populated() {
        let empty = StateSnapshot::default();
        let json = serde_json::to_string(&empty).unwrap();
        assert_eq!(empty, serde_json::from_str(&json).unwrap());

        let snapshot = StateSnapshot {
            profiles: vec![Profile {
                id: Uuid::new_v4(),
                name: "Default".into(),
                video: VideoConfig {
                    width: 1920,
                    height: 1080,
                    fps_num: 60,
                    fps_den: 1,
                },
                settings: serde_json::Value::Null,
            }],
            audio: AudioMixerConfig {
                buses: vec![AudioBus {
                    id: Uuid::new_v4(),
                    name: "Master".into(),
                }],
                routes: Vec::new(),
                mixer: vec![MixerEntry {
                    source_id: Uuid::new_v4(),
                    state: AudioMixerState {
                        volume_db: -3.0,
                        muted: false,
                        solo: false,
                        monitor: MonitorMode::MonitorAndOutput,
                        balance: 0.0,
                        sync_offset_ms: 40,
                    },
                }],
            },
            ..StateSnapshot::default()
        };
        let json = serde_json::to_string(&snapshot).unwrap();
        assert_eq!(snapshot, serde_json::from_str(&json).unwrap());
    }
}
