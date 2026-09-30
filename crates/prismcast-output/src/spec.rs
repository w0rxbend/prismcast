//! Encoder share-identity (PLAN.md §11).
//!
//! Two outputs may share one encoder instance — one encoder feeding an
//! encoded-packet tee into per-output muxers — **iff** their encoder settings
//! are identical across every dimension PLAN §11 lists: resolution, FPS,
//! codec, profile, bitrate, GOP (keyframe interval), and color format.
//!
//! [`EncoderSpec`] is exactly that share-identity: it is derived from the
//! domain `prismcast_core::EncoderSettings` plus the program [`VideoConfig`]
//! (resolution/FPS) and an optional color-format tag, and compared by value.
//! Equality of [`EncoderSpec`] is the *only* sharing criterion; encoder IDs
//! play no role, so two outputs configured with independently-created but
//! identical `EncoderSettings` still share.

use std::fmt;

use serde::{Deserialize, Serialize};

use prismcast_core::{EncoderSettings, VideoConfig};

/// The share-identity of an encoder: everything that must match for two
/// outputs to legally share one encoded stream.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EncoderSpec {
    /// Codec identifier (`"h264"`, `"h265"`, `"av1"`, `"aac"`, `"opus"`, ...).
    pub codec: String,
    /// Target bitrate in kbit/s.
    pub bitrate_kbps: u32,
    /// Optional keyframe/GOP interval in frames.
    pub keyframe_interval: Option<u32>,
    /// Codec/backend-specific settings (rate control, profile, preset, ...).
    ///
    /// Compared by JSON value equality; object key order is irrelevant.
    pub settings: serde_json::Value,
    /// Video-only dimensions of the share-identity; `None` for audio encoders.
    pub video: Option<VideoSpec>,
}

/// The video-specific part of an [`EncoderSpec`]: resolution, frame rate, and
/// color format (PLAN.md §11).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct VideoSpec {
    /// Encoded width in pixels.
    pub width: u32,
    /// Encoded height in pixels.
    pub height: u32,
    /// Frame rate numerator.
    pub fps_num: u32,
    /// Frame rate denominator.
    pub fps_den: u32,
    /// Color format tag (`"nv12"`, `"p010_10le"`, ...); `None` when the
    /// backend default applies.
    pub color_format: Option<String>,
}

impl EncoderSpec {
    /// Builds the share-identity of a **video** encoder from its domain
    /// settings, the program video configuration, and an optional color
    /// format.
    pub fn from_video(
        settings: &EncoderSettings,
        video: &VideoConfig,
        color_format: Option<&str>,
    ) -> Self {
        Self {
            codec: settings.codec.clone(),
            bitrate_kbps: settings.bitrate_kbps,
            keyframe_interval: settings.keyframe_interval,
            settings: settings.settings.clone(),
            video: Some(VideoSpec {
                width: video.width,
                height: video.height,
                fps_num: video.fps_num,
                fps_den: video.fps_den,
                color_format: color_format.map(str::to_string),
            }),
        }
    }

    /// Builds the share-identity of an **audio** encoder. Audio specs carry no
    /// video dimensions; identical codec/bitrate/settings share.
    pub fn from_audio(settings: &EncoderSettings) -> Self {
        Self {
            codec: settings.codec.clone(),
            bitrate_kbps: settings.bitrate_kbps,
            keyframe_interval: settings.keyframe_interval,
            settings: settings.settings.clone(),
            video: None,
        }
    }

    /// Whether this spec describes a video encoder.
    pub fn is_video(&self) -> bool {
        self.video.is_some()
    }
}

impl fmt::Display for EncoderSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}kbps", self.codec, self.bitrate_kbps)?;
        if let Some(video) = &self.video {
            write!(
                f,
                " {}x{}@{}/{}fps",
                video.width, video.height, video.fps_num, video.fps_den
            )?;
            if let Some(format) = &video.color_format {
                write!(f, " {format}")?;
            }
        }
        Ok(())
    }
}
