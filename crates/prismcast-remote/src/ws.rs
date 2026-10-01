//! The WebSocket transport (WS-001; PLAN.md §22, protocol doc §1, §8).
//!
//! [`WsServer`] serves the native protocol over `ws://` on a plain
//! `TcpListener` via `tokio-tungstenite` — no axum (the axum-based web UI
//! backend is the separate future `prismcast-web` crate), and no TLS yet
//! (`rustls`-terminated `wss://` is a planned follow-up; until then the
//! server binds loopback by default and token auth is mandatory).
//!
//! - **Framing**: one protocol message per WebSocket text frame, JSON-encoded
//!   (`ClientMessage`/`ServerMessage`). Binary frames are reserved for the
//!   future `prismcast.msgpack` subprotocol (protocol doc §1); receiving one
//!   closes the session with [`CloseCode::MessageDecodeError`].
//! - **Subprotocol**: clients may offer `Sec-WebSocket-Protocol:
//!   prismcast.json`; the server echoes it when offered. No subprotocol
//!   requested means JSON, which is the only codec in v1.
//! - **Close codes**: session termination maps [`CloseCode`] to WebSocket
//!   close frames in the application-private 4000+ range (including
//!   `SlowConsumer` 4013), instead of IPC's synthetic `closing` frame.
//! - **Session machinery**: everything past the framing — handshake,
//!   dispatch, subscriptions, throttle, rate limiting, backpressure — is the
//!   transport-generic [`crate::session`] shared with the IPC server.
//!
//! ## Configuration gating
//!
//! The server is **disabled by default**: [`WsServerConfig::enabled`] must be
//! set explicitly before anything binds, and [`WsServer::bind`] rejects the
//! local-trust auth policy — a network transport requires a credential
//! ([`AuthConfig::token`] or [`AuthConfig::password`], PLAN.md §24), unlike
//! the Unix socket where filesystem permissions gate access.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::protocol::frame::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::{Error as TungsteniteError, Message};
use tokio_tungstenite::WebSocketStream;
use tracing::{debug, info, info_span, warn, Instrument};

use prismcast_app::{AppHandle, DEFAULT_SUBSCRIBER_CAPACITY};

use crate::auth::AuthConfig;
use crate::codec::ClosingNotice;
use crate::server::EventFanout;
use crate::session::{run_session, SessionConfig, SessionContext};
use crate::session_kit::{FrameReadError, FrameReader, FrameWriteError, FrameWriter};

/// The WebSocket subprotocol tag for the JSON codec (protocol doc §1). The
/// MessagePack subprotocol name `prismcast.msgpack` is reserved but not
/// served in v1.
pub const SUBPROTOCOL_JSON: &str = "prismcast.json";

/// Default maximum inbound message payload (1 MiB, protocol doc §1). Larger
/// messages close the session with [`CloseCode::MessageDecodeError`](prismcast_protocol::handshake::CloseCode).
pub const DEFAULT_MAX_MESSAGE_SIZE: usize = 1024 * 1024;

/// Default bind address: loopback only, until TLS exists.
pub const DEFAULT_BIND: SocketAddr =
    SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), 4465);

/// Tuning for [`WsServer`]. The default is inert twice over: `enabled` is
/// `false`, and the placeholder auth policy would be rejected by
/// [`WsServer::bind`] — enabling the server requires an explicit credential
/// (token or password).
#[derive(Debug, Clone)]
pub struct WsServerConfig {
    /// Master switch; nothing binds unless this is `true`.
    pub enabled: bool,
    /// Address to bind (loopback by default; TLS does not exist yet).
    pub bind: SocketAddr,
    /// Authentication policy. Must be [`AuthConfig::Token`] or
    /// [`AuthConfig::Password`]; the local-trust policy is refused on a
    /// network transport.
    pub auth: AuthConfig,
    /// Maximum inbound message payload in bytes (bounds per-connection
    /// memory; protocol doc §1).
    pub max_message_size: usize,
    /// Bound of each session's outbound queue (protocol doc §Backpressure).
    pub outbound_capacity: usize,
    /// Capacity of the server-wide event fan-out channel and of the upstream
    /// broadcaster subscription.
    pub event_queue_capacity: usize,
    /// How long a response enqueue may block before the session is shed with
    /// `SlowConsumer`.
    pub send_timeout: Duration,
    /// Deadline for the client's `Identify` after connect.
    pub handshake_timeout: Duration,
}

impl Default for WsServerConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bind: DEFAULT_BIND,
            auth: AuthConfig::allow_local(),
            max_message_size: DEFAULT_MAX_MESSAGE_SIZE,
            outbound_capacity: 256,
            event_queue_capacity: DEFAULT_SUBSCRIBER_CAPACITY,
            send_timeout: Duration::from_secs(1),
            handshake_timeout: Duration::from_secs(10),
        }
    }
}

impl WsServerConfig {
    /// The transport-independent part of the configuration, consumed by the
    /// shared session machinery.
    fn session_config(&self) -> SessionConfig {
        SessionConfig {
            auth: self.auth.clone(),
            outbound_capacity: self.outbound_capacity,
            send_timeout: self.send_timeout,
            handshake_timeout: self.handshake_timeout,
        }
    }
}

/// Errors binding or running the WebSocket server.
#[derive(Debug, thiserror::Error)]
pub enum WsError {
    /// A socket operation failed.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// [`WsServer::bind`] was called with `enabled: false`; use
    /// [`WsServer::bind_if_enabled`] when the flag is user-controlled.
    #[error("WebSocket server is disabled (WsServerConfig::enabled is false)")]
    Disabled,
    /// A network transport must not run with the local-trust auth policy.
    #[error(
        "WebSocket server requires token or password authentication \
         (AuthConfig::token / AuthConfig::password); the allow-local policy \
         is only valid on the Unix socket"
    )]
    AuthRequired,
}

/// The running WebSocket server.
pub struct WsServer {
    local_addr: SocketAddr,
    accept_task: JoinHandle<()>,
    fanout_task: JoinHandle<()>,
    sessions: Arc<Mutex<Vec<JoinHandle<()>>>>,
    shutdown_tx: watch::Sender<()>,
}

impl WsServer {
    /// Binds the listener, starts the accept loop and the event fan-out.
    ///
    /// Fails with [`WsError::Disabled`] when the config is not enabled, and
    /// with [`WsError::AuthRequired`] when the auth policy is the local-trust
    /// policy.
    pub async fn bind(app: AppHandle, config: WsServerConfig) -> Result<Self, WsError> {
        if !config.enabled {
            return Err(WsError::Disabled);
        }
        if matches!(config.auth, AuthConfig::AllowLocal { .. }) {
            return Err(WsError::AuthRequired);
        }
        let listener = TcpListener::bind(config.bind).await?;
        let local_addr = listener.local_addr()?;
        info!(%local_addr, "WebSocket server listening");

        let session_config = Arc::new(config.session_config());
        let tuning = WsTuning {
            websocket: ws_protocol_config(config.max_message_size),
            max_message_size: config.max_message_size,
        };
        let (shutdown_tx, shutdown_rx) = watch::channel(());
        let (fanout, fanout_task) = EventFanout::spawn(&app, config.event_queue_capacity);
        let sessions: Arc<Mutex<Vec<JoinHandle<()>>>> = Arc::new(Mutex::new(Vec::new()));
        let accept_task = tokio::spawn(
            accept_loop(
                listener,
                app,
                session_config,
                tuning,
                fanout,
                sessions.clone(),
                shutdown_rx,
            )
            .instrument(info_span!("ws_accept")),
        );
        Ok(Self {
            local_addr,
            accept_task,
            fanout_task,
            sessions,
            shutdown_tx,
        })
    }

    /// Like [`bind`](Self::bind), but returns `Ok(None)` instead of an error
    /// when the server is disabled — for embedders wiring a user config flag.
    pub async fn bind_if_enabled(
        app: AppHandle,
        config: WsServerConfig,
    ) -> Result<Option<Self>, WsError> {
        if config.enabled {
            Self::bind(app, config).await.map(Some)
        } else {
            Ok(None)
        }
    }

    /// The address the server is bound to (with the concrete port when the
    /// config used port 0).
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Number of live session tasks (metrics/tests).
    pub fn session_count(&self) -> usize {
        self.lock_sessions()
            .iter()
            .filter(|h| !h.is_finished())
            .count()
    }

    fn lock_sessions(&self) -> MutexGuard<'_, Vec<JoinHandle<()>>> {
        self.sessions.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Graceful shutdown: sessions are signaled (each sends a close frame
    /// with `ServerShutdown` 4015) and the accept loop stops.
    pub async fn shutdown(self) {
        let _ = self.shutdown_tx.send(());
        self.accept_task.abort();
        let sessions: Vec<JoinHandle<()>> = self.lock_sessions().drain(..).collect();
        for mut session in sessions {
            if tokio::time::timeout(Duration::from_secs(2), &mut session)
                .await
                .is_err()
            {
                session.abort();
            }
        }
        self.fanout_task.abort();
        info!("WebSocket server stopped");
    }
}

impl Drop for WsServer {
    fn drop(&mut self) {
        let _ = self.shutdown_tx.send(());
        self.accept_task.abort();
        self.fanout_task.abort();
        for session in self.lock_sessions().drain(..) {
            session.abort();
        }
    }
}

/// Tungstenite-level limits. The protocol's message-size limit is enforced
/// by [`WsFrameReader`] so an oversized message closes with the protocol's
/// `MessageDecodeError` (4002); tungstenite's own cap stays as a memory
/// backstop at 4× the limit (its automatic close would use the generic WS
/// code 1009 instead).
fn ws_protocol_config(max_message_size: usize) -> WebSocketConfig {
    let mut config = WebSocketConfig::default();
    config.max_message_size = Some(max_message_size.saturating_mul(4));
    config.max_frame_size = Some(max_message_size.saturating_mul(4));
    config
}

/// Per-connection WebSocket tuning (tungstenite limits + the protocol's
/// message-size limit).
#[derive(Clone, Copy)]
struct WsTuning {
    websocket: WebSocketConfig,
    max_message_size: usize,
}

/// Negotiates the codec subprotocol: echo `prismcast.json` when the client
/// offered it; otherwise select none (JSON is the default, protocol doc §1).
// The error type is dictated by tungstenite's `AcceptCallback` trait.
#[allow(clippy::result_large_err)]
fn negotiate_subprotocol(
    request: &Request,
    mut response: Response,
) -> Result<Response, ErrorResponse> {
    use tokio_tungstenite::tungstenite::http::header::SEC_WEBSOCKET_PROTOCOL;
    let offered_json = request
        .headers()
        .get(SEC_WEBSOCKET_PROTOCOL)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(',')
                .any(|token| token.trim() == SUBPROTOCOL_JSON)
        });
    if offered_json {
        response.headers_mut().insert(
            SEC_WEBSOCKET_PROTOCOL,
            HeaderValue::from_static(SUBPROTOCOL_JSON),
        );
    }
    Ok(response)
}

async fn accept_loop(
    listener: TcpListener,
    app: AppHandle,
    config: Arc<SessionConfig>,
    tuning: WsTuning,
    fanout: EventFanout,
    sessions: Arc<Mutex<Vec<JoinHandle<()>>>>,
    mut shutdown: watch::Receiver<()>,
) {
    let mut next_connection = 0_u64;
    loop {
        tokio::select! {
            _ = shutdown.changed() => {
                debug!("accept loop shutting down");
                break;
            }
            accepted = listener.accept() => match accepted {
                Ok((stream, peer)) => {
                    next_connection += 1;
                    debug!(connection_id = next_connection, %peer, "accepted connection");
                    let context = SessionContext {
                        app: app.clone(),
                        config: config.clone(),
                        fanout: fanout.clone(),
                        shutdown: shutdown.clone(),
                        transport: "ws",
                    };
                    let handle = tokio::spawn(
                        upgrade_and_run(stream, tuning, next_connection, context)
                            .instrument(info_span!("ws_upgrade", connection_id = next_connection)),
                    );
                    let mut guard = sessions.lock().unwrap_or_else(|p| p.into_inner());
                    guard.retain(|h| !h.is_finished());
                    guard.push(handle);
                }
                Err(error) => warn!(%error, "accept failed"),
            },
        }
    }
}

/// Performs the HTTP → WebSocket upgrade, then hands the connection to the
/// shared session machinery.
async fn upgrade_and_run(
    stream: TcpStream,
    tuning: WsTuning,
    connection_id: u64,
    context: SessionContext,
) {
    let upgraded = tokio_tungstenite::accept_hdr_async_with_config(
        stream,
        negotiate_subprotocol,
        Some(tuning.websocket),
    )
    .await;
    let websocket = match upgraded {
        Ok(websocket) => websocket,
        Err(error) => {
            debug!(%error, "WebSocket upgrade failed; dropping connection");
            return;
        }
    };
    let (writer, reader) = websocket.split();
    let reader = WsFrameReader {
        inner: reader,
        max_message_size: tuning.max_message_size,
    };
    let writer = WsFrameWriter { inner: writer };
    run_session(reader, writer, connection_id, context).await;
}

type WsStream = WebSocketStream<TcpStream>;

/// WS [`FrameReader`]: one JSON text frame per protocol message. Binary
/// frames are rejected (the `prismcast.msgpack` subprotocol is reserved,
/// protocol doc §1); ping/pong is handled by tungstenite.
struct WsFrameReader {
    inner: SplitStream<WsStream>,
    max_message_size: usize,
}

impl FrameReader for WsFrameReader {
    async fn read_value(&mut self) -> Result<Option<serde_json::Value>, FrameReadError> {
        loop {
            match self.inner.next().await {
                None => return Ok(None),
                Some(Ok(Message::Text(text))) => {
                    if text.len() > self.max_message_size {
                        return Err(FrameReadError(format!(
                            "message payload {} bytes exceeds limit of {}",
                            text.len(),
                            self.max_message_size
                        )));
                    }
                    return serde_json::from_str(&text)
                        .map(Some)
                        .map_err(|e| FrameReadError(format!("malformed JSON: {e}")));
                }
                Some(Ok(Message::Binary(_))) => {
                    return Err(FrameReadError(
                        "binary frames are not supported: v1 speaks JSON text; \
                         the prismcast.msgpack subprotocol is reserved"
                            .to_string(),
                    ));
                }
                Some(Ok(Message::Close(_))) => return Ok(None),
                // Ping/Pong (answered automatically by tungstenite) and raw
                // frames carry no protocol payload.
                Some(Ok(_)) => continue,
                Some(Err(TungsteniteError::ConnectionClosed))
                | Some(Err(TungsteniteError::AlreadyClosed)) => return Ok(None),
                Some(Err(error)) => return Err(FrameReadError(error.to_string())),
            }
        }
    }
}

/// WS [`FrameWriter`]: JSON text frames; the closing notice becomes a
/// WebSocket close frame carrying the protocol's numeric code (4000+ range,
/// protocol doc §8).
struct WsFrameWriter {
    inner: SplitSink<WsStream, Message>,
}

impl FrameWriter for WsFrameWriter {
    async fn write_message(
        &mut self,
        message: &prismcast_protocol::message::ServerMessage,
    ) -> Result<(), FrameWriteError> {
        // Encoding this type is total; a failure here is a bug, not a client
        // error.
        let json = serde_json::to_string(message).map_err(|e| FrameWriteError(e.to_string()))?;
        self.inner
            .send(Message::Text(json.into()))
            .await
            .map_err(|e| FrameWriteError(e.to_string()))
    }

    async fn write_close(&mut self, notice: &ClosingNotice) -> Result<(), FrameWriteError> {
        let frame = CloseFrame {
            code: notice.code.into(),
            reason: notice.message.clone().into(),
        };
        self.inner
            .send(Message::Close(Some(frame)))
            .await
            .map_err(|e| FrameWriteError(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_disabled() {
        let config = WsServerConfig::default();
        assert!(!config.enabled);
        assert!(config.bind.ip().is_loopback());
    }

    #[tokio::test]
    async fn bind_requires_enabled_flag() {
        let app = AppHandle::spawn(prismcast_app::CoreConfig::default());
        let result = WsServer::bind(app.clone(), WsServerConfig::default()).await;
        assert!(matches!(result, Err(WsError::Disabled)));
        let none = WsServer::bind_if_enabled(app.clone(), WsServerConfig::default())
            .await
            .expect("bind_if_enabled");
        assert!(none.is_none());
        app.shutdown().await;
    }

    #[tokio::test]
    async fn enabled_bind_requires_token_auth() {
        let app = AppHandle::spawn(prismcast_app::CoreConfig::default());
        let result = WsServer::bind(
            app.clone(),
            WsServerConfig {
                enabled: true,
                auth: AuthConfig::allow_local(),
                ..WsServerConfig::default()
            },
        )
        .await;
        assert!(matches!(result, Err(WsError::AuthRequired)));
        app.shutdown().await;
    }
}
