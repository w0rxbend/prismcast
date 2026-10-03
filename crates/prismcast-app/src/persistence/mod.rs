//! Project persistence (CORE-004; `docs/architecture/persistence-model.md`,
//! ADR-0008, PLAN §19).
//!
//! ## Layout
//!
//! ```text
//! $XDG_CONFIG_HOME/prismcast/          (fallback ~/.config/prismcast)
//!     prismcast.toml                   pointer file: active profile/collection
//!     profiles/<slug>/profile.toml     hand-editable TOML (+ .bak sibling)
//!     collections/<slug>/collection.json   machine-managed JSON (+ .bak)
//! ```
//!
//! The root is resolved once and injected ([`ConfigRoot`]); tests point it at
//! a tempdir. Directory names are slugified entity names; the authoritative
//! identity is the UUID inside the file.
//!
//! ## Guarantees
//!
//! - **Envelope structs, not domain structs**: [`CollectionFileV1`],
//!   [`ProfileDocument`], [`PointerDocument`] live here and map to/from domain
//!   aggregates. Unknown fields survive a load → save cycle: JSON via
//!   `#[serde(flatten)]` capture maps (including per-scene/source/item
//!   wrappers), TOML via a retained `toml_edit` document that the saver
//!   patches instead of reserializing (comments and unknown keys survive).
//! - **Atomic writes** ([`atomic`]): serialize to memory → temp file in the
//!   same directory (`O_EXCL`, unique name) → write → fsync → rename → dir
//!   fsync. Leftover temp files are reaped on load. All blocking syscalls run
//!   in `tokio::task::spawn_blocking`.
//! - **Schema versioning** ([`migrate`]): `schemaVersion` per file family,
//!   read from the raw document before typed deserialization; stepwise
//!   migration chains (currently V1 only); newer-than-supported is a typed
//!   [`PersistenceError::NewerSchema`] and the file is left untouched.
//! - **Corruption recovery** ([`store`]): primary → `.bak` (refreshed only
//!   from verified-good loads) → fresh defaults; recovery is surfaced via
//!   [`Recovery`] on the load outcome, never silent.
//! - **Actor ownership** ([`PersistenceHandle`]): the persistence actor is
//!   the only component that opens files under the root. The core actor
//!   notifies it of applied commands ([`dirty_class`] classifies all 52
//!   variants); writes are debounced; [`PersistenceHandle::save_now`] /
//!   [`PersistenceHandle::shutdown`] flush synchronously.
//!
//! ## Schema bump procedure
//!
//! 1. Bump the family's `CURRENT_*_SCHEMA` constant in [`migrate`].
//! 2. Append one migration function to the family's chain (index `i`
//!    migrates `i + 1 → i + 2`; never drop data — move retired fields under
//!    an `x-legacy/<field>` key in the envelope's unknown map).
//! 3. Add migration unit tests (Vn fixture → Vn+1 expectation).
//! 4. Update golden files under `tests/golden/`.
//!
//! ## Follow-ups (need prismcast-core changes; not done in CORE-004)
//!
//! - `SystemEvent::PersistenceRecovered { path, reason }` core event so
//!   recovery reaches GTK/CLI/WebSocket controllers; today recovery info is
//!   returned from load functions as [`Recovery`].
//! - Outputs/encoders/services nested under the profile in `AppState`
//!   (persistence-model §3): today outputs live in a flat map and are
//!   attributed to the active profile on save; encoder/service registries
//!   have no domain home yet, so profile files written from app state carry
//!   empty `[[encoders]]`/`[[services]]`.
//! - File deletion for removed profiles/collections (their directories are
//!   currently kept as orphans).
//! - Secret Service (D-Bus) storage for stream keys instead of `0600` files.

pub mod actor;
pub mod atomic;
pub mod envelope;
pub mod error;
pub mod migrate;
pub mod paths;
pub mod pointer;
pub mod profile;
pub mod store;
pub mod validate;

pub use actor::{
    dirty_class, DirtyClass, DirtyMarks, PersistenceConfig, PersistenceEvent, PersistenceHandle,
    PointerSelection, DEFAULT_CHANNEL_CAPACITY, DEFAULT_DEBOUNCE, DEFAULT_MAX_DELAY,
};
pub use envelope::{CollectionFileV1, CollectionSnapshot, SessionState, WithExtras};
pub use error::PersistenceError;
pub use migrate::{CURRENT_COLLECTION_SCHEMA, CURRENT_POINTER_SCHEMA, CURRENT_PROFILE_SCHEMA};
pub use paths::{slugify, unique_slug, ConfigRoot};
pub use pointer::{PointerDocument, PointerState};
pub use profile::{OutputFileV1, ProfileDocument, ProfileSnapshot};
pub use store::{LoadOutcome, LoadedCollection, LoadedProfile, ProjectStore, Recovery};
