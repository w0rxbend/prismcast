//! [`OutputBackend`]: one live output instance (PLAN.md §10–§14, ADR-0007).
//!
//! Each output in the graph (recording, N×RTMP, SRT, WHIP, virtual camera)
//! gets its own backend instance with independent state, statistics, and
//! failure handling — one broken output must never stop another.

use serde::{Deserialize, Serialize};

use prismcast_core::{OutputId, OutputKind, Result};

use crate::component::BackendComponent;

/// What a specific output backend instance can do, so callers can
/// enable/disable UI and skip unsupported calls instead of failing at
/// runtime.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputCapabilities {
    /// Supports pause/resume (PLAN.md §13 recording feature).
    pub pause_resume: bool,
    /// Supports manual file splitting (splitmux-style, PLAN.md §13).
    pub manual_split: bool,
    /// Produces meaningful [`OutputStats`] (e.g. `rtmp2sink`/`srtsink`
    /// `stats`, RES-003 §7).
    pub statistics: bool,
}

/// A point-in-time statistics snapshot of a running output.
///
/// Never persisted; polled by the media control actor for status displays
/// and health monitoring. Backend-specific extras (SRT RTT, RTMP ack
/// counters, ...) go into `extra`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OutputStats {
    /// Milliseconds since the output entered `Running`.
    pub active_duration_ms: u64,
    /// Bytes written/sent so far.
    pub bytes_total: u64,
    /// Video frames rendered by this output's branch.
    pub frames_rendered: u64,
    /// Video frames dropped (queue overflow, encoder lag, ...).
    pub frames_dropped: u64,
    /// Current combined bitrate in kbit/s, if measurable.
    pub current_bitrate_kbps: Option<u32>,
    /// Backend-specific counters (`rtmp2sink`/`srtsink` `stats` structures).
    pub extra: serde_json::Value,
}

/// Controls one live output instance.
///
/// The instance is created from a domain [`prismcast_core::Output`] plus its
/// encoders; wiring into the compositor/mixer graph is engine-internal.
/// Observed lifecycle is reported via [`BackendComponent`]; the media control
/// actor maps it onto the persisted [`prismcast_core::OutputState`] machine
/// (including reconnect bookkeeping per
/// [`prismcast_core::ReconnectPolicy`]).
///
/// Threading and failure semantics match [`crate::SourceBackend`].
pub trait OutputBackend: BackendComponent {
    /// The domain output this instance implements.
    fn output_id(&self) -> OutputId;

    /// The output destination kind (fixed at creation).
    fn kind(&self) -> OutputKind;

    /// What this instance can do.
    fn capabilities(&self) -> OutputCapabilities;

    /// Connects the branch and starts producing.
    fn start(&mut self) -> Result<()>;

    /// Drains and finalizes (muxer EOS, connection teardown), then stops.
    ///
    /// Finalization must complete before reporting
    /// [`crate::ComponentState::Stopped`] — MP4-class muxers are corrupt
    /// otherwise (RES-003 §6 EOS discipline).
    fn stop(&mut self) -> Result<()>;

    /// Pauses recording without tearing down (PLAN.md §13).
    fn pause(&mut self) -> Result<()>;

    /// Resumes a paused recording.
    fn resume(&mut self) -> Result<()>;

    /// Splits the current recording file at the next keyframe
    /// (splitmux-style). Backends should issue a force-keyframe upstream to
    /// align the split (RES-003 §5/§6).
    fn split(&mut self) -> Result<()>;

    /// Point-in-time statistics snapshot.
    fn statistics(&self) -> OutputStats;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_stats_serde_roundtrip() {
        let stats = OutputStats {
            active_duration_ms: 65_000,
            bytes_total: 48_750_000,
            frames_rendered: 3_890,
            frames_dropped: 4,
            current_bitrate_kbps: Some(6_000),
            extra: serde_json::json!({"rtt_ms": 24}),
        };
        let json = serde_json::to_string(&stats).unwrap();
        assert_eq!(stats, serde_json::from_str(&json).unwrap());
    }

    #[test]
    fn output_capabilities_serde_roundtrip() {
        let caps = OutputCapabilities {
            pause_resume: true,
            manual_split: true,
            statistics: false,
        };
        let json = serde_json::to_string(&caps).unwrap();
        assert_eq!(caps, serde_json::from_str(&json).unwrap());
    }
}
