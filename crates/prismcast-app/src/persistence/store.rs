//! The on-disk store: load/save with the corruption-recovery state machine
//! (`docs/architecture/persistence-model.md` §5–§6).
//!
//! All methods are blocking filesystem operations; the persistence actor
//! calls them inside `tokio::task::spawn_blocking`.
//!
//! ## Load state machine
//!
//! ```text
//! parse primary
//!   ok        → validate → use; refresh .bak if bytes differ
//!   failure   → parse primary.bak
//!       ok      → restore: copy .bak over primary (atomic), Recovery::FromBackup
//!       failure → Recovery::Defaults with fresh defaults; nothing deleted
//! ```
//!
//! `.bak` is refreshed only from verified-good content (a file that parsed,
//! migrated, and validated). A schema newer than supported is *not*
//! corruption: it returns [`PersistenceError::NewerSchema`] and the file is
//! left byte-identical.
//!
//! Quarantine of unrecoverable files (`<file>.corrupt-<timestamp>`) is
//! deliberately **not** done here: the spec gates it on explicit user
//! confirmation, which needs UI flow — a follow-up. Nothing is ever deleted.

use std::path::PathBuf;

use prismcast_core::project::VideoConfig;
use prismcast_core::project::{Profile, SceneCollection};

use super::atomic;
use super::envelope::{CollectionFileV1, CollectionSnapshot, SessionState};
use super::error::{PersistenceError, Result};
use super::paths::{backup_path, create_dir_private, ConfigRoot};
use super::pointer::{PointerDocument, PointerState};
use super::profile::{ProfileDocument, ProfileSnapshot};
use super::validate;

/// File mode for collections and the pointer file (no secrets).
const MODE_PLAIN: u32 = 0o644;
/// File mode for profiles, which carry `SecretString` stream keys.
const MODE_SECRET: u32 = 0o600;

/// How a load ended — surfaced so callers can notify the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recovery {
    /// The primary file loaded cleanly.
    Clean,
    /// The primary was corrupt/missing and the verified `.bak` copy was
    /// restored over it. The reason describes the primary's failure.
    FromBackup {
        /// Why the primary could not be used.
        reason: String,
    },
    /// Both primary and backup were unusable (or the file never existed):
    /// fresh defaults are in use. Prismcast always boots (PLAN §61).
    Defaults {
        /// Why neither file could be used (`None` = file simply absent).
        reason: Option<String>,
    },
}

/// The result of a load: the value plus how it was obtained.
#[derive(Debug)]
pub struct LoadOutcome<T> {
    /// The loaded (or default) value.
    pub value: T,
    /// How the load ended.
    pub recovery: Recovery,
    /// Whether a schema migration ran (callers re-save at the current
    /// version so migration cost is paid once).
    pub migrated: bool,
    /// Non-fatal warnings (e.g. reset session references).
    pub warnings: Vec<String>,
}

impl<T> LoadOutcome<T> {
    fn clean(value: T, migrated: bool, warnings: Vec<String>) -> Self {
        Self {
            value,
            recovery: Recovery::Clean,
            migrated,
            warnings,
        }
    }
}

/// A loaded collection plus its retained envelope (unknown fields for the
/// next save).
#[derive(Debug)]
pub struct LoadedCollection {
    /// The domain aggregate.
    pub snapshot: CollectionSnapshot,
    /// The envelope as loaded; pass back to [`ProjectStore::save_collection`]
    /// as `retained` so unknown fields survive.
    pub envelope: CollectionFileV1,
}

/// A loaded profile plus its retained TOML document (unknown keys and
/// comments for the next save).
#[derive(Debug)]
pub struct LoadedProfile {
    /// The domain aggregate.
    pub snapshot: ProfileSnapshot,
    /// The retained document; pass back to [`ProjectStore::save_profile`] as
    /// `retained` so unknown keys and comments survive.
    pub document: ProfileDocument,
    /// Whether a schema migration ran.
    pub migrated: bool,
}

/// Owns the config tree layout. The persistence actor is the only component
/// that opens files under the root.
#[derive(Debug, Clone)]
pub struct ProjectStore {
    root: ConfigRoot,
}

impl ProjectStore {
    /// Creates a store rooted at the given config root.
    pub fn new(root: ConfigRoot) -> Self {
        Self { root }
    }

    /// The config root.
    pub fn root(&self) -> &ConfigRoot {
        &self.root
    }

    // --- collections ---

    /// Saves a collection snapshot atomically (`collection.json`, 0644).
    /// Returns the envelope as written, to become the new `retained`.
    pub fn save_collection(
        &self,
        slug: &str,
        snapshot: &CollectionSnapshot,
        retained: Option<&CollectionFileV1>,
    ) -> Result<CollectionFileV1> {
        self.root
            .ensure_dirs()
            .map_err(PersistenceError::io(self.root.root()))?;
        let dir = self.root.collection_dir(slug);
        create_dir_private(&dir).map_err(PersistenceError::io(&dir))?;
        let envelope = CollectionFileV1::from_snapshot(snapshot, retained);
        let bytes = envelope.to_json_bytes()?;
        let path = self.root.collection_file(slug);
        atomic::atomic_write(&path, &bytes, MODE_PLAIN).map_err(PersistenceError::io(&path))?;
        Ok(envelope)
    }

    /// Loads a collection with the full recovery state machine.
    pub fn load_collection(&self, slug: &str) -> Result<LoadOutcome<LoadedCollection>> {
        let dir = self.root.collection_dir(slug);
        // Reap temp files from crashed writers (persistence-model §5).
        atomic::reap_temp_files(&dir).map_err(PersistenceError::io(&dir))?;
        let primary = self.root.collection_file(slug);

        match load_collection_file(&primary) {
            Ok(loaded) => {
                // Refresh .bak from verified-good content if it differs.
                refresh_backup(&primary, MODE_PLAIN)?;
                Ok(loaded)
            }
            // Newer schema is not corruption: report, never fall back or
            // rewrite the file.
            Err(err @ PersistenceError::NewerSchema { .. }) => Err(err),
            Err(primary_err) => {
                let backup = backup_path(&primary);
                match load_collection_file(&backup) {
                    Ok(mut outcome) => {
                        // Restore the verified copy over the primary.
                        atomic::atomic_copy(&backup, &primary, MODE_PLAIN)
                            .map_err(PersistenceError::io(&primary))?;
                        outcome.recovery = Recovery::FromBackup {
                            reason: primary_err.to_string(),
                        };
                        Ok(outcome)
                    }
                    Err(_) => Ok(LoadOutcome {
                        value: default_collection(),
                        recovery: Recovery::Defaults {
                            reason: if primary.exists() {
                                Some(primary_err.to_string())
                            } else {
                                None
                            },
                        },
                        migrated: false,
                        warnings: Vec::new(),
                    }),
                }
            }
        }
    }

    // --- profiles ---

    /// Saves a profile snapshot atomically (`profile.toml`, 0600 — it carries
    /// stream keys). Returns the document as written, to become the new
    /// `retained`.
    pub fn save_profile(
        &self,
        slug: &str,
        snapshot: &ProfileSnapshot,
        retained: Option<&ProfileDocument>,
    ) -> Result<ProfileDocument> {
        self.root
            .ensure_dirs()
            .map_err(PersistenceError::io(self.root.root()))?;
        let dir = self.root.profile_dir(slug);
        create_dir_private(&dir).map_err(PersistenceError::io(&dir))?;
        let bytes = ProfileDocument::to_bytes(snapshot, retained)?;
        let path = self.root.profile_file(slug);
        atomic::atomic_write(&path, &bytes, MODE_SECRET).map_err(PersistenceError::io(&path))?;
        // Re-parse what we wrote so the returned retained document is exactly
        // the on-disk content (and a self-check that our writes parse).
        ProfileDocument::parse(&bytes, &path)
    }

    /// Loads a profile with the full recovery state machine.
    pub fn load_profile(&self, slug: &str) -> Result<LoadOutcome<LoadedProfile>> {
        let dir = self.root.profile_dir(slug);
        atomic::reap_temp_files(&dir).map_err(PersistenceError::io(&dir))?;
        let primary = self.root.profile_file(slug);

        match load_profile_file(&primary) {
            Ok(loaded) => {
                refresh_backup(&primary, MODE_SECRET)?;
                Ok(loaded)
            }
            Err(err @ PersistenceError::NewerSchema { .. }) => Err(err),
            Err(primary_err) => {
                let backup = backup_path(&primary);
                match load_profile_file(&backup) {
                    Ok(mut outcome) => {
                        atomic::atomic_copy(&backup, &primary, MODE_SECRET)
                            .map_err(PersistenceError::io(&primary))?;
                        outcome.recovery = Recovery::FromBackup {
                            reason: primary_err.to_string(),
                        };
                        Ok(outcome)
                    }
                    Err(_) => Ok(LoadOutcome {
                        value: default_profile(),
                        recovery: Recovery::Defaults {
                            reason: if primary.exists() {
                                Some(primary_err.to_string())
                            } else {
                                None
                            },
                        },
                        migrated: false,
                        warnings: Vec::new(),
                    }),
                }
            }
        }
    }

    // --- pointer file ---

    /// Saves the pointer file atomically (no `.bak` — recovery is trivial).
    pub fn save_pointer(
        &self,
        state: &PointerState,
        retained: Option<&PointerDocument>,
    ) -> Result<PointerDocument> {
        self.root
            .ensure_dirs()
            .map_err(PersistenceError::io(self.root.root()))?;
        let bytes = PointerDocument::to_bytes(state, retained)?;
        let path = self.root.pointer_file();
        atomic::atomic_write(&path, &bytes, MODE_PLAIN).map_err(PersistenceError::io(&path))?;
        PointerDocument::parse(&bytes, &path)
    }

    /// Loads the pointer file. A missing or corrupt pointer is **not** an
    /// error: recovery is picking the first profile/collection on disk, done
    /// by the caller with [`ProjectStore::list_profile_slugs`] /
    /// [`ProjectStore::list_collection_slugs`].
    pub fn load_pointer(&self) -> LoadOutcome<PointerState> {
        let path = self.root.pointer_file();
        // Best-effort: a failure to reap must not block pointer recovery.
        let _ = atomic::reap_temp_files(self.root.root());
        match std::fs::read(&path) {
            Ok(bytes) => match PointerDocument::parse(&bytes, &path) {
                Ok(doc) => LoadOutcome::clean(doc.state, false, Vec::new()),
                Err(err) => LoadOutcome {
                    value: PointerState::default(),
                    recovery: Recovery::Defaults {
                        reason: Some(err.to_string()),
                    },
                    migrated: false,
                    warnings: Vec::new(),
                },
            },
            Err(_) => LoadOutcome {
                value: PointerState::default(),
                recovery: Recovery::Defaults { reason: None },
                migrated: false,
                warnings: Vec::new(),
            },
        }
    }

    /// Loads the pointer file as a retained document (for re-saving with
    /// unknown keys preserved).
    pub fn load_pointer_document(&self) -> Option<PointerDocument> {
        let path = self.root.pointer_file();
        let bytes = std::fs::read(&path).ok()?;
        PointerDocument::parse(&bytes, &path).ok()
    }

    /// Lists profile directory slugs on disk (for pointer-less fallback).
    pub fn list_profile_slugs(&self) -> Vec<String> {
        list_slugs(&self.root.profiles_dir())
    }

    /// Lists collection directory slugs on disk (for pointer-less fallback).
    pub fn list_collection_slugs(&self) -> Vec<String> {
        list_slugs(&self.root.collections_dir())
    }
}

fn list_slugs(dir: &std::path::Path) -> Vec<String> {
    let mut slugs: Vec<String> = std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .filter(|e| e.path().is_dir())
                .filter_map(|e| e.file_name().into_string().ok())
                .collect()
        })
        .unwrap_or_default();
    slugs.sort();
    slugs
}

/// Refreshes `.bak` from a verified-good primary when the bytes differ.
fn refresh_backup(primary: &std::path::Path, mode: u32) -> Result<()> {
    let backup = backup_path(primary);
    let primary_bytes = std::fs::read(primary).map_err(PersistenceError::io(primary))?;
    let differs = match std::fs::read(&backup) {
        Ok(existing) => existing != primary_bytes,
        Err(_) => true,
    };
    if differs {
        atomic::atomic_write(&backup, &primary_bytes, mode)
            .map_err(PersistenceError::io(&backup))?;
    }
    Ok(())
}

fn load_collection_file(path: &PathBuf) -> Result<LoadOutcome<LoadedCollection>> {
    let bytes = std::fs::read(path).map_err(PersistenceError::io(path))?;
    let (envelope, migrated) = CollectionFileV1::from_json_bytes(&bytes, path)?;
    let mut snapshot = envelope.to_snapshot();
    let warnings =
        validate::validate_collection(&snapshot).map_err(|reason| PersistenceError::Integrity {
            path: path.clone(),
            reason,
        })?;
    validate::repair_session(&mut snapshot);
    Ok(LoadOutcome {
        value: LoadedCollection { snapshot, envelope },
        recovery: Recovery::Clean,
        migrated,
        warnings,
    })
}

fn load_profile_file(path: &PathBuf) -> Result<LoadOutcome<LoadedProfile>> {
    let bytes = std::fs::read(path).map_err(PersistenceError::io(path))?;
    let document = ProfileDocument::parse(&bytes, path)?;
    let snapshot = document.snapshot();
    validate::validate_profile(&snapshot).map_err(|reason| PersistenceError::Integrity {
        path: path.clone(),
        reason,
    })?;
    let migrated = document.migrated();
    Ok(LoadOutcome::clean(
        LoadedProfile {
            snapshot,
            document,
            migrated,
        },
        migrated,
        Vec::new(),
    ))
}

/// Fresh defaults for total loss: mirrors `AppState::new()`'s seeds.
fn default_collection() -> LoadedCollection {
    let snapshot = CollectionSnapshot {
        collection: SceneCollection::new("Default"),
        session: SessionState::default(),
    };
    let envelope = CollectionFileV1::from_snapshot(&snapshot, None);
    LoadedCollection { snapshot, envelope }
}

fn default_profile() -> LoadedProfile {
    let snapshot = ProfileSnapshot {
        profile: Profile::new("Default", VideoConfig::default()),
        encoders: Vec::new(),
        services: Vec::new(),
        outputs: Vec::new(),
    };
    let bytes = ProfileDocument::to_bytes(&snapshot, None)
        .unwrap_or_else(|_| b"schemaVersion = 1\n".to_vec());
    // Impossible invariant: we just serialized these bytes ourselves.
    let document = ProfileDocument::parse(&bytes, std::path::Path::new("<defaults>"))
        .unwrap_or_else(|_| unreachable!("freshly serialized default profile must parse"));
    LoadedProfile {
        snapshot,
        document,
        migrated: false,
    }
}
