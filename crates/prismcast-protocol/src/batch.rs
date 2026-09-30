//! Request batches (RES-007 conclusion 7).
//!
//! A [`RequestBatch`] executes its members **serially, in order, as fast as
//! possible** — the only execution mode in protocol v1. Deliberately absent
//! from obs-websocket's modes:
//!
//! - `SerialFrame` (graphics-thread-synchronized execution) couples the RPC
//!   layer to the compositor; frame-accurate sequencing is a compositor
//!   concern, not a protocol one.
//! - `Parallel` undercuts ordering guarantees.
//!
//! Batches are **best-effort, not atomic**: a member that fails does not
//! roll back earlier members. Clients needing atomicity send a single
//! `transaction` request instead (which maps to
//! `prismcast_core::Command::Transaction`, all-or-nothing). Batches are not
//! allowed to nest.

use serde::{Deserialize, Serialize};

use crate::request::RequestKind;
use crate::response::{ResponseData, ResponseStatus};

/// One member of a [`RequestBatch`]: a request kind plus an optional
/// per-member correlation ID.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BatchRequest {
    /// Optional per-member correlation ID, echoed in the member's result.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// The operation to perform.
    #[serde(flatten)]
    pub kind: RequestKind,
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// A serial batch of requests, correlated as one unit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RequestBatch {
    /// Client-chosen correlation ID for the whole batch.
    pub request_id: String,
    /// Stop the batch at the first failed member (default `false`); the
    /// response then contains only the results produced so far.
    #[serde(default, skip_serializing_if = "is_false")]
    pub halt_on_failure: bool,
    /// The requests to execute, in order.
    pub requests: Vec<BatchRequest>,
}

/// The outcome of one batch member.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BatchResult {
    /// The member's correlation ID, if it had one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// The member's request type tag.
    pub request_type: String,
    /// Success/failure and structured error.
    pub status: ResponseStatus,
    /// Typed result payload; present on success when the member yields data.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<ResponseData>,
}

/// Server answer to a [`RequestBatch`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RequestBatchResponse {
    /// The batch's correlation ID, echoed verbatim.
    pub request_id: String,
    /// Per-member results, in execution order. Shorter than
    /// `requests` when `halt_on_failure` stopped the batch early.
    pub results: Vec<BatchResult>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{ErrorKind, WireError};

    #[test]
    fn batch_roundtrip() {
        let batch = RequestBatch {
            request_id: "batch-1".into(),
            halt_on_failure: true,
            requests: vec![
                BatchRequest {
                    request_id: Some("m-1".into()),
                    kind: RequestKind::AddScene { name: "A".into() },
                },
                BatchRequest {
                    request_id: None,
                    kind: RequestKind::SetStudioModeEnabled { enabled: true },
                },
            ],
        };
        let json = serde_json::to_string(&batch).unwrap();
        assert_eq!(batch, serde_json::from_str(&json).unwrap());
    }

    #[test]
    fn halt_on_failure_defaults_false_and_is_omitted() {
        let batch = RequestBatch {
            request_id: "b".into(),
            halt_on_failure: false,
            requests: Vec::new(),
        };
        let json = serde_json::to_string(&batch).unwrap();
        assert!(!json.contains("halt_on_failure"));
        let back: RequestBatch = serde_json::from_str(&json).unwrap();
        assert!(!back.halt_on_failure);
    }

    #[test]
    fn batch_response_roundtrip_mixed_results() {
        let response = RequestBatchResponse {
            request_id: "batch-1".into(),
            results: vec![
                BatchResult {
                    request_id: Some("m-1".into()),
                    request_type: "add_scene".into(),
                    status: ResponseStatus::ok(),
                    data: Some(ResponseData::SceneCreated {
                        scene_id: uuid::Uuid::new_v4(),
                    }),
                },
                BatchResult {
                    request_id: None,
                    request_type: "stop_output".into(),
                    status: ResponseStatus::error(WireError::new(
                        ErrorKind::StateConflict,
                        "output not running",
                    )),
                    data: None,
                },
            ],
        };
        let json = serde_json::to_string(&response).unwrap();
        assert_eq!(response, serde_json::from_str(&json).unwrap());
    }
}
