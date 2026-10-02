//! obs-websocket 5.x adapter: `BroadcastCustomEvent` → `CustomEvent`
//! (OBSWS-002). A client relays an arbitrary JSON object to every session
//! subscribed to `General` — originator included (it is a broadcast, not a
//! whisper) — over a server-wide bounded bus, in both codecs.
//!
//! The TestBed/raw harness is duplicated from `tests/obs_ws.rs` (deliberate:
//! each protocol test file owns its harness).

use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::SEC_WEBSOCKET_PROTOCOL;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::{Message, Utf8Bytes};
use tokio_tungstenite::WebSocketStream;

use prismcast_app::{AppHandle, CoreConfig};
use prismcast_protocol::handshake::{AuthChallenge, Permission};
use prismcast_remote::auth::{challenge_response, AuthConfig};
use prismcast_remote::obs_ws::proto::{self, subscription};
use prismcast_remote::obs_ws::{ObsWsServer, ObsWsServerConfig};

const TIMEOUT: Duration = Duration::from_secs(5);
/// How long to listen when asserting that *no* frame arrives.
const SILENCE: Duration = Duration::from_millis(300);
const PASSWORD: &str = "hunter2";

struct TestBed {
    app: AppHandle,
    server: ObsWsServer,
    addr: SocketAddr,
}

async fn password_bed() -> TestBed {
    let app = AppHandle::spawn(CoreConfig::default());
    let server = ObsWsServer::bind(
        app.clone(),
        ObsWsServerConfig {
            enabled: true,
            bind: SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
            auth: AuthConfig::password(PASSWORD, vec![Permission::Admin]),
            ..ObsWsServerConfig::default()
        },
    )
    .await
    .expect("bind server");
    let addr = server.local_addr();
    TestBed { app, server, addr }
}

impl TestBed {
    async fn shutdown(self) {
        self.server.shutdown().await;
        self.app.shutdown().await;
    }
}

/// Raw-frame helpers over a bare tungstenite client, in both codecs.
mod raw {
    use super::*;

    pub type RawStream = WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

    pub async fn connect(addr: SocketAddr, subprotocol: Option<&str>) -> RawStream {
        let mut request = format!("ws://{addr}/")
            .into_client_request()
            .expect("request");
        if let Some(subprotocol) = subprotocol {
            request.headers_mut().insert(
                SEC_WEBSOCKET_PROTOCOL,
                HeaderValue::from_str(subprotocol).expect("header value"),
            );
        }
        let (stream, _response) =
            tokio::time::timeout(TIMEOUT, tokio_tungstenite::connect_async(request))
                .await
                .expect("connect timed out")
                .expect("ws connect");
        stream
    }

    pub async fn write_json(stream: &mut RawStream, value: Value) {
        stream
            .send(Message::Text(Utf8Bytes::from(value.to_string())))
            .await
            .expect("write");
    }

    pub async fn read_json(stream: &mut RawStream) -> Value {
        loop {
            match tokio::time::timeout(TIMEOUT, stream.next())
                .await
                .expect("read timed out")
            {
                Some(Ok(Message::Text(text))) => {
                    return serde_json::from_str(&text).expect("json");
                }
                Some(Ok(Message::Ping(_)) | Ok(Message::Pong(_))) => continue,
                other => panic!("expected text frame, got {other:?}"),
            }
        }
    }

    pub async fn write_msgpack(stream: &mut RawStream, value: Value) {
        let payload = rmp_serde::to_vec_named(&value).expect("msgpack encode");
        stream
            .send(Message::Binary(payload.into()))
            .await
            .expect("write");
    }

    pub async fn read_msgpack(stream: &mut RawStream) -> Value {
        loop {
            match tokio::time::timeout(TIMEOUT, stream.next())
                .await
                .expect("read timed out")
            {
                Some(Ok(Message::Binary(payload))) => {
                    return rmp_serde::from_slice(&payload).expect("msgpack decode");
                }
                Some(Ok(Message::Ping(_)) | Ok(Message::Pong(_))) => continue,
                other => panic!("expected binary frame, got {other:?}"),
            }
        }
    }
}

/// A connected, identified obs session (JSON codec). `event_subscriptions`
/// of `None` means the field is omitted (the obs `All` default).
async fn connect_identified(bed: &TestBed, event_subscriptions: Option<u32>) -> raw::RawStream {
    let mut stream = raw::connect(bed.addr, None).await;
    let hello = raw::read_json(&mut stream).await;
    assert_eq!(hello["op"], 0, "first message must be Hello: {hello}");
    let challenge: AuthChallenge =
        serde_json::from_value(hello["d"]["authentication"].clone()).expect("challenge in hello");
    let mut d = json!({
        "rpcVersion": 1,
        "authentication": challenge_response(PASSWORD, &challenge),
    });
    if let Some(mask) = event_subscriptions {
        d["eventSubscriptions"] = mask.into();
    }
    raw::write_json(&mut stream, json!({ "op": 1, "d": d })).await;
    let identified = raw::read_json(&mut stream).await;
    assert_eq!(identified["op"], 2, "expected Identified: {identified}");
    stream
}

/// Sends one `Request` and returns the `RequestResponse` payload `d`.
/// Events (op 5) may interleave with responses; skip them, like a real
/// obs-websocket client must.
async fn request(
    stream: &mut raw::RawStream,
    request_type: &str,
    request_data: Option<Value>,
) -> Value {
    let request_id = uuid::Uuid::new_v4().to_string();
    let mut d = json!({ "requestType": request_type, "requestId": request_id });
    if let Some(request_data) = request_data {
        d["requestData"] = request_data;
    }
    raw::write_json(stream, json!({ "op": 6, "d": d })).await;
    loop {
        let response = raw::read_json(stream).await;
        if response["op"] == 5 {
            continue;
        }
        assert_eq!(response["op"], 7, "expected RequestResponse: {response}");
        assert_eq!(response["d"]["requestType"], request_type);
        assert_eq!(response["d"]["requestId"], request_id);
        return response["d"].clone();
    }
}

/// Reads frames until the next `Event` (op 5) and returns its `d`.
async fn read_event(stream: &mut raw::RawStream) -> Value {
    loop {
        let frame = raw::read_json(stream).await;
        if frame["op"] == 5 {
            return frame["d"].clone();
        }
    }
}

/// Asserts one `CustomEvent` envelope payload: the obs `General` intent bit
/// and the broadcast payload carried verbatim.
fn assert_custom_event(d: &Value, expected_payload: &Value) {
    assert_eq!(d["eventType"], "CustomEvent", "event payload: {d}");
    assert_eq!(
        d["eventIntent"],
        subscription::GENERAL,
        "CustomEvent reports the General bit: {d}"
    );
    assert_eq!(
        &d["eventData"], expected_payload,
        "eventData is carried verbatim: {d}"
    );
}

#[tokio::test]
async fn broadcast_reaches_all_general_subscribers_including_originator() {
    let bed = password_bed().await;
    // Default Identify subscribes to `All`, which includes `General`.
    let mut originator = connect_identified(&bed, None).await;
    let mut bystander = connect_identified(&bed, None).await;

    let payload = json!({
        "emote": "pog",
        "nested": { "count": 3, "flags": [true, false], "nothing": null },
    });
    let d = request(
        &mut originator,
        "BroadcastCustomEvent",
        Some(json!({ "eventData": payload })),
    )
    .await;
    assert_eq!(d["requestStatus"]["code"], 100, "success: {d}");
    assert_eq!(d["requestStatus"]["result"], true);
    assert!(
        d.get("responseData").is_none(),
        "payload-less success, like the other mutation requests: {d}"
    );

    // The originator receives its own event (a broadcast, not a whisper)…
    assert_custom_event(&read_event(&mut originator).await, &payload);
    // …and so does every other General-subscribed session.
    assert_custom_event(&read_event(&mut bystander).await, &payload);

    bed.shutdown().await;
}

#[tokio::test]
async fn sessions_without_the_general_bit_receive_nothing() {
    let bed = password_bed().await;
    let mut originator = connect_identified(&bed, None).await;
    // Scenes only: no General bit.
    let mut scenes_only = connect_identified(&bed, Some(subscription::SCENES)).await;

    let payload = json!({ "kind": "chat_highlight", "user": "worxbend" });
    let d = request(
        &mut originator,
        "BroadcastCustomEvent",
        Some(json!({ "eventData": payload })),
    )
    .await;
    assert_eq!(d["requestStatus"]["code"], 100, "success: {d}");

    // The originator (General via All) gets it…
    assert_custom_event(&read_event(&mut originator).await, &payload);
    // …the unsubscribed session gets nothing at all.
    match tokio::time::timeout(SILENCE, scenes_only.next()).await {
        Err(_) => {}
        Ok(frame) => panic!("expected silence, got {frame:?}"),
    }

    bed.shutdown().await;
}

#[tokio::test]
async fn missing_or_non_object_event_data_is_a_typed_error() {
    let bed = password_bed().await;
    let mut stream = connect_identified(&bed, None).await;

    // No requestData at all → 300 (the drift guard's bare request).
    let d = request(&mut stream, "BroadcastCustomEvent", None).await;
    assert_eq!(d["requestStatus"]["code"], 300, "missing eventData: {d}");
    assert_eq!(d["requestStatus"]["result"], false);
    assert!(d["requestStatus"]["comment"].is_string());

    // Non-object eventData → 401.
    for bad in [json!([1, 2]), json!("pog"), json!(42), json!(null)] {
        let d = request(
            &mut stream,
            "BroadcastCustomEvent",
            Some(json!({ "eventData": bad })),
        )
        .await;
        assert_eq!(
            d["requestStatus"]["code"], 401,
            "eventData must be an object ({bad}): {d}"
        );
    }

    // An empty object is a valid payload.
    let d = request(
        &mut stream,
        "BroadcastCustomEvent",
        Some(json!({ "eventData": {} })),
    )
    .await;
    assert_eq!(d["requestStatus"]["code"], 100, "empty object: {d}");
    assert_custom_event(&read_event(&mut stream).await, &json!({}));

    bed.shutdown().await;
}

#[tokio::test]
async fn msgpack_session_round_trips_custom_events() {
    let bed = password_bed().await;
    let mut stream = raw::connect(bed.addr, Some(proto::SUBPROTOCOL_MSGPACK)).await;

    // Password handshake, all binary frames (default subscriptions = All).
    let hello = raw::read_msgpack(&mut stream).await;
    assert_eq!(hello["op"], 0, "first message must be Hello: {hello}");
    let challenge: AuthChallenge =
        serde_json::from_value(hello["d"]["authentication"].clone()).expect("challenge");
    raw::write_msgpack(
        &mut stream,
        json!({
            "op": 1,
            "d": {"rpcVersion": 1, "authentication": challenge_response(PASSWORD, &challenge)},
        }),
    )
    .await;
    let identified = raw::read_msgpack(&mut stream).await;
    assert_eq!(identified["op"], 2, "expected Identified: {identified}");

    let payload = json!({ "emote": "kappa", "nested": { "streak": 7 } });
    raw::write_msgpack(
        &mut stream,
        json!({
            "op": 6,
            "d": {
                "requestType": "BroadcastCustomEvent",
                "requestId": "mp-custom-1",
                "requestData": { "eventData": payload },
            },
        }),
    )
    .await;

    // The response (100) and the relayed CustomEvent arrive as binary
    // frames; the session enqueues the response before the event.
    let response = raw::read_msgpack(&mut stream).await;
    assert_eq!(response["op"], 7, "expected RequestResponse: {response}");
    assert_eq!(response["d"]["requestId"], "mp-custom-1");
    assert_eq!(response["d"]["requestStatus"]["code"], 100);
    let event = raw::read_msgpack(&mut stream).await;
    assert_eq!(event["op"], 5, "expected Event: {event}");
    assert_custom_event(&event["d"], &payload);

    bed.shutdown().await;
}
