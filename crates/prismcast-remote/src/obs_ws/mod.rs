//! obs-websocket 5.x compatibility adapter (OBSWS-001, OBSWS-002; ADR-0010,
//! ADR-0020, ADR-0021; RES-007).
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
//! - [`codec`] — the wire codec negotiated by subprotocol: JSON text frames
//!   (`obswebsocket.json`, the default) or MessagePack binary frames
//!   (`obswebsocket.msgpack`, struct-as-map; OBSWS-002, ADR-0021). Lives at
//!   the session framing boundary; everything above it is codec-agnostic.
//! - [`bitmask`] — the `eventSubscriptions` bitmask ↔ native
//!   [`SubscriptionSet`](prismcast_protocol::subscription::SubscriptionSet)
//!   mapping, and the `eventIntent` bit per native event category.
//!
//! ## Slice status
//!
//! Real: the handshake (Hello/Identify/Identified with obs close codes), auth
//! via the shared [`AuthConfig`](crate::AuthConfig) (obs's SHA-256
//! challenge-response *is* [`crate::auth::challenge_response`]), `Reidentify`
//! subscription updates, event gating by bitmask, **domain event → obs event
//! translation** ([`translate`]: scenes/program/preview, scene items, input
//! CRUD + mute/volume, output state incl. the stream/record primaries, studio
//! mode), RequestBatch execution (serial realtime/frame with
//! `haltOnFailure`, bounded `Sleep` incl. frame-timed `sleepFrames` under
//! `SerialFrame`, and bounded-concurrency `Parallel` with request-ordered
//! results), and **request translation**
//! (`requests`: the advertised MVP request set pivots through native
//! `RequestKind` → [`map::command_from_wire`](crate::map::command_from_wire)
//! → Core Commands; queries read snapshots; `names`: stateless name→ID
//! resolution plus the stateful, eviction-tracked `ItemIdMap` for numeric
//! `sceneItemId`s). Unknown request types get a typed 204
//! (`UnknownRequestType`). The wire codec is negotiated per connection:
//! JSON (the obs default) or MessagePack binary frames
//! (`obswebsocket.msgpack`), both encoding the same `{op, d}` envelopes —
//! the codec sits at the session framing boundary and the engine above it is
//! codec-agnostic (OBSWS-002, ADR-0021).
//!
//! ## Documented divergences from upstream obs-websocket
//!
//! - Unknown/offered-but-unsupported subprotocols refuse the HTTP upgrade
//!   (400) instead of silently defaulting to JSON; when both known tags are
//!   offered, JSON wins.
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
//! - `OutputStateChanged` (per-output, `outputName`/`outputUuid` addressed)
//!   is a Prismcast extension event: upstream only has the singleton
//!   `StreamStateChanged`/`RecordStateChanged`, which are emitted here only
//!   when the changing output is the designated primary (ADR-0020 §e).
//! - `eventIntent` is obs-exact, but delivery gating uses native event
//!   categories (bitmask → `SubscriptionSet`, see [`bitmask`]), which are
//!   coarser: e.g. `CurrentPreviewSceneChanged` is native `System`, so it is
//!   admitted by the `Config`/`Transitions`/`Ui` bits, not by `Scenes`.

pub mod bitmask;
pub(crate) mod codec;
mod names;
pub mod proto;
mod requests;
mod server;
mod session;
pub(crate) mod translate;

pub use server::{ObsWsError, ObsWsServer, ObsWsServerConfig};
