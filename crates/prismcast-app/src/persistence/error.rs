//! Typed errors for the persistence layer.

use std::path::PathBuf;

/// Everything that can go wrong while loading or saving persisted state.
///
/// At layer boundaries this converts into [`prismcast_core::Error::Persistence`]
/// so controllers see the workspace-wide error type.
#[derive(Debug, thiserror::Error)]
pub enum PersistenceError {
    /// A filesystem operation failed.
    #[error("I/O error on {}: {source}", path.display())]
    Io {
        /// The file or directory being operated on.
        path: PathBuf,
        /// The underlying OS error.
        source: std::io::Error,
    },

    /// The file exists but is not well-formed (bad JSON/TOML, wrong types).
    #[error("{}: cannot parse: {reason}", path.display())]
    Parse {
        /// The file that failed to parse.
        path: PathBuf,
        /// What went wrong.
        reason: String,
    },

    /// No `schemaVersion` key (version 0 = pre-versioning, never shipped).
    #[error("{}: missing or invalid schemaVersion", path.display())]
    MissingSchemaVersion {
        /// The file lacking a version.
        path: PathBuf,
    },

    /// The file was written by a newer build. Never guessed at, never
    /// rewritten (ADR-0008): the file is left byte-identical.
    #[error("{}: schema version {found} is newer than supported {supported}", path.display())]
    NewerSchema {
        /// The file with the unsupported version.
        path: PathBuf,
        /// Version found in the file.
        found: u32,
        /// Highest version this build supports.
        supported: u32,
    },

    /// A migration step failed.
    #[error("{}: migration from schema v{from} failed: {reason}", path.display())]
    Migration {
        /// The file being migrated.
        path: PathBuf,
        /// The version the failed step migrates from.
        from: u32,
        /// What went wrong.
        reason: String,
    },

    /// The file parsed but failed domain validation (referential integrity).
    /// Treated as corruption for recovery purposes.
    #[error("{}: failed validation: {reason}", path.display())]
    Integrity {
        /// The invalid file.
        path: PathBuf,
        /// Which references are broken.
        reason: String,
    },

    /// Serializing an envelope failed (the filesystem is left untouched).
    #[error("serializing {what} failed: {reason}")]
    Serialize {
        /// Which envelope failed.
        what: &'static str,
        /// What went wrong.
        reason: String,
    },
}

impl PersistenceError {
    /// Wraps an I/O error with the path it happened on.
    pub fn io(path: impl Into<PathBuf>) -> impl FnOnce(std::io::Error) -> Self {
        let path = path.into();
        move |source| Self::Io { path, source }
    }
}

impl From<PersistenceError> for prismcast_core::error::Error {
    fn from(err: PersistenceError) -> Self {
        Self::Persistence(err.to_string())
    }
}

/// Convenience alias for persistence operations.
pub type Result<T> = std::result::Result<T, PersistenceError>;
