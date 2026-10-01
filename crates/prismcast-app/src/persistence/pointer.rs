//! The `prismcast.toml` pointer file: which profile and collection are active
//! (`docs/architecture/persistence-model.md` §3).
//!
//! The pointer file is minimal: `schemaVersion`, active profile slug, active
//! collection slug. It gets atomic writes but **no `.bak`** — if it is lost or
//! corrupt, recovery is trivial (pick the first profile/collection on disk),
//! handled by the store. Unknown keys are preserved via the same
//! retained-document approach as profiles.

use serde::{Deserialize, Serialize};
use toml_edit::DocumentMut;

use super::error::{PersistenceError, Result};
use super::migrate::{self, CURRENT_POINTER_SCHEMA};

/// Known top-level keys the saver owns.
const KNOWN_KEYS: &[&str] = &["schemaVersion", "active_profile", "active_collection"];

/// Which profile/collection the pointer file selects, by directory slug.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PointerState {
    /// Slug of the active profile (`profiles/<slug>/`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_profile: Option<String>,
    /// Slug of the active collection (`collections/<slug>/`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_collection: Option<String>,
}

#[derive(Serialize)]
struct PointerFileV1Ref<'a> {
    #[serde(rename = "schemaVersion")]
    schema_version: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    active_profile: &'a Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    active_collection: &'a Option<String>,
}

/// A loaded pointer file: retained document plus the typed selection.
#[derive(Debug, Clone)]
pub struct PointerDocument {
    doc: DocumentMut,
    /// The parsed selection.
    pub state: PointerState,
}

impl PointerDocument {
    /// Parses a pointer file.
    pub fn parse(bytes: &[u8], path: &std::path::Path) -> Result<Self> {
        let text = std::str::from_utf8(bytes).map_err(|err| PersistenceError::Parse {
            path: path.into(),
            reason: format!("not valid UTF-8: {err}"),
        })?;
        let doc: DocumentMut =
            text.parse()
                .map_err(|err: toml_edit::TomlError| PersistenceError::Parse {
                    path: path.into(),
                    reason: err.to_string(),
                })?;
        let version = migrate::toml_version(&doc, path)?;
        // No TOML migration chain for the pointer file yet; reuse the profile
        // chain machinery is not applicable, so guard the version directly.
        migrate::check_not_newer(version, CURRENT_POINTER_SCHEMA, path)?;
        let typed: PointerState =
            toml_edit::de::from_document(doc.clone()).map_err(|err| PersistenceError::Parse {
                path: path.into(),
                reason: err.to_string(),
            })?;
        Ok(Self { doc, state: typed })
    }

    /// Serializes the selection, patching the retained document when given.
    pub fn to_bytes(state: &PointerState, retained: Option<&Self>) -> Result<Vec<u8>> {
        let rendered = toml::to_string(&PointerFileV1Ref {
            schema_version: CURRENT_POINTER_SCHEMA,
            active_profile: &state.active_profile,
            active_collection: &state.active_collection,
        })
        .map_err(|err| PersistenceError::Serialize {
            what: "prismcast.toml",
            reason: err.to_string(),
        })?;
        let fresh: DocumentMut =
            rendered
                .parse()
                .map_err(|err: toml_edit::TomlError| PersistenceError::Serialize {
                    what: "prismcast.toml",
                    reason: err.to_string(),
                })?;
        let mut doc = match retained {
            Some(retained) => {
                let mut doc = retained.doc.clone();
                for key in KNOWN_KEYS {
                    doc.remove(key);
                }
                doc
            }
            None => DocumentMut::new(),
        };
        for (key, item) in fresh.iter() {
            doc[key] = item.clone();
        }
        let mut text = doc.to_string();
        if !text.ends_with('\n') {
            text.push('\n');
        }
        Ok(text.into_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pointer_roundtrip() {
        let state = PointerState {
            active_profile: Some("twitch-1080p".into()),
            active_collection: Some("dev-stream".into()),
        };
        let bytes = PointerDocument::to_bytes(&state, None).unwrap();
        let doc = PointerDocument::parse(&bytes, std::path::Path::new("prismcast.toml")).unwrap();
        assert_eq!(doc.state, state);
    }

    #[test]
    fn empty_selection_roundtrip() {
        let state = PointerState::default();
        let bytes = PointerDocument::to_bytes(&state, None).unwrap();
        let doc = PointerDocument::parse(&bytes, std::path::Path::new("prismcast.toml")).unwrap();
        assert_eq!(doc.state, state);
    }

    #[test]
    fn unknown_keys_survive() {
        let state = PointerState {
            active_profile: Some("p".into()),
            active_collection: None,
        };
        let bytes = PointerDocument::to_bytes(&state, None).unwrap();
        let mut text = String::from_utf8(bytes).unwrap();
        text.push_str("ui_theme = \"dark\"\n");
        let doc = PointerDocument::parse(text.as_bytes(), std::path::Path::new("p.toml")).unwrap();
        let resaved = PointerDocument::to_bytes(&doc.state.clone(), Some(&doc)).unwrap();
        let text = String::from_utf8(resaved).unwrap();
        assert!(text.contains("ui_theme = \"dark\""));
        assert!(text.contains("active_profile = \"p\""));
    }
}
