//! Crash-safe file writes (`docs/architecture/persistence-model.md` §5).
//!
//! Every persisted file is written with the same algorithm:
//! serialize fully into memory → open `<file>.tmp-<pid>-<seq>` in the *same
//! directory* with `O_EXCL` → write → fsync the temp file → `rename(2)` over
//! the target → fsync the containing directory. A crash at any point leaves
//! either the old file or the new file, never a torn one; leftover temp files
//! from crashed writers are reaped on load ([`reap_temp_files`]).
//!
//! All functions here are blocking syscall work — async callers must go
//! through `tokio::task::spawn_blocking` (the persistence actor does).

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::SystemTime;

static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// The moment this process started; temp files older than this belong to
/// crashed previous runs and are safe to reap.
pub fn process_start() -> SystemTime {
    static START: OnceLock<SystemTime> = OnceLock::new();
    *START.get_or_init(SystemTime::now)
}

/// Atomically writes `bytes` to `path` (see module docs). The file is created
/// with the given permission mode when it does not exist yet; an existing
/// file's mode is preserved only if the temp file inherits it — it does not,
/// so `mode` always applies (the rename installs the temp file).
pub fn atomic_write(path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no file name"))?
        .to_string_lossy();

    // O_EXCL loop: a collision means another writer (or a stale temp from a
    // crashed run sharing pid+seq) — bump the sequence and retry.
    let mut temp_path = PathBuf::new();
    let mut temp = None;
    for _ in 0..16 {
        let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
        temp_path = dir.join(format!(".{file_name}.tmp-{}-{seq}", std::process::id()));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(mode);
        }
        match options.open(&temp_path) {
            Ok(file) => {
                temp = Some(file);
                break;
            }
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
    }
    let mut temp = temp.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not allocate a unique temp file name",
        )
    })?;

    let result = (|| {
        temp.write_all(bytes)?;
        temp.sync_all()?;
        drop(temp);
        std::fs::rename(&temp_path, path)?;
        sync_dir(dir)?;
        Ok(())
    })();

    if result.is_err() {
        // Never leave a partial temp file behind on error.
        let _ = std::fs::remove_file(&temp_path);
    }
    result
}

/// Atomically copies `src` to `dst` (used for `.bak` refresh and restore).
pub fn atomic_copy(src: &Path, dst: &Path, mode: u32) -> io::Result<()> {
    let bytes = std::fs::read(src)?;
    atomic_write(dst, &bytes, mode)
}

/// Deletes leftover `.*.tmp-*` files in `dir` older than this process's start
/// time. Returns how many were reaped. Missing directory is not an error.
pub fn reap_temp_files(dir: &Path) -> io::Result<usize> {
    reap_temp_files_older_than(dir, process_start())
}

/// Testable core of [`reap_temp_files`]: reaps temp files whose modification
/// time is older than `cutoff`.
fn reap_temp_files_older_than(dir: &Path, cutoff: SystemTime) -> io::Result<usize> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(err) => return Err(err),
    };
    let mut reaped = 0;
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !(name.starts_with('.') && name.contains(".tmp-")) {
            continue;
        }
        let too_new = entry
            .metadata()
            .and_then(|m| m.modified())
            .map(|mtime| mtime >= cutoff)
            .unwrap_or(true);
        if too_new {
            continue;
        }
        std::fs::remove_file(entry.path())?;
        reaped += 1;
    }
    Ok(reaped)
}

/// fsyncs a directory so a rename inside it is durable.
fn sync_dir(dir: &Path) -> io::Result<()> {
    File::open(dir)?.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn atomic_write_roundtrip_and_replace() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("f.json");
        atomic_write(&file, b"one", 0o644).unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"one");
        atomic_write(&file, b"two", 0o644).unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"two");
        // No temp files remain after a successful write.
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
            .collect();
        assert!(leftovers.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_applies_mode() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("secret.toml");
        atomic_write(&file, b"k = 1", 0o600).unwrap();
        let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn atomic_copy_duplicates_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("a");
        let dst = dir.path().join("b");
        atomic_write(&src, b"payload", 0o644).unwrap();
        atomic_copy(&src, &dst, 0o600).unwrap();
        assert_eq!(std::fs::read(&dst).unwrap(), b"payload");
    }

    #[test]
    fn reaps_only_old_temp_files() {
        let dir = tempfile::tempdir().unwrap();
        let stale = dir.path().join(".collection.json.tmp-1-1");
        let fresh = dir.path().join(".collection.json.tmp-9-9");
        let keep = dir.path().join("collection.json");
        std::fs::write(&stale, b"x").unwrap();
        std::fs::write(&fresh, b"x").unwrap();
        std::fs::write(&keep, b"x").unwrap();

        // Cutoff in the future: both temp files are "old".
        let reaped =
            reap_temp_files_older_than(dir.path(), SystemTime::now() + Duration::from_secs(60))
                .unwrap();
        assert_eq!(reaped, 2);
        assert!(!stale.exists());
        assert!(!fresh.exists());
        assert!(keep.exists());

        // Cutoff in the past: nothing reaped.
        std::fs::write(&stale, b"x").unwrap();
        let reaped =
            reap_temp_files_older_than(dir.path(), SystemTime::now() - Duration::from_secs(60))
                .unwrap();
        assert_eq!(reaped, 0);
        assert!(stale.exists());
    }
}
