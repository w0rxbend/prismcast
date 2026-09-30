//! [`EncoderBackend`] and [`EncoderRegistry`]: encoder instances and runtime
//! capability probing (PLAN.md §12, RES-003 §5).
//!
//! Hardware encoder availability is per-system (NVENC elements don't register
//! without CUDA; `vaav1enc` depends on the driver), so backends must be
//! probed at startup via [`EncoderRegistry::probe`] — never assumed.

use serde::{Deserialize, Serialize};

use prismcast_core::{EncoderId, EncoderSettings, Result};

use crate::component::BackendComponent;

/// The acceleration technology behind an encoder implementation.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HardwareAccel {
    /// CPU encoding (x264, x265, SVT-AV1, ...).
    #[default]
    Software,
    /// VA-API (Intel/AMD).
    VaApi,
    /// NVIDIA NVENC.
    Nvenc,
    /// Vulkan Video (experimental, RES-003 §5).
    VulkanVideo,
    /// Anything else (QSV-class, platform-specific, ...).
    Other(String),
}

/// One probed encoder implementation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EncoderCapability {
    /// Codec identifier matching [`EncoderSettings::codec`]
    /// (`"h264"`, `"h265"`, `"av1"`, `"aac"`, `"opus"`, ...).
    pub codec: String,
    /// Human-readable implementation name (`"vah264enc"`, `"x264enc"`, ...).
    pub display_name: String,
    /// Acceleration technology.
    pub hardware: HardwareAccel,
    /// Supported rate-control modes (`"cbr"`, `"vbr"`, `"crf"`, ...).
    pub rate_controls: Vec<String>,
    /// Whether the implementation honors force-keyframe requests
    /// (needed for replay-buffer save and splitmux alignment, RES-003 §5).
    pub supports_force_keyframe: bool,
}

/// Controls one live encoder instance.
///
/// Encoder instances may feed multiple outputs when settings are identical
/// (PLAN.md §11 shared-encoder tee); fan-out wiring is owned by the output
/// graph, not this trait.
///
/// Threading and failure semantics match [`crate::SourceBackend`].
pub trait EncoderBackend: BackendComponent {
    /// The domain encoder this instance implements.
    fn encoder_id(&self) -> EncoderId;

    /// The settings the instance was created with.
    fn settings(&self) -> &EncoderSettings;

    /// Applies live-adjustable settings (bitrate, rate-control parameters).
    ///
    /// Structural changes (codec, resolution-affecting parameters) are not
    /// required to be supported; backends return
    /// [`prismcast_core::Error::InvalidInput`] and the caller recreates the
    /// instance instead.
    fn update_settings(&mut self, settings: &EncoderSettings) -> Result<()>;

    /// Requests the next frame be encoded as a keyframe (replay-buffer save,
    /// recording split). Check [`EncoderCapability::supports_force_keyframe`]
    /// first; unsupported implementations return
    /// [`prismcast_core::Error::InvalidInput`].
    fn force_keyframe(&mut self) -> Result<()>;
}

/// Probes and instantiates encoder implementations (RES-003 conclusion 4).
///
/// The registry is stateless with respect to domain entities: it enumerates
/// what the current machine can do and creates instances on demand. It does
/// not implement [`BackendComponent`] because it is not a live media
/// component.
pub trait EncoderRegistry: Send {
    /// Probes the machine for usable encoder implementations.
    ///
    /// Implementations should query the media framework's element registry /
    /// device monitors, so the result reflects this system right now.
    fn probe(&self) -> Vec<EncoderCapability>;

    /// Creates an encoder instance for the given settings, or
    /// [`prismcast_core::Error::Media`] if no probed implementation can
    /// satisfy them.
    fn create(&self, settings: EncoderSettings) -> Result<Box<dyn EncoderBackend>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoder_capability_serde_roundtrip() {
        let capability = EncoderCapability {
            codec: "h264".to_string(),
            display_name: "vah264enc".to_string(),
            hardware: HardwareAccel::VaApi,
            rate_controls: vec!["cbr".to_string(), "vbr".to_string()],
            supports_force_keyframe: true,
        };
        let json = serde_json::to_string(&capability).unwrap();
        assert_eq!(capability, serde_json::from_str(&json).unwrap());

        for hardware in [
            HardwareAccel::Software,
            HardwareAccel::VaApi,
            HardwareAccel::Nvenc,
            HardwareAccel::VulkanVideo,
            HardwareAccel::Other("qsv".to_string()),
        ] {
            let json = serde_json::to_string(&hardware).unwrap();
            assert_eq!(hardware, serde_json::from_str(&json).unwrap());
        }
    }
}
