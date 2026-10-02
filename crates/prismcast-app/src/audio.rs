//! Bounded owner-authenticated audio telemetry and capture ingress (ADR-0023/24).

use std::collections::BTreeMap;
use std::sync::Arc;

use prismcast_core::{
    CaptureGeneration, CaptureStatus, Error, PipeWireAudioSettings, SourceId, SourceRuntime,
};
use tokio::sync::{mpsc, oneshot, watch};

use crate::{
    actor::ActorMessage,
    capture::{sanitize_message, validate_audio_runtime},
    AppSnapshot, HandleError,
};

/// Maximum simultaneously retained source observations.
pub const MAX_METER_SOURCES: usize = 32;
/// Maximum channels in each source observation.
pub const MAX_METER_CHANNELS: usize = 8;
/// Finite silence floor, preserving JSON interoperability.
pub const METER_FLOOR_DBFS: f32 = -120.0;

/// Latest measured levels for one source.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceMeter {
    /// Shared source identity.
    pub source_id: SourceId,
    /// Per-channel peak levels in dBFS.
    pub peak_dbfs: Vec<f32>,
    /// Per-channel RMS levels in dBFS.
    pub rms_dbfs: Vec<f32>,
}

/// Latest-only, bounded transient levels, independent of persisted state.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MeterSnapshot {
    /// At most [`MAX_METER_SOURCES`] entries with bounded channel arrays.
    pub levels: BTreeMap<SourceId, SourceMeter>,
}

/// Exclusive trusted audio owner. Dropping it revokes all reporter clones.
pub struct AudioOwner {
    /// Explicit capture effects only; snapshots never create authorizations.
    pub requests: mpsc::Receiver<AudioCaptureAuthorizationRequest>,
    /// Opaque capability for reporting native measurements.
    pub runtime: AudioRuntimeHandle,
    /// Latest command state; the reconciled revision accompanies every report.
    pub snapshots: watch::Receiver<Arc<AppSnapshot>>,
    pub(crate) _liveness: oneshot::Sender<()>,
}

/// Immutable command effect with frozen advisory settings, never a native grant.
#[derive(Debug)]
pub struct AudioCaptureAuthorizationRequest {
    /// Shared capture source.
    pub source_id: SourceId,
    /// Echo this token with lifecycle reports and captured measurements.
    pub generation: CaptureGeneration,
    /// Settings validated and frozen by the authorizing command.
    pub settings: PipeWireAudioSettings,
}

impl std::fmt::Debug for AudioOwner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioOwner").finish_non_exhaustive()
    }
}

/// Local-only reporter; controllers cannot fabricate its owner identity.
#[derive(Clone)]
pub struct AudioRuntimeHandle {
    pub(crate) tx: mpsc::Sender<ActorMessage>,
    pub(crate) owner_id: uuid::Uuid,
}

impl std::fmt::Debug for AudioRuntimeHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AudioRuntimeHandle(<capability>)")
    }
}

impl AudioRuntimeHandle {
    /// Reports native capture lifecycle without fabricated video dimensions.
    /// Only the application actor may initiate Authorizing status.
    pub async fn report_capture(
        &self,
        source_id: SourceId,
        generation: CaptureGeneration,
        status: CaptureStatus,
        message: Option<String>,
    ) -> Result<(), HandleError> {
        let runtime = SourceRuntime {
            generation,
            status,
            dimensions: None,
            message: sanitize_message(message),
        };
        validate_audio_runtime(&runtime)?;
        let (reply, result) = oneshot::channel();
        self.tx
            .send(ActorMessage::AudioCaptureRuntime {
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

    /// Reports physical audio only for its active authorization generation.
    /// The reconciled revision additionally excludes configuration races.
    pub async fn report_capture_levels(
        &self,
        revision: u64,
        generation: CaptureGeneration,
        source_id: SourceId,
        peak_dbfs: Vec<f32>,
        rms_dbfs: Vec<f32>,
    ) -> Result<(), HandleError> {
        self.report_levels_inner(revision, Some(generation), source_id, peak_dbfs, rms_dbfs)
            .await
    }

    /// Invalidates retained observations after native graph failure.
    /// This fabricates no measurement/event and changes no persisted state,
    /// command revision or history. The exclusive capability remains usable
    /// when the service retries on the next configuration revision.
    pub async fn clear_levels(&self) -> Result<(), HandleError> {
        let (reply, result) = oneshot::channel();
        self.tx
            .send(ActorMessage::ClearAudioLevels {
                owner_id: self.owner_id,
                reply,
            })
            .await
            .map_err(|_| HandleError::Shutdown)?;
        result
            .await
            .map_err(|_| HandleError::Shutdown)?
            .map_err(HandleError::Core)
    }

    /// Reports a diagnostic-tone observation through the bounded actor queue.
    /// Physical captures must use [`Self::report_capture_levels`] with a grant
    /// generation; this path rejects those source kinds.
    /// Rejected observations change neither events nor the latest meter watch.
    pub async fn report_levels(
        &self,
        revision: u64,
        source_id: SourceId,
        peak_dbfs: Vec<f32>,
        rms_dbfs: Vec<f32>,
    ) -> Result<(), HandleError> {
        self.report_levels_inner(revision, None, source_id, peak_dbfs, rms_dbfs)
            .await
    }

    async fn report_levels_inner(
        &self,
        revision: u64,
        generation: Option<CaptureGeneration>,
        source_id: SourceId,
        peak_dbfs: Vec<f32>,
        rms_dbfs: Vec<f32>,
    ) -> Result<(), HandleError> {
        let levels = SourceMeter {
            source_id,
            peak_dbfs,
            rms_dbfs,
        };
        validate_levels(&levels)?;
        let (reply, result) = oneshot::channel();
        self.tx
            .send(ActorMessage::AudioLevels {
                owner_id: self.owner_id,
                revision,
                generation,
                levels,
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

pub(crate) fn validate_levels(levels: &SourceMeter) -> Result<(), Error> {
    let channels = levels.peak_dbfs.len();
    if !(1..=MAX_METER_CHANNELS).contains(&channels)
        || channels != levels.rms_dbfs.len()
        || levels
            .peak_dbfs
            .iter()
            .chain(&levels.rms_dbfs)
            .any(|level| !level.is_finite() || *level < METER_FLOOR_DBFS)
    {
        return Err(Error::InvalidInput("invalid source audio levels".into()));
    }
    Ok(())
}
