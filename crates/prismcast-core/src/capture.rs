//! Framework-free transient capture observations, never persisted grants.
use serde::{Deserialize, Serialize};

/// Monotonic request token; a capture owner echoes it on completion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CaptureGeneration(u64);
impl CaptureGeneration {
    /// Wraps a request generation; only the application actor allocates live ones.
    pub const fn new(value: u64) -> Self {
        Self(value)
    }
    /// The monotonic value, for protocol mapping.
    pub const fn value(self) -> u64 {
        self.0
    }
}
/// Actual negotiated video pixels, not portal logical coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceDimensions {
    /// Pixel width.
    pub width: u32,
    /// Pixel height.
    pub height: u32,
}
/// Transient portal/capture lifecycle; absence of runtime means authorization is required.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureStatus {
    /// Explicit request is being processed.
    Authorizing,
    /// Negotiated native producer is usable.
    Active,
    /// User dismissed or canceled the request.
    Cancelled,
    /// Portal rejected access.
    Denied,
    /// A previously granted session was revoked.
    Revoked,
    /// Recoverable service or native producer failure.
    Failed,
}
/// Application-only runtime observation. Contains no session, node or FD grant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceRuntime {
    /// Request this observation belongs to.
    pub generation: CaptureGeneration,
    /// Current lifecycle.
    pub status: CaptureStatus,
    /// Actual native caps; required for Active and absent on terminal failure.
    pub dimensions: Option<SourceDimensions>,
    /// Bounded sanitized user-facing diagnostic.
    pub message: Option<String>,
}
