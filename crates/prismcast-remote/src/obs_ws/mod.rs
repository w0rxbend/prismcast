//! obs-websocket 5.x compatibility adapter (OBSWS-001; ADR-0010, ADR-0020;
//! RES-007).
//!
//! A second WebSocket server speaking the obs-websocket 5.x wire protocol so
//! the existing OBS ecosystem (Stream Deck tools, mobile clients, automation
//! scripts) can drive Prismcast unchanged. Per ADR-0010 this is a **thin
//! adapter**: the internal domain protocol stays native, and everything here
//! translates onto the same Core Command/Event contract the native WS and
//! IPC servers use.
//!
//! ## Public surface
//!
//! - [`ObsWsServer`] / [`ObsWsServerConfig`] / [`ObsWsError`] — the server,
//!   modeled on [`crate::ws::WsServer`]: disabled by default, default bind
//!   `127.0.0.1:4455`, [`AuthConfig::AllowLocal`](crate::AuthConfig) refused
//!   at bind (password or token required on a network transport).
//! - [`proto`] — the obs-shaped wire types (`{op, d}` envelopes, opcodes
//!   0/1/2/3/5/6/7/8/9, `RequestStatus` codes, obs close codes, the
//!   `EventSubscription` bitmask constants, `RequestBatchExecutionType`).
//!   Serde shapes are pinned to obs-websocket 5.7.4 by golden fixtures.
//! - [`bitmask`] — the `eventSubscriptions` bitmask ↔ native
//!   [`SubscriptionSet`](prismcast_protocol::subscription::SubscriptionSet)
//!   mapping, and the `eventIntent` bit per native event category.
//!
//! ## Slice status
//!
//! The handshake (Hello/Identify/Identified with obs close codes), auth via
//! the shared [`AuthConfig`](crate::AuthConfig) (obs's SHA-256
//! challenge-response *is* [`crate::auth::challenge_response`]), `Reidentify`
//! subscription updates, event gating by bitmask, the RequestBatch
//! scaffolding (serial execution, `haltOnFailure`, bounded `Sleep`,
//! whole-batch 206 for `SerialFrame`/`Parallel`), and **request translation**
//! (`requests`: the advertised MVP request set pivots through native
//! `RequestKind` → [`map::command_from_wire`](crate::map::command_from_wire)
//! → Core Commands; queries read snapshots; `names`: stateless name→ID
//! resolution plus the stateful, eviction-tracked `ItemIdMap` for numeric
//! `sceneItemId`s) are real. Unknown request types get a typed 204
//! (`UnknownRequestType`). Domain events are not yet translated to obs
//! events ([`translate::event_to_obs`] is a stub — the events slice).
//!
//! ## Documented divergences from upstream obs-websocket
//!
//! - Unknown/offered-but-unsupported subprotocols refuse the HTTP upgrade
//!   (400) instead of silently defaulting to JSON; `obswebsocket.msgpack` is
//!   deferred (OBSWS-002+).
//! - Malformed `d` payloads close with 4002 (`MessageDecodeError`) where
//!   upstream sometimes uses the more specific 4003/4004/4005; invalid
//!   `executionType` values do close with 4005 like upstream.
//! - Token auth is a Prismcast extension: on token-configured servers the
//!   `Identify.authentication` string carries the bearer token itself.
//! - Backpressure (absent upstream, RES-007 weakness 5): bounded outbound
//!   queue; persistent overflow sheds the session with 4000
//!   (`UnknownReason`), since obs defines no slow-consumer code.
//! - Server shutdown closes with RFC 6455 1001 (`going_away`), like
//!   upstream's "Server stopping.".
//! - `obsStudioVersion` is omitted from `Hello` (ADR-0020 §d).

pub mod bitmask;
mod names;
pub mod proto;
mod requests;
mod server;
mod session;
pub(crate) mod translate;

pub use server::{ObsWsError, ObsWsServer, ObsWsServerConfig};
