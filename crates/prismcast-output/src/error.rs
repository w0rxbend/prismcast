//! Typed errors for the output graph runtime model.

use prismcast_core::{EncoderId, OutputId, OutputState};
use thiserror::Error;

/// Errors produced by [`crate::OutputGraph`] and [`crate::OutputRuntime`]
/// operations.
///
/// These are runtime-graph errors, distinct from the workspace-wide
/// `prismcast_core::Error` used by the Command/Event API: a controller turns
/// them into `Error::InvalidInput` / `Error::Media` at the boundary.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum OutputGraphError {
    /// The referenced output is not part of the graph.
    #[error("unknown output: {0}")]
    UnknownOutput(OutputId),

    /// An output references an encoder whose settings were never registered.
    #[error("unknown encoder: {0}")]
    UnknownEncoder(EncoderId),

    /// An encoder was registered as audio but referenced as video, or the
    /// reverse.
    #[error("encoder kind mismatch: {0}")]
    EncoderKindMismatch(EncoderId),

    /// The output is already present in the graph.
    #[error("duplicate output: {0}")]
    DuplicateOutput(OutputId),

    /// An output can only be removed while `Stopped` or `Failed`
    /// (mirrors the domain rule in `prismcast-core`).
    #[error("output {output_id} must be stopped before removal (state: {state:?})")]
    OutputNotStopped {
        /// Output that was asked to be removed.
        output_id: OutputId,
        /// State the output was in.
        state: OutputState,
    },

    /// A state transition that the PLAN §61 lifecycle forbids.
    #[error("illegal output state transition for {output_id}: {from:?} -> {to:?}")]
    IllegalTransition {
        /// Output that attempted the transition.
        output_id: OutputId,
        /// State the transition started from.
        from: OutputState,
        /// State the transition targeted.
        to: OutputState,
    },

    /// A reconnect was requested from a state that cannot reconnect.
    #[error("output {0} cannot reconnect from its current state")]
    NotReconnectable(OutputId),
}

/// Convenience alias for output-graph results.
pub type Result<T> = std::result::Result<T, OutputGraphError>;
