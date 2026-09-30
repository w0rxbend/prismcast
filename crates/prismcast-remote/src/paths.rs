//! Socket path resolution and filesystem setup (IPC-001; ADR-0006 §1).
//!
//! The control socket lives at `$XDG_RUNTIME_DIR/prismcast/control.sock`.
//! When `XDG_RUNTIME_DIR` is unset (atypical session, container, tests) the
//! fallback is `/tmp/prismcast-$UID/control.sock`. In both cases the
//! directory is created with mode `0700` and the socket with mode `0600`, so
//! filesystem permissions are the primary access control (see
//! [`crate::auth`]).

use std::path::PathBuf;

/// The socket file name within the runtime directory.
pub const SOCKET_FILE_NAME: &str = "control.sock";

/// The default socket path: `$XDG_RUNTIME_DIR/prismcast/control.sock`, or
/// `/tmp/prismcast-$UID/control.sock` when `XDG_RUNTIME_DIR` is unset.
pub fn default_socket_path() -> PathBuf {
    default_socket_dir().join(SOCKET_FILE_NAME)
}

/// The directory holding the control socket.
pub fn default_socket_dir() -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()) {
        Some(dir) => PathBuf::from(dir).join("prismcast"),
        None => PathBuf::from("/tmp").join(format!("prismcast-{}", current_uid())),
    }
}

/// The effective UID, read from `/proc/self` metadata (no `libc` dependency).
/// Falls back to `"unknown"` when even that fails.
fn current_uid() -> String {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata("/proc/self")
        .map(|m| m.uid().to_string())
        .unwrap_or_else(|_| "unknown".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_path_is_under_prismcast_dir() {
        let path = default_socket_path();
        assert_eq!(
            path.file_name().and_then(|n| n.to_str()),
            Some(SOCKET_FILE_NAME)
        );
        let parent = path.parent().expect("has parent");
        assert!(
            parent.ends_with("prismcast") || parent.starts_with("/tmp/prismcast-"),
            "unexpected dir: {}",
            parent.display()
        );
    }
}
