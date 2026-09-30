//! # prismcast-remote
//!
//! Remote-first control: Unix-socket IPC and WebSocket servers with
//! authentication, exposing the application core's command/event API
//! (PLAN.md §21–§22).
//!
//! ## Unix-socket IPC (IPC-001, ADR-0006)
//!
//! [`IpcServer`] serves the native protocol (`prismcast-protocol`) over a
//! Unix domain socket at `$XDG_RUNTIME_DIR/prismcast/control.sock`:
//!
//! - **Framing**: 4-byte big-endian length prefix + MessagePack payload
//!   ([`codec`]), bounded by a max frame size.
//! - **Handshake**: server-first `Hello` → `Identify` (protocol version
//!   negotiation, auth) → `Identified` ([`session`]).
//! - **Requests**: wire `RequestKind`s map to `prismcast_core::Command`s or
//!   read-only queries ([`map`]); commands are dispatched with the session's
//!   permissions through
//!   [`AppHandle::dispatch_with_permissions`](prismcast_app::AppHandle).
//! - **Events**: subscriptions with per-category entity filters and
//!   latest-wins throttle coalescing; per-session sequence numbers signal
//!   drops as gaps (protocol doc §7).
//! - **Auth**: filesystem permissions are the primary control; the default
//!   local policy grants full `Admin` access. A bearer token from
//!   `$XDG_CONFIG_HOME/prismcast/remote.toml` (or an injected
//!   [`AuthConfig`]) can restrict sessions ([`auth`]).
//!
//! [`IpcClient`] is the shared minimal client used by `prismcast-cli` and
//! the integration tests.
//!
//! **Layer: Interfaces.**

pub mod auth;
pub mod client;
pub mod codec;
pub mod map;
mod paths;
mod server;
mod session;

pub use auth::{AuthConfig, AuthError};
pub use client::{ClientError, IpcClient, IpcClientConfig};
pub use codec::{ClosingNotice, DEFAULT_MAX_FRAME_SIZE};
pub use paths::{default_socket_dir, default_socket_path, SOCKET_FILE_NAME};
pub use server::{IpcError, IpcServer, IpcServerConfig};
