//! Shared error model for all Prismcast layers.
//!
//! Every fallible operation in the workspace returns [`Result`] with this
//! [`Error`] type, so controllers (GTK, CLI, WebSocket, IPC, Web UI) can
//! render failures uniformly.

use thiserror::Error;

/// The workspace-wide error type.
#[derive(Debug, Error)]
pub enum Error {
    /// A referenced entity (scene, source, output, ...) does not exist.
    #[error("not found: {0}")]
    NotFound(String),

    /// A command or configuration value failed validation.
    #[error("invalid input: {0}")]
    InvalidInput(String),

    /// A conditional edit's expectation no longer matches authoritative
    /// state (ADR-0026); nothing was changed.
    #[error("conflict: {0}")]
    Conflict(String),

    /// The media engine (GStreamer graph, capture, encoding) failed.
    #[error("media error: {0}")]
    Media(String),

    /// A filesystem or OS I/O operation failed.
    #[error("I/O error: {0}")]
    Io(String),

    /// Loading or saving persisted state (profiles, scene collections) failed.
    #[error("persistence error: {0}")]
    Persistence(String),

    /// Wire-protocol (IPC/WebSocket) encoding, decoding, or versioning failed.
    #[error("protocol error: {0}")]
    Protocol(String),

    /// The caller is not authenticated or not allowed to perform the operation.
    #[error("unauthorized: {0}")]
    Unauthorized(String),
}

/// Convenience alias used across the workspace.
pub type Result<T> = std::result::Result<T, Error>;

impl From<std::io::Error> for Error {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_includes_variant_context() {
        let err = Error::NotFound("scene abc".to_string());
        assert_eq!(err.to_string(), "not found: scene abc");
    }

    #[test]
    fn io_error_converts() {
        let io = std::io::Error::other("boom");
        let err: Error = io.into();
        assert!(matches!(err, Error::Io(_)));
        assert_eq!(err.to_string(), "I/O error: boom");
    }

    #[test]
    fn result_alias_roundtrips_values() {
        fn fallible(ok: bool) -> Result<u32> {
            if ok {
                Ok(42)
            } else {
                Err(Error::InvalidInput("nope".to_string()))
            }
        }
        assert_eq!(fallible(true).ok(), Some(42));
        assert!(fallible(false).is_err());
    }
}
