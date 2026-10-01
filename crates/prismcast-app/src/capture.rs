//! Bounded capability interface between the application actor and capture owner.
use crate::{
    actor::{ActorMessage, HandleError},
    AppSnapshot,
};
use prismcast_core::{CaptureGeneration, CaptureStatus, SourceDimensions, SourceId, SourceRuntime};
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot, watch};

/// Maximum queued requests and retained runtime observations.
pub const CAPTURE_CAPACITY: usize = 8;
/// Maximum bytes in an ephemeral exported parent identifier.
pub const MAX_CAPTURE_PARENT_BYTES: usize = 2048;
/// Maximum bytes in a user-facing runtime diagnostic.
pub const MAX_CAPTURE_MESSAGE_BYTES: usize = 512;

/// Validated local-only parent context; Debug never exposes the identifier.
pub struct CaptureParentWindow(pub(crate) String);
impl std::fmt::Debug for CaptureParentWindow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CaptureParentWindow(<redacted>)")
    }
}
impl CaptureParentWindow {
    pub(crate) fn new(value: String) -> Result<Self, HandleError> {
        let valid = value.strip_prefix("wayland:").is_some_and(|v| {
            !v.is_empty() && v.chars().all(|c| !c.is_control() && !c.is_whitespace())
        }) || value
            .strip_prefix("x11:")
            .is_some_and(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_hexdigit()));
        if value.len() > MAX_CAPTURE_PARENT_BYTES || !valid {
            return Err(prismcast_core::Error::InvalidInput(
                "invalid capture parent context".into(),
            )
            .into());
        }
        Ok(Self(value))
    }
}
/// Immutable explicit authorization effect; contains no captured grant.
pub struct CaptureAuthorizationRequest {
    /// Shared source identity.
    pub source_id: SourceId,
    /// Echo this token with every asynchronous runtime update.
    pub generation: CaptureGeneration,
    /// Local exported parent context, not a wire/persistent field.
    pub parent_window: Option<String>,
}
impl std::fmt::Debug for CaptureAuthorizationRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CaptureAuthorizationRequest")
            .field("source_id", &self.source_id)
            .field("generation", &self.generation)
            .field(
                "parent_window",
                &self.parent_window.as_ref().map(|_| "<redacted>"),
            )
            .finish()
    }
}
/// Exclusive request receiver. Dropping it revokes its reporting capability.
pub struct CaptureOwner {
    /// Bounded explicit picker requests; snapshots never create requests.
    pub requests: mpsc::Receiver<CaptureAuthorizationRequest>,
    /// Capability for reporting terminal states and negotiated caps.
    pub runtime: CaptureRuntimeHandle,
    /// Latest source/runtime state for cancellation on removal/disable/generation change.
    pub snapshots: watch::Receiver<Arc<AppSnapshot>>,
    pub(crate) _liveness: oneshot::Sender<()>,
}
impl std::fmt::Debug for CaptureOwner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CaptureOwner").finish_non_exhaustive()
    }
}
/// Opaque owner-authenticated reporter. Ordinary AppHandle controllers cannot report.
#[derive(Clone)]
pub struct CaptureRuntimeHandle {
    pub(crate) tx: mpsc::Sender<ActorMessage>,
    pub(crate) owner_id: uuid::Uuid,
}
impl std::fmt::Debug for CaptureRuntimeHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CaptureRuntimeHandle(<capability>)")
    }
}
impl CaptureRuntimeHandle {
    /// Reports a native observation through the bounded actor queue.
    /// Graph mutation must happen outside runtime block_on calls.
    pub async fn report(
        &self,
        source_id: SourceId,
        generation: CaptureGeneration,
        status: CaptureStatus,
        dimensions: Option<SourceDimensions>,
        message: Option<String>,
    ) -> Result<(), HandleError> {
        let message = message.map(|text| {
            let mut bounded = String::new();
            for c in text.chars() {
                let c = if c.is_control() { ' ' } else { c };
                if bounded.len() + c.len_utf8() > MAX_CAPTURE_MESSAGE_BYTES {
                    break;
                }
                bounded.push(c);
            }
            bounded
        });
        let runtime = SourceRuntime {
            generation,
            status,
            dimensions,
            message,
        };
        validate_runtime(&runtime)?;
        let (reply, result) = oneshot::channel();
        self.tx
            .send(ActorMessage::CaptureRuntime {
                owner_id: self.owner_id,
                source_id,
                runtime,
                reply,
            })
            .await
            .map_err(|_| HandleError::Shutdown)?;
        result
            .await
            .map_err(|_| HandleError::Shutdown)?
            .map_err(HandleError::Core)
    }
}
pub(crate) fn validate_runtime(runtime: &SourceRuntime) -> Result<(), prismcast_core::Error> {
    if runtime.status == CaptureStatus::Authorizing {
        return Err(prismcast_core::Error::InvalidInput(
            "only the application starts capture authorization".into(),
        ));
    }
    let valid_caps = runtime
        .dimensions
        .is_some_and(|d| (1..=8192).contains(&d.width) && (1..=8192).contains(&d.height));
    if (runtime.status == CaptureStatus::Active && !valid_caps)
        || (runtime.status != CaptureStatus::Active && runtime.dimensions.is_some())
    {
        return Err(prismcast_core::Error::InvalidInput(
            "invalid negotiated capture dimensions".into(),
        ));
    }
    if runtime
        .message
        .as_ref()
        .is_some_and(|s| s.len() > MAX_CAPTURE_MESSAGE_BYTES || s.chars().any(char::is_control))
    {
        return Err(prismcast_core::Error::InvalidInput(
            "invalid capture runtime diagnostic".into(),
        ));
    }
    Ok(())
}
