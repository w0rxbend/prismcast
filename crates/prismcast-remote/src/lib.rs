//! # prismcast-remote
//!
//! Remote-first control: Unix-socket IPC and WebSocket servers with
//! authentication, exposing the application core's command/event API
//! (PLAN.md §21–§22).
//!
//! ## Transports
//!
//! Both transports serve the same native protocol (`prismcast-protocol`)
//! through one transport-generic session engine ([`session`]): the handshake
//! state machine, request dispatch ([`map`]), subscriptions with throttle
//! coalescing, per-session sequence numbers, rate limiting, and bounded
//! backpressure live in `session` exactly once. A transport only implements
//! frame reading/writing and close semantics:
//!
//! - **Unix-socket IPC** (IPC-001, ADR-0006): [`IpcServer`] at
//!   `$XDG_RUNTIME_DIR/prismcast/control.sock`; 4-byte big-endian length
//!   prefix + MessagePack payloads ([`codec`]); close codes are delivered as
//!   a synthetic `closing` frame (protocol doc §8). Auth defaults to the
//!   allow-local policy — filesystem permissions (`0700`/`0600`) are the
//!   primary control — with an optional bearer token from
//!   `$XDG_CONFIG_HOME/prismcast/remote.toml` or an injected [`AuthConfig`].
//! - **WebSocket** (WS-001, PLAN.md §22): [`WsServer`] over a plain
//!   `TcpListener` (no axum, no TLS yet — both are follow-ups); one JSON text
//!   frame per protocol message; close codes map to WebSocket close frames in
//!   the 4000+ range. **Disabled by default** and token auth is mandatory on
//!   this network transport ([`WsServerConfig`]).
//!
//! Requests map wire `RequestKind`s to `prismcast_core::Command`s or
//! read-only queries and are dispatched with the session's permissions
//! through [`AppHandle::dispatch_with_permissions`](prismcast_app::AppHandle).
//!
//! [`IpcClient`] (shared with `prismcast-cli`) and [`WsClient`] (test support
//! and future web tooling) are the matching minimal clients.
//!
//! **Layer: Interfaces.**

pub mod auth;
pub mod client;
pub mod codec;
pub mod map;
mod paths;
mod server;
mod session;
pub mod ws;
pub mod ws_client;

pub use auth::{AuthConfig, AuthError};
pub use client::{ClientError, IpcClient, IpcClientConfig};
pub use codec::{ClosingNotice, DEFAULT_MAX_FRAME_SIZE};
pub use paths::{default_socket_dir, default_socket_path, SOCKET_FILE_NAME};
pub use server::{IpcError, IpcServer, IpcServerConfig};
pub use ws::{WsError, WsServer, WsServerConfig};
pub use ws_client::{WsClient, WsClientConfig, WsClientError};
