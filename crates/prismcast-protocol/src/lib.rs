//! # prismcast-protocol
//!
//! Versioned wire protocol types for the Prismcast remote interfaces: the
//! native WebSocket API (PLAN.md §22) and the Unix-socket IPC channel
//! (PLAN.md §21, ADR-0006). One serde data model serves both transports:
//! JSON text frames on WebSocket, length-prefixed MessagePack frames on the
//! Unix socket (codec selection is a transport concern and lives in
//! `prismcast-remote`, not here).
//!
//! The interaction shape intentionally resembles obs-websocket 5.x
//! (ADR-0010 §2, RES-007): a server-first `Hello`/`Identify`/`Identified`
//! handshake with protocol-version negotiation, request/response correlation
//! by client-supplied IDs, serial request batches, and category-based event
//! subscriptions. Deliberate deviations from obs-websocket are documented in
//! `docs/protocols/native-protocol.md`; the headline ones:
//!
//! - Typed ID-addressing ([`uuid::Uuid`]) instead of mutable names.
//! - String type tags instead of numeric op codes.
//! - Structured error payloads ([`error::WireError`]) instead of
//!   integer-plus-comment.
//! - Typed subscription sets with per-entity filters and explicit throttle
//!   intervals ([`subscription`]) instead of a category bitmask.
//! - Subscription changes are a request (`update_subscriptions`), not a
//!   special `Reidentify` op.
//! - Per-session event sequence numbers for drop detection.
//!
//! Per PLAN.md §75 these protocol structs are *not* domain structs and must
//! never be reused as such; mapping to `prismcast-core` types happens at the
//! interface boundary (`prismcast-remote`, `prismcast-cli`). The full
//! request surface mirrors `prismcast_core::Command` one-to-one — enforced
//! by `tests/command_coverage.rs`.
//!
//! **Layer: Interfaces.**

pub mod batch;
pub mod data;
pub mod error;
pub mod event;
pub mod handshake;
pub mod message;
pub mod request;
pub mod response;
pub mod subscription;
pub mod version;

pub use batch::{BatchRequest, BatchResult, RequestBatch, RequestBatchResponse};
pub use error::{codes, ErrorKind, WireError};
pub use event::{
    AudioEvent, EventMessage, MeterEvent, OutputEvent, SceneEvent, SourceEvent, SystemEvent,
    WireEvent,
};
pub use handshake::{
    AuthChallenge, AuthResponse, ClientInfo, CloseCode, Hello, Identified, Identify, Permission,
};
pub use message::{ClientMessage, ServerMessage};
pub use request::{Request, RequestKind};
pub use response::{RequestResponse, ResponseData, ResponseStatus};
pub use subscription::{
    EventCategory, Subscription, SubscriptionError, SubscriptionSet, DEFAULT_METER_INTERVAL_MS,
    MAX_ENTITY_FILTER, MAX_THROTTLE_MS, MIN_THROTTLE_MS,
};
pub use version::{negotiate, MIN_PROTOCOL_VERSION, PROTOCOL_VERSION};
