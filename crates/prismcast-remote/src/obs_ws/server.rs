//! The obs-websocket 5.x server (OBSWS-001; ADR-0010, ADR-0020).
//!
//! [`ObsWsServer`] serves the obs-websocket wire protocol over `ws://` on a
//! plain `TcpListener` via `tokio-tungstenite`, modeled on
//! [`crate::ws::WsServer`]: same bind/shutdown pattern, same
//! [`EventFanout`] reuse, same disabled-by-default and
//! network-transport-requires-a-credential gating.
//!
//! - **Subprotocol**: clients offering `Sec-WebSocket-Protocol:
//!   obswebsocket.json` get it echoed (JSON wins when both known tags are
//!   offered); `obswebsocket.msgpack` alone selects MessagePack binary
//!   frames (OBSWS-002; ADR-0021); no subprotocol requested means JSON (the
//!   obs default). Any other subprotocol set is refused at the HTTP upgrade
//!   with a 400. (Upstream accepts anything and defaults to JSON; refusing
//!   unknown codecs is a deliberate hardening, documented in the module
//!   docs.)
//! - **Defaults**: `127.0.0.1:4455` (the obs-websocket default port),
//!   disabled by default, and [`AuthConfig::AllowLocal`] rejected at
//!   [`ObsWsServer::bind`] — password (obs's own model) or token (Prismcast
//!   extension: the `authentication` string carries the token).
//! - **Session machinery**: everything past the framing is
//!   [`super::session`], built on [`crate::session_kit`].

use std::net::SocketAddr;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http::{HeaderValue, StatusCode};
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tracing::{debug, info, info_span, warn, Instrument};

use prismcast_app::{AppHandle, DEFAULT_SUBSCRIBER_CAPACITY};

use crate::auth::AuthConfig;
use crate::server::EventFanout;

use super::codec::ObsCodec;
use super::names;
use super::proto::{SUBPROTOCOL_JSON, SUBPROTOCOL_MSGPACK};
use super::session::{run_session, ObsSessionConfig, ObsSessionContext};

/// Default bind address: loopback, the obs-websocket default port 4455.
pub const DEFAULT_BIND: SocketAddr =
    SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), 4455);

/// Default maximum inbound message payload (1 MiB, matching the native WS
/// server). Larger messages close the session with `MessageDecodeError`
/// (4002).
pub const DEFAULT_MAX_MESSAGE_SIZE: usize = 1024 * 1024;

/// Tuning for [`ObsWsServer`]. The default is inert twice over: `enabled` is
/// `false`, and the placeholder auth policy would be rejected by
/// [`ObsWsServer::bind`] — enabling the server requires an explicit
/// credential (password or token).
#[derive(Debug, Clone)]
pub struct ObsWsServerConfig {
    /// Master switch; nothing binds unless this is `true`.
    pub enabled: bool,
    /// Address to bind (loopback:4455 by default; TLS does not exist yet).
    pub bind: SocketAddr,
    /// Authentication policy. Must be [`AuthConfig::Password`] (obs's own
    /// challenge-response) or [`AuthConfig::Token`] (Prismcast extension);
    /// the local-trust policy is refused on a network transport.
    pub auth: AuthConfig,
    /// Maximum inbound message payload in bytes.
    pub max_message_size: usize,
    /// Bound of each session's outbound queue.
    pub outbound_capacity: usize,
    /// Capacity of the server-wide event fan-out channel and of the upstream
    /// broadcaster subscription.
    pub event_queue_capacity: usize,
    /// How long a response enqueue may block before the session is shed.
    pub send_timeout: Duration,
    /// Deadline for the client's `Identify` after connect.
    pub handshake_timeout: Duration,
}

impl Default for ObsWsServerConfig {
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

impl ObsWsServerConfig {
    /// The transport-independent part of the configuration, consumed by the
    /// session engine.
    fn session_config(&self) -> ObsSessionConfig {
        ObsSessionConfig {
            auth: self.auth.clone(),
            outbound_capacity: self.outbound_capacity,
            send_timeout: self.send_timeout,
            handshake_timeout: self.handshake_timeout,
            max_message_size: self.max_message_size,
        }
    }
}

/// Errors binding or running the obs-websocket server.
#[derive(Debug, thiserror::Error)]
pub enum ObsWsError {
    /// A socket operation failed.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// [`ObsWsServer::bind`] was called with `enabled: false`; use
    /// [`ObsWsServer::bind_if_enabled`] when the flag is user-controlled.
    #[error("obs-websocket server is disabled (ObsWsServerConfig::enabled is false)")]
    Disabled,
    /// A network transport must not run with the local-trust auth policy.
    #[error(
        "obs-websocket server requires password or token authentication \
         (AuthConfig::password / AuthConfig::token); the allow-local policy \
         is only valid on the Unix socket"
    )]
    AuthRequired,
}

/// The running obs-websocket server.
pub struct ObsWsServer {
    local_addr: SocketAddr,
    accept_task: JoinHandle<()>,
    fanout_task: JoinHandle<()>,
    item_id_task: JoinHandle<()>,
    sessions: Arc<Mutex<Vec<JoinHandle<()>>>>,
    shutdown_tx: watch::Sender<()>,
}

impl ObsWsServer {
    /// Binds the listener, starts the accept loop and the event fan-out.
    ///
    /// Fails with [`ObsWsError::Disabled`] when the config is not enabled,
    /// and with [`ObsWsError::AuthRequired`] when the auth policy is the
    /// local-trust policy.
    pub async fn bind(app: AppHandle, config: ObsWsServerConfig) -> Result<Self, ObsWsError> {
        if !config.enabled {
            return Err(ObsWsError::Disabled);
        }
        if matches!(config.auth, AuthConfig::AllowLocal { .. }) {
            return Err(ObsWsError::AuthRequired);
        }
        let listener = TcpListener::bind(config.bind).await?;
        let local_addr = listener.local_addr()?;
        info!(%local_addr, "obs-websocket server listening");

        let session_config = Arc::new(config.session_config());
        let websocket_config = ws_protocol_config(config.max_message_size);
        let (shutdown_tx, shutdown_rx) = watch::channel(());
        let (fanout, fanout_task) = EventFanout::spawn(&app, config.event_queue_capacity);
        // Server-wide scene-item ID registry with its eviction listener on
        // the fan-out (ADR-0020 §c: evict on every removal path).
        let item_ids = names::ItemIdMap::shared();
        let item_id_task = tokio::spawn(names::eviction_listener(
            fanout.subscribe(),
            item_ids.clone(),
        ));
        let sessions: Arc<Mutex<Vec<JoinHandle<()>>>> = Arc::new(Mutex::new(Vec::new()));
        let shared = SharedServices {
            app,
            fanout,
            item_ids,
        };
        let accept_task = tokio::spawn(
            accept_loop(
                listener,
                session_config,
                websocket_config,
                shared,
                sessions.clone(),
                shutdown_rx,
            )
            .instrument(info_span!("obs_ws_accept")),
        );
        Ok(Self {
            local_addr,
            accept_task,
            fanout_task,
            item_id_task,
            sessions,
            shutdown_tx,
        })
    }

    /// Like [`bind`](Self::bind), but returns `Ok(None)` instead of an error
    /// when the server is disabled — for embedders wiring a user config flag.
    pub async fn bind_if_enabled(
        app: AppHandle,
        config: ObsWsServerConfig,
    ) -> Result<Option<Self>, ObsWsError> {
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
    /// with RFC 6455 `going_away`, like upstream's "Server stopping.") and
    /// the accept loop stops.
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
        self.item_id_task.abort();
        info!("obs-websocket server stopped");
    }
}

impl Drop for ObsWsServer {
    fn drop(&mut self) {
        let _ = self.shutdown_tx.send(());
        self.accept_task.abort();
        self.fanout_task.abort();
        self.item_id_task.abort();
        for session in self.lock_sessions().drain(..) {
            session.abort();
        }
    }
}

/// Tungstenite-level limits. The protocol's message-size limit is enforced
/// by the session reader so an oversized message closes with 4002;
/// tungstenite's own cap stays as a memory backstop at 4× the limit.
fn ws_protocol_config(max_message_size: usize) -> WebSocketConfig {
    let mut config = WebSocketConfig::default();
    config.max_message_size = Some(max_message_size.saturating_mul(4));
    config.max_frame_size = Some(max_message_size.saturating_mul(4));
    config
}

/// Records the negotiated codec in the shared cell (tungstenite's accept
/// callback cannot return a value, so the codec is captured here and read
/// after the upgrade completes).
fn record_codec(negotiated: &Mutex<Option<ObsCodec>>, codec: ObsCodec) {
    *negotiated.lock().unwrap_or_else(|p| p.into_inner()) = Some(codec);
}

/// Negotiates the codec subprotocol and records the outcome in `negotiated`.
/// Priority: `obswebsocket.json` when offered (JSON wins when both known
/// tags are offered); else `obswebsocket.msgpack` (echoed, MessagePack
/// binary frames); no header means JSON (the obs default); any other
/// subprotocol set refuses the upgrade with HTTP 400.
// The error type is dictated by tungstenite's `Callback` trait.
#[allow(clippy::result_large_err)]
fn negotiate_subprotocol(
    request: &Request,
    mut response: Response,
    negotiated: &Mutex<Option<ObsCodec>>,
) -> Result<Response, ErrorResponse> {
    use tokio_tungstenite::tungstenite::http::header::SEC_WEBSOCKET_PROTOCOL;
    let Some(offered) = request
        .headers()
        .get(SEC_WEBSOCKET_PROTOCOL)
        .and_then(|value| value.to_str().ok())
    else {
        // No subprotocol requested: JSON by default.
        record_codec(negotiated, ObsCodec::Json);
        return Ok(response);
    };
    let selected = if offered
        .split(',')
        .any(|token| token.trim() == SUBPROTOCOL_JSON)
    {
        Some((ObsCodec::Json, SUBPROTOCOL_JSON))
    } else if offered
        .split(',')
        .any(|token| token.trim() == SUBPROTOCOL_MSGPACK)
    {
        Some((ObsCodec::MsgPack, SUBPROTOCOL_MSGPACK))
    } else {
        None
    };
    if let Some((codec, subprotocol)) = selected {
        response.headers_mut().insert(
            SEC_WEBSOCKET_PROTOCOL,
            HeaderValue::from_static(subprotocol),
        );
        record_codec(negotiated, codec);
        return Ok(response);
    }
    warn!(%offered, "rejecting unsupported obs-websocket subprotocol");
    let mut rejection = ErrorResponse::new(Some(format!(
        "unsupported subprotocol(s) `{offered}`: this server speaks {SUBPROTOCOL_JSON} \
         and {SUBPROTOCOL_MSGPACK}"
    )));
    *rejection.status_mut() = StatusCode::BAD_REQUEST;
    Err(rejection)
}

/// Handles shared by every session: the core handle, the event fan-out, and
/// the server-wide scene-item ID registry.
struct SharedServices {
    app: AppHandle,
    fanout: EventFanout,
    item_ids: Arc<names::ItemIdMap>,
}

async fn accept_loop(
    listener: TcpListener,
    config: Arc<ObsSessionConfig>,
    websocket_config: WebSocketConfig,
    shared: SharedServices,
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
                    let context = ObsSessionContext {
                        config: config.clone(),
                        // Placeholder: the negotiated codec is known only
                        // after the upgrade; `upgrade_and_run` overwrites it.
                        codec: ObsCodec::Json,
                        app: shared.app.clone(),
                        item_ids: shared.item_ids.clone(),
                        fanout: shared.fanout.clone(),
                        shutdown: shutdown.clone(),
                    };
                    let handle = tokio::spawn(
                        upgrade_and_run(stream, websocket_config, next_connection, context)
                            .instrument(info_span!("obs_ws_upgrade", connection_id = next_connection)),
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

/// Performs the HTTP → WebSocket upgrade (with subprotocol negotiation),
/// then hands the connection to the obs session engine with the negotiated
/// codec.
async fn upgrade_and_run(
    stream: TcpStream,
    websocket_config: WebSocketConfig,
    connection_id: u64,
    mut context: ObsSessionContext,
) {
    let negotiated = Arc::new(Mutex::new(None));
    let callback_cell = negotiated.clone();
    let upgraded = tokio_tungstenite::accept_hdr_async_with_config(
        stream,
        move |request: &Request, response| negotiate_subprotocol(request, response, &callback_cell),
        Some(websocket_config),
    )
    .await;
    match upgraded {
        Ok(websocket) => {
            // Every accepted path records a codec; JSON is the fallback.
            context.codec = negotiated
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .unwrap_or(ObsCodec::Json);
            run_session(websocket, connection_id, context).await;
        }
        Err(error) => {
            debug!(%error, "WebSocket upgrade failed; dropping connection");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_disabled_on_obs_port() {
        let config = ObsWsServerConfig::default();
        assert!(!config.enabled);
        assert!(config.bind.ip().is_loopback());
        assert_eq!(config.bind.port(), 4455);
    }

    #[tokio::test]
    async fn bind_requires_enabled_flag() {
        let app = AppHandle::spawn(prismcast_app::CoreConfig::default());
        let result = ObsWsServer::bind(app.clone(), ObsWsServerConfig::default()).await;
        assert!(matches!(result, Err(ObsWsError::Disabled)));
        let none = ObsWsServer::bind_if_enabled(app.clone(), ObsWsServerConfig::default())
            .await
            .expect("bind_if_enabled");
        assert!(none.is_none());
        app.shutdown().await;
    }

    #[tokio::test]
    async fn enabled_bind_rejects_allow_local() {
        let app = AppHandle::spawn(prismcast_app::CoreConfig::default());
        let result = ObsWsServer::bind(
            app.clone(),
            ObsWsServerConfig {
                enabled: true,
                auth: AuthConfig::allow_local(),
                ..ObsWsServerConfig::default()
            },
        )
        .await;
        assert!(matches!(result, Err(ObsWsError::AuthRequired)));
        app.shutdown().await;
    }
}
