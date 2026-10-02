//! obs-websocket 5.x adapter: the MessagePack subprotocol
//! (`obswebsocket.msgpack`, OBSWS-002; ADR-0021). Golden JSON ↔ MessagePack
//! byte fixtures for every envelope type, a full MessagePack session over a
//! real socket (password handshake, request/batch round-trips, event gating
//! — all binary frames), cross-codec rejection (close 4002), and subprotocol
//! negotiation priority.
//!
//! The TestBed/raw harness is duplicated from `tests/obs_ws.rs` (deliberate:
//! each protocol test file owns its harness).

use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::SEC_WEBSOCKET_PROTOCOL;
use tokio_tungstenite::tungstenite::http::{HeaderValue, StatusCode};
use tokio_tungstenite::tungstenite::{Error as TungsteniteError, Message, Utf8Bytes};
use tokio_tungstenite::WebSocketStream;

use prismcast_app::{AppHandle, CoreConfig};
use prismcast_core::Command;
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

    /// Connects offering `subprotocol` (None = no header); on success returns
    /// the stream and the upgrade response (for subprotocol assertions).
    pub async fn connect_opt(
        addr: SocketAddr,
        subprotocol: Option<&str>,
    ) -> Result<
        (
            RawStream,
            tokio_tungstenite::tungstenite::handshake::client::Response,
        ),
        TungsteniteError,
    > {
        let mut request = format!("ws://{addr}/")
            .into_client_request()
            .expect("request");
        if let Some(subprotocol) = subprotocol {
            request.headers_mut().insert(
                SEC_WEBSOCKET_PROTOCOL,
                HeaderValue::from_str(subprotocol).expect("header value"),
            );
        }
        tokio::time::timeout(TIMEOUT, tokio_tungstenite::connect_async(request))
            .await
            .expect("connect timed out")
    }

    /// Connects a MessagePack session and returns the stream.
    pub async fn connect_msgpack(addr: SocketAddr) -> RawStream {
        connect_opt(addr, Some(proto::SUBPROTOCOL_MSGPACK))
            .await
            .expect("msgpack connect")
            .0
    }

    /// Encodes a value as struct-as-map MessagePack (the wire encoding).
    pub fn msgpack_bytes(value: &Value) -> Vec<u8> {
        rmp_serde::to_vec_named(value).expect("msgpack encode")
    }

    pub async fn write_msgpack(stream: &mut RawStream, value: Value) {
        stream
            .send(Message::Binary(msgpack_bytes(&value).into()))
            .await
            .expect("write");
    }

    pub async fn write_json(stream: &mut RawStream, value: Value) {
        stream
            .send(Message::Text(Utf8Bytes::from(value.to_string())))
            .await
            .expect("write");
    }

    /// Reads the next binary frame as a MessagePack-decoded value; any other
    /// frame kind fails the test (every session frame must be binary).
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

    /// Reads the next text frame as a JSON value.
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

    /// Reads until the server's close frame; returns its code and reason.
    pub async fn read_close(stream: &mut RawStream) -> (u16, String) {
        loop {
            match tokio::time::timeout(TIMEOUT, stream.next())
                .await
                .expect("close timed out")
            {
                Some(Ok(Message::Close(Some(frame)))) => {
                    return (frame.code.into(), frame.reason.to_string());
                }
                Some(Ok(Message::Close(None))) => return (1005, "no_status".into()),
                Some(Ok(_)) => continue,
                Some(Err(error)) => panic!("read failed before close frame: {error}"),
                None => panic!("connection dropped without a close frame"),
            }
        }
    }
}

// --- golden fixtures: JSON string ↔ MessagePack bytes (base64) ---
//
// JSON strings are canonical (serde_json map order); the MessagePack bytes
// are the struct-as-map (`to_vec_named`) encoding of the same envelope —
// exactly what the server puts on the wire. Values mirror the proto.rs
// golden fixtures.

const HELLO_JSON: &str = r#"{"d":{"authentication":{"challenge":"ztTBnnuqrqaKDzRM3xcVdbYm38ZX7L8CMv0cRAKGYFg=","salt":"lM1GncleQOaCu9lT1yeUZhFYnqhsLLP1G5lAGo3ixaI="},"obsWebSocketVersion":"5.7.4","rpcVersion":1},"op":0}"#;
const HELLO_MSGPACK: &str = "gqFkg65hdXRoZW50aWNhdGlvboKpY2hhbGxlbmdl2Sx6dFRCbm51cXJxYUtEelJNM3hjVmRiWW0zOFpYN0w4Q012MGNSQUtHWUZnPaRzYWx02SxsTTFHbmNsZVFPYUN1OWxUMXllVVpoRllucWhzTExQMUc1bEFHbzNpeGFJPbNvYnNXZWJTb2NrZXRWZXJzaW9upTUuNy40qnJwY1ZlcnNpb24Bom9wAA==";

const IDENTIFY_JSON: &str = r#"{"d":{"authentication":"ScJezpEqvLaUbYm7fZOAYGVa+hwkSJeMSkPA/s7nTtE=","eventSubscriptions":33,"rpcVersion":1},"op":1}"#;
const IDENTIFY_MSGPACK: &str = "gqFkg65hdXRoZW50aWNhdGlvbtksU2NKZXpwRXF2TGFVYlltN2ZaT0FZR1ZhK2h3a1NKZU1Ta1BBL3M3blR0RT2yZXZlbnRTdWJzY3JpcHRpb25zIapycGNWZXJzaW9uAaJvcAE=";

const REQUEST_JSON: &str = r#"{"d":{"requestData":{"sceneName":"Main"},"requestId":"f819dcf0-89cc-11eb-8f0d-382c4ac93b9c","requestType":"SetCurrentProgramScene"},"op":6}"#;
const REQUEST_MSGPACK: &str = "gqFkg6tyZXF1ZXN0RGF0YYGpc2NlbmVOYW1lpE1haW6pcmVxdWVzdElk2SRmODE5ZGNmMC04OWNjLTExZWItOGYwZC0zODJjNGFjOTNiOWOrcmVxdWVzdFR5cGW2U2V0Q3VycmVudFByb2dyYW1TY2VuZaJvcAY=";

const REQUEST_RESPONSE_JSON: &str = r#"{"d":{"requestId":"req-1","requestStatus":{"code":100,"result":true},"requestType":"GetVersion","responseData":{"rpcVersion":1}},"op":7}"#;
const REQUEST_RESPONSE_MSGPACK: &str = "gqFkhKlyZXF1ZXN0SWSlcmVxLTGtcmVxdWVzdFN0YXR1c4KkY29kZWSmcmVzdWx0w6tyZXF1ZXN0VHlwZapHZXRWZXJzaW9urHJlc3BvbnNlRGF0YYGqcnBjVmVyc2lvbgGib3AH";

const EVENT_JSON: &str = r#"{"d":{"eventData":{"isGroup":false,"sceneName":"Main"},"eventIntent":4,"eventType":"SceneCreated"},"op":5}"#;
const EVENT_MSGPACK: &str = "gqFkg6lldmVudERhdGGCp2lzR3JvdXDCqXNjZW5lTmFtZaRNYWluq2V2ZW50SW50ZW50BKlldmVudFR5cGWsU2NlbmVDcmVhdGVkom9wBQ==";

const REQUEST_BATCH_JSON: &str = r#"{"d":{"executionType":2,"haltOnFailure":true,"requestId":"batch-1","requests":[{"requestData":{"sleepMillis":100},"requestType":"Sleep"},{"requestType":"GetVersion"}]},"op":8}"#;
const REQUEST_BATCH_MSGPACK: &str = "gqFkhK1leGVjdXRpb25UeXBlAq1oYWx0T25GYWlsdXJlw6lyZXF1ZXN0SWSnYmF0Y2gtMahyZXF1ZXN0c5KCq3JlcXVlc3REYXRhgatzbGVlcE1pbGxpc2SrcmVxdWVzdFR5cGWlU2xlZXCBq3JlcXVlc3RUeXBlqkdldFZlcnNpb26ib3AI";

const REQUEST_BATCH_RESPONSE_JSON: &str = r#"{"d":{"requestId":"batch-1","results":[{"requestStatus":{"code":100,"result":true},"requestType":"Sleep"},{"requestStatus":{"code":204,"comment":"unknown","result":false},"requestType":"GetVersion"}]},"op":9}"#;
const REQUEST_BATCH_RESPONSE_MSGPACK: &str = "gqFkgqlyZXF1ZXN0SWSnYmF0Y2gtMadyZXN1bHRzkoKtcmVxdWVzdFN0YXR1c4KkY29kZWSmcmVzdWx0w6tyZXF1ZXN0VHlwZaVTbGVlcIKtcmVxdWVzdFN0YXR1c4OkY29kZczMp2NvbW1lbnSndW5rbm93bqZyZXN1bHTCq3JlcXVlc3RUeXBlqkdldFZlcnNpb26ib3AJ";

/// Pins one fixture in both directions: the JSON string encodes to exactly
/// the pinned MessagePack bytes, the bytes decode back to exactly the JSON
/// fixture's value (map shape), and decode → re-encode is the identity.
fn assert_fixture(name: &str, json: &str, msgpack_b64: &str) -> Value {
    let value: Value = serde_json::from_str(json).expect("fixture json");
    assert_eq!(value.to_string(), json, "{name}: JSON must be canonical");
    let expected_bytes = BASE64.decode(msgpack_b64).expect("fixture base64");
    let encoded = raw::msgpack_bytes(&value);
    assert_eq!(encoded, expected_bytes, "{name}: msgpack bytes drifted");
    let decoded: Value = rmp_serde::from_slice(&expected_bytes).expect("msgpack decode");
    assert_eq!(decoded, value, "{name}: map shape drifted");
    assert_eq!(
        raw::msgpack_bytes(&decoded),
        expected_bytes,
        "{name}: decode → re-encode identity"
    );
    decoded
}

#[test]
fn hello_golden_msgpack_fixture() {
    assert_fixture("hello", HELLO_JSON, HELLO_MSGPACK);
}

#[test]
fn identify_golden_msgpack_fixture() {
    let value = assert_fixture("identify", IDENTIFY_JSON, IDENTIFY_MSGPACK);
    // eventSubscriptions is a u32 on the wire.
    assert_eq!(value["d"]["eventSubscriptions"].as_u64(), Some(33));
    let identify: proto::Identify =
        serde_json::from_value(value["d"].clone()).expect("typed identify");
    assert_eq!(identify.event_subscriptions, Some(33));
    assert_eq!(identify.rpc_version, proto::RPC_VERSION);
}

#[test]
fn request_golden_msgpack_fixture() {
    let value = assert_fixture("request", REQUEST_JSON, REQUEST_MSGPACK);
    let request: proto::Request =
        serde_json::from_value(value["d"].clone()).expect("typed request");
    assert_eq!(request.request_type, "SetCurrentProgramScene");
}

#[test]
fn request_response_golden_msgpack_fixture() {
    let value = assert_fixture(
        "request_response",
        REQUEST_RESPONSE_JSON,
        REQUEST_RESPONSE_MSGPACK,
    );
    let response: proto::RequestResponse =
        serde_json::from_value(value["d"].clone()).expect("typed response");
    assert_eq!(response.request_status, proto::RequestStatus::ok());
}

#[test]
fn event_golden_msgpack_fixture() {
    let value = assert_fixture("event", EVENT_JSON, EVENT_MSGPACK);
    // eventIntent is a u32 on the wire.
    assert_eq!(value["d"]["eventIntent"].as_u64(), Some(4));
    let event: proto::Event = serde_json::from_value(value["d"].clone()).expect("typed event");
    assert_eq!(event.event_intent, subscription::SCENES);
}

#[test]
fn request_batch_golden_msgpack_fixture() {
    let value = assert_fixture("request_batch", REQUEST_BATCH_JSON, REQUEST_BATCH_MSGPACK);
    // executionType is an i64 on the wire.
    assert_eq!(value["d"]["executionType"].as_i64(), Some(2));
    let batch: proto::RequestBatch =
        serde_json::from_value(value["d"].clone()).expect("typed batch");
    assert_eq!(batch.execution_type, Some(2));
    assert_eq!(batch.requests.len(), 2);
}

#[test]
fn request_batch_response_golden_msgpack_fixture() {
    let value = assert_fixture(
        "request_batch_response",
        REQUEST_BATCH_RESPONSE_JSON,
        REQUEST_BATCH_RESPONSE_MSGPACK,
    );
    let response: proto::RequestBatchResponse =
        serde_json::from_value(value["d"].clone()).expect("typed batch response");
    assert_eq!(response.results.len(), 2);
    assert!(response.results[0].request_status.result);
    assert!(!response.results[1].request_status.result);
}

// --- live msgpack session ---

/// Connects a MessagePack session and performs the password handshake;
/// returns the identified stream (every frame asserted binary).
async fn identify_msgpack(stream: &mut raw::RawStream, event_subscriptions: Option<u32>) -> Value {
    let hello = raw::read_msgpack(stream).await;
    assert_eq!(hello["op"], 0, "first message must be Hello: {hello}");
    let challenge: AuthChallenge =
        serde_json::from_value(hello["d"]["authentication"].clone()).expect("challenge");
    let mut d = json!({
        "rpcVersion": 1,
        "authentication": challenge_response(PASSWORD, &challenge),
    });
    if let Some(mask) = event_subscriptions {
        d["eventSubscriptions"] = mask.into();
    }
    raw::write_msgpack(stream, json!({ "op": 1, "d": d })).await;
    let identified = raw::read_msgpack(stream).await;
    assert_eq!(identified["op"], 2, "expected Identified: {identified}");
    assert_eq!(identified["d"]["negotiatedRpcVersion"], 1);
    identified
}

#[tokio::test]
async fn msgpack_session_request_batch_and_event_gating() {
    let bed = password_bed().await;

    // The subprotocol is echoed at the upgrade.
    let (mut stream, response) = raw::connect_opt(bed.addr, Some(proto::SUBPROTOCOL_MSGPACK))
        .await
        .expect("msgpack connect");
    assert_eq!(
        response.headers().get(SEC_WEBSOCKET_PROTOCOL),
        Some(&HeaderValue::from_static(proto::SUBPROTOCOL_MSGPACK)),
        "msgpack subprotocol echoed"
    );

    // Identify, subscribing to the Scenes category only.
    identify_msgpack(&mut stream, Some(subscription::SCENES)).await;

    // One Request/RequestResponse round-trip, all binary frames.
    raw::write_msgpack(
        &mut stream,
        json!({"op": 6, "d": {"requestType": "GetVersion", "requestId": "mp-1"}}),
    )
    .await;
    let response = raw::read_msgpack(&mut stream).await;
    assert_eq!(response["op"], 7, "expected RequestResponse: {response}");
    assert_eq!(response["d"]["requestId"], "mp-1");
    assert_eq!(response["d"]["requestStatus"]["code"], 100);
    assert_eq!(
        response["d"]["responseData"]["obsWebSocketVersion"],
        proto::OBS_WEBSOCKET_VERSION
    );

    // One serial RequestBatch: a Sleep (succeeds) and a stub (typed 204).
    raw::write_msgpack(
        &mut stream,
        json!({
            "op": 8,
            "d": {
                "requestId": "mp-batch",
                "requests": [
                    {"requestType": "Sleep", "requestData": {"sleepMillis": 10}},
                    {"requestType": "GetStats"},
                ]
            }
        }),
    )
    .await;
    let batch = raw::read_msgpack(&mut stream).await;
    assert_eq!(batch["op"], 9, "expected RequestBatchResponse: {batch}");
    assert_eq!(batch["d"]["requestId"], "mp-batch");
    assert_eq!(batch["d"]["results"][0]["requestStatus"]["code"], 100);
    assert_eq!(batch["d"]["results"][1]["requestStatus"]["code"], 204);

    // Event delivery is gated by the subscription bitmask: a scene add is
    // subscribed (Scenes) and arrives as binary Event frames — the add makes
    // the first scene current, so both obs events fire.
    bed.app
        .dispatch(Command::AddScene {
            name: "MsgpackScene".into(),
        })
        .await
        .expect("add scene");
    let event = raw::read_msgpack(&mut stream).await;
    assert_eq!(event["op"], 5, "expected Event: {event}");
    assert_eq!(event["d"]["eventType"], "SceneCreated");
    assert_eq!(event["d"]["eventIntent"], subscription::SCENES);
    assert_eq!(event["d"]["eventData"]["sceneName"], "MsgpackScene");
    let event = raw::read_msgpack(&mut stream).await;
    assert_eq!(event["op"], 5, "expected Event: {event}");
    assert_eq!(event["d"]["eventType"], "CurrentProgramSceneChanged");
    assert_eq!(event["d"]["eventIntent"], subscription::SCENES);

    // ...while an input add (Inputs bit not subscribed) admits nothing.
    bed.app
        .dispatch(Command::AddSource {
            kind: prismcast_core::source::SourceKind::Color,
            name: "MsgpackInput".into(),
        })
        .await
        .expect("add source");
    match tokio::time::timeout(SILENCE, stream.next()).await {
        Err(_) => {}
        Ok(frame) => panic!("expected silence, got {frame:?}"),
    }

    bed.shutdown().await;
}

// --- cross-codec rejection (close 4002) ---

#[tokio::test]
async fn text_frame_in_msgpack_session_closes_4002() {
    let bed = password_bed().await;
    let mut stream = raw::connect_msgpack(bed.addr).await;
    identify_msgpack(&mut stream, None).await;

    raw::write_json(
        &mut stream,
        json!({"op": 6, "d": {"requestType": "GetVersion", "requestId": "wrong-kind"}}),
    )
    .await;
    let (code, reason) = raw::read_close(&mut stream).await;
    assert_eq!(code, 4002, "text frame in msgpack session");
    assert!(
        reason.contains("MsgPack") && reason.contains("text"),
        "reason names the mismatch: {reason}"
    );

    bed.shutdown().await;
}

#[tokio::test]
async fn binary_frame_in_json_session_closes_4002() {
    let bed = password_bed().await;
    let mut stream = raw::connect_opt(bed.addr, None)
        .await
        .expect("json connect")
        .0;

    // Identify over JSON text (no subprotocol = JSON default).
    let hello = raw::read_json(&mut stream).await;
    assert_eq!(hello["op"], 0, "first message must be Hello: {hello}");
    let challenge: AuthChallenge =
        serde_json::from_value(hello["d"]["authentication"].clone()).expect("challenge");
    raw::write_json(
        &mut stream,
        json!({
            "op": 1,
            "d": {"rpcVersion": 1, "authentication": challenge_response(PASSWORD, &challenge)}
        }),
    )
    .await;
    let identified = raw::read_json(&mut stream).await;
    assert_eq!(identified["op"], 2, "expected Identified: {identified}");

    // A well-formed MessagePack frame is still the wrong kind here.
    let payload = raw::msgpack_bytes(
        &json!({"op": 6, "d": {"requestType": "GetVersion", "requestId": "wrong-kind"}}),
    );
    stream
        .send(Message::Binary(payload.into()))
        .await
        .expect("write");
    let (code, reason) = raw::read_close(&mut stream).await;
    assert_eq!(code, 4002, "binary frame in json session");
    assert!(
        reason.contains("Json") && reason.contains("binary"),
        "reason names the mismatch: {reason}"
    );

    bed.shutdown().await;
}

#[tokio::test]
async fn hostile_msgpack_closes_4002_never_panics() {
    let bed = password_bed().await;

    // Garbage bytes (0xc1 is never valid msgpack).
    let mut stream = raw::connect_msgpack(bed.addr).await;
    identify_msgpack(&mut stream, None).await;
    stream
        .send(Message::Binary(vec![0xc1, 0xc1, 0xc1].into()))
        .await
        .expect("write");
    let (code, reason) = raw::read_close(&mut stream).await;
    assert_eq!(code, 4002, "garbage msgpack");
    assert!(reason.contains("MsgPack"), "reason: {reason}");

    // A msgpack ext type (0xd4 fixext1), not representable as a JSON value.
    let mut stream = raw::connect_msgpack(bed.addr).await;
    identify_msgpack(&mut stream, None).await;
    stream
        .send(Message::Binary(vec![0xd4, 0x01, 0x00].into()))
        .await
        .expect("write");
    let (code, _) = raw::read_close(&mut stream).await;
    assert_eq!(code, 4002, "ext-type msgpack");

    // The server is still healthy afterwards.
    let mut stream = raw::connect_msgpack(bed.addr).await;
    identify_msgpack(&mut stream, None).await;

    bed.shutdown().await;
}

// --- negotiation priority ---

#[tokio::test]
async fn both_subprotocols_offered_negotiate_json() {
    let bed = password_bed().await;

    for offer in [
        "obswebsocket.msgpack, obswebsocket.json",
        "obswebsocket.json, obswebsocket.msgpack",
    ] {
        let (mut stream, response) = raw::connect_opt(bed.addr, Some(offer))
            .await
            .expect("connect");
        assert_eq!(
            response.headers().get(SEC_WEBSOCKET_PROTOCOL),
            Some(&HeaderValue::from_static(proto::SUBPROTOCOL_JSON)),
            "JSON wins when both are offered ({offer})"
        );
        // The session really speaks JSON: the Hello arrives as a text frame.
        let hello = raw::read_json(&mut stream).await;
        assert_eq!(hello["op"], 0, "Hello is JSON text: {hello}");
    }

    bed.shutdown().await;
}

#[tokio::test]
async fn unknown_subprotocols_refuse_the_upgrade_400() {
    let bed = password_bed().await;

    match raw::connect_opt(bed.addr, Some("chat, superchat")).await {
        Err(TungsteniteError::Http(response)) => {
            assert_eq!(response.status(), StatusCode::BAD_REQUEST)
        }
        other => panic!(
            "unknown subprotocols must be refused, got {}",
            other.is_ok()
        ),
    }
    // Unknown tags mixed with a known one still select the known one.
    let (_stream, response) = raw::connect_opt(bed.addr, Some("chat, obswebsocket.msgpack"))
        .await
        .expect("connect");
    assert_eq!(
        response.headers().get(SEC_WEBSOCKET_PROTOCOL),
        Some(&HeaderValue::from_static(proto::SUBPROTOCOL_MSGPACK)),
    );

    bed.shutdown().await;
}

#[tokio::test]
async fn proto_constants_pin_the_msgpack_subprotocol_tag() {
    assert_eq!(proto::SUBPROTOCOL_MSGPACK, "obswebsocket.msgpack");
    assert_eq!(proto::SUBPROTOCOL_JSON, "obswebsocket.json");
}
