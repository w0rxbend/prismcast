//! The obs-websocket session engine (OBSWS-001; RES-007 §Connection
//! lifecycle), built on [`crate::session_kit`]'s backpressure machinery.
//!
//! Unlike the native session, this engine is WebSocket-only (obs-websocket
//! has no IPC transport), so it drives the tungstenite halves directly; the
//! bounded outbound queue + writer task pattern, the [`RateLimiter`], and
//! [`OverflowStrikes`] slow-consumer shedding come from the shared kit. The
//! kit's writer task itself stays native-typed (`ServerMessage` /
//! `ClosingNotice`) and is not reused here.
//!
//! Lifecycle, mirroring upstream `WebSocketServer.cpp`:
//!
//! 1. Server-first `Hello` (op 0) with versions and, for password auth, the
//!    per-session challenge (the salt/challenge construction is shared with
//!    the native protocol — [`crate::auth::challenge_response`] is exactly
//!    obs's SHA-256 construction).
//! 2. A single `Identify` (op 1) within `handshake_timeout`: `rpcVersion`
//!    must be 1 (else close 4010); authentication verifies through
//!    [`AuthConfig::authenticate`] (else 4009); anything else before
//!    `Identify` closes 4007; a top-level `request-type` field marks a 4.x
//!    client and closes 4010.
//! 3. `Identified` (op 2, `negotiatedRpcVersion` 1).
//! 4. Steady state: `Reidentify` (op 3) updates `eventSubscriptions` only
//!    and is answered with a fresh `Identified` (upstream behavior);
//!    `Request` (op 6) / `RequestBatch` (op 8) are dispatched; a second
//!    `Identify` closes 4008; any other opcode closes 4006.
//!
//! Request translation is the follow-up slice's job: for the foundation,
//! every request is answered with a typed 204 (`UnknownRequestType`) stub.
//! What *is* real: opcode dispatch, envelope mirroring of
//! `requestType`/`requestId`, serial batch execution with `haltOnFailure`
//! and bounded `Sleep`, and the whole-batch 206 answer for execution types
//! 1/2.
//!
//! obs-websocket defines no backpressure policy (RES-007 weakness 5); this
//! engine applies Prismcast's: a bounded outbound queue, drop + strike on
//! overflow, and session shed after consecutive strikes — closed with 4000
//! `UnknownReason` since obs has no slow-consumer code (documented
//! divergence).

use std::sync::Arc;
use std::time::Duration;

use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, watch};
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::protocol::frame::CloseFrame;
use tokio_tungstenite::tungstenite::{Error as TungsteniteError, Message};
use tokio_tungstenite::WebSocketStream;
use tracing::{debug, info, info_span, warn, Instrument};
use uuid::Uuid;

use prismcast_app::broadcaster::StreamEvent;
use prismcast_protocol::handshake::AuthResponse;
use prismcast_protocol::subscription::SubscriptionSet;

use crate::auth::AuthConfig;
use crate::map;
use crate::server::EventFanout;
use crate::session_kit::{OverflowStrikes, RateLimiter};

use super::bitmask;
use super::proto::{self, op, RequestStatus};
use super::translate;

/// Inbound request rate limit (burst per 1 s window), matching the native
/// protocol's budget (protocol doc §1). obs-websocket has no rate limiting;
/// excess requests get a typed 702 response instead of a silent drop.
const REQUEST_BURST: u32 = 200;

/// `Sleep` requests may delay a serial batch by at most this many
/// milliseconds (upstream's `sleepMillis` cap).
const MAX_SLEEP_MILLIS: u64 = 50_000;

/// WebSocket close code used when the server shuts down: RFC 6455
/// `going_away`, matching upstream ("Server stopping.").
const CLOSE_GOING_AWAY: u16 = 1001;

/// Transport-independent obs-session tuning (the server config converts into
/// this).
#[derive(Debug, Clone)]
pub(crate) struct ObsSessionConfig {
    /// Authentication policy applied at `Identify`.
    pub auth: AuthConfig,
    /// Bound of each session's outbound queue.
    pub outbound_capacity: usize,
    /// How long a response enqueue may block before the session is shed.
    pub send_timeout: Duration,
    /// Deadline for the client's `Identify` after connect.
    pub handshake_timeout: Duration,
    /// Maximum inbound message payload in bytes.
    pub max_message_size: usize,
}

/// Everything an obs session task needs besides its WebSocket.
pub(crate) struct ObsSessionContext {
    /// Transport-independent tuning.
    pub config: Arc<ObsSessionConfig>,
    /// Server-wide event fan-out.
    pub fanout: EventFanout,
    /// Shutdown signal from the owning server.
    pub shutdown: watch::Receiver<()>,
}

/// One item in a session's bounded outbound queue: an `{op, d}` envelope or
/// the terminal close frame (code + reason).
enum ObsOutbound {
    Message(serde_json::Value),
    Close(u16, String),
}

/// How the session ends: silently (peer disappeared) or with a WebSocket
/// close frame carrying an obs close code.
enum ObsExit {
    Silent,
    Close(u16, String),
}

impl ObsExit {
    fn close(code: u16, reason: impl Into<String>) -> Self {
        Self::Close(code, reason.into())
    }
}

/// Runs one obs-websocket connection to completion.
pub(crate) async fn run_session(
    stream: WebSocketStream<TcpStream>,
    connection_id: u64,
    context: ObsSessionContext,
) {
    let span = info_span!("obs_session", connection_id);
    run_session_inner(stream, context).instrument(span).await;
}

async fn run_session_inner(stream: WebSocketStream<TcpStream>, context: ObsSessionContext) {
    let ObsSessionContext {
        config,
        fanout,
        mut shutdown,
    } = context;
    let (writer, mut reader) = stream.split();
    let (out_tx, out_rx) = mpsc::channel::<ObsOutbound>(config.outbound_capacity.max(1));
    let mut writer_task = tokio::spawn(run_writer(writer, out_rx));

    let exit = match handshake(&mut reader, &out_tx, &config).await {
        Ok(established) => {
            let session = Session::new(config.clone(), fanout, out_tx.clone(), established);
            session
                .steady_state(&mut reader, &mut shutdown, config.max_message_size)
                .await
        }
        Err(exit) => exit,
    };

    if let ObsExit::Close(code, reason) = &exit {
        debug!(code, %reason, "closing obs session");
        // Best effort: the queue may be exactly what is failing.
        let _ = timeout(
            config.send_timeout,
            out_tx.send(ObsOutbound::Close(*code, reason.clone())),
        )
        .await;
    }
    drop(out_tx);
    // Give the writer a moment to flush the close frame, then stop it.
    if timeout(Duration::from_millis(500), &mut writer_task)
        .await
        .is_err()
    {
        writer_task.abort();
    }
}

/// Socket-writing half of a session: drains the bounded outbound queue; a
/// close item is written as a WebSocket close frame and terminates the task.
/// (Mirrors `session_kit::run_writer`, which stays typed to the native
/// `ServerMessage`/`ClosingNotice`.)
async fn run_writer(
    mut writer: SplitSink<WebSocketStream<TcpStream>, Message>,
    mut rx: mpsc::Receiver<ObsOutbound>,
) {
    while let Some(item) = rx.recv().await {
        let written = match item {
            ObsOutbound::Message(value) => {
                // Serializing a Value is total; a failure here is a bug.
                let text = value.to_string();
                writer.send(Message::Text(text.into())).await
            }
            ObsOutbound::Close(code, reason) => {
                let frame = CloseFrame {
                    code: code.into(),
                    reason: reason.into(),
                };
                let _ = writer.send(Message::Close(Some(frame))).await;
                return;
            }
        };
        if let Err(error) = written {
            debug!(%error, "transport write failed; closing writer");
            return;
        }
    }
}

/// Reads one inbound text frame as a JSON value. `Ok(None)` means the peer
/// closed cleanly; `Err` maps to a `MessageDecodeError` (4002) close.
async fn read_value(
    reader: &mut SplitStream<WebSocketStream<TcpStream>>,
    max_message_size: usize,
) -> Result<Option<serde_json::Value>, ObsExit> {
    loop {
        match reader.next().await {
            None => return Ok(None),
            Some(Ok(Message::Text(text))) => {
                if text.len() > max_message_size {
                    return Err(ObsExit::close(
                        proto::close::MESSAGE_DECODE_ERROR,
                        format!(
                            "message payload {} bytes exceeds limit of {max_message_size}",
                            text.len()
                        ),
                    ));
                }
                return serde_json::from_str(&text).map(Some).map_err(|e| {
                    ObsExit::close(
                        proto::close::MESSAGE_DECODE_ERROR,
                        format!("unable to decode Json: {e}"),
                    )
                });
            }
            Some(Ok(Message::Binary(_))) => {
                return Err(ObsExit::close(
                    proto::close::MESSAGE_DECODE_ERROR,
                    "session encoding is Json, but a binary message was received",
                ));
            }
            Some(Ok(Message::Close(_))) => return Ok(None),
            // Ping/Pong (answered automatically by tungstenite) carry no
            // protocol payload.
            Some(Ok(_)) => continue,
            Some(Err(TungsteniteError::ConnectionClosed))
            | Some(Err(TungsteniteError::AlreadyClosed)) => return Ok(None),
            Some(Err(error)) => {
                return Err(ObsExit::close(
                    proto::close::MESSAGE_DECODE_ERROR,
                    format!("transport read failed: {error}"),
                ));
            }
        }
    }
}

/// Successful handshake result.
struct Established {
    session_id: Uuid,
    subscriptions: SubscriptionSet,
}

/// Server-first handshake: `Hello` (op 0) → `Identify` (op 1) → `Identified`
/// (op 2). Violations close with the obs codes (RES-007 §Close codes).
async fn handshake(
    reader: &mut SplitStream<WebSocketStream<TcpStream>>,
    out_tx: &mpsc::Sender<ObsOutbound>,
    config: &ObsSessionConfig,
) -> Result<Established, ObsExit> {
    let challenge = config.auth.challenge_for_session();
    let hello = proto::Hello {
        obs_web_socket_version: proto::OBS_WEBSOCKET_VERSION.to_string(),
        rpc_version: proto::RPC_VERSION,
        authentication: challenge.clone().map(|c| proto::Authentication {
            challenge: c.challenge,
            salt: c.salt,
        }),
    };
    out_tx
        .send(ObsOutbound::Message(proto::envelope(op::HELLO, &hello)))
        .await
        .map_err(|_| ObsExit::Silent)?;

    let frame = timeout(
        config.handshake_timeout,
        read_value(reader, config.max_message_size),
    )
    .await
    .map_err(|_| ObsExit::close(proto::close::NOT_IDENTIFIED, "identify timeout"))??;
    let Some(value) = frame else {
        return Err(ObsExit::Silent);
    };

    // A top-level `request-type` field marks a pre-5.0.0 (4.x) client
    // (upstream hard rule, RES-007 §Connection lifecycle).
    if value.get("request-type").is_some() {
        return Err(ObsExit::close(
            proto::close::UNSUPPORTED_RPC_VERSION,
            "you appear to be running the pre-5.0.0 plugin protocol",
        ));
    }
    let op_code = value.get("op").and_then(serde_json::Value::as_u64);
    let Some(op_code) = op_code else {
        return Err(ObsExit::close(
            proto::close::UNKNOWN_OPCODE,
            "missing or non-numeric `op`",
        ));
    };
    if op_code != op::IDENTIFY {
        return Err(ObsExit::close(
            proto::close::NOT_IDENTIFIED,
            "the first message must be Identify (op 1)",
        ));
    }
    let data = value.get("d").cloned().unwrap_or(serde_json::Value::Null);
    let identify: proto::Identify = serde_json::from_value(data).map_err(|e| {
        ObsExit::close(
            proto::close::MESSAGE_DECODE_ERROR,
            format!("malformed identify: {e}"),
        )
    })?;

    // Authentication before version negotiation, like upstream.
    let auth_response = identify
        .authentication
        .as_deref()
        .map(|presented| match &config.auth {
            // Prismcast extension: on token-configured servers the obs
            // `authentication` string carries the bearer token itself (obs
            // clients only know the challenge-response construction).
            AuthConfig::Token { .. } => AuthResponse::Token {
                token: presented.to_string(),
            },
            _ => AuthResponse::Challenge {
                response: presented.to_string(),
            },
        });
    let permissions = config
        .auth
        .authenticate(auth_response.as_ref(), challenge.as_ref())
        .ok_or_else(|| {
            ObsExit::close(proto::close::AUTHENTICATION_FAILED, "authentication failed")
        })?;

    if identify.rpc_version != proto::RPC_VERSION {
        return Err(ObsExit::close(
            proto::close::UNSUPPORTED_RPC_VERSION,
            format!(
                "requested rpcVersion {} is not supported (this server speaks {})",
                identify.rpc_version,
                proto::RPC_VERSION
            ),
        ));
    }

    let mask = identify
        .event_subscriptions
        .unwrap_or(proto::subscription::ALL);
    let subscriptions = bitmask::subscription_set_from_bitmask(mask);

    let session_id = Uuid::new_v4();
    out_tx
        .send(ObsOutbound::Message(proto::envelope(
            op::IDENTIFIED,
            &proto::Identified {
                negotiated_rpc_version: proto::RPC_VERSION,
            },
        )))
        .await
        .map_err(|_| ObsExit::Silent)?;

    info!(
        %session_id,
        event_subscriptions = mask,
        granted = ?permissions,
        "obs session identified"
    );
    Ok(Established {
        session_id,
        subscriptions,
    })
}

/// Live session state for the steady-state loop.
struct Session {
    config: Arc<ObsSessionConfig>,
    session_id: Uuid,
    out_tx: mpsc::Sender<ObsOutbound>,
    events: ObsEventPipe,
    rate_limiter: RateLimiter,
}

impl Session {
    fn new(
        config: Arc<ObsSessionConfig>,
        fanout: EventFanout,
        out_tx: mpsc::Sender<ObsOutbound>,
        established: Established,
    ) -> Self {
        Self {
            config,
            session_id: established.session_id,
            out_tx,
            events: ObsEventPipe::new(&fanout, established.subscriptions),
            rate_limiter: RateLimiter::new(REQUEST_BURST),
        }
    }

    /// The steady-state loop: inbound frames, subscribed events, shutdown.
    async fn steady_state(
        mut self,
        reader: &mut SplitStream<WebSocketStream<TcpStream>>,
        shutdown: &mut watch::Receiver<()>,
        max_message_size: usize,
    ) -> ObsExit {
        loop {
            tokio::select! {
                _ = shutdown.changed() => {
                    return ObsExit::close(CLOSE_GOING_AWAY, "server is shutting down");
                }
                frame = read_value(reader, max_message_size) => match frame {
                    Ok(Some(value)) => {
                        if let Some(exit) = self.handle_client_value(value).await {
                            return exit;
                        }
                    }
                    Ok(None) => return ObsExit::Silent,
                    Err(exit) => return exit,
                },
                item = recv_event(&mut self.events.rx) => match item {
                    StreamItem::Item(stream_event) => {
                        if let Err(exit) = self.events.handle(stream_event, &self.out_tx) {
                            return exit;
                        }
                    }
                    // obs has no sequence numbers or resync contract; lag is
                    // logged, not signaled (documented divergence).
                    StreamItem::Lagged(dropped) => {
                        warn!(dropped, "obs session event stream lagged; events were dropped");
                    }
                    StreamItem::Closed => self.events.rx = None,
                },
            }
        }
    }

    /// Handles one decoded inbound frame. Returns `Some` to end the session.
    async fn handle_client_value(&mut self, value: serde_json::Value) -> Option<ObsExit> {
        let op_code = value.get("op").and_then(serde_json::Value::as_u64);
        let Some(op_code) = op_code else {
            return Some(ObsExit::close(
                proto::close::UNKNOWN_OPCODE,
                "missing or non-numeric `op`",
            ));
        };
        let data = value.get("d").cloned().unwrap_or(serde_json::Value::Null);
        match op_code {
            op::IDENTIFY => Some(ObsExit::close(
                proto::close::ALREADY_IDENTIFIED,
                "session is already identified",
            )),
            op::REIDENTIFY => match serde_json::from_value::<proto::Reidentify>(data) {
                Ok(reidentify) => {
                    if let Some(mask) = reidentify.event_subscriptions {
                        self.events
                            .replace_set(bitmask::subscription_set_from_bitmask(mask));
                        debug!(%self.session_id, event_subscriptions = mask, "obs reidentify");
                    }
                    // Upstream answers Reidentify with a fresh Identified.
                    self.send(proto::envelope(
                        op::IDENTIFIED,
                        &proto::Identified {
                            negotiated_rpc_version: proto::RPC_VERSION,
                        },
                    ))
                    .await
                }
                Err(error) => Some(ObsExit::close(
                    proto::close::MESSAGE_DECODE_ERROR,
                    format!("malformed reidentify: {error}"),
                )),
            },
            op::REQUEST => match serde_json::from_value::<proto::Request>(data) {
                Ok(request) => self.handle_request(request).await,
                Err(error) => Some(ObsExit::close(
                    proto::close::MESSAGE_DECODE_ERROR,
                    format!("malformed request: {error}"),
                )),
            },
            op::REQUEST_BATCH => match serde_json::from_value::<proto::RequestBatch>(data) {
                Ok(batch) => self.handle_batch(batch).await,
                Err(error) => Some(ObsExit::close(
                    proto::close::MESSAGE_DECODE_ERROR,
                    format!("malformed request batch: {error}"),
                )),
            },
            other => Some(ObsExit::close(
                proto::close::UNKNOWN_OPCODE,
                format!("unknown opcode: {other}"),
            )),
        }
    }

    /// Answers one request. The opcode dispatch and the response envelope
    /// are real; per-request translation is the follow-up slice's job, so
    /// every type is answered with a typed 204 (`UnknownRequestType`) stub.
    async fn handle_request(&mut self, request: proto::Request) -> Option<ObsExit> {
        debug!(%self.session_id, request_id = %request.request_id, request_type = %request.request_type, "obs request");
        let status = if !self.rate_limiter.check() {
            RequestStatus::error(
                proto::status::REQUEST_PROCESSING_FAILED,
                "request rate limit exceeded",
            )
        } else {
            unknown_request_status(&request.request_type)
        };
        self.send(proto::envelope(
            op::REQUEST_RESPONSE,
            &proto::RequestResponse {
                request_type: request.request_type,
                request_id: request.request_id,
                request_status: status,
                response_data: None,
            },
        ))
        .await
    }

    /// Executes a batch (op 8). `SerialRealtime` runs serially with
    /// `haltOnFailure` and bounded `Sleep`; `SerialFrame`/`Parallel` are
    /// answered with a whole-batch 206 (`UnsupportedRequestBatchExecutionType`)
    /// — the same "the batch did not run" shape upstream uses for batches it
    /// refuses (one result per request, or one when `haltOnFailure`).
    async fn handle_batch(&mut self, batch: proto::RequestBatch) -> Option<ObsExit> {
        debug!(%self.session_id, request_id = %batch.request_id, requests = batch.requests.len(), "obs request batch");
        let execution = match batch.execution_type {
            None => proto::RequestBatchExecutionType::SerialRealtime,
            Some(value) => match proto::RequestBatchExecutionType::from_wire(value) {
                Some(execution) => execution,
                None => {
                    return Some(ObsExit::close(
                        proto::close::INVALID_DATA_FIELD_VALUE,
                        format!("executionType {value} has an invalid value"),
                    ));
                }
            },
        };
        let results = match execution {
            proto::RequestBatchExecutionType::SerialRealtime => self.run_serial_batch(&batch).await,
            proto::RequestBatchExecutionType::SerialFrame
            | proto::RequestBatchExecutionType::Parallel => {
                let unsupported = || {
                    RequestStatus::error(
                        proto::status::UNSUPPORTED_REQUEST_BATCH_EXECUTION_TYPE,
                        format!(
                            "executionType {} ({execution:?}) is not supported by this server",
                            execution.code()
                        ),
                    )
                };
                if batch.halt_on_failure {
                    batch
                        .requests
                        .first()
                        .map(|request| proto::BatchResult {
                            request_type: request.request_type.clone(),
                            request_status: unsupported(),
                            response_data: None,
                        })
                        .into_iter()
                        .collect()
                } else {
                    batch
                        .requests
                        .iter()
                        .map(|request| proto::BatchResult {
                            request_type: request.request_type.clone(),
                            request_status: unsupported(),
                            response_data: None,
                        })
                        .collect()
                }
            }
        };
        self.send(proto::envelope(
            op::REQUEST_BATCH_RESPONSE,
            &proto::RequestBatchResponse {
                request_id: batch.request_id.clone(),
                results,
            },
        ))
        .await
    }

    /// Serial batch body: requests run in order; `haltOnFailure` stops at
    /// the first failed result.
    async fn run_serial_batch(&mut self, batch: &proto::RequestBatch) -> Vec<proto::BatchResult> {
        let mut results = Vec::with_capacity(batch.requests.len());
        for request in &batch.requests {
            let status = if !self.rate_limiter.check() {
                RequestStatus::error(
                    proto::status::REQUEST_PROCESSING_FAILED,
                    "request rate limit exceeded",
                )
            } else if request.request_type == "Sleep" {
                execute_sleep(request.request_data.as_ref()).await
            } else {
                unknown_request_status(&request.request_type)
            };
            let failed = !status.result;
            results.push(proto::BatchResult {
                request_type: request.request_type.clone(),
                request_status: status,
                response_data: None,
            });
            if failed && batch.halt_on_failure {
                break;
            }
        }
        results
    }

    /// Enqueues a response; a persistently blocked queue sheds the session
    /// (obs has no slow-consumer code: 4000 `UnknownReason`).
    async fn send(&self, message: serde_json::Value) -> Option<ObsExit> {
        match timeout(
            self.config.send_timeout,
            self.out_tx.send(ObsOutbound::Message(message)),
        )
        .await
        {
            Ok(Ok(())) => None,
            Ok(Err(_)) => Some(ObsExit::Silent),
            Err(_) => Some(ObsExit::close(
                proto::close::UNKNOWN_REASON,
                "outbound queue blocked",
            )),
        }
    }
}

/// The foundation-slice request stub: a typed 204, never a silent no-op.
fn unknown_request_status(request_type: &str) -> RequestStatus {
    RequestStatus::error(
        proto::status::UNKNOWN_REQUEST_TYPE,
        format!("request type '{request_type}' is not implemented by this server"),
    )
}

/// The batch-only `Sleep` request: real for `SerialRealtime` batches, with
/// upstream's 50 000 ms cap. `sleepFrames` belongs to the unsupported
/// `SerialFrame` execution type.
async fn execute_sleep(data: Option<&serde_json::Value>) -> RequestStatus {
    let field = |data: Option<&serde_json::Value>, key: &str| {
        data.and_then(|d| d.get(key))
            .and_then(serde_json::Value::as_u64)
    };
    let sleep_millis = field(data, "sleepMillis");
    let sleep_frames = field(data, "sleepFrames");
    match (sleep_millis, sleep_frames) {
        (Some(ms), _) if ms > MAX_SLEEP_MILLIS => RequestStatus::error(
            proto::status::REQUEST_FIELD_OUT_OF_RANGE,
            format!("sleepMillis {ms} exceeds the maximum of {MAX_SLEEP_MILLIS}"),
        ),
        (Some(ms), _) => {
            tokio::time::sleep(Duration::from_millis(ms)).await;
            RequestStatus::ok()
        }
        (None, Some(_)) => RequestStatus::error(
            proto::status::INVALID_REQUEST_FIELD,
            "sleepFrames requires SerialFrame execution, which this server does not support",
        ),
        (None, None) => RequestStatus::error(
            proto::status::MISSING_REQUEST_FIELD,
            "Sleep requires a `sleepMillis` field",
        ),
    }
}

/// Per-session event pipeline: the subscription set translated from the obs
/// bitmask, the fan-out receiver, and slow-consumer strike state.
struct ObsEventPipe {
    rx: Option<tokio::sync::broadcast::Receiver<StreamEvent>>,
    fanout: EventFanout,
    set: SubscriptionSet,
    overflow_strikes: OverflowStrikes,
}

impl ObsEventPipe {
    fn new(fanout: &EventFanout, set: SubscriptionSet) -> Self {
        Self {
            rx: (!set.entries.is_empty()).then(|| fanout.subscribe()),
            fanout: fanout.clone(),
            set,
            overflow_strikes: OverflowStrikes::default(),
        }
    }

    /// Atomically swaps the subscription set (`Reidentify`, replacement
    /// semantics).
    fn replace_set(&mut self, set: SubscriptionSet) {
        if set.entries.is_empty() {
            self.rx = None;
        } else if self.rx.is_none() {
            self.rx = Some(self.fanout.subscribe());
        }
        self.set = set;
    }

    /// Gates one domain event by the subscription bitmask and delivers the
    /// translated obs event. On a full outbound queue the event is dropped;
    /// persistent overflow sheds the session (obs has no slow-consumer code:
    /// 4000 `UnknownReason`).
    fn handle(
        &mut self,
        stream_event: StreamEvent,
        out_tx: &mpsc::Sender<ObsOutbound>,
    ) -> Result<(), ObsExit> {
        let event = match stream_event {
            StreamEvent::Lagged { dropped } => {
                warn!(
                    dropped,
                    "obs session event stream lagged; events were dropped"
                );
                return Ok(());
            }
            StreamEvent::Event { event, .. } => event,
        };
        // Category classification reuses the native mapping so the two
        // protocols never disagree on which domain bucket an event is in.
        let category = map::event_to_wire(&event).category();
        if self.set.get(category).is_none() {
            return Ok(());
        }
        let Some(event) = translate::event_to_obs(&event) else {
            return Ok(());
        };
        match out_tx.try_send(ObsOutbound::Message(proto::envelope(op::EVENT, &event))) {
            Ok(()) => {
                self.overflow_strikes.reset();
                Ok(())
            }
            Err(mpsc::error::TrySendError::Full(_)) => {
                let shed = self.overflow_strikes.strike();
                warn!(
                    strikes = self.overflow_strikes.strikes(),
                    "outbound queue full; dropped obs event"
                );
                if shed {
                    Err(ObsExit::close(
                        proto::close::UNKNOWN_REASON,
                        "slow consumer: persistent outbound overflow",
                    ))
                } else {
                    Ok(())
                }
            }
            Err(mpsc::error::TrySendError::Closed(_)) => Err(ObsExit::Silent),
        }
    }
}

/// Result of awaiting the session's event receiver.
enum StreamItem {
    Item(StreamEvent),
    Lagged(u64),
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
