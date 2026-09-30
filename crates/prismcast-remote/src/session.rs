//! Per-connection session task (IPC-001; protocol doc §3, §5, §7).
//!
//! One task per connection drives the full lifecycle: server-first `Hello`,
//! single `Identify` with version negotiation and authentication, then a
//! steady-state loop multiplexing inbound frames, the session's event
//! subscription, and throttle-flush timers over a `tokio::select!`.
//!
//! Socket writes live in a separate writer task fed by a **bounded** outbound
//! queue ([`Outbound`]); producers never write to the socket directly.
//!
//! ## Events, throttle, and backpressure (protocol doc §7)
//!
//! - One [`EventStream`](prismcast_app::EventStream) per *server* fans out to
//!   sessions through a bounded `tokio::sync::broadcast` channel (see
//!   [`crate::server::EventFanout`]); category/entity filtering happens
//!   in-session so `update_subscriptions` never has to resubscribe (the
//!   broadcaster has no unsubscribe API) and per-category entity filters —
//!   which `EventFilter` cannot express — work.
//! - `throttle_ms` coalesces per `(category, entity)` key: the first event in
//!   a window is delivered immediately; later ones replace a single pending
//!   slot flushed when the window expires (latest-wins, safe because events
//!   carry full snapshots).
//! - `seq` is per-session, starting at 0 after `identified`. Broadcaster lag
//!   and dropped-on-overflow events consume sequence numbers without sending,
//!   so clients always detect loss as a gap and re-sync with `get_snapshot`.
//! - Outbound overflow: event delivery uses `try_send` (drop + seq gap);
//!   after [`MAX_OVERFLOW_STRIKES`] consecutive failures, or if a response
//!   cannot be enqueued within `send_timeout`, the session is closed with
//!   `SlowConsumer` (4013).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::net::UnixStream;
use tokio::sync::{mpsc, watch};
use tokio::time::{timeout, Instant};
use tracing::{debug, info, info_span, warn, Instrument};
use uuid::Uuid;

use prismcast_app::actor::{AppHandle, HandleError};
use prismcast_app::broadcaster::StreamEvent;
use prismcast_app::dispatch::{Permissions, Query, QueryResponse};
use prismcast_app::snapshot::AppSnapshot;
use prismcast_core::id::{OutputId, SceneId, SourceId};
use prismcast_protocol::batch::{BatchResult, RequestBatch, RequestBatchResponse};
use prismcast_protocol::error::{codes, ErrorKind, WireError};
use prismcast_protocol::event::{EventMessage, WireEvent};
use prismcast_protocol::handshake::{CloseCode, Hello, Identified, Identify};
use prismcast_protocol::message::ServerMessage;
use prismcast_protocol::request::{Request, RequestKind};
use prismcast_protocol::response::{RequestResponse, ResponseData, ResponseStatus};
use prismcast_protocol::subscription::{EventCategory, SubscriptionSet};
use prismcast_protocol::version;

use crate::codec::{self, ClosingNotice};
use crate::map::{self, RequestClass};
use crate::server::{EventFanout, IpcServerConfig};

/// Consecutive outbound-queue overflows tolerated before the session is shed
/// with [`CloseCode::SlowConsumer`].
const MAX_OVERFLOW_STRIKES: u32 = 8;

/// One item in a session's bounded outbound queue.
pub(crate) enum Outbound {
    /// A regular protocol message.
    Message(ServerMessage),
    /// Terminal closing notice; the writer sends it and exits (IPC substitute
    /// for WebSocket close codes, protocol doc §8).
    Close(ClosingNotice),
}

/// How the session ends: silently (peer disappeared) or with a closing
/// notice carrying the close code.
enum SessionExit {
    /// The peer closed the connection or the writer is gone; nothing to send.
    Silent,
    /// Send this notice, then close.
    Notify(ClosingNotice),
}

impl SessionExit {
    fn notify(code: CloseCode, message: impl Into<String>) -> Self {
        Self::Notify(ClosingNotice::new(code, message))
    }
}

/// Runs one connection to completion. Called from the accept loop with
/// `connection_id` for structured logging.
pub(crate) async fn run_session(
    stream: UnixStream,
    app: AppHandle,
    config: Arc<IpcServerConfig>,
    fanout: EventFanout,
    connection_id: u64,
    shutdown: watch::Receiver<()>,
) {
    let span = info_span!("ipc_session", connection_id);
    run_session_inner(stream, app, config, fanout, shutdown)
        .instrument(span)
        .await;
}

async fn run_session_inner(
    stream: UnixStream,
    app: AppHandle,
    config: Arc<IpcServerConfig>,
    fanout: EventFanout,
    mut shutdown: watch::Receiver<()>,
) {
    let (mut reader, writer) = stream.into_split();
    let (out_tx, out_rx) = mpsc::channel::<Outbound>(config.outbound_capacity.max(1));
    let mut writer_task = tokio::spawn(run_writer(writer, out_rx));

    let exit = match handshake(&mut reader, &out_tx, &config).await {
        Ok(established) => {
            let session = Session::new(app, config.clone(), fanout, out_tx.clone(), established);
            session.steady_state(&mut reader, &mut shutdown).await
        }
        Err(exit) => exit,
    };

    if let SessionExit::Notify(notice) = &exit {
        debug!(code = notice.code, reason = %notice.reason, "closing session with notice");
        // Best effort: the queue may be exactly what is failing.
        let _ = timeout(
            config.send_timeout,
            out_tx.send(Outbound::Close(notice.clone())),
        )
        .await;
    }
    drop(out_tx);
    // Give the writer a moment to flush the closing notice, then stop it.
    if timeout(Duration::from_millis(500), &mut writer_task)
        .await
        .is_err()
    {
        writer_task.abort();
    }
}

/// Socket-writing half of a session: drains the bounded outbound queue; a
/// [`Outbound::Close`] item is written and terminates the task.
async fn run_writer(
    mut writer: tokio::net::unix::OwnedWriteHalf,
    mut rx: mpsc::Receiver<Outbound>,
) {
    while let Some(item) = rx.recv().await {
        let closing = matches!(item, Outbound::Close(_));
        let encoded = match &item {
            Outbound::Message(message) => codec::encode(message),
            Outbound::Close(notice) => codec::encode_closing(notice),
        };
        match encoded {
            Ok(bytes) => {
                if let Err(error) = codec::write_frame(&mut writer, &bytes).await {
                    debug!(%error, "socket write failed; closing writer");
                    return;
                }
            }
            // Encoding these types is total; a failure here is a bug, not a
            // client error.
            Err(error) => warn!(%error, "failed to encode outbound message"),
        }
        if closing {
            return;
        }
    }
}

/// Reads one frame and decodes it to a generic value for tag classification.
async fn read_message(
    reader: &mut tokio::net::unix::OwnedReadHalf,
    max_frame_size: usize,
) -> Result<Option<serde_json::Value>, SessionExit> {
    let Some(payload) = codec::read_frame(reader, max_frame_size)
        .await
        .map_err(|e| SessionExit::notify(CloseCode::MessageDecodeError, e.to_string()))?
    else {
        return Ok(None);
    };
    codec::decode_value(&payload)
        .map(Some)
        .map_err(|e| SessionExit::notify(CloseCode::MessageDecodeError, e.to_string()))
}

/// Successful handshake result.
struct Established {
    session_id: Uuid,
    negotiated_protocol_version: u32,
    permissions: Vec<prismcast_protocol::handshake::Permission>,
    subscriptions: SubscriptionSet,
}

/// Server-first handshake (protocol doc §3): `Hello` → `Identify` →
/// `Identified`. Anything but a single well-formed `Identify` closes the
/// session with the corresponding close code.
async fn handshake(
    reader: &mut tokio::net::unix::OwnedReadHalf,
    out_tx: &mpsc::Sender<Outbound>,
    config: &IpcServerConfig,
) -> Result<Established, SessionExit> {
    out_tx
        .send(Outbound::Message(ServerMessage::Hello(Hello {
            prismcast_version: env!("CARGO_PKG_VERSION").to_string(),
            protocol_version: version::PROTOCOL_VERSION,
            min_protocol_version: version::MIN_PROTOCOL_VERSION,
            // Token auth needs no challenge; the challenge-response method is
            // not offered by this server yet.
            authentication: None,
        })))
        .await
        .map_err(|_| SessionExit::Silent)?;

    let frame = timeout(
        config.handshake_timeout,
        read_message(reader, config.max_frame_size),
    )
    .await
    .map_err(|_| SessionExit::notify(CloseCode::NotIdentified, "identify timeout"))??;
    let Some(value) = frame else {
        return Err(SessionExit::Silent);
    };
    if map::value_str(&value, "type") != Some("identify") {
        return Err(SessionExit::notify(
            CloseCode::NotIdentified,
            "the first message must be identify",
        ));
    }
    let identify: Identify = serde_json::from_value(
        value
            .get("data")
            .cloned()
            .unwrap_or(serde_json::Value::Null),
    )
    .map_err(|e| {
        SessionExit::notify(
            CloseCode::MessageDecodeError,
            format!("malformed identify: {e}"),
        )
    })?;

    let negotiated = version::negotiate(
        identify.protocol_version,
        version::MIN_PROTOCOL_VERSION,
        version::PROTOCOL_VERSION,
    )
    .ok_or_else(|| {
        SessionExit::notify(
            CloseCode::UnsupportedProtocolVersion,
            format!(
                "requested protocol version {} is unsupported ({}..={})",
                identify.protocol_version,
                version::MIN_PROTOCOL_VERSION,
                version::PROTOCOL_VERSION
            ),
        )
    })?;

    let permissions = config
        .auth
        .authenticate(identify.authentication.as_ref())
        .ok_or_else(|| {
            SessionExit::notify(CloseCode::AuthenticationFailed, "authentication failed")
        })?;

    let subscriptions = match identify.subscriptions {
        Some(set) => {
            // An invalid initial set cannot be answered with a request error
            // (no request id), so the session is refused with an explanatory
            // closing notice instead of silently clamping (protocol doc §7).
            set.validate().map_err(|e| {
                SessionExit::notify(
                    CloseCode::UnknownReason,
                    format!("invalid subscriptions: {e}"),
                )
            })?;
            set
        }
        None => SubscriptionSet::default_all(),
    };

    let session_id = Uuid::new_v4();
    out_tx
        .send(Outbound::Message(ServerMessage::Identified(Identified {
            negotiated_protocol_version: negotiated,
            session_id,
            permissions: permissions.clone(),
        })))
        .await
        .map_err(|_| SessionExit::Silent)?;

    info!(
        %session_id,
        negotiated_protocol_version = negotiated,
        client = ?identify.client,
        "session identified"
    );
    Ok(Established {
        session_id,
        negotiated_protocol_version: negotiated,
        permissions,
        subscriptions,
    })
}

/// Live session state for the steady-state loop.
struct Session {
    app: AppHandle,
    config: Arc<IpcServerConfig>,
    session_id: Uuid,
    negotiated_protocol_version: u32,
    permissions: Permissions,
    out_tx: mpsc::Sender<Outbound>,
    events: EventPipe,
    rate_limiter: RateLimiter,
}

impl Session {
    fn new(
        app: AppHandle,
        config: Arc<IpcServerConfig>,
        fanout: EventFanout,
        out_tx: mpsc::Sender<Outbound>,
        established: Established,
    ) -> Self {
        let events = EventPipe::new(&fanout, established.subscriptions);
        Self {
            app,
            config,
            session_id: established.session_id,
            negotiated_protocol_version: established.negotiated_protocol_version,
            permissions: map::permissions_to_app(&established.permissions),
            out_tx,
            events,
            rate_limiter: RateLimiter::new(200),
        }
    }

    /// The steady-state loop (protocol doc §3 step 4).
    async fn steady_state(
        mut self,
        reader: &mut tokio::net::unix::OwnedReadHalf,
        shutdown: &mut watch::Receiver<()>,
    ) -> SessionExit {
        loop {
            let deadline = self.events.throttle.next_deadline();
            tokio::select! {
                _ = shutdown.changed() => {
                    return SessionExit::notify(CloseCode::ServerShutdown, "server is shutting down");
                }
                frame = read_message(reader, self.config.max_frame_size) => match frame {
                    Ok(Some(value)) => {
                        if let Some(exit) = self.handle_client_value(value).await {
                            return exit;
                        }
                    }
                    Ok(None) => return SessionExit::Silent,
                    Err(exit) => return exit,
                },
                item = recv_event(&mut self.events.rx) => match item {
                    StreamItem::Item(stream_event) => {
                        if let Err(exit) = self.events.handle(stream_event, &self.out_tx) {
                            return exit;
                        }
                    }
                    StreamItem::Lagged(dropped) => self.events.note_lagged(dropped),
                    StreamItem::Closed => self.events.rx = None,
                },
                _ = sleep_until_opt(deadline) => {
                    if let Err(exit) = self.events.flush(&self.out_tx) {
                        return exit;
                    }
                }
            }
        }
    }

    /// Handles one decoded inbound frame. Returns `Some` to end the session.
    async fn handle_client_value(&mut self, value: serde_json::Value) -> Option<SessionExit> {
        match map::value_str(&value, "type") {
            Some("request") => {
                let data = value
                    .get("data")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                match serde_json::from_value::<Request>(data) {
                    Ok(request) => self.handle_request(request).await,
                    Err(error) => {
                        self.send_response(malformed_request_response(&value, error))
                            .await
                    }
                }
            }
            Some("request_batch") => {
                let data = value
                    .get("data")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                match serde_json::from_value::<RequestBatch>(data) {
                    Ok(batch) => self.handle_batch(batch).await,
                    Err(error) => {
                        let notice = ClosingNotice::new(
                            CloseCode::MessageDecodeError,
                            format!("malformed request_batch: {error}"),
                        );
                        Some(SessionExit::Notify(notice))
                    }
                }
            }
            Some("identify") => Some(SessionExit::notify(
                CloseCode::AlreadyIdentified,
                "session is already identified",
            )),
            other => Some(SessionExit::notify(
                CloseCode::UnknownMessageType,
                format!("unknown message type {}", other.unwrap_or("<missing>")),
            )),
        }
    }

    /// Executes one request and enqueues its response.
    async fn handle_request(&mut self, request: Request) -> Option<SessionExit> {
        let request_type = request.kind.tag().to_string();
        debug!(%self.session_id, request_id = %request.request_id, request_type, "request");
        if !self.rate_limiter.check() {
            let response = RequestResponse {
                request_id: request.request_id,
                request_type,
                status: ResponseStatus::error(WireError::new(
                    ErrorKind::RateLimited,
                    "request rate limit exceeded",
                )),
                data: None,
            };
            return self
                .send_response(ServerMessage::RequestResponse(response))
                .await;
        }
        let (status, data) = match self.execute_request(request.kind).await {
            Ok(data) => (ResponseStatus::ok(), Some(data)),
            Err(error) => (ResponseStatus::error(error), None),
        };
        self.send_response(ServerMessage::RequestResponse(RequestResponse {
            request_id: request.request_id,
            request_type,
            status,
            data,
        }))
        .await
    }

    /// Executes a serial, best-effort batch (protocol doc §6).
    async fn handle_batch(&mut self, batch: RequestBatch) -> Option<SessionExit> {
        let mut results = Vec::with_capacity(batch.requests.len());
        for member in batch.requests {
            let request_type = member.kind.tag().to_string();
            if !self.rate_limiter.check() {
                results.push(BatchResult {
                    request_id: member.request_id,
                    request_type,
                    status: ResponseStatus::error(WireError::new(
                        ErrorKind::RateLimited,
                        "request rate limit exceeded",
                    )),
                    data: None,
                });
                if batch.halt_on_failure {
                    break;
                }
                continue;
            }
            match self.execute_request(member.kind).await {
                Ok(data) => results.push(BatchResult {
                    request_id: member.request_id,
                    request_type,
                    status: ResponseStatus::ok(),
                    data: Some(data),
                }),
                Err(error) => {
                    results.push(BatchResult {
                        request_id: member.request_id,
                        request_type,
                        status: ResponseStatus::error(error),
                        data: None,
                    });
                    if batch.halt_on_failure {
                        break;
                    }
                }
            }
        }
        self.send_response(ServerMessage::RequestBatchResponse(RequestBatchResponse {
            request_id: batch.request_id,
            results,
        }))
        .await
    }

    /// Maps a request to its outcome: commands dispatch into the core actor,
    /// queries read the latest snapshot, session requests mutate the
    /// subscription set.
    async fn execute_request(&mut self, kind: RequestKind) -> Result<ResponseData, WireError> {
        match map::classify(&kind) {
            RequestClass::Command => {
                let tag = kind.tag().to_string();
                let command = map::command_from_wire(kind)?;
                match self
                    .app
                    .dispatch_with_permissions(command, self.permissions)
                    .await
                {
                    Ok(response) => Ok(map::response_data_for(&tag, &response.events)),
                    Err(HandleError::Shutdown) => Err(WireError::new(
                        ErrorKind::NotReady,
                        "core actor is shut down",
                    )),
                    Err(HandleError::Core(error)) => Err(map::wire_error(&error)),
                }
            }
            RequestClass::Query => self.execute_query(&kind),
            RequestClass::Session => self.execute_session_request(kind),
        }
    }

    fn execute_query(&self, kind: &RequestKind) -> Result<ResponseData, WireError> {
        match kind {
            RequestKind::GetVersion => Ok(ResponseData::Version {
                prismcast_version: env!("CARGO_PKG_VERSION").to_string(),
                protocol_version: self.negotiated_protocol_version,
                available_requests: map::AVAILABLE_REQUESTS
                    .iter()
                    .map(|tag| (*tag).to_string())
                    .collect(),
            }),
            RequestKind::GetSnapshot => {
                let snapshot = self.snapshot()?;
                Ok(ResponseData::Snapshot {
                    snapshot: Box::new(map::snapshot_to_wire(&snapshot)),
                })
            }
            RequestKind::ListScenes => match self.query(Query::ListScenes)? {
                QueryResponse::Scenes(scenes) => Ok(ResponseData::SceneList {
                    scenes: scenes.iter().map(map::scene_to_wire).collect(),
                }),
                other => Err(unexpected_response(&other)),
            },
            RequestKind::GetScene { scene_id } => {
                match self.query(Query::GetScene {
                    scene_id: SceneId::from(*scene_id),
                })? {
                    QueryResponse::Scene(Some(scene)) => Ok(ResponseData::Scene {
                        scene: Box::new(map::scene_to_wire(&scene)),
                    }),
                    QueryResponse::Scene(None) => Err(WireError::new(
                        ErrorKind::NotFound,
                        format!("scene {scene_id}"),
                    )
                    .with_field("scene_id")),
                    other => Err(unexpected_response(&other)),
                }
            }
            RequestKind::ListSources => match self.query(Query::ListSources)? {
                QueryResponse::Sources(sources) => Ok(ResponseData::SourceList {
                    sources: sources.iter().map(map::source_to_wire).collect(),
                }),
                other => Err(unexpected_response(&other)),
            },
            RequestKind::GetSource { source_id } => {
                match self.query(Query::GetSource {
                    source_id: SourceId::from(*source_id),
                })? {
                    QueryResponse::Source(Some(source)) => Ok(ResponseData::Source {
                        source: Box::new(map::source_to_wire(&source)),
                    }),
                    QueryResponse::Source(None) => Err(WireError::new(
                        ErrorKind::NotFound,
                        format!("source {source_id}"),
                    )
                    .with_field("source_id")),
                    other => Err(unexpected_response(&other)),
                }
            }
            RequestKind::ListOutputs => match self.query(Query::ListOutputs)? {
                QueryResponse::Outputs(outputs) => Ok(ResponseData::OutputList {
                    outputs: outputs.iter().map(map::output_to_wire).collect(),
                }),
                other => Err(unexpected_response(&other)),
            },
            RequestKind::GetOutput { output_id } => {
                match self.query(Query::GetOutput {
                    output_id: OutputId::from(*output_id),
                })? {
                    QueryResponse::Output(Some(output)) => Ok(ResponseData::Output {
                        output: Box::new(map::output_to_wire(&output)),
                    }),
                    QueryResponse::Output(None) => Err(WireError::new(
                        ErrorKind::NotFound,
                        format!("output {output_id}"),
                    )
                    .with_field("output_id")),
                    other => Err(unexpected_response(&other)),
                }
            }
            RequestKind::GetAudioState => {
                let snapshot = self.snapshot()?;
                Ok(ResponseData::AudioState {
                    audio: map::audio_config_to_wire(&snapshot.state().audio),
                })
            }
            RequestKind::ListProfiles => {
                let snapshot = self.snapshot()?;
                let state = snapshot.state();
                Ok(ResponseData::ProfileList {
                    profiles: state.profiles.values().map(map::profile_to_wire).collect(),
                    active: state.active_profile.map(|id| *id.as_uuid()),
                })
            }
            RequestKind::ListSceneCollections => {
                let snapshot = self.snapshot()?;
                let state = snapshot.state();
                Ok(ResponseData::CollectionList {
                    collections: state
                        .collections
                        .values()
                        .map(map::collection_to_wire)
                        .collect(),
                    active: state.active_collection.map(|id| *id.as_uuid()),
                })
            }
            other => Err(WireError::new(
                ErrorKind::InvalidRequest,
                format!("'{}' is not a query", other.tag()),
            )),
        }
    }

    fn execute_session_request(&mut self, kind: RequestKind) -> Result<ResponseData, WireError> {
        match kind {
            RequestKind::GetSubscriptions => Ok(ResponseData::Subscriptions {
                subscriptions: self.events.set.clone(),
            }),
            RequestKind::UpdateSubscriptions { subscriptions } => {
                subscriptions.validate().map_err(|error| {
                    WireError::new(ErrorKind::InvalidSubscription, error.to_string())
                        .with_details(serde_json::json!({ "error": error.to_string() }))
                })?;
                self.events.replace_set(subscriptions);
                Ok(ResponseData::Subscriptions {
                    subscriptions: self.events.set.clone(),
                })
            }
            other => Err(WireError::new(
                ErrorKind::InvalidRequest,
                format!("'{}' is not a session request", other.tag()),
            )),
        }
    }

    fn query(&self, query: Query) -> Result<QueryResponse, WireError> {
        self.app
            .query(&query, self.permissions)
            .map_err(|error| map::wire_error(&error))
    }

    fn snapshot(&self) -> Result<Arc<AppSnapshot>, WireError> {
        match self.query(Query::GetSnapshot)? {
            QueryResponse::Snapshot(snapshot) => Ok(snapshot),
            other => Err(unexpected_response(&other)),
        }
    }

    /// Enqueues a response; a persistently blocked queue sheds the session
    /// with [`CloseCode::SlowConsumer`] (protocol doc §Backpressure step 3).
    async fn send_response(&self, message: ServerMessage) -> Option<SessionExit> {
        match timeout(
            self.config.send_timeout,
            self.out_tx.send(Outbound::Message(message)),
        )
        .await
        {
            Ok(Ok(())) => None,
            Ok(Err(_)) => Some(SessionExit::Silent),
            Err(_) => Some(SessionExit::notify(
                CloseCode::SlowConsumer,
                "outbound queue blocked",
            )),
        }
    }
}

fn unexpected_response(response: &QueryResponse) -> WireError {
    WireError::new(
        ErrorKind::Internal,
        format!("unexpected query response variant: {response:?}"),
    )
}

/// Builds an error response for an undecodable `request` frame, recovering
/// `request_id`/`request` from the raw value (protocol doc §2: post-identify
/// unknown types yield an error response when a `request_id` is recoverable).
fn malformed_request_response(
    value: &serde_json::Value,
    error: serde_json::Error,
) -> ServerMessage {
    let empty = serde_json::Value::Null;
    let data = value.get("data").unwrap_or(&empty);
    let request_id = map::value_str(data, "request_id").unwrap_or("").to_string();
    let tag = map::value_str(data, "request");
    let wire_error = match tag {
        Some(tag) if !map::is_known_request_tag(tag) => WireError {
            code: codes::UNKNOWN_REQUEST_TYPE,
            ..WireError::new(
                ErrorKind::InvalidRequest,
                format!("unknown request type '{tag}'"),
            )
        },
        _ => WireError::new(
            ErrorKind::InvalidField,
            format!("malformed request: {error}"),
        ),
    };
    ServerMessage::RequestResponse(RequestResponse {
        request_id,
        request_type: tag.unwrap_or("unknown").to_string(),
        status: ResponseStatus::error(wire_error),
        data: None,
    })
}

/// Result of awaiting the session's event receiver.
enum StreamItem {
    /// An event (or broadcaster-lag notice) from the fan-out.
    Item(StreamEvent),
    /// The session's fan-out queue overflowed; `dropped` events were lost.
    Lagged(u64),
    /// The fan-out closed (core actor shutdown).
    Closed,
}

async fn recv_event(rx: &mut Option<tokio::sync::broadcast::Receiver<StreamEvent>>) -> StreamItem {
    match rx {
        Some(rx) => match rx.recv().await {
            Ok(item) => StreamItem::Item(item),
            Err(tokio::sync::broadcast::error::RecvError::Lagged(dropped)) => {
                StreamItem::Lagged(dropped)
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => StreamItem::Closed,
        },
        None => std::future::pending().await,
    }
}

async fn sleep_until_opt(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

/// Coalescing throttle state: minimum delivery interval per
/// `(category, entity)` key with latest-wins pending slots.
#[derive(Default)]
struct Throttle {
    last_sent: HashMap<(EventCategory, Option<Uuid>), Instant>,
    pending: HashMap<(EventCategory, Option<Uuid>), PendingEvent>,
}

struct PendingEvent {
    event: WireEvent,
    deliver_at: Instant,
}

/// Outcome of offering an event to the throttle.
enum ThrottleDecision {
    /// Deliver this event immediately (first in window or window expired).
    DeliverNow(WireEvent),
    /// The event replaced the coalescing slot; flush at `next_deadline()`.
    Deferred,
}

impl Throttle {
    /// Offers an event for throttled delivery.
    fn offer(
        &mut self,
        key: (EventCategory, Option<Uuid>),
        interval: Duration,
        event: WireEvent,
        now: Instant,
    ) -> ThrottleDecision {
        match self.last_sent.get(&key) {
            Some(last) if now < *last + interval => {
                let deliver_at = *last + interval;
                let pending = PendingEvent { event, deliver_at };
                self.pending.insert(key, pending);
                ThrottleDecision::Deferred
            }
            _ => {
                self.last_sent.insert(key, now);
                // A stale pending slot holds an older snapshot of the same
                // entity; the newer event supersedes it.
                self.pending.remove(&key);
                ThrottleDecision::DeliverNow(event)
            }
        }
    }

    /// Drains pending events whose deadline has passed, in deadline order.
    fn take_expired(&mut self, now: Instant) -> Vec<PendingEvent> {
        let expired: Vec<(EventCategory, Option<Uuid>)> = self
            .pending
            .iter()
            .filter(|(_, pending)| pending.deliver_at <= now)
            .map(|(key, _)| *key)
            .collect();
        let mut drained: Vec<PendingEvent> = expired
            .into_iter()
            .filter_map(|key| {
                let pending = self.pending.remove(&key)?;
                self.last_sent.insert(key, now);
                Some(pending)
            })
            .collect();
        drained.sort_by_key(|pending| pending.deliver_at);
        drained
    }

    /// The earliest pending flush deadline.
    fn next_deadline(&self) -> Option<Instant> {
        self.pending
            .values()
            .map(|pending| pending.deliver_at)
            .min()
    }

    fn clear(&mut self) {
        self.last_sent.clear();
        self.pending.clear();
    }
}

/// Per-session event pipeline: subscription set, fan-out receiver, sequence
/// numbering, and throttle state.
struct EventPipe {
    rx: Option<tokio::sync::broadcast::Receiver<StreamEvent>>,
    fanout: EventFanout,
    set: SubscriptionSet,
    next_seq: u64,
    throttle: Throttle,
    overflow_strikes: u32,
}

impl EventPipe {
    fn new(fanout: &EventFanout, set: SubscriptionSet) -> Self {
        Self {
            rx: (!set.entries.is_empty()).then(|| fanout.subscribe()),
            fanout: fanout.clone(),
            set,
            next_seq: 0,
            throttle: Throttle::default(),
            overflow_strikes: 0,
        }
    }

    /// Atomically swaps the subscription set (`update_subscriptions`,
    /// protocol doc §7: replacement semantics). Throttle windows restart.
    fn replace_set(&mut self, set: SubscriptionSet) {
        self.throttle.clear();
        if set.entries.is_empty() {
            self.rx = None;
        } else if self.rx.is_none() {
            self.rx = Some(self.fanout.subscribe());
        }
        self.set = set;
    }

    /// Records a fan-out/broadcaster lag as skipped sequence numbers, so the
    /// client observes a gap and re-syncs (protocol doc §7).
    fn note_lagged(&mut self, dropped: u64) {
        warn!(dropped, "session event stream lagged; signaling seq gap");
        self.next_seq = self.next_seq.saturating_add(dropped);
    }

    /// Filters, throttles, and delivers one domain event.
    fn handle(
        &mut self,
        stream_event: StreamEvent,
        out_tx: &mpsc::Sender<Outbound>,
    ) -> Result<(), SessionExit> {
        let event = match stream_event {
            // The upstream broadcaster's own lag notice.
            StreamEvent::Lagged { dropped } => {
                self.note_lagged(dropped);
                return Ok(());
            }
            StreamEvent::Event { event, .. } => event,
        };
        let wire = map::event_to_wire(&event);
        let category = wire.category();
        let Some(entry) = self.set.get(category) else {
            return Ok(());
        };
        let entity = wire.primary_entity();
        // Entity-restricted subscriptions suppress events about other
        // entities and events without a filterable primary entity.
        if !entry.entity_ids.is_empty() && !entity.is_some_and(|e| entry.entity_ids.contains(&e)) {
            return Ok(());
        }
        match entry.effective_interval_ms() {
            None => self.deliver(wire, out_tx),
            Some(ms) => {
                let now = Instant::now();
                let key = (category, entity);
                match self
                    .throttle
                    .offer(key, Duration::from_millis(u64::from(ms)), wire, now)
                {
                    ThrottleDecision::DeliverNow(wire) => self.deliver(wire, out_tx),
                    ThrottleDecision::Deferred => Ok(()),
                }
            }
        }
    }

    /// Flushes expired throttle-pending events.
    fn flush(&mut self, out_tx: &mpsc::Sender<Outbound>) -> Result<(), SessionExit> {
        let now = Instant::now();
        for pending in self.throttle.take_expired(now) {
            self.deliver(pending.event, out_tx)?;
        }
        Ok(())
    }

    /// Assigns the next sequence number and enqueues the event. On a full
    /// outbound queue the event is dropped but its sequence number is
    /// consumed (client sees a gap and re-syncs); persistent overflow sheds
    /// the session with [`CloseCode::SlowConsumer`].
    fn deliver(
        &mut self,
        wire: WireEvent,
        out_tx: &mpsc::Sender<Outbound>,
    ) -> Result<(), SessionExit> {
        let seq = self.next_seq;
        self.next_seq = self.next_seq.saturating_add(1);
        let category = wire.category();
        let message = ServerMessage::Event(EventMessage {
            seq,
            category,
            event: wire,
        });
        match out_tx.try_send(Outbound::Message(message)) {
            Ok(()) => {
                self.overflow_strikes = 0;
                Ok(())
            }
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.overflow_strikes += 1;
                warn!(
                    seq,
                    strikes = self.overflow_strikes,
                    "outbound queue full; dropped event"
                );
                if self.overflow_strikes >= MAX_OVERFLOW_STRIKES {
                    Err(SessionExit::notify(
                        CloseCode::SlowConsumer,
                        "persistent outbound overflow",
                    ))
                } else {
                    Ok(())
                }
            }
            Err(mpsc::error::TrySendError::Closed(_)) => Err(SessionExit::Silent),
        }
    }
}

/// Fixed-window inbound request rate limiter (protocol doc §1: 100 req/s,
/// burst 200; excess → `rate_limited` error responses).
struct RateLimiter {
    window_start: Instant,
    count: u32,
    burst: u32,
}

impl RateLimiter {
    fn new(burst: u32) -> Self {
        Self {
            window_start: Instant::now(),
            count: 0,
            burst,
        }
    }

    /// Whether the request may proceed.
    fn check(&mut self) -> bool {
        let now = Instant::now();
        if now.duration_since(self.window_start) >= Duration::from_secs(1) {
            self.window_start = now;
            self.count = 0;
        }
        self.count += 1;
        self.count <= self.burst
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use prismcast_protocol::subscription::EventCategory as Cat;

    fn wire_scene_event(name: &str) -> WireEvent {
        WireEvent::Scene(prismcast_protocol::event::SceneEvent::Added {
            scene_id: Uuid::new_v4(),
            name: name.into(),
        })
    }

    fn assert_deliver_now(decision: ThrottleDecision) {
        assert!(matches!(decision, ThrottleDecision::DeliverNow(_)));
    }

    fn assert_deferred(decision: ThrottleDecision) {
        assert!(matches!(decision, ThrottleDecision::Deferred));
    }

    #[test]
    fn throttle_delivers_first_event_immediately() {
        let mut throttle = Throttle::default();
        let now = Instant::now();
        let key = (Cat::Scene, Some(Uuid::new_v4()));
        assert_deliver_now(throttle.offer(
            key,
            Duration::from_millis(100),
            wire_scene_event("a"),
            now,
        ));
        assert!(throttle.next_deadline().is_none());
    }

    #[test]
    fn throttle_coalesces_within_window_latest_wins() {
        let mut throttle = Throttle::default();
        let now = Instant::now();
        let key = (Cat::Scene, Some(Uuid::new_v4()));
        let interval = Duration::from_millis(100);
        assert_deliver_now(throttle.offer(key, interval, wire_scene_event("first"), now));
        assert_deferred(throttle.offer(
            key,
            interval,
            wire_scene_event("second"),
            now + Duration::from_millis(10),
        ));
        assert_eq!(throttle.next_deadline(), Some(now + interval));
        assert_deferred(throttle.offer(
            key,
            interval,
            wire_scene_event("third"),
            now + Duration::from_millis(20),
        ));
        assert_eq!(
            throttle.next_deadline(),
            Some(now + interval),
            "same window, same deadline"
        );

        // Nothing expires inside the window.
        assert!(throttle
            .take_expired(now + Duration::from_millis(50))
            .is_empty());
        let expired = throttle.take_expired(now + interval);
        assert_eq!(expired.len(), 1, "coalesced to a single latest event");
        assert!(matches!(
            &expired[0].event,
            WireEvent::Scene(prismcast_protocol::event::SceneEvent::Added { name, .. }) if name == "third"
        ));
        // After flushing at the window's end, the window restarts at the
        // flush moment (fixed cadence): an event right after is deferred,
        // one past the new window delivers immediately.
        assert_deferred(throttle.offer(
            key,
            interval,
            wire_scene_event("fourth"),
            now + interval + Duration::from_millis(1),
        ));
        assert_eq!(throttle.take_expired(now + 2 * interval).len(), 1);
        // The flush at t = now+2i is itself a delivery; an event a full
        // window later delivers immediately.
        assert_deliver_now(throttle.offer(
            key,
            interval,
            wire_scene_event("fifth"),
            now + 3 * interval,
        ));
    }

    #[test]
    fn throttle_windows_are_per_entity() {
        let mut throttle = Throttle::default();
        let now = Instant::now();
        let interval = Duration::from_millis(100);
        let a = (Cat::Scene, Some(Uuid::new_v4()));
        let b = (Cat::Scene, Some(Uuid::new_v4()));
        assert_deliver_now(throttle.offer(a, interval, wire_scene_event("a1"), now));
        assert_deliver_now(throttle.offer(b, interval, wire_scene_event("b1"), now));
        assert_deferred(throttle.offer(
            a,
            interval,
            wire_scene_event("a2"),
            now + Duration::from_millis(10),
        ));
        assert_deferred(throttle.offer(
            b,
            interval,
            wire_scene_event("b2"),
            now + Duration::from_millis(10),
        ));
        assert_eq!(throttle.take_expired(now + interval).len(), 2);
    }

    #[test]
    fn entityless_events_throttle_under_their_own_key() {
        let mut throttle = Throttle::default();
        let now = Instant::now();
        let key = (Cat::System, None);
        let interval = Duration::from_millis(50);
        assert_deliver_now(throttle.offer(
            key,
            interval,
            WireEvent::System(prismcast_protocol::event::SystemEvent::StudioModeChanged {
                enabled: true,
            }),
            now,
        ));
        assert_deferred(throttle.offer(
            key,
            interval,
            WireEvent::System(prismcast_protocol::event::SystemEvent::StudioModeChanged {
                enabled: false,
            }),
            now,
        ));
    }

    #[test]
    fn rate_limiter_allows_burst_then_rejects() {
        let mut limiter = RateLimiter::new(3);
        assert!(limiter.check());
        assert!(limiter.check());
        assert!(limiter.check());
        assert!(!limiter.check());
    }
}
