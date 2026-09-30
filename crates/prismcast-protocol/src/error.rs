//! Structured error payloads (RES-007 conclusion 6).
//!
//! obs-websocket signals request failures as an integer code plus a
//! free-text `comment`; the native protocol keeps the grouped integer code
//! space (convenient for client `switch` statements, and numerically aligned
//! with obs-websocket's ranges to ease the future adapter) but replaces the
//! comment with a structured payload: a typed [`ErrorKind`], a human message,
//! and optional machine-readable context (`field`, `details`).
//!
//! Code ranges:
//!
//! | Range | Meaning |
//! |------:|---------|
//! | 2xx   | Request-shape errors (unknown request, malformed batch, not ready) |
//! | 3xx   | Missing data (required field absent) |
//! | 4xx   | Invalid field values (type, range, empty) |
//! | 5xx   | State conflicts (output running, studio mode active, ...) |
//! | 6xx   | Resource problems (not found, already exists, wrong type) |
//! | 7xx   | Action failures (processing failed, cannot act) |
//! | 8xx   | Authorization (insufficient permissions) |
//! | 9xx   | Rate/overload (rate limited, subscription invalid) |

use serde::{Deserialize, Serialize};

/// Numeric code constants, grouped by range. Servers should prefer
/// [`ErrorKind::default_code`] unless a more specific code is documented.
pub mod codes {
    /// Catch-all request failure.
    pub const GENERIC_ERROR: u16 = 200;
    /// The `request` tag is missing from a request payload.
    pub const MISSING_REQUEST_TYPE: u16 = 201;
    /// The request type is not known to this server.
    pub const UNKNOWN_REQUEST_TYPE: u16 = 202;
    /// A batch contained an unsupported shape (e.g. nested batch).
    pub const INVALID_BATCH: u16 = 203;
    /// The server is not ready (e.g. mid scene-collection switch).
    pub const NOT_READY: u16 = 204;

    /// A required field is absent.
    pub const MISSING_FIELD: u16 = 300;

    /// A field value failed validation.
    pub const INVALID_FIELD: u16 = 400;
    /// A field has the wrong JSON type.
    pub const INVALID_FIELD_TYPE: u16 = 401;
    /// A numeric field is out of its allowed range.
    pub const FIELD_OUT_OF_RANGE: u16 = 402;

    /// The request conflicts with current state (illegal lifecycle
    /// transition, studio-mode precondition, delete-policy rejection).
    pub const STATE_CONFLICT: u16 = 500;

    /// A referenced entity does not exist.
    pub const NOT_FOUND: u16 = 600;
    /// The request would create a duplicate of a unique entity.
    pub const ALREADY_EXISTS: u16 = 601;

    /// The operation failed in the domain or media layer.
    pub const PROCESSING_FAILED: u16 = 700;

    /// The session lacks the permission the request requires.
    pub const FORBIDDEN: u16 = 800;

    /// The client exceeded a rate limit.
    pub const RATE_LIMITED: u16 = 900;
    /// A subscription set failed validation.
    pub const INVALID_SUBSCRIPTION: u16 = 901;
}

/// The machine-switchable failure class of a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    /// The request payload itself is malformed.
    InvalidRequest,
    /// A required field is absent.
    MissingField,
    /// A field value failed validation.
    InvalidField,
    /// The request conflicts with current state.
    StateConflict,
    /// A referenced entity does not exist.
    NotFound,
    /// The request would create a duplicate.
    AlreadyExists,
    /// The operation failed while processing.
    ProcessingFailed,
    /// The server is temporarily not ready.
    NotReady,
    /// The session lacks the required permission.
    Forbidden,
    /// The client exceeded a rate limit.
    RateLimited,
    /// A subscription set failed validation.
    InvalidSubscription,
    /// Anything else.
    Internal,
}

impl ErrorKind {
    /// The default numeric code for this kind (see module docs).
    pub fn default_code(self) -> u16 {
        match self {
            Self::InvalidRequest => codes::GENERIC_ERROR,
            Self::MissingField => codes::MISSING_FIELD,
            Self::InvalidField => codes::INVALID_FIELD,
            Self::StateConflict => codes::STATE_CONFLICT,
            Self::NotFound => codes::NOT_FOUND,
            Self::AlreadyExists => codes::ALREADY_EXISTS,
            Self::ProcessingFailed => codes::PROCESSING_FAILED,
            Self::NotReady => codes::NOT_READY,
            Self::Forbidden => codes::FORBIDDEN,
            Self::RateLimited => codes::RATE_LIMITED,
            Self::InvalidSubscription => codes::INVALID_SUBSCRIPTION,
            Self::Internal => codes::GENERIC_ERROR,
        }
    }
}

/// A structured request failure.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireError {
    /// Grouped numeric code (see [`codes`]).
    pub code: u16,
    /// Machine-switchable failure class.
    pub kind: ErrorKind,
    /// Human-readable explanation (logs, UIs); never the only information.
    pub message: String,
    /// The offending request field, when applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    /// Kind-specific machine-readable context (expected type/range, the
    /// conflicting state, the invalid subscription entries, ...).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

impl WireError {
    /// Builds an error with the kind's default code.
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            code: kind.default_code(),
            kind,
            message: message.into(),
            field: None,
            details: None,
        }
    }

    /// Attaches the offending field name.
    pub fn with_field(mut self, field: impl Into<String>) -> Self {
        self.field = Some(field.into());
        self
    }

    /// Attaches machine-readable details.
    pub fn with_details(mut self, details: serde_json::Value) -> Self {
        self.details = Some(details);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_codes_land_in_documented_ranges() {
        assert_eq!(ErrorKind::MissingField.default_code() / 100, 3);
        assert_eq!(ErrorKind::InvalidField.default_code() / 100, 4);
        assert_eq!(ErrorKind::StateConflict.default_code() / 100, 5);
        assert_eq!(ErrorKind::NotFound.default_code() / 100, 6);
        assert_eq!(ErrorKind::ProcessingFailed.default_code() / 100, 7);
        assert_eq!(ErrorKind::Forbidden.default_code() / 100, 8);
        assert_eq!(ErrorKind::RateLimited.default_code() / 100, 9);
    }

    #[test]
    fn error_roundtrip_and_optional_fields() {
        let err = WireError::new(ErrorKind::NotFound, "scene does not exist");
        let json = serde_json::to_string(&err).unwrap();
        assert_eq!(
            json,
            r#"{"code":600,"kind":"not_found","message":"scene does not exist"}"#
        );
        assert_eq!(err, serde_json::from_str(&json).unwrap());

        let detailed = err
            .with_field("scene_id")
            .with_details(serde_json::json!({"hint": "list_scenes first"}));
        let json = serde_json::to_string(&detailed).unwrap();
        assert_eq!(detailed, serde_json::from_str(&json).unwrap());
    }
}
