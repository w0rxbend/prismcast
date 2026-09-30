//! Strongly-typed ID newtypes for all domain entities.
//!
//! Per PLAN.md §75, raw `String` (or bare `Uuid`) IDs are forbidden; every
//! entity class gets its own newtype so IDs cannot be mixed up at compile
//! time. Each wraps a [`uuid::Uuid`] v4, displays and parses as the canonical
//! hyphenated UUID string, and serializes transparently as that string.

/// Generates an ID newtype wrapping [`uuid::Uuid`] with `new()`, `Display`,
/// `FromStr`, `Default`, and transparent serde support.
macro_rules! define_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(
            Debug,
            Clone,
            Copy,
            PartialEq,
            Eq,
            Hash,
            PartialOrd,
            Ord,
            serde::Serialize,
            serde::Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(uuid::Uuid);

        impl $name {
            /// Generates a fresh random (v4) ID.
            pub fn new() -> Self {
                Self(uuid::Uuid::new_v4())
            }

            /// Returns the underlying UUID.
            pub fn as_uuid(&self) -> &uuid::Uuid {
                &self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}", self.0)
            }
        }

        impl std::str::FromStr for $name {
            type Err = uuid::Error;

            fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
                Ok(Self(uuid::Uuid::parse_str(s)?))
            }
        }

        impl From<uuid::Uuid> for $name {
            fn from(uuid: uuid::Uuid) -> Self {
                Self(uuid)
            }
        }
    };
}

define_id!(
    /// Identifies a scene within the current scene collection.
    SceneId
);
define_id!(
    /// Identifies a source (capture device, media file, browser, ...).
    SourceId
);
define_id!(
    /// Identifies an item (a placed source) within a scene.
    SceneItemId
);
define_id!(
    /// Identifies a filter attached to a source or scene item.
    FilterId
);
define_id!(
    /// Identifies an output (recording, stream, virtual camera, ...).
    OutputId
);
define_id!(
    /// Identifies an encoder instance in the output graph.
    EncoderId
);
define_id!(
    /// Identifies a streaming service configuration.
    ServiceId
);
define_id!(
    /// Identifies an audio bus in the mixer.
    AudioBusId
);
define_id!(
    /// Identifies a settings profile.
    ProfileId
);
define_id!(
    /// Identifies a scene collection.
    SceneCollectionId
);
define_id!(
    /// Identifies a canvas (render target with its own resolution).
    ///
    /// Reserved up front (RES-002 open question) so per-canvas resolution can be
    /// added later without a persisted-schema break.
    CanvasId
);

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    macro_rules! id_tests {
        ($mod:ident, $name:ident) => {
            mod $mod {
                use super::*;

                #[test]
                fn display_fromstr_roundtrip() {
                    let id = $name::new();
                    let parsed = $name::from_str(&id.to_string())
                        .unwrap_or_else(|e| panic!("parse failed: {e}"));
                    assert_eq!(id, parsed);
                }

                #[test]
                fn serde_roundtrip() {
                    let id = $name::new();
                    let json = serde_json::to_string(&id).unwrap();
                    assert_eq!(json, format!("\"{}\"", id.as_uuid()));
                    let back: $name = serde_json::from_str(&json).unwrap();
                    assert_eq!(id, back);
                }

                #[test]
                fn rejects_garbage() {
                    assert!($name::from_str("not-a-uuid").is_err());
                }

                #[test]
                fn distinct_newtypes_do_not_mix() {
                    // Compile-time property; at runtime just confirm new() is unique.
                    assert_ne!($name::new(), $name::new());
                }
            }
        };
    }

    id_tests!(scene_id_tests, SceneId);
    id_tests!(source_id_tests, SourceId);
    id_tests!(scene_item_id_tests, SceneItemId);
    id_tests!(filter_id_tests, FilterId);
    id_tests!(output_id_tests, OutputId);
    id_tests!(encoder_id_tests, EncoderId);
    id_tests!(service_id_tests, ServiceId);
    id_tests!(audio_bus_id_tests, AudioBusId);
    id_tests!(profile_id_tests, ProfileId);
    id_tests!(scene_collection_id_tests, SceneCollectionId);
    id_tests!(canvas_id_tests, CanvasId);
}
