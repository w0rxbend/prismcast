//! A minimal IPC client shared by `prismcast-cli` and integration tests
//! (IPC-002; ADR-0006 §6: the CLI speaks only this protocol and must not
//! initialize GTK or GStreamer).
//!
//! [`IpcClient`] performs the handshake, then multiplexes outbound requests
//! and inbound frames: a background reader task routes `request_response`s to
//! their callers by `request_id` and delivers `event`s to a bounded channel
//! ([`IpcClient::next_event`]). A terminal `closing` frame or a broken
//! connection fails all pending requests with [`ClientError::Closed`].
//!
//! Batches are intentionally not exposed yet (the CLI does not need them);
//! see the follow-ups in `.agent/BACKLOG.yaml`.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use tokio::io::WriteHalf;
use tokio::net::UnixStream;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tracing::{debug, warn};
use uuid::Uuid;

use prismcast_protocol::error::WireError;
use prismcast_protocol::event::EventMessage;
use prismcast_protocol::handshake::{
    AuthResponse, ClientInfo, Hello, Identified, Identify, Permission,
};
use prismcast_protocol::message::{ClientMessage, ServerMessage};
use prismcast_protocol::request::{Request, RequestKind};
use prismcast_protocol::response::{RequestResponse, ResponseData};
use prismcast_protocol::subscription::SubscriptionSet;
use prismcast_protocol::version;

use crate::codec::{self, ClosingNotice, CLOSING_FRAME_TYPE, DEFAULT_MAX_FRAME_SIZE};
use crate::map;
use crate::paths::default_socket_path;

/// Client tuning.
#[derive(Debug, Clone)]
pub struct IpcClientConfig {
    /// Protocol version to request in `Identify`.
    pub protocol_version: u32,
    /// Bearer token for servers configured with token auth.
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
    /// Maximum inbound frame payload.
    pub max_frame_size: usize,
    /// Bound of the inbound event channel; events for a client that does not
    /// drain are dropped (with a warning) rather than blocking the reader.
    pub event_channel_capacity: usize,
}

impl Default for IpcClientConfig {
    fn default() -> Self {
        Self {
            protocol_version: version::PROTOCOL_VERSION,
            token: None,
            subscriptions: None,
            client: None,
            request_timeout: Duration::from_secs(10),
            handshake_timeout: Duration::from_secs(5),
            max_frame_size: DEFAULT_MAX_FRAME_SIZE,
            event_channel_capacity: 256,
        }
    }
}

/// Errors talking to the server.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// Socket I/O failed.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// A frame could not be encoded or decoded.
    #[error("protocol codec error: {0}")]
    Decode(String),
    /// The server closed the session, with the close code it sent (or a
    /// plain EOF before any notice).
    #[error("server closed the session: {reason} (code {code})")]
    Closed {
        /// Numeric close code (see `prismcast_protocol::handshake::CloseCode`).
        code: u16,
        /// Snake-case reason label.
        reason: String,
    },
    /// A request or handshake step exceeded its deadline.
    #[error("timed out waiting for the server")]
    Timeout,
    /// The server answered a request with a structured failure.
    #[error("request failed: {0:?}")]
    RequestFailed(WireError),
}

/// A connected, identified session.
pub struct IpcClient {
    writer: tokio::sync::Mutex<WriteHalf<UnixStream>>,
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<RequestResponse>>>>,
    closed: Arc<Mutex<Option<ClosingNotice>>>,
    events_rx: mpsc::Receiver<EventMessage>,
    reader_task: JoinHandle<()>,
    next_request_id: u64,
    config: IpcClientConfig,
    /// The negotiated protocol version.
    pub negotiated_protocol_version: u32,
    /// The server-assigned session ID.
    pub session_id: Uuid,
    /// The permissions granted to this session.
    pub permissions: Vec<Permission>,
}

impl IpcClient {
    /// Connects to the default socket with default settings.
    pub async fn connect_default() -> Result<Self, ClientError> {
        Self::connect(default_socket_path()).await
    }

    /// Connects to `path` with default settings.
    pub async fn connect(path: impl AsRef<Path>) -> Result<Self, ClientError> {
        Self::connect_with(path, IpcClientConfig::default()).await
    }

    /// Connects and performs the `Hello`/`Identify`/`Identified` handshake.
    pub async fn connect_with(
        path: impl AsRef<Path>,
        config: IpcClientConfig,
    ) -> Result<Self, ClientError> {
        let stream = UnixStream::connect(path.as_ref()).await?;
        let (mut reader, mut writer) = tokio::io::split(stream);

        // --- handshake (before the reader task exists) ---
        let hello_value = read_value(&mut reader, config.max_frame_size, config.handshake_timeout)
            .await?
            .ok_or_else(|| ClientError::Decode("server closed before hello".into()))?;
        let _hello: Hello = decode_data(&hello_value, "hello")?;

        let identify = ClientMessage::Identify(Identify {
            protocol_version: config.protocol_version,
            authentication: config
                .token
                .clone()
                .map(|token| AuthResponse::Token { token }),
            subscriptions: config.subscriptions.clone(),
            client: config.client.clone(),
        });
        write_message(&mut writer, &identify).await?;

        let identified_value =
            read_value(&mut reader, config.max_frame_size, config.handshake_timeout)
                .await?
                .ok_or_else(|| ClientError::Decode("server closed before identified".into()))?;
        if let Some(notice) = closing_notice(&identified_value)? {
            return Err(ClientError::Closed {
                code: notice.code,
                reason: notice.reason,
            });
        }
        let identified: Identified = decode_data(&identified_value, "identified")?;

        // --- steady state: background reader routes frames ---
        let pending: Arc<Mutex<HashMap<String, oneshot::Sender<RequestResponse>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let closed: Arc<Mutex<Option<ClosingNotice>>> = Arc::new(Mutex::new(None));
        let (events_tx, events_rx) = mpsc::channel(config.event_channel_capacity.max(1));
        let reader_task = tokio::spawn(reader_loop(
            reader,
            config.max_frame_size,
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
    pub async fn request(&mut self, kind: RequestKind) -> Result<RequestResponse, ClientError> {
        self.next_request_id += 1;
        let request_id = format!("req-{}", self.next_request_id);
        let (tx, rx) = oneshot::channel();
        self.lock_pending().insert(request_id.clone(), tx);
        let message = ClientMessage::Request(Request {
            request_id: request_id.clone(),
            kind,
        });
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
                Err(ClientError::Timeout)
            }
        }
    }

    /// Like [`request`](Self::request), but unwraps the status: a failed
    /// request becomes [`ClientError::RequestFailed`].
    pub async fn request_data(&mut self, kind: RequestKind) -> Result<ResponseData, ClientError> {
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
            Err(ClientError::RequestFailed(error))
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
    ) -> Result<SubscriptionSet, ClientError> {
        match self
            .request_data(RequestKind::UpdateSubscriptions { subscriptions })
            .await?
        {
            ResponseData::Subscriptions { subscriptions } => Ok(subscriptions),
            other => Err(ClientError::Decode(format!(
                "unexpected update_subscriptions response: {other:?}"
            ))),
        }
    }

    /// Closes the connection.
    pub async fn close(self) {
        self.reader_task.abort();
        let mut writer = self.writer.lock().await;
        let _ = tokio::io::AsyncWriteExt::shutdown(&mut *writer).await;
    }

    fn lock_pending(&self) -> MutexGuard<'_, HashMap<String, oneshot::Sender<RequestResponse>>> {
        self.pending.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn closed_error(&self) -> ClientError {
        let notice = self
            .closed
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        match notice {
            Some(notice) => ClientError::Closed {
                code: notice.code,
                reason: notice.reason,
            },
            None => ClientError::Closed {
                code: 4000,
                reason: "connection_lost".to_string(),
            },
        }
    }
}

impl Drop for IpcClient {
    fn drop(&mut self) {
        self.reader_task.abort();
    }
}

/// The reader task: routes responses to pending callers, events to the event
/// channel, and records terminal closing notices.
async fn reader_loop(
    mut reader: tokio::io::ReadHalf<UnixStream>,
    max_frame_size: usize,
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<RequestResponse>>>>,
    closed: Arc<Mutex<Option<ClosingNotice>>>,
    events_tx: mpsc::Sender<EventMessage>,
) {
    loop {
        let value = match codec::read_frame(&mut reader, max_frame_size).await {
            Ok(Some(payload)) => match codec::decode_value(&payload) {
                Ok(value) => value,
                Err(error) => {
                    debug!(%error, "undecodable frame; closing");
                    break;
                }
            },
            Ok(None) => break, // clean EOF
            Err(error) => {
                debug!(%error, "read failed; closing");
                break;
            }
        };
        match map::value_str(&value, "type") {
            Some(CLOSING_FRAME_TYPE) => {
                let notice = serde_json::from_value::<ClosingNotice>(
                    value
                        .get("data")
                        .cloned()
                        .unwrap_or(serde_json::Value::Null),
                );
                match notice {
                    Ok(notice) => *closed.lock().unwrap_or_else(|p| p.into_inner()) = Some(notice),
                    Err(error) => debug!(%error, "malformed closing notice"),
                }
                break;
            }
            _ => match serde_json::from_value::<ServerMessage>(value) {
                Ok(ServerMessage::RequestResponse(response)) => {
                    let tx = pending
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .remove(&response.request_id);
                    if let Some(tx) = tx {
                        let _ = tx.send(response);
                    } else {
                        debug!(request_id = %response.request_id, "response for unknown request");
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
            },
        }
    }
    // Fail every pending request so callers observe the close.
    for (_, tx) in pending.lock().unwrap_or_else(|p| p.into_inner()).drain() {
        drop(tx);
    }
}

async fn read_value(
    reader: &mut tokio::io::ReadHalf<UnixStream>,
    max_frame_size: usize,
    deadline: Duration,
) -> Result<Option<serde_json::Value>, ClientError> {
    let frame = timeout(deadline, codec::read_frame(reader, max_frame_size))
        .await
        .map_err(|_| ClientError::Timeout)??;
    frame
        .map(|payload| {
            codec::decode_value(&payload).map_err(|e| ClientError::Decode(e.to_string()))
        })
        .transpose()
}

async fn write_message(
    writer: &mut WriteHalf<UnixStream>,
    message: &ClientMessage,
) -> Result<(), ClientError> {
    let payload = codec::encode(message).map_err(|e| ClientError::Decode(e.to_string()))?;
    codec::write_frame(writer, &payload).await?;
    Ok(())
}

/// Extracts `data` from a frame expecting the given `type` tag.
fn decode_data<T: serde::de::DeserializeOwned>(
    value: &serde_json::Value,
    expected: &str,
) -> Result<T, ClientError> {
    if map::value_str(value, "type") != Some(expected) {
        return Err(ClientError::Decode(format!(
            "expected '{expected}' frame, got {:?}",
            map::value_str(value, "type")
        )));
    }
    serde_json::from_value(
        value
            .get("data")
            .cloned()
            .unwrap_or(serde_json::Value::Null),
    )
    .map_err(|e| ClientError::Decode(format!("malformed {expected}: {e}")))
}

/// Returns the closing notice if the frame is a closing frame.
fn closing_notice(value: &serde_json::Value) -> Result<Option<ClosingNotice>, ClientError> {
    if map::value_str(value, "type") != Some(CLOSING_FRAME_TYPE) {
        return Ok(None);
    }
    serde_json::from_value(
        value
            .get("data")
            .cloned()
            .unwrap_or(serde_json::Value::Null),
    )
    .map(Some)
    .map_err(|e| ClientError::Decode(format!("malformed closing notice: {e}")))
}
