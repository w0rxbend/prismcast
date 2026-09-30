//! Output graph domain (PLAN.md §10–§11, ADR-0007).
//!
//! Outputs are **never** modeled as a singleton/optional: the state holds an
//! unbounded set of independent [`Output`]s (recording, N×RTMP, SRT, WHIP,
//! virtual camera). Each output owns its state machine, reconnect policy,
//! encoders, and credentials — one broken output must never stop another.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::id::{EncoderId, OutputId, ServiceId};

/// A single output destination in the output graph.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Output {
    /// Unique output ID.
    pub id: OutputId,
    /// Output destination kind.
    pub kind: OutputKind,
    /// User-facing name.
    pub name: String,
    /// Video encoder feeding this output.
    pub video_encoder: EncoderId,
    /// Audio encoders feeding this output (one per output track).
    pub audio_encoders: Vec<EncoderId>,
    /// Streaming service configuration, for network outputs.
    pub service: Option<ServiceId>,
    /// Reconnect/backoff policy for network outputs.
    pub reconnect_policy: ReconnectPolicy,
    /// Lifecycle state (PLAN.md §61 failure model).
    pub state: OutputState,
}

impl Output {
    /// Creates a stopped output with a fresh ID.
    pub fn new(kind: OutputKind, name: impl Into<String>, video_encoder: EncoderId) -> Self {
        Self {
            id: OutputId::new(),
            kind,
            name: name.into(),
            video_encoder,
            audio_encoders: Vec::new(),
            service: None,
            reconnect_policy: ReconnectPolicy::default(),
            state: OutputState::Stopped,
        }
    }
}

/// Output destination kinds (PLAN.md §10).
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

/// Output lifecycle state (PLAN.md §61 failure model).
///
/// Legal transitions are enforced by `crate::state::apply`:
/// `Stopped|Failed → Starting → Running|Failed`, `Running → Degraded |
/// Reconnecting | Stopping`, `Degraded|Reconnecting → Running | Failed |
/// Stopping`, `Stopping → Stopped`.
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

impl Default for ReconnectPolicy {
    fn default() -> Self {
        Self {
            max_retries: 10,
            initial_backoff_ms: 1_000,
            max_backoff_ms: 30_000,
        }
    }
}

/// Video/audio encoder descriptor. Encoder instances may be shared between
/// outputs when settings are identical (PLAN.md §11 shared-encoder tee).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EncoderSettings {
    /// Unique encoder ID.
    pub id: EncoderId,
    /// Codec identifier (`"h264"`, `"h265"`, `"av1"`, `"aac"`, `"opus"`, ...).
    pub codec: String,
    /// Target bitrate in kbit/s.
    pub bitrate_kbps: u32,
    /// Optional keyframe/GOP interval in frames.
    pub keyframe_interval: Option<u32>,
    /// Codec/backend-specific settings (rate control, profile, preset, ...).
    pub settings: serde_json::Value,
}

/// Streaming service descriptor (server endpoint + credentials).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Service {
    /// Unique service ID.
    pub id: ServiceId,
    /// User-facing name (`"Twitch"`, `"YouTube"`, ...).
    pub name: String,
    /// Server/ingest URL.
    pub url: String,
    /// Stream key or passphrase; redacted in logs and display.
    pub key: SecretString,
    /// Extra service-specific settings (bind IP, auth flags, ...).
    pub settings: serde_json::Value,
}

/// A secret string (stream key, passphrase) that serializes for persistence
/// but never leaks through `Display`/`Debug`.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SecretString(String);

impl SecretString {
    /// Wraps a secret value.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Exposes the secret. Call sites must never log the result.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED]")
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretString([REDACTED])")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_serde_roundtrip() {
        let mut output = Output::new(OutputKind::Rtmp, "Twitch", EncoderId::new());
        output.audio_encoders = vec![EncoderId::new(), EncoderId::new()];
        output.service = Some(ServiceId::new());
        output.state = OutputState::Reconnecting { attempt: 3 };
        output.reconnect_policy = ReconnectPolicy {
            max_retries: 5,
            initial_backoff_ms: 250,
            max_backoff_ms: 10_000,
        };
        let json = serde_json::to_string(&output).unwrap();
        let back: Output = serde_json::from_str(&json).unwrap();
        assert_eq!(output, back);
    }

    #[test]
    fn output_state_serde_roundtrip_all_variants() {
        let states = [
            OutputState::Stopped,
            OutputState::Starting,
            OutputState::Running,
            OutputState::Reconnecting { attempt: 1 },
            OutputState::Degraded,
            OutputState::Failed,
            OutputState::Stopping,
        ];
        for state in states {
            let json = serde_json::to_string(&state).unwrap();
            let back: OutputState = serde_json::from_str(&json).unwrap();
            assert_eq!(state, back);
        }
    }

    #[test]
    fn encoder_and_service_serde_roundtrip() {
        let encoder = EncoderSettings {
            id: EncoderId::new(),
            codec: "h264".to_string(),
            bitrate_kbps: 6_000,
            keyframe_interval: Some(120),
            settings: serde_json::json!({"preset": "veryfast"}),
        };
        let json = serde_json::to_string(&encoder).unwrap();
        assert_eq!(encoder, serde_json::from_str(&json).unwrap());

        let service = Service {
            id: ServiceId::new(),
            name: "Twitch".to_string(),
            url: "rtmps://live.twitch.tv/app".to_string(),
            key: SecretString::new("live_123"),
            settings: serde_json::Value::Null,
        };
        let json = serde_json::to_string(&service).unwrap();
        assert!(json.contains("live_123"), "secret must persist");
        assert_eq!(service, serde_json::from_str(&json).unwrap());
    }

    #[test]
    fn secret_string_redacts_display_and_debug() {
        let secret = SecretString::new("super-secret-key");
        assert_eq!(secret.to_string(), "[REDACTED]");
        assert_eq!(format!("{secret:?}"), "SecretString([REDACTED])");
        assert_eq!(secret.expose(), "super-secret-key");
    }

    #[test]
    fn output_kind_serde_roundtrip() {
        for kind in [
            OutputKind::Recording,
            OutputKind::Rtmp,
            OutputKind::Srt,
            OutputKind::Whip,
            OutputKind::VirtualCamera,
        ] {
            let json = serde_json::to_string(&kind).unwrap();
            assert_eq!(kind, serde_json::from_str(&json).unwrap());
        }
    }
}
