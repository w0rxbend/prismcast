//! obs-websocket 5.x adapter: `RequestBatch` execution modes `SerialFrame`
//! (executionType 1) and `Parallel` (executionType 2) over a real socket
//! (OBSWS-002 batches slice; ADR-0021 §d). The TestBed/raw harness is
//! deliberately duplicated from `tests/obs_ws.rs`.

use std::net::{Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::{Message, Utf8Bytes};
use tokio_tungstenite::WebSocketStream;

use prismcast_app::broadcaster::StreamEvent;
use prismcast_app::{AppHandle, CoreConfig, EventFilter};
use prismcast_core::event::{AudioEvent, Event, SceneEvent};
use prismcast_core::source::SourceKind;
use prismcast_core::Command;
use prismcast_protocol::handshake::{AuthChallenge, Permission};
use prismcast_remote::auth::{challenge_response, AuthConfig};
use prismcast_remote::obs_ws::{ObsWsServer, ObsWsServerConfig};

const TIMEOUT: Duration = Duration::from_secs(5);
const PASSWORD: &str = "hunter2";

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
        let request = format!("ws://{addr}/")
            .into_client_request()
            .expect("request");
        tokio::time::timeout(TIMEOUT, tokio_tungstenite::connect_async(request))
            .await
            .expect("connect timed out")
            .expect("ws connect")
            .0
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
}

/// Completes the obs handshake with password auth.
async fn identify_with_password(stream: &mut raw::RawStream, password: &str) {
    let hello = raw::read_value(stream).await;
    assert_eq!(hello["op"], 0, "first message must be Hello: {hello}");
    let challenge: AuthChallenge =
        serde_json::from_value(hello["d"]["authentication"].clone()).expect("challenge in hello");
    let response = challenge_response(password, &challenge);
    raw::write_value(
        stream,
        serde_json::json!({ "op": 1, "d": { "rpcVersion": 1, "authentication": response } }),
    )
    .await;
    let identified = raw::read_value(stream).await;
    assert_eq!(identified["op"], 2, "expected Identified: {identified}");
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

fn result_types(d: &serde_json::Value) -> Vec<&str> {
    d["results"]
        .as_array()
        .expect("results array")
        .iter()
        .map(|r| r["requestType"].as_str().expect("requestType"))
        .collect()
}

// --- SerialFrame (executionType 1) ---

#[tokio::test]
async fn serial_frame_preserves_order_like_serial_realtime() {
    let bed = password_bed().await;
    let mut stream = raw::connect(bed.addr).await;
    identify_with_password(&mut stream, PASSWORD).await;

    let d = roundtrip_batch(
        &mut stream,
        serde_json::json!({
            "requestId": "sf-order",
            "executionType": 1,
            "requests": [
                {"requestType": "GetVersion"},
                {"requestType": "CreateScene", "requestData": {"sceneName": "SF"}},
                {"requestType": "GetSceneList"},
            ]
        }),
    )
    .await;
    assert_eq!(
        result_types(&d),
        ["GetVersion", "CreateScene", "GetSceneList"]
    );
    for result in d["results"].as_array().expect("results") {
        assert_eq!(result["requestStatus"]["code"], 100, "{result}");
    }
    // Serial semantics: the created scene is visible to the trailing query.
    let names: Vec<&str> = d["results"][2]["responseData"]["scenes"]
        .as_array()
        .expect("scenes")
        .iter()
        .filter_map(|s| s["sceneName"].as_str())
        .collect();
    assert!(names.contains(&"SF"), "scene list after create: {names:?}");

    bed.shutdown().await;
}

#[tokio::test]
async fn serial_frame_halt_on_failure_stops_early() {
    let bed = password_bed().await;
    let mut stream = raw::connect(bed.addr).await;
    identify_with_password(&mut stream, PASSWORD).await;

    let d = roundtrip_batch(
        &mut stream,
        serde_json::json!({
            "requestId": "sf-halt",
            "executionType": 1,
            "haltOnFailure": true,
            "requests": [
                {"requestType": "Sleep", "requestData": {"sleepMillis": 10}},
                {"requestType": "GetStats"},
                {"requestType": "GetVersion"},
            ]
        }),
    )
    .await;
    let results = d["results"].as_array().expect("results");
    assert_eq!(results.len(), 2, "haltOnFailure stops after the failure");
    assert_eq!(results[0]["requestStatus"]["code"], 100);
    assert_eq!(results[1]["requestStatus"]["code"], 204);

    bed.shutdown().await;
}

#[tokio::test]
async fn serial_frame_sleep_frames_uses_profile_frame_clock() {
    let bed = password_bed().await;
    let mut stream = raw::connect(bed.addr).await;
    identify_with_password(&mut stream, PASSWORD).await;

    // The default state activates a 1080p60 profile, so 30 frames ≈ 500 ms.
    let started = Instant::now();
    let d = roundtrip_batch(
        &mut stream,
        serde_json::json!({
            "requestId": "sf-sleep-frames",
            "executionType": 1,
            "requests": [{"requestType": "Sleep", "requestData": {"sleepFrames": 30}}]
        }),
    )
    .await;
    let elapsed = started.elapsed();
    assert_eq!(d["results"][0]["requestStatus"]["code"], 100);
    assert!(
        elapsed >= Duration::from_millis(450),
        "30 frames at 60 fps ≈ 500 ms; elapsed {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "frame sleep must not overshoot wildly; elapsed {elapsed:?}"
    );

    bed.shutdown().await;
}

#[tokio::test]
async fn serial_frame_sleep_frames_over_cap_gets_402_without_sleeping() {
    let bed = password_bed().await;
    let mut stream = raw::connect(bed.addr).await;
    identify_with_password(&mut stream, PASSWORD).await;

    // 3 M frames at 60 fps is ~13.9 h — far beyond the 50 s sleep cap.
    let started = Instant::now();
    let d = roundtrip_batch(
        &mut stream,
        serde_json::json!({
            "requestId": "sf-sleep-frames-cap",
            "executionType": 1,
            "requests": [{"requestType": "Sleep", "requestData": {"sleepFrames": 3_000_000}}]
        }),
    )
    .await;
    assert_eq!(d["results"][0]["requestStatus"]["code"], 402);
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "over-cap sleepFrames must not sleep"
    );

    bed.shutdown().await;
}

// --- Parallel (executionType 2) ---

#[tokio::test]
async fn parallel_results_in_request_order_and_core_events_commit() {
    let bed = password_bed().await;
    // Setup through the native path: one source to mute from the batch.
    bed.app
        .dispatch(Command::AddSource {
            kind: SourceKind::Color,
            name: "color".to_string(),
        })
        .await
        .expect("native dispatch");
    let source_id = bed
        .app
        .snapshot()
        .state()
        .sources
        .values()
        .next()
        .map(|s| s.id)
        .expect("source");

    let mut stream = raw::connect(bed.addr).await;
    identify_with_password(&mut stream, PASSWORD).await;
    // Subscribe only now, so setup events are not collected below.
    let mut events = bed.app.subscribe(EventFilter::all());

    let d = roundtrip_batch(
        &mut stream,
        serde_json::json!({
            "requestId": "par-members",
            "executionType": 2,
            "requests": [
                {"requestType": "CreateScene", "requestData": {"sceneName": "P1"}},
                {"requestType": "CreateScene", "requestData": {"sceneName": "P2"}},
                {"requestType": "CreateScene", "requestData": {"sceneName": "P3"}},
                {"requestType": "CreateScene", "requestData": {"sceneName": "P4"}},
                {"requestType": "SetInputMute", "requestData": {"inputName": "color", "inputMuted": true}},
            ]
        }),
    )
    .await;

    // Results come back in request order, one per member, all successful.
    assert_eq!(
        result_types(&d),
        [
            "CreateScene",
            "CreateScene",
            "CreateScene",
            "CreateScene",
            "SetInputMute"
        ]
    );
    for result in d["results"].as_array().expect("results") {
        assert_eq!(result["requestStatus"]["code"], 100, "{result}");
    }

    // Every member committed its Core Event (order-insensitive: upstream
    // defines no ordering between parallel members).
    let mut scene_adds = 0;
    let mut muted = false;
    while scene_adds < 4 || !muted {
        match tokio::time::timeout(TIMEOUT, events.recv())
            .await
            .expect("event timed out")
        {
            Some(StreamEvent::Event { event, .. }) => match event {
                Event::Scene(SceneEvent::Added { .. }) => scene_adds += 1,
                Event::Audio(AudioEvent::MixerChanged {
                    source_id: sid,
                    state,
                }) if sid == source_id && state.muted => {
                    muted = true;
                }
                _ => {}
            },
            Some(StreamEvent::Lagged { .. }) => continue,
            None => panic!("event stream closed early"),
        }
    }
    assert_eq!(scene_adds, 4, "all four scene creates committed");
    assert!(muted, "the mute member committed MixerChanged(muted=true)");

    bed.shutdown().await;
}

#[tokio::test]
async fn parallel_sleep_member_does_not_serialize_batch() {
    let bed = password_bed().await;
    let mut stream = raw::connect(bed.addr).await;
    identify_with_password(&mut stream, PASSWORD).await;

    // Three 250 ms Sleeps: ~750 ms serially, ~250 ms in parallel.
    let started = Instant::now();
    let d = roundtrip_batch(
        &mut stream,
        serde_json::json!({
            "requestId": "par-sleeps",
            "executionType": 2,
            "requests": [
                {"requestType": "Sleep", "requestData": {"sleepMillis": 250}},
                {"requestType": "Sleep", "requestData": {"sleepMillis": 250}},
                {"requestType": "Sleep", "requestData": {"sleepMillis": 250}},
            ]
        }),
    )
    .await;
    let elapsed = started.elapsed();
    for result in d["results"].as_array().expect("results") {
        assert_eq!(result["requestStatus"]["code"], 100, "{result}");
    }
    assert!(
        elapsed >= Duration::from_millis(240),
        "sleeps must still be honored; elapsed {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_millis(700),
        "sleep members must not serialize the batch; elapsed {elapsed:?}"
    );

    bed.shutdown().await;
}

#[tokio::test]
async fn parallel_ignores_halt_on_failure() {
    let bed = password_bed().await;
    let mut stream = raw::connect(bed.addr).await;
    identify_with_password(&mut stream, PASSWORD).await;

    let d = roundtrip_batch(
        &mut stream,
        serde_json::json!({
            "requestId": "par-halt-ignored",
            "executionType": 2,
            "haltOnFailure": true,
            "requests": [
                {"requestType": "GetStats"},
                {"requestType": "GetVersion"},
                {"requestType": "GetSceneList"},
            ]
        }),
    )
    .await;
    let results = d["results"].as_array().expect("results");
    assert_eq!(
        results.len(),
        3,
        "haltOnFailure is ignored in Parallel (upstream semantics)"
    );
    assert_eq!(result_types(&d), ["GetStats", "GetVersion", "GetSceneList"]);
    assert_eq!(
        results[0]["requestStatus"]["code"], 204,
        "unknown type stub"
    );
    assert_eq!(results[1]["requestStatus"]["code"], 100);
    assert_eq!(results[2]["requestStatus"]["code"], 100);

    bed.shutdown().await;
}
