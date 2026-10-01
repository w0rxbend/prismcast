//! A minimal WebSocket client for the native protocol (WS-001).
//!
//! [`WsClient`] mirrors [`crate::client::IpcClient`] over the `ws://`
//! transport: it performs the `Hello`/`Identify`/`Identified` handshake
//! (token or challenge-response per [`ClientAuth`]), then a background
//! reader task routes `request_response`s to their callers by `request_id`
//! and delivers `event`s to a bounded channel ([`WsClient::next_event`]). A
//! server close frame fails all pending requests with
//! [`WsClientError::Closed`] carrying the numeric close code (protocol doc
//! §8).
//!
//! Used by the integration tests and by future web tooling.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::SEC_WEBSOCKET_PROTOCOL;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::{Error as TungsteniteError, Message};
use tokio_tungstenite::WebSocketStream;
use tracing::{debug, warn};
use uuid::Uuid;

use prismcast_protocol::batch::{RequestBatch, RequestBatchResponse};
use prismcast_protocol::error::WireError;
use prismcast_protocol::event::EventMessage;
use prismcast_protocol::handshake::{ClientInfo, Hello, Identified, Identify, Permission};
use prismcast_protocol::message::{ClientMessage, ServerMessage};
use prismcast_protocol::request::{Request, RequestKind};
use prismcast_protocol::response::{RequestResponse, ResponseData};
use prismcast_protocol::subscription::SubscriptionSet;
use prismcast_protocol::version;

use crate::client::ClientAuth;
use crate::ws::{DEFAULT_MAX_MESSAGE_SIZE, SUBPROTOCOL_JSON};

/// Client tuning.
#[derive(Debug, Clone)]
pub struct WsClientConfig {
    /// Protocol version to request in `Identify`.
    pub protocol_version: u32,
    /// Credential presented in `Identify` (supersedes `token`); the server
    /// requires a credential on this transport.
    pub auth: ClientAuth,
    /// Legacy bearer-token field, superseded by [`ClientAuth`] (`auth`) and
    /// consulted only when `auth` is [`ClientAuth::None`]. Kept for source
    /// compatibility with pre-WS-002 callers.
    pub token: Option<String>,
    /// Initial subscriptions; `None` = server default (all standard
    /// categories), `Some(empty)` = no events.
    pub subscriptions: Option<SubscriptionSet>,
    /// Self-description sent in `Identify`.
    pub client: Option<ClientInfo>,
    /// Deadline for a single request's response.
    pub request_timeout: Duration,
    /// Deadline for the handshake.
    pub handshake_timeout: Duration,
    /// Maximum inbound message payload.
    pub max_message_size: usize,
    /// Bound of the inbound event channel; events for a client that does not
    /// drain are dropped (with a warning) rather than blocking the reader.
    pub event_channel_capacity: usize,
}

impl Default for WsClientConfig {
    fn default() -> Self {
        Self {
            protocol_version: version::PROTOCOL_VERSION,
            auth: ClientAuth::None,
            token: None,
            subscriptions: None,
            client: None,
            request_timeout: Duration::from_secs(10),
            handshake_timeout: Duration::from_secs(5),
            max_message_size: DEFAULT_MAX_MESSAGE_SIZE,
            event_channel_capacity: 256,
        }
    }
}

/// Errors talking to the server.
#[derive(Debug, thiserror::Error)]
pub enum WsClientError {
    /// Socket I/O failed.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// The WebSocket layer failed (upgrade, framing).
    #[error("WebSocket error: {0}")]
    WebSocket(#[from] TungsteniteError),
    /// A frame could not be encoded or decoded.
    #[error("protocol codec error: {0}")]
    Decode(String),
    /// The server closed the session, with the close code it sent (or a
    /// dropped connection before any close frame).
    #[error("server closed the session: {reason} (code {code})")]
    Closed {
        /// Numeric close code (see `prismcast_protocol::handshake::CloseCode`).
        code: u16,
        /// Close reason string.
        reason: String,
    },
    /// A request or handshake step exceeded its deadline.
    #[error("timed out waiting for the server")]
    Timeout,
    /// The server answered a request with a structured failure.
    #[error("request failed: {0:?}")]
    RequestFailed(WireError),
}

type WsStream = WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>;

/// A server-initiated close, recorded by the reader task.
#[derive(Debug, Clone)]
struct CloseRecord {
    code: u16,
    reason: String,
}

/// A connected, identified WebSocket session.
pub struct WsClient {
    writer: tokio::sync::Mutex<SplitSink<WsStream, Message>>,
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<PendingResponse>>>>,
    closed: Arc<Mutex<Option<CloseRecord>>>,
    events_rx: mpsc::Receiver<EventMessage>,
    reader_task: JoinHandle<()>,
    next_request_id: u64,
    config: WsClientConfig,
    /// The negotiated protocol version.
    pub negotiated_protocol_version: u32,
    /// The server-assigned session ID.
    pub session_id: Uuid,
    /// The permissions granted to this session.
    pub permissions: Vec<Permission>,
}

/// What a pending caller waits for: a single response or a batch response.
enum PendingResponse {
    Single(Box<RequestResponse>),
    Batch(Box<RequestBatchResponse>),
}

impl WsClient {
    /// Connects to `addr` with default settings and performs the handshake.
    pub async fn connect(addr: SocketAddr) -> Result<Self, WsClientError> {
        Self::connect_with(addr, WsClientConfig::default()).await
    }

    /// Connects and performs the `Hello`/`Identify`/`Identified` handshake,
    /// offering the `prismcast.json` subprotocol (protocol doc §1).
    pub async fn connect_with(
        addr: SocketAddr,
        config: WsClientConfig,
    ) -> Result<Self, WsClientError> {
        let mut request = format!("ws://{addr}/").into_client_request()?;
        request.headers_mut().insert(
            SEC_WEBSOCKET_PROTOCOL,
            HeaderValue::from_static(SUBPROTOCOL_JSON),
        );
        let (stream, _response) = tokio_tungstenite::connect_async(request).await?;
        let (writer, mut reader) = stream.split();

        // --- handshake (before the reader task exists) ---
        let hello_value = handshake_read(&mut reader, &config).await?;
        let hello: Hello = decode_data(&hello_value, "hello")?;

        let authentication = crate::client::auth_response(
            &crate::client::effective_auth(&config.auth, &config.token),
            &hello,
        )
        .map_err(WsClientError::Decode)?;
        let identify = ClientMessage::Identify(Identify {
            protocol_version: config.protocol_version,
            authentication,
            subscriptions: config.subscriptions.clone(),
            client: config.client.clone(),
        });
        let mut writer = writer;
        write_message(&mut writer, &identify).await?;

        let identified_value = handshake_read(&mut reader, &config).await?;
        let identified: Identified = decode_data(&identified_value, "identified")?;

        // --- steady state: background reader routes frames ---
        let pending: Arc<Mutex<HashMap<String, oneshot::Sender<PendingResponse>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let closed: Arc<Mutex<Option<CloseRecord>>> = Arc::new(Mutex::new(None));
        let (events_tx, events_rx) = mpsc::channel(config.event_channel_capacity.max(1));
        let reader_task = tokio::spawn(reader_loop(
            reader,
            config.max_message_size,
            pending.clone(),
            closed.clone(),
            events_tx,
        ));

        Ok(Self {
            writer: tokio::sync::Mutex::new(writer),
            pending,
            closed,
            events_rx,
            reader_task,
            next_request_id: 0,
            config,
            negotiated_protocol_version: identified.negotiated_protocol_version,
            session_id: identified.session_id,
            permissions: identified.permissions,
        })
    }

    /// Sends a request and awaits its correlated response (events arriving in
    /// between are queued for [`next_event`](Self::next_event)).
    pub async fn request(&mut self, kind: RequestKind) -> Result<RequestResponse, WsClientError> {
        let request_id = self.next_request_id();
        let message = ClientMessage::Request(Request {
            request_id: request_id.clone(),
            kind,
        });
        let response = self.roundtrip(request_id, message).await?;
        match response {
            PendingResponse::Single(response) => Ok(*response),
            PendingResponse::Batch(_) => Err(WsClientError::Decode(
                "expected request_response, got request_batch_response".into(),
            )),
        }
    }

    /// Sends a serial batch and awaits its correlated response (protocol
    /// doc §6).
    pub async fn request_batch(
        &mut self,
        batch: RequestBatch,
    ) -> Result<RequestBatchResponse, WsClientError> {
        let request_id = batch.request_id.clone();
        let response = self
            .roundtrip(request_id, ClientMessage::RequestBatch(batch))
            .await?;
        match response {
            PendingResponse::Batch(response) => Ok(*response),
            PendingResponse::Single(_) => Err(WsClientError::Decode(
                "expected request_batch_response, got request_response".into(),
            )),
        }
    }

    /// Like [`request`](Self::request), but unwraps the status: a failed
    /// request becomes [`WsClientError::RequestFailed`].
    pub async fn request_data(&mut self, kind: RequestKind) -> Result<ResponseData, WsClientError> {
        let response = self.request(kind).await?;
        if response.status.ok {
            Ok(response.data.unwrap_or(ResponseData::Empty))
        } else {
            let error = response.status.error.unwrap_or_else(|| {
                WireError::new(
                    prismcast_protocol::error::ErrorKind::Internal,
                    "missing error payload",
                )
            });
            Err(WsClientError::RequestFailed(error))
        }
    }

    /// Receives the next subscribed event, or `None` after the server closed
    /// the session and the queue is drained.
    pub async fn next_event(&mut self) -> Option<EventMessage> {
        self.events_rx.recv().await
    }

    /// Replaces the session's subscriptions; returns the applied set.
    pub async fn update_subscriptions(
        &mut self,
        subscriptions: SubscriptionSet,
    ) -> Result<SubscriptionSet, WsClientError> {
        match self
            .request_data(RequestKind::UpdateSubscriptions { subscriptions })
            .await?
        {
            ResponseData::Subscriptions { subscriptions } => Ok(subscriptions),
            other => Err(WsClientError::Decode(format!(
                "unexpected update_subscriptions response: {other:?}"
            ))),
        }
    }

    /// Closes the connection with a normal WebSocket close frame.
    pub async fn close(self) {
        self.reader_task.abort();
        let mut writer = self.writer.lock().await;
        let _ = writer.send(Message::Close(None)).await;
    }

    fn next_request_id(&mut self) -> String {
        self.next_request_id += 1;
        format!("req-{}", self.next_request_id)
    }

    /// Registers the correlation slot, sends the message, and awaits the
    /// routed response.
    async fn roundtrip(
        &mut self,
        request_id: String,
        message: ClientMessage,
    ) -> Result<PendingResponse, WsClientError> {
        let (tx, rx) = oneshot::channel();
        self.lock_pending().insert(request_id.clone(), tx);
        let write_result = {
            let mut writer = self.writer.lock().await;
            write_message(&mut writer, &message).await
        };
        if let Err(error) = write_result {
            self.lock_pending().remove(&request_id);
            return Err(error);
        }
        match timeout(self.config.request_timeout, rx).await {
            Ok(Ok(response)) => Ok(response),
            Ok(Err(_)) => Err(self.closed_error()),
            Err(_) => {
                self.lock_pending().remove(&request_id);
                Err(WsClientError::Timeout)
            }
        }
    }

    fn lock_pending(&self) -> MutexGuard<'_, HashMap<String, oneshot::Sender<PendingResponse>>> {
        self.pending.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn closed_error(&self) -> WsClientError {
        let record = self
            .closed
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        match record {
            Some(record) => WsClientError::Closed {
                code: record.code,
                reason: record.reason,
            },
            None => WsClientError::Closed {
                code: 4000,
                reason: "connection_lost".to_string(),
            },
        }
    }
}

impl Drop for WsClient {
    fn drop(&mut self) {
        self.reader_task.abort();
    }
}

/// The reader task: routes responses to pending callers, events to the event
/// channel, and records the terminal close frame.
async fn reader_loop(
    mut reader: SplitStream<WsStream>,
    max_message_size: usize,
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<PendingResponse>>>>,
    closed: Arc<Mutex<Option<CloseRecord>>>,
    events_tx: mpsc::Sender<EventMessage>,
) {
    loop {
        let value = match read_value(&mut reader, max_message_size).await {
            Ok(Some(value)) => value,
            Ok(None) => break, // clean close
            Err(ReadFailure::Closed(record)) => {
                *closed.lock().unwrap_or_else(|p| p.into_inner()) = Some(record);
                break;
            }
            Err(ReadFailure::Undecodable(error)) => {
                debug!(%error, "undecodable frame; closing");
                break;
            }
        };
        match serde_json::from_value::<ServerMessage>(value) {
            Ok(ServerMessage::RequestResponse(response)) => {
                let tx = pending
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .remove(&response.request_id);
                if let Some(tx) = tx {
                    let _ = tx.send(PendingResponse::Single(Box::new(response)));
                } else {
                    debug!(request_id = %response.request_id, "response for unknown request");
                }
            }
            Ok(ServerMessage::RequestBatchResponse(response)) => {
                let tx = pending
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .remove(&response.request_id);
                if let Some(tx) = tx {
                    let _ = tx.send(PendingResponse::Batch(Box::new(response)));
                } else {
                    debug!(request_id = %response.request_id, "batch response for unknown request");
                }
            }
            Ok(ServerMessage::Event(event)) => {
                if events_tx.try_send(event).is_err() {
                    warn!("client event channel full; dropping event");
                }
            }
            Ok(other) => {
                debug!(tag = other.tag(), "unexpected post-handshake message")
            }
            Err(error) => debug!(%error, "undecodable server message; ignored"),
        }
    }
    // Fail every pending request so callers observe the close.
    for (_, tx) in pending.lock().unwrap_or_else(|p| p.into_inner()).drain() {
        drop(tx);
    }
}

/// Why reading one inbound frame failed.
enum ReadFailure {
    /// The server sent a close frame.
    Closed(CloseRecord),
    /// The frame was not usable JSON text.
    Undecodable(String),
}

/// Reads one inbound text frame as a generic value. `Ok(None)` is a clean
/// close without a code (EOF); a close frame becomes
/// [`ReadFailure::Closed`].
async fn read_value(
    reader: &mut SplitStream<WsStream>,
    max_message_size: usize,
) -> Result<Option<serde_json::Value>, ReadFailure> {
    loop {
        match reader.next().await {
            None => return Ok(None),
            Some(Ok(Message::Text(text))) => {
                if text.len() > max_message_size {
                    return Err(ReadFailure::Undecodable(format!(
                        "message payload {} bytes exceeds limit of {max_message_size}",
                        text.len()
                    )));
                }
                return serde_json::from_str(&text)
                    .map(Some)
                    .map_err(|e| ReadFailure::Undecodable(format!("malformed JSON: {e}")));
            }
            Some(Ok(Message::Close(frame))) => {
                let record = frame
                    .map(|frame| CloseRecord {
                        code: frame.code.into(),
                        reason: frame.reason.to_string(),
                    })
                    .unwrap_or(CloseRecord {
                        code: 1005,
                        reason: "no_status".to_string(),
                    });
                return Err(ReadFailure::Closed(record));
            }
            Some(Ok(Message::Binary(_))) => {
                return Err(ReadFailure::Undecodable(
                    "unexpected binary frame (v1 speaks JSON text)".to_string(),
                ));
            }
            // Ping/Pong and raw frames carry no protocol payload.
            Some(Ok(_)) => continue,
            Some(Err(TungsteniteError::ConnectionClosed))
            | Some(Err(TungsteniteError::AlreadyClosed)) => return Ok(None),
            Some(Err(error)) => return Err(ReadFailure::Undecodable(error.to_string())),
        }
    }
}

/// Handshake-phase read with a deadline: a close frame surfaces as
/// [`WsClientError::Closed`] (e.g. 4009 on a wrong token).
async fn handshake_read(
    reader: &mut SplitStream<WsStream>,
    config: &WsClientConfig,
) -> Result<serde_json::Value, WsClientError> {
    let read = timeout(
        config.handshake_timeout,
        read_value(reader, config.max_message_size),
    )
    .await
    .map_err(|_| WsClientError::Timeout)?;
    match read {
        Ok(Some(value)) => Ok(value),
        Ok(None) => Err(WsClientError::Closed {
            code: 4000,
            reason: "connection_lost".to_string(),
        }),
        Err(ReadFailure::Closed(record)) => Err(WsClientError::Closed {
            code: record.code,
            reason: record.reason,
        }),
        Err(ReadFailure::Undecodable(error)) => Err(WsClientError::Decode(error)),
    }
}

async fn write_message(
    writer: &mut SplitSink<WsStream, Message>,
    message: &ClientMessage,
) -> Result<(), WsClientError> {
    let json = serde_json::to_string(message).map_err(|e| WsClientError::Decode(e.to_string()))?;
    writer.send(Message::Text(json.into())).await?;
    Ok(())
}

/// Extracts `data` from a frame expecting the given `type` tag.
fn decode_data<T: serde::de::DeserializeOwned>(
    value: &serde_json::Value,
    expected: &str,
) -> Result<T, WsClientError> {
    if value.get("type").and_then(|t| t.as_str()) != Some(expected) {
        return Err(WsClientError::Decode(format!(
            "expected '{expected}' frame, got {:?}",
            value.get("type").and_then(|t| t.as_str())
        )));
    }
    serde_json::from_value(
        value
            .get("data")
            .cloned()
            .unwrap_or(serde_json::Value::Null),
    )
    .map_err(|e| WsClientError::Decode(format!("malformed {expected}: {e}")))
}
