//! obs-websocket 5.x adapter: real-socket handshake matrix, auth, subprotocol
//! negotiation, request-stub dispatch, Reidentify, and RequestBatch
//! scaffolding (OBSWS-001 foundation slice). Modeled on `tests/auth_ws.rs`.

use std::net::{Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::SEC_WEBSOCKET_PROTOCOL;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::{Error as TungsteniteError, Message, Utf8Bytes};
use tokio_tungstenite::WebSocketStream;

use prismcast_app::{AppHandle, CoreConfig};
use prismcast_protocol::handshake::{AuthChallenge, Permission};
use prismcast_remote::auth::{challenge_response, AuthConfig};
use prismcast_remote::obs_ws::proto;
use prismcast_remote::obs_ws::{ObsWsError, ObsWsServer, ObsWsServerConfig};

const TIMEOUT: Duration = Duration::from_secs(5);
const PASSWORD: &str = "hunter2";
const TOKEN: &str = "s3cret";

struct TestBed {
    app: AppHandle,
    server: ObsWsServer,
    addr: SocketAddr,
}

async fn spawn_bed(auth: AuthConfig) -> TestBed {
    let app = AppHandle::spawn(CoreConfig::default());
    let server = ObsWsServer::bind(
        app.clone(),
        ObsWsServerConfig {
            enabled: true,
            bind: SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
            auth,
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

async fn password_bed() -> TestBed {
    spawn_bed(AuthConfig::password(PASSWORD, vec![Permission::Admin])).await
}

/// Raw-frame helpers over a bare tungstenite client.
mod raw {
    use super::*;

    pub type RawStream = WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

    /// Connects without offering a subprotocol.
    pub async fn connect(addr: SocketAddr) -> RawStream {
        connect_opt(addr, None).await.expect("ws connect").0
    }

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

    pub async fn write_value(stream: &mut RawStream, value: serde_json::Value) {
        stream
            .send(Message::Text(Utf8Bytes::from(value.to_string())))
            .await
            .expect("write");
    }

    /// Reads the next text frame as a JSON value (skips ping/pong).
    pub async fn read_value(stream: &mut RawStream) -> serde_json::Value {
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

/// Reads the server-first `Hello` and returns it.
async fn read_hello(stream: &mut raw::RawStream) -> serde_json::Value {
    let hello = raw::read_value(stream).await;
    assert_eq!(hello["op"], 0, "first message must be Hello: {hello}");
    hello
}

/// Sends an `Identify` (op 1) with the given fields.
async fn send_identify(
    stream: &mut raw::RawStream,
    rpc_version: i64,
    authentication: Option<String>,
    event_subscriptions: Option<u32>,
) {
    let mut d = serde_json::json!({ "rpcVersion": rpc_version });
    if let Some(authentication) = authentication {
        d["authentication"] = authentication.into();
    }
    if let Some(event_subscriptions) = event_subscriptions {
        d["eventSubscriptions"] = event_subscriptions.into();
    }
    raw::write_value(stream, serde_json::json!({ "op": 1, "d": d })).await;
}

/// Completes the obs handshake with password auth and returns the
/// `Identified` payload.
async fn identify_with_password(stream: &mut raw::RawStream, password: &str) -> serde_json::Value {
    let hello = read_hello(stream).await;
    let challenge: AuthChallenge =
        serde_json::from_value(hello["d"]["authentication"].clone()).expect("challenge in hello");
    let response = challenge_response(password, &challenge);
    send_identify(stream, 1, Some(response), None).await;
    let identified = raw::read_value(stream).await;
    assert_eq!(identified["op"], 2, "expected Identified: {identified}");
    assert_eq!(identified["d"]["negotiatedRpcVersion"], 1);
    identified
}

#[tokio::test]
async fn bind_gating_disabled_and_allow_local() {
    let app = AppHandle::spawn(CoreConfig::default());
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
    let none = ObsWsServer::bind_if_enabled(app.clone(), ObsWsServerConfig::default())
        .await
        .expect("bind_if_enabled");
    assert!(none.is_none());
    app.shutdown().await;
}

#[tokio::test]
async fn hello_shape_versions_and_challenge() {
    let bed = password_bed().await;

    let mut first = raw::connect(bed.addr).await;
    let hello_a = read_hello(&mut first).await;
    let mut second = raw::connect(bed.addr).await;
    let hello_b = read_hello(&mut second).await;

    assert_eq!(hello_a["d"]["obsWebSocketVersion"], "5.7.4");
    assert_eq!(hello_a["d"]["rpcVersion"], 1);
    assert!(
        hello_a["d"].get("obsStudioVersion").is_none(),
        "no OBS version is advertised: {hello_a}"
    );
    let salt_a = hello_a["d"]["authentication"]["salt"]
        .as_str()
        .expect("salt");
    let salt_b = hello_b["d"]["authentication"]["salt"]
        .as_str()
        .expect("salt");
    let challenge_a = hello_a["d"]["authentication"]["challenge"]
        .as_str()
        .expect("challenge");
    let challenge_b = hello_b["d"]["authentication"]["challenge"]
        .as_str()
        .expect("challenge");
    assert_eq!(salt_a, salt_b, "salt is stable per server start");
    assert_ne!(challenge_a, challenge_b, "challenge differs per session");
    assert_eq!(salt_a.len(), 44);
    assert_eq!(challenge_a.len(), 44);

    bed.shutdown().await;
}

#[tokio::test]
async fn token_hello_carries_no_challenge() {
    let bed = spawn_bed(AuthConfig::token(TOKEN, vec![Permission::Admin])).await;
    let mut stream = raw::connect(bed.addr).await;
    let hello = read_hello(&mut stream).await;
    assert!(
        hello["d"].get("authentication").is_none(),
        "token hello must not offer a challenge: {hello}"
    );
    bed.shutdown().await;
}

#[tokio::test]
async fn password_identify_succeeds() {
    let bed = password_bed().await;
    let mut stream = raw::connect(bed.addr).await;
    identify_with_password(&mut stream, PASSWORD).await;
    bed.shutdown().await;
}

#[tokio::test]
async fn wrong_or_missing_password_closes_4009() {
    let bed = password_bed().await;

    let mut stream = raw::connect(bed.addr).await;
    let hello = read_hello(&mut stream).await;
    let challenge: AuthChallenge =
        serde_json::from_value(hello["d"]["authentication"].clone()).expect("challenge");
    let wrong = challenge_response("wrong", &challenge);
    send_identify(&mut stream, 1, Some(wrong), None).await;
    let (code, _) = raw::read_close(&mut stream).await;
    assert_eq!(code, 4009, "wrong password");

    let mut stream = raw::connect(bed.addr).await;
    let _hello = read_hello(&mut stream).await;
    send_identify(&mut stream, 1, None, None).await;
    let (code, _) = raw::read_close(&mut stream).await;
    assert_eq!(code, 4009, "missing authentication");

    bed.shutdown().await;
}

#[tokio::test]
async fn token_policy_takes_the_authentication_string_as_token() {
    let bed = spawn_bed(AuthConfig::token(TOKEN, vec![Permission::Admin])).await;

    let mut stream = raw::connect(bed.addr).await;
    let _hello = read_hello(&mut stream).await;
    send_identify(&mut stream, 1, Some(TOKEN.into()), None).await;
    let identified = raw::read_value(&mut stream).await;
    assert_eq!(
        identified["op"], 2,
        "correct token identifies: {identified}"
    );

    let mut stream = raw::connect(bed.addr).await;
    let _hello = read_hello(&mut stream).await;
    send_identify(&mut stream, 1, Some("wrong".into()), None).await;
    let (code, _) = raw::read_close(&mut stream).await;
    assert_eq!(code, 4009, "wrong token");

    bed.shutdown().await;
}

#[tokio::test]
async fn pre_identify_traffic_closes_4007_and_missing_op_4006() {
    let bed = password_bed().await;

    // A Request before Identify.
    let mut stream = raw::connect(bed.addr).await;
    let _hello = read_hello(&mut stream).await;
    raw::write_value(
        &mut stream,
        serde_json::json!({"op": 6, "d": {"requestType": "GetVersion", "requestId": "x"}}),
    )
    .await;
    let (code, _) = raw::read_close(&mut stream).await;
    assert_eq!(code, 4007, "request before identify");

    // No `op` field at all.
    let mut stream = raw::connect(bed.addr).await;
    let _hello = read_hello(&mut stream).await;
    raw::write_value(&mut stream, serde_json::json!({"d": {"rpcVersion": 1}})).await;
    let (code, _) = raw::read_close(&mut stream).await;
    assert_eq!(code, 4006, "missing op");

    // A 4.x client probe (top-level request-type).
    let mut stream = raw::connect(bed.addr).await;
    let _hello = read_hello(&mut stream).await;
    raw::write_value(
        &mut stream,
        serde_json::json!({"request-type": "GetVersion", "message-id": "1"}),
    )
    .await;
    let (code, _) = raw::read_close(&mut stream).await;
    assert_eq!(code, 4010, "4.x client probe");

    bed.shutdown().await;
}

#[tokio::test]
async fn unsupported_rpc_version_closes_4010() {
    let bed = password_bed().await;
    let mut stream = raw::connect(bed.addr).await;
    let hello = read_hello(&mut stream).await;
    let challenge: AuthChallenge =
        serde_json::from_value(hello["d"]["authentication"].clone()).expect("challenge");
    let response = challenge_response(PASSWORD, &challenge);
    send_identify(&mut stream, 2, Some(response), None).await;
    let (code, _) = raw::read_close(&mut stream).await;
    assert_eq!(code, 4010);
    bed.shutdown().await;
}

#[tokio::test]
async fn second_identify_closes_4008_and_unknown_opcode_4006() {
    let bed = password_bed().await;

    let mut stream = raw::connect(bed.addr).await;
    identify_with_password(&mut stream, PASSWORD).await;
    send_identify(&mut stream, 1, None, None).await;
    let (code, _) = raw::read_close(&mut stream).await;
    assert_eq!(code, 4008, "second identify");

    for unknown_op in [4, 5, 7, 9, 99] {
        let mut stream = raw::connect(bed.addr).await;
        identify_with_password(&mut stream, PASSWORD).await;
        raw::write_value(&mut stream, serde_json::json!({"op": unknown_op, "d": {}})).await;
        let (code, _) = raw::read_close(&mut stream).await;
        assert_eq!(code, 4006, "opcode {unknown_op} post-identify");
    }

    bed.shutdown().await;
}

#[tokio::test]
async fn subprotocol_negotiation() {
    let bed = password_bed().await;

    // obswebsocket.json is echoed.
    let (_stream, response) = raw::connect_opt(bed.addr, Some("obswebsocket.json"))
        .await
        .expect("json connect");
    assert_eq!(
        response.headers().get(SEC_WEBSOCKET_PROTOCOL),
        Some(&HeaderValue::from_static("obswebsocket.json")),
        "json subprotocol echoed"
    );

    // No subprotocol: accepted, no subprotocol selected (JSON default).
    let (mut stream, response) = raw::connect_opt(bed.addr, None).await.expect("connect");
    assert!(response.headers().get(SEC_WEBSOCKET_PROTOCOL).is_none());
    let hello = read_hello(&mut stream).await;
    assert_eq!(hello["d"]["rpcVersion"], 1);

    // MessagePack-only: accepted and echoed (OBSWS-002; full msgpack
    // behavior is pinned in tests/obs_ws_msgpack.rs).
    let (_stream, response) = raw::connect_opt(bed.addr, Some("obswebsocket.msgpack"))
        .await
        .expect("msgpack connect");
    assert_eq!(
        response.headers().get(SEC_WEBSOCKET_PROTOCOL),
        Some(&HeaderValue::from_static("obswebsocket.msgpack")),
        "msgpack subprotocol echoed"
    );

    // An unrelated subprotocol set is refused too.
    let refused = raw::connect_opt(bed.addr, Some("chat, superchat")).await;
    assert!(matches!(refused, Err(TungsteniteError::Http(_))));

    bed.shutdown().await;
}

/// Sends one `Request` and returns the `RequestResponse` payload `d`.
async fn roundtrip_request(
    stream: &mut raw::RawStream,
    request_type: &str,
    request_id: &str,
) -> serde_json::Value {
    raw::write_value(
        stream,
        serde_json::json!({
            "op": 6,
            "d": { "requestType": request_type, "requestId": request_id }
        }),
    )
    .await;
    let response = raw::read_value(stream).await;
    assert_eq!(response["op"], 7, "expected RequestResponse: {response}");
    response["d"].clone()
}

#[tokio::test]
async fn requests_get_typed_204_stub_mirroring_type_and_id() {
    let bed = password_bed().await;
    let mut stream = raw::connect(bed.addr).await;
    identify_with_password(&mut stream, PASSWORD).await;

    let d = roundtrip_request(
        &mut stream,
        "GetStats",
        "f819dcf0-89cc-11eb-8f0d-382c4ac93b9c",
    )
    .await;
    assert_eq!(d["requestType"], "GetStats");
    assert_eq!(d["requestId"], "f819dcf0-89cc-11eb-8f0d-382c4ac93b9c");
    assert_eq!(d["requestStatus"]["result"], false);
    assert_eq!(d["requestStatus"]["code"], 204);
    assert!(d["requestStatus"]["comment"].is_string());
    assert!(d.get("responseData").is_none());

    bed.shutdown().await;
}

#[tokio::test]
async fn reidentify_updates_subscriptions_and_replies_identified() {
    let bed = password_bed().await;
    let mut stream = raw::connect(bed.addr).await;
    identify_with_password(&mut stream, PASSWORD).await;

    // Reidentify with a new bitmask is answered with a fresh Identified.
    raw::write_value(
        &mut stream,
        serde_json::json!({"op": 3, "d": {"eventSubscriptions": 0}}),
    )
    .await;
    let identified = raw::read_value(&mut stream).await;
    assert_eq!(identified["op"], 2, "Reidentify answered: {identified}");
    assert_eq!(identified["d"]["negotiatedRpcVersion"], 1);

    // A Reidentify without eventSubscriptions leaves the session alive too.
    raw::write_value(&mut stream, serde_json::json!({"op": 3, "d": {}})).await;
    let identified = raw::read_value(&mut stream).await;
    assert_eq!(identified["op"], 2);

    // The session is fully usable afterwards.
    let d = roundtrip_request(&mut stream, "GetStats", "after-reidentify").await;
    assert_eq!(d["requestStatus"]["code"], 204);

    bed.shutdown().await;
}

/// Sends a `RequestBatch` and returns the `RequestBatchResponse` payload `d`.
async fn roundtrip_batch(
    stream: &mut raw::RawStream,
    batch: serde_json::Value,
) -> serde_json::Value {
    raw::write_value(stream, serde_json::json!({"op": 8, "d": batch})).await;
    let response = raw::read_value(stream).await;
    assert_eq!(
        response["op"], 9,
        "expected RequestBatchResponse: {response}"
    );
    assert_eq!(response["d"]["requestId"], batch["requestId"]);
    response["d"].clone()
}

fn stub_request(request_type: &str) -> serde_json::Value {
    serde_json::json!({"requestType": request_type})
}

#[tokio::test]
async fn serial_batch_preserves_request_order() {
    let bed = password_bed().await;
    let mut stream = raw::connect(bed.addr).await;
    identify_with_password(&mut stream, PASSWORD).await;

    let d = roundtrip_batch(
        &mut stream,
        serde_json::json!({
            "requestId": "batch-order",
            "requests": [
                stub_request("GetStats"),
                stub_request("GetProfileList"),
                stub_request("GetSceneCollectionList"),
            ]
        }),
    )
    .await;
    let results = d["results"].as_array().expect("results array");
    assert_eq!(results.len(), 3);
    let types: Vec<&str> = results
        .iter()
        .map(|r| r["requestType"].as_str().expect("requestType"))
        .collect();
    assert_eq!(
        types,
        ["GetStats", "GetProfileList", "GetSceneCollectionList"]
    );
    for result in results {
        assert_eq!(result["requestStatus"]["code"], 204);
        assert_eq!(result["requestStatus"]["result"], false);
    }

    bed.shutdown().await;
}

#[tokio::test]
async fn halt_on_failure_stops_at_first_failure() {
    let bed = password_bed().await;
    let mut stream = raw::connect(bed.addr).await;
    identify_with_password(&mut stream, PASSWORD).await;

    // All-stub batch: the first 204 (a failure) halts the batch.
    let d = roundtrip_batch(
        &mut stream,
        serde_json::json!({
            "requestId": "batch-halt",
            "haltOnFailure": true,
            "requests": [stub_request("GetStats"), stub_request("GetProfileList")]
        }),
    )
    .await;
    let results = d["results"].as_array().expect("results");
    assert_eq!(results.len(), 1, "haltOnFailure stops after the failure");
    assert_eq!(results[0]["requestType"], "GetStats");

    // A succeeding request (Sleep) does not halt the batch.
    let d = roundtrip_batch(
        &mut stream,
        serde_json::json!({
            "requestId": "batch-halt-after-success",
            "haltOnFailure": true,
            "requests": [
                {"requestType": "Sleep", "requestData": {"sleepMillis": 10}},
                stub_request("GetStats"),
            ]
        }),
    )
    .await;
    let results = d["results"].as_array().expect("results");
    assert_eq!(results.len(), 2, "success does not halt");
    assert_eq!(results[0]["requestStatus"]["code"], 100);
    assert_eq!(results[1]["requestStatus"]["code"], 204);

    bed.shutdown().await;
}

#[tokio::test]
async fn sleep_is_honored_and_capped() {
    let bed = password_bed().await;
    let mut stream = raw::connect(bed.addr).await;
    identify_with_password(&mut stream, PASSWORD).await;

    // A bounded Sleep actually delays the serial batch.
    let started = Instant::now();
    let d = roundtrip_batch(
        &mut stream,
        serde_json::json!({
            "requestId": "batch-sleep",
            "requests": [{"requestType": "Sleep", "requestData": {"sleepMillis": 80}}]
        }),
    )
    .await;
    let elapsed = started.elapsed();
    assert_eq!(d["results"][0]["requestStatus"]["code"], 100);
    assert!(
        elapsed >= Duration::from_millis(80),
        "Sleep must be honored; elapsed {elapsed:?}"
    );

    // Beyond the cap: typed 402 without sleeping.
    let started = Instant::now();
    let d = roundtrip_batch(
        &mut stream,
        serde_json::json!({
            "requestId": "batch-sleep-cap",
            "requests": [{"requestType": "Sleep", "requestData": {"sleepMillis": 60000}}]
        }),
    )
    .await;
    assert_eq!(d["results"][0]["requestStatus"]["code"], 402);
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "over-cap Sleep must not sleep"
    );

    // sleepFrames belongs to the unsupported SerialFrame execution type.
    let d = roundtrip_batch(
        &mut stream,
        serde_json::json!({
            "requestId": "batch-sleep-frames",
            "requests": [{"requestType": "Sleep", "requestData": {"sleepFrames": 10}}]
        }),
    )
    .await;
    assert_eq!(d["results"][0]["requestStatus"]["code"], 400);

    // No delay field at all: typed 300.
    let d = roundtrip_batch(
        &mut stream,
        serde_json::json!({
            "requestId": "batch-sleep-empty",
            "requests": [stub_request("Sleep")]
        }),
    )
    .await;
    assert_eq!(d["results"][0]["requestStatus"]["code"], 300);

    bed.shutdown().await;
}

#[tokio::test]
async fn invalid_execution_type_closes_4005() {
    let bed = password_bed().await;
    let mut stream = raw::connect(bed.addr).await;
    identify_with_password(&mut stream, PASSWORD).await;

    // executionType 1 (SerialFrame) and 2 (Parallel) are implemented
    // (tests/obs_ws_batches.rs); an out-of-range value still closes 4005
    // like upstream.
    raw::write_value(
        &mut stream,
        serde_json::json!({
            "op": 8,
            "d": {"requestId": "batch-exec-bogus", "executionType": 7, "requests": []}
        }),
    )
    .await;
    let (code, _) = raw::read_close(&mut stream).await;
    assert_eq!(code, 4005);

    bed.shutdown().await;
}

#[tokio::test]
async fn proto_constants_pin_the_advertised_versions() {
    assert_eq!(proto::OBS_WEBSOCKET_VERSION, "5.7.4");
    assert_eq!(proto::RPC_VERSION, 1);
    assert_eq!(proto::SUBPROTOCOL_JSON, "obswebsocket.json");
}
