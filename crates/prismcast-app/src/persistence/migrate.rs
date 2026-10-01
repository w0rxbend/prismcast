//! Schema versioning and migration scaffolding
//! (`docs/architecture/persistence-model.md` §4).
//!
//! Every persisted file carries a top-level `schemaVersion: N`, versioned
//! **per file family** (profiles and collections evolve independently). The
//! version is read from the *raw document* before any typed deserialization;
//! a missing version is version 0 = invalid (pre-versioning files never
//! shipped), a newer version is a typed [`PersistenceError::NewerSchema`].
//!
//! Migrations are pure functions on the raw document — no I/O, no clock, no
//! randomness — chained stepwise (`V1 → V2 → V3 → …`). There are no
//! migrations yet: V1 is the first and current schema of every family.
//!
//! **Bump procedure** (CI-enforceable checklist, ADR-0008 consequences):
//! 1. Bump the family's `CURRENT_*_SCHEMA` constant by one.
//! 2. Append one migration function to the family's chain (index `i` migrates
//!    version `i + 1` to `i + 2`).
//! 3. Add migration unit tests (Vn fixture → Vn+1 expectation).
//! 4. Update the golden files (`tests/golden/`).
//!
//! Migrations never drop data: renamed fields are moved; retired fields go
//! under an `x-legacy/<field>` key in the envelope's unknown map.

use serde_json::Value;
use toml_edit::DocumentMut;

use super::error::{PersistenceError, Result};

/// Current schema version of `collection.json` files.
pub const CURRENT_COLLECTION_SCHEMA: u32 = 1;
/// Current schema version of `profile.toml` files.
pub const CURRENT_PROFILE_SCHEMA: u32 = 1;
/// Current schema version of the `prismcast.toml` pointer file.
pub const CURRENT_POINTER_SCHEMA: u32 = 1;

/// The version key every persisted file carries at top level.
pub const VERSION_KEY: &str = "schemaVersion";

/// One JSON migration step, `Vn → Vn+1`. Pure: no I/O, no clock, no
/// randomness. The `String` error is a human-readable reason.
pub type JsonMigration = fn(Value) -> std::result::Result<Value, String>;

/// One TOML migration step, `Vn → Vn+1`, operating on the retained document.
pub type TomlMigration = fn(&mut DocumentMut) -> std::result::Result<(), String>;

/// Collection migration chain: `COLLECTION_MIGRATIONS[i]` migrates schema
/// `i + 1 → i + 2`. Empty while V1 is current.
pub const COLLECTION_MIGRATIONS: &[JsonMigration] = &[];

/// Profile migration chain: `PROFILE_MIGRATIONS[i]` migrates schema
/// `i + 1 → i + 2`. Empty while V1 is current.
pub const PROFILE_MIGRATIONS: &[TomlMigration] = &[];

/// Reads `schemaVersion` from a raw JSON document.
pub fn json_version(doc: &Value, path: &std::path::Path) -> Result<u32> {
    match doc.get(VERSION_KEY).and_then(Value::as_u64) {
        Some(v) => u32::try_from(v)
            .map_err(|_| PersistenceError::MissingSchemaVersion { path: path.into() }),
        None => Err(PersistenceError::MissingSchemaVersion { path: path.into() }),
    }
}

/// Reads `schemaVersion` from a raw TOML document.
pub fn toml_version(doc: &DocumentMut, path: &std::path::Path) -> Result<u32> {
    match doc.get(VERSION_KEY).and_then(|item| item.as_integer()) {
        Some(v) if v >= 0 => u32::try_from(v)
            .map_err(|_| PersistenceError::MissingSchemaVersion { path: path.into() }),
        _ => Err(PersistenceError::MissingSchemaVersion { path: path.into() }),
    }
}

/// Guards against a newer-than-supported version.
pub fn check_not_newer(version: u32, supported: u32, path: &std::path::Path) -> Result<()> {
    if version > supported {
        return Err(PersistenceError::NewerSchema {
            path: path.into(),
            found: version,
            supported,
        });
    }
    Ok(())
}

/// Applies the JSON migration chain from `from` up to `current`. Returns the
/// migrated document and whether any step ran.
pub fn migrate_json(
    mut doc: Value,
    from: u32,
    chain: &[JsonMigration],
    current: u32,
    path: &std::path::Path,
) -> Result<(Value, bool)> {
    check_not_newer(from, current, path)?;
    let mut version = from;
    while version < current {
        let step =
            chain
                .get((version - 1) as usize)
                .ok_or_else(|| PersistenceError::Migration {
                    path: path.into(),
                    from: version,
                    reason: format!("no migration defined for v{version} (chain is incomplete)"),
                })?;
        doc = step(doc).map_err(|reason| PersistenceError::Migration {
            path: path.into(),
            from: version,
            reason,
        })?;
        version += 1;
    }
    Ok((doc, version != from))
}

/// Applies the TOML migration chain from `from` up to `current`, mutating the
/// retained document in place. Returns whether any step ran.
pub fn migrate_toml(
    doc: &mut DocumentMut,
    from: u32,
    chain: &[TomlMigration],
    current: u32,
    path: &std::path::Path,
) -> Result<bool> {
    check_not_newer(from, current, path)?;
    let mut version = from;
    while version < current {
        let step =
            chain
                .get((version - 1) as usize)
                .ok_or_else(|| PersistenceError::Migration {
                    path: path.into(),
                    from: version,
                    reason: format!("no migration defined for v{version} (chain is incomplete)"),
                })?;
        step(doc).map_err(|reason| PersistenceError::Migration {
            path: path.into(),
            from: version,
            reason,
        })?;
        version += 1;
    }
    Ok(version != from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bump_doc(mut doc: Value) -> std::result::Result<Value, String> {
        // Test-only migration: renames `oldKey` to `newKey`.
        let obj = doc.as_object_mut().ok_or("not an object")?;
        if let Some(v) = obj.remove("oldKey") {
            obj.insert("newKey".to_string(), v);
        }
        obj.insert(VERSION_KEY.to_string(), Value::from(2));
        Ok(doc)
    }

    fn bump_again(mut doc: Value) -> std::result::Result<Value, String> {
        doc.as_object_mut()
            .ok_or("not an object")?
            .insert(VERSION_KEY.to_string(), Value::from(3));
        Ok(doc)
    }

    #[test]
    fn chain_applies_steps_in_order() {
        let chain: &[JsonMigration] = &[bump_doc, bump_again];
        let doc = serde_json::json!({VERSION_KEY: 1, "oldKey": 42});
        let (doc, migrated) =
            migrate_json(doc, 1, chain, 3, std::path::Path::new("f.json")).unwrap();
        assert!(migrated);
        assert_eq!(doc, serde_json::json!({VERSION_KEY: 3, "newKey": 42}));
    }

    #[test]
    fn chain_at_current_version_is_noop() {
        let doc = serde_json::json!({VERSION_KEY: 1});
        let (_, migrated) =
            migrate_json(doc.clone(), 1, &[], 1, std::path::Path::new("f")).unwrap();
        assert!(!migrated);
    }

    #[test]
    fn newer_version_rejected() {
        let doc = serde_json::json!({VERSION_KEY: 9});
        let err = migrate_json(doc, 9, &[], 1, std::path::Path::new("f")).unwrap_err();
        assert!(matches!(
            err,
            PersistenceError::NewerSchema {
                found: 9,
                supported: 1,
                ..
            }
        ));
    }

    #[test]
    fn gap_in_chain_is_typed_error() {
        let doc = serde_json::json!({VERSION_KEY: 1});
        let err = migrate_json(doc, 1, &[], 2, std::path::Path::new("f")).unwrap_err();
        assert!(matches!(err, PersistenceError::Migration { from: 1, .. }));
    }

    #[test]
    fn missing_version_is_version_zero_invalid() {
        let err =
            json_version(&serde_json::json!({"name": "x"}), std::path::Path::new("f")).unwrap_err();
        assert!(matches!(err, PersistenceError::MissingSchemaVersion { .. }));
        let doc: DocumentMut = "name = \"x\"".parse().unwrap();
        let err = toml_version(&doc, std::path::Path::new("f")).unwrap_err();
        assert!(matches!(err, PersistenceError::MissingSchemaVersion { .. }));
    }
}
