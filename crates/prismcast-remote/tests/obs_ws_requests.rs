//! obs-websocket 5.x adapter: request-translation integration tests over a
//! real socket (OBSWS-001 request slice). Every family of the advertised
//! request set is exercised after a real handshake; Core-Event parity is
//! asserted against an `AppHandle` event stream.
//!
//! The harness mirrors `tests/obs_ws.rs` (kept separate so the parallel
//! event-translation slice can add `tests/obs_ws_events.rs` without merge
//! conflicts).

use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::{Message, Utf8Bytes};
use tokio_tungstenite::WebSocketStream;

use prismcast_app::broadcaster::StreamEvent;
use prismcast_app::{AppHandle, CoreConfig, EventFilter};
use prismcast_core::event::{AudioEvent, Event, OutputEvent, SceneEvent};
use prismcast_core::id::{EncoderId, SceneId, SceneItemId};
use prismcast_core::output::{Output, OutputKind, OutputState};
use prismcast_core::scene::Scene;
use prismcast_core::source::SourceKind;
use prismcast_core::state::AppState;
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

async fn spawn_bed_with_state(state: AppState, auth: AuthConfig) -> TestBed {
    let app = AppHandle::spawn_with_state(state, CoreConfig::default());
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

async fn spawn_bed(auth: AuthConfig) -> TestBed {
    spawn_bed_with_state(AppState::new(), auth).await
}

async fn password_bed() -> TestBed {
    spawn_bed(AuthConfig::password(PASSWORD, vec![Permission::Admin])).await
}

impl TestBed {
    async fn shutdown(self) {
        self.server.shutdown().await;
        self.app.shutdown().await;
    }

    /// Native command path (trusted local controller) for test setup.
    async fn dispatch(&self, command: Command) -> Vec<Event> {
        self.app
            .dispatch(command)
            .await
            .expect("native dispatch")
            .events
    }

    async fn add_scene(&self, name: &str) -> SceneId {
        let events = self
            .dispatch(Command::AddScene {
                name: name.to_string(),
            })
            .await;
        events
            .iter()
            .find_map(|event| match event {
                Event::Scene(SceneEvent::Added { scene_id, .. }) => Some(*scene_id),
                _ => None,
            })
            .expect("scene added")
    }

    async fn add_source(&self, kind: SourceKind, name: &str) -> prismcast_core::SourceId {
        let events = self
            .dispatch(Command::AddSource {
                kind,
                name: name.to_string(),
            })
            .await;
        events
            .iter()
            .find_map(|event| match event {
                Event::Source(prismcast_core::SourceEvent::Added { source }) => Some(source.id),
                _ => None,
            })
            .expect("source added")
    }

    async fn add_output(&self, kind: OutputKind, name: &str) {
        self.dispatch(Command::AddOutput {
            output: Output::new(kind, name, EncoderId::new()),
        })
        .await;
    }
}

/// Raw-frame helpers over a bare tungstenite client.
mod raw {
    use super::*;

    pub type RawStream = WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

    pub async fn connect(addr: SocketAddr) -> RawStream {
        let request = format!("ws://{addr}/")
            .into_client_request()
            .expect("request");
        let (stream, _response) =
            tokio::time::timeout(TIMEOUT, tokio_tungstenite::connect_async(request))
                .await
                .expect("connect timed out")
                .expect("ws connect");
        stream
    }

    pub async fn write_value(stream: &mut RawStream, value: serde_json::Value) {
        stream
            .send(Message::Text(Utf8Bytes::from(value.to_string())))
            .await
            .expect("write");
    }

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

/// A connected, identified obs session.
async fn connect_identified(bed: &TestBed) -> raw::RawStream {
    let mut stream = raw::connect(bed.addr).await;
    let hello = raw::read_value(&mut stream).await;
    assert_eq!(hello["op"], 0, "first message must be Hello: {hello}");
    let challenge: AuthChallenge =
        serde_json::from_value(hello["d"]["authentication"].clone()).expect("challenge in hello");
    let response = challenge_response(PASSWORD, &challenge);
    raw::write_value(
        &mut stream,
        serde_json::json!({"op": 1, "d": {"rpcVersion": 1, "authentication": response}}),
    )
    .await;
    let identified = raw::read_value(&mut stream).await;
    assert_eq!(identified["op"], 2, "expected Identified: {identified}");
    stream
}

/// Sends one `Request` and returns the `RequestResponse` payload `d`.
/// Events (op 5) may interleave with responses; skip them until the matching
/// RequestResponse arrives, like a real obs-websocket client must.
async fn request(
    stream: &mut raw::RawStream,
    request_type: &str,
    request_data: Option<serde_json::Value>,
) -> serde_json::Value {
    let request_id = uuid::Uuid::new_v4().to_string();
    let mut d = serde_json::json!({ "requestType": request_type, "requestId": request_id });
    if let Some(request_data) = request_data {
        d["requestData"] = request_data;
    }
    raw::write_value(stream, serde_json::json!({ "op": 6, "d": d })).await;
    loop {
        let response = raw::read_value(stream).await;
        if response["op"] == 5 {
            continue;
        }
        assert_eq!(response["op"], 7, "expected RequestResponse: {response}");
        assert_eq!(response["d"]["requestType"], request_type);
        assert_eq!(response["d"]["requestId"], request_id);
        return response["d"].clone();
    }
}

fn assert_ok(d: &serde_json::Value) -> &serde_json::Value {
    assert_eq!(d["requestStatus"]["code"], 100, "expected success: {d}");
    assert_eq!(d["requestStatus"]["result"], true);
    d
}

fn assert_code(d: &serde_json::Value, code: u16) {
    assert_eq!(d["requestStatus"]["code"], code, "status of {d}");
    assert_eq!(d["requestStatus"]["result"], false);
    assert!(
        d["requestStatus"]["comment"].is_string(),
        "failures carry a comment: {d}"
    );
}

/// Sends a `RequestBatch` and returns the `results` array.
async fn batch(
    stream: &mut raw::RawStream,
    halt_on_failure: bool,
    requests: serde_json::Value,
) -> serde_json::Value {
    raw::write_value(
        stream,
        serde_json::json!({
            "op": 8,
            "d": {
                "requestId": uuid::Uuid::new_v4().to_string(),
                "haltOnFailure": halt_on_failure,
                "requests": requests,
            }
        }),
    )
    .await;
    let response = loop {
        let frame = raw::read_value(stream).await;
        if frame["op"] == 5 {
            continue;
        }
        break frame;
    };
    assert_eq!(
        response["op"], 9,
        "expected RequestBatchResponse: {response}"
    );
    response["d"]["results"].clone()
}

// --- GetVersion / drift guard ---

#[tokio::test]
async fn get_version_reports_obs_shape_and_drift_guarded_available_requests() {
    let bed = password_bed().await;
    let mut stream = connect_identified(&bed).await;

    let d = request(&mut stream, "GetVersion", None).await;
    assert_ok(&d);
    let data = &d["responseData"];
    assert_eq!(data["obsWebSocketVersion"], "5.7.4");
    assert_eq!(data["rpcVersion"], 1);
    assert_eq!(data["platform"], "linux");
    assert_eq!(data["obsVersion"], "30.2.0");
    assert!(data["supportedImageFormats"].is_array());

    // Drift guard (mirrors map.rs's available_requests_are_real_tags): every
    // advertised request type must dispatch to a real implementation — a bare
    // request may fail for missing fields (300) or missing state (5xx/6xx)
    // but never with 204 (unknown type).
    let available = data["availableRequests"]
        .as_array()
        .expect("availableRequests array")
        .iter()
        .map(|v| v.as_str().expect("string").to_string())
        .collect::<Vec<_>>();
    assert!(
        available.len() >= 40,
        "the MVP set is advertised: {available:?}"
    );
    for request_type in &available {
        let d = request(&mut stream, request_type, None).await;
        let code = d["requestStatus"]["code"].as_u64().expect("code");
        assert_ne!(
            code, 204,
            "advertised request '{request_type}' is not implemented"
        );
    }

    // Genuinely unknown types still get the typed 204 stub.
    let d = request(&mut stream, "GetStats", None).await;
    assert_code(&d, 204);

    bed.shutdown().await;
}

// --- scenes ---

#[tokio::test]
async fn scene_family_roundtrips_and_mirrors_native_state() {
    let bed = password_bed().await;
    let scene_a = bed.add_scene("A").await;
    let _scene_b = bed.add_scene("B").await;
    let mut stream = connect_identified(&bed).await;

    // GetSceneList: names, uuids, indices, current program.
    let d = request(&mut stream, "GetSceneList", None).await;
    assert_ok(&d);
    let data = &d["responseData"];
    assert_eq!(data["currentProgramSceneName"], "A");
    assert_eq!(
        data["currentProgramSceneUuid"],
        scene_a.as_uuid().to_string()
    );
    let scenes = data["scenes"].as_array().expect("scenes");
    assert_eq!(scenes.len(), 2);
    assert_eq!(scenes[0]["sceneName"], "A");
    assert_eq!(scenes[0]["sceneIndex"], 0);
    assert_eq!(scenes[1]["sceneName"], "B");
    assert_eq!(scenes[1]["sceneIndex"], 1);
    assert!(scenes[0]["sceneUuid"].is_string());
    // No studio mode: no preview fields.
    assert!(data.get("currentPreviewSceneName").is_none());

    // GetCurrentProgramScene.
    let d = request(&mut stream, "GetCurrentProgramScene", None).await;
    assert_eq!(d["responseData"]["currentProgramSceneName"], "A");

    // SetCurrentProgramScene switches program.
    let d = request(
        &mut stream,
        "SetCurrentProgramScene",
        Some(serde_json::json!({"sceneName": "B"})),
    )
    .await;
    assert_ok(&d);
    let d = request(&mut stream, "GetCurrentProgramScene", None).await;
    assert_eq!(d["responseData"]["currentProgramSceneName"], "B");

    // Unknown scene → 600; missing field → 300; mistyped → 401.
    let d = request(
        &mut stream,
        "SetCurrentProgramScene",
        Some(serde_json::json!({"sceneName": "Nope"})),
    )
    .await;
    assert_code(&d, 600);
    let d = request(&mut stream, "SetCurrentProgramScene", None).await;
    assert_code(&d, 300);
    let d = request(
        &mut stream,
        "SetCurrentProgramScene",
        Some(serde_json::json!({"sceneName": 42})),
    )
    .await;
    assert_code(&d, 401);

    // CreateScene returns the new sceneUuid; duplicates → 601.
    let d = request(
        &mut stream,
        "CreateScene",
        Some(serde_json::json!({"sceneName": "C"})),
    )
    .await;
    assert_ok(&d);
    let scene_c_uuid = d["responseData"]["sceneUuid"]
        .as_str()
        .expect("uuid")
        .to_string();
    assert!(uuid::Uuid::parse_str(&scene_c_uuid).is_ok());
    let d = request(
        &mut stream,
        "CreateScene",
        Some(serde_json::json!({"sceneName": "C"})),
    )
    .await;
    assert_code(&d, 601);

    // SetSceneName renames; collision → 601.
    let d = request(
        &mut stream,
        "SetSceneName",
        Some(serde_json::json!({"sceneName": "C", "newSceneName": "C2"})),
    )
    .await;
    assert_ok(&d);
    let d = request(
        &mut stream,
        "SetSceneName",
        Some(serde_json::json!({"sceneName": "C2", "newSceneName": "A"})),
    )
    .await;
    assert_code(&d, 601);

    // RemoveScene removes; removing the same name twice → 600.
    let d = request(
        &mut stream,
        "RemoveScene",
        Some(serde_json::json!({"sceneName": "C2"})),
    )
    .await;
    assert_ok(&d);
    let d = request(
        &mut stream,
        "RemoveScene",
        Some(serde_json::json!({"sceneName": "C2"})),
    )
    .await;
    assert_code(&d, 600);

    // The native snapshot agrees with every obs mutation.
    let snapshot = bed.app.snapshot();
    let state_names: Vec<&str> = snapshot
        .state()
        .scenes
        .values()
        .map(|s| s.name.as_str())
        .collect();
    assert_eq!(state_names, ["A", "B"]);
    bed.shutdown().await;
}

#[tokio::test]
async fn duplicate_scene_names_resolve_to_first_match() {
    // The core auto-uniquifies names through commands, so duplicates are
    // injected directly into the state (restore/injection edge case).
    let mut state = AppState::new();
    let first = Scene::new("Dup");
    let first_id = first.id;
    state.scenes.insert(first_id, first);
    let second = Scene::new("Dup");
    let second_id = second.id;
    state.scenes.insert(second_id, second);
    state.current_scene = Some(second_id);
    let bed = spawn_bed_with_state(
        state,
        AuthConfig::password(PASSWORD, vec![Permission::Admin]),
    )
    .await;
    let mut stream = connect_identified(&bed).await;

    let d = request(
        &mut stream,
        "SetCurrentProgramScene",
        Some(serde_json::json!({"sceneName": "Dup"})),
    )
    .await;
    assert_ok(&d);
    assert_eq!(
        bed.app.snapshot().state().current_scene,
        Some(first_id),
        "duplicate names resolve to the first match in list order"
    );
    bed.shutdown().await;
}

// --- studio mode ---

#[tokio::test]
async fn studio_mode_family() {
    let bed = password_bed().await;
    bed.add_scene("Main").await;
    bed.add_scene("Alt").await;
    let mut stream = connect_identified(&bed).await;

    // Disabled: getters report false, preview/transition requests get 506.
    let d = request(&mut stream, "GetStudioModeEnabled", None).await;
    assert_eq!(d["responseData"]["studioModeEnabled"], false);
    let d = request(&mut stream, "GetCurrentPreviewScene", None).await;
    assert_code(&d, 506);
    let d = request(
        &mut stream,
        "SetCurrentPreviewScene",
        Some(serde_json::json!({"sceneName": "Alt"})),
    )
    .await;
    assert_code(&d, 506);
    let d = request(&mut stream, "TriggerStudioModeTransition", None).await;
    assert_code(&d, 506);

    // Enable → preview appears; SetCurrentPreviewScene switches it.
    let d = request(
        &mut stream,
        "SetStudioModeEnabled",
        Some(serde_json::json!({"studioModeEnabled": true})),
    )
    .await;
    assert_ok(&d);
    let d = request(&mut stream, "GetStudioModeEnabled", None).await;
    assert_eq!(d["responseData"]["studioModeEnabled"], true);
    let d = request(&mut stream, "GetCurrentPreviewScene", None).await;
    assert_eq!(d["responseData"]["currentPreviewSceneName"], "Alt");

    let d = request(
        &mut stream,
        "SetCurrentPreviewScene",
        Some(serde_json::json!({"sceneName": "Main"})),
    )
    .await;
    assert_code(&d, 400);
    let d = request(
        &mut stream,
        "SetCurrentPreviewScene",
        Some(serde_json::json!({"sceneName": "Nope"})),
    )
    .await;
    assert_code(&d, 600);

    // TriggerStudioModeTransition swaps preview to program.
    let d = request(&mut stream, "TriggerStudioModeTransition", None).await;
    assert_ok(&d);
    let d = request(&mut stream, "GetCurrentProgramScene", None).await;
    assert_eq!(d["responseData"]["currentProgramSceneName"], "Alt");
    // GetSceneList now carries preview fields.
    let d = request(&mut stream, "GetSceneList", None).await;
    assert_eq!(d["responseData"]["currentPreviewSceneName"], "Main");

    // Disable again.
    let d = request(
        &mut stream,
        "SetStudioModeEnabled",
        Some(serde_json::json!({"studioModeEnabled": false})),
    )
    .await;
    assert_ok(&d);
    assert!(bed.app.snapshot().state().studio_mode.is_none());
    bed.shutdown().await;
}

// --- scene items ---

/// Sets up scene "Main" + color source "color" + one item via obs; returns
/// the minted sceneItemId.
async fn setup_scene_with_item(stream: &mut raw::RawStream, bed: &TestBed) -> u64 {
    bed.add_scene("Main").await;
    bed.add_source(SourceKind::Color, "color").await;
    let d = request(
        stream,
        "CreateSceneItem",
        Some(serde_json::json!({"sceneName": "Main", "sourceName": "color"})),
    )
    .await;
    assert_ok(&d);
    d["responseData"]["sceneItemId"]
        .as_u64()
        .expect("sceneItemId")
}

#[tokio::test]
async fn scene_item_family_crud_enable_transform() {
    let bed = password_bed().await;
    let mut stream = connect_identified(&bed).await;
    let item_id = setup_scene_with_item(&mut stream, &bed).await;
    assert_eq!(item_id, 1, "first item mints sceneItemId 1");

    // GetSceneItemList: one entry, obs-shaped.
    let d = request(
        &mut stream,
        "GetSceneItemList",
        Some(serde_json::json!({"sceneName": "Main"})),
    )
    .await;
    assert_ok(&d);
    let items = d["responseData"]["sceneItems"].as_array().expect("items");
    assert_eq!(items.len(), 1);
    let item = &items[0];
    assert_eq!(item["sceneItemId"], 1);
    assert_eq!(item["sceneItemIndex"], 0);
    assert_eq!(item["sourceName"], "color");
    assert_eq!(item["sourceType"], "OBS_SOURCE_TYPE_INPUT");
    assert_eq!(item["inputKind"], "color_source");
    assert_eq!(item["isGroup"], false);
    assert_eq!(item["sceneItemEnabled"], true);
    assert_eq!(item["sceneItemLocked"], false);
    let transform = &item["sceneItemTransform"];
    for key in [
        "positionX",
        "positionY",
        "rotation",
        "scaleX",
        "scaleY",
        "sourceWidth",
        "sourceHeight",
        "width",
        "height",
        "alignment",
        "boundsType",
        "boundsAlignment",
        "boundsWidth",
        "boundsHeight",
        "cropLeft",
        "cropTop",
        "cropRight",
        "cropBottom",
    ] {
        assert!(
            transform.get(key).is_some(),
            "missing transform key {key}: {transform}"
        );
    }
    // Default item: origin, unscaled, top-left anchored, no crop.
    assert_eq!(transform["positionX"], 0.0);
    assert_eq!(transform["scaleX"], 1.0);
    assert_eq!(transform["alignment"], 5, "top-left");
    assert_eq!(transform["boundsType"], "OBS_BOUNDS_NONE");
    assert_eq!(transform["cropLeft"], 0);

    // GetSceneItemId resolves by source name (and honors searchOffset).
    let d = request(
        &mut stream,
        "GetSceneItemId",
        Some(serde_json::json!({"sceneName": "Main", "sourceName": "color"})),
    )
    .await;
    assert_eq!(d["responseData"]["sceneItemId"], item_id);
    let d = request(
        &mut stream,
        "GetSceneItemId",
        Some(serde_json::json!({"sceneName": "Main", "sourceName": "color", "searchOffset": 1})),
    )
    .await;
    assert_code(&d, 600);

    // SetSceneItemEnabled toggles visibility (and produces the native event;
    // parity is asserted in obs_requests_produce_native_core_events).
    let d = request(
        &mut stream,
        "SetSceneItemEnabled",
        Some(serde_json::json!({"sceneName": "Main", "sceneItemId": item_id, "sceneItemEnabled": false})),
    )
    .await;
    assert_ok(&d);
    let d = request(
        &mut stream,
        "GetSceneItemList",
        Some(serde_json::json!({"sceneName": "Main"})),
    )
    .await;
    assert_eq!(
        d["responseData"]["sceneItems"][0]["sceneItemEnabled"],
        false
    );

    // SetSceneItemTransform merges partial fields onto the existing state.
    let d = request(
        &mut stream,
        "SetSceneItemTransform",
        Some(serde_json::json!({
            "sceneName": "Main",
            "sceneItemId": item_id,
            "sceneItemTransform": {"positionX": 100.5, "rotation": 90.0, "cropLeft": 12, "alignment": 0}
        })),
    )
    .await;
    assert_ok(&d);
    let d = request(
        &mut stream,
        "GetSceneItemTransform",
        Some(serde_json::json!({"sceneName": "Main", "sceneItemId": item_id})),
    )
    .await;
    let transform = &d["responseData"]["sceneItemTransform"];
    assert_eq!(transform["positionX"], 100.5);
    assert_eq!(transform["rotation"], 90.0);
    assert_eq!(transform["cropLeft"], 12);
    assert_eq!(transform["alignment"], 0, "center");
    assert_eq!(
        transform["positionY"], 0.0,
        "untouched fields keep their values"
    );
    assert_eq!(transform["scaleX"], 1.0);
    // The native snapshot agrees.
    let scene_id = bed
        .app
        .snapshot()
        .state()
        .scenes
        .values()
        .next()
        .expect("scene")
        .id;
    let native = bed
        .app
        .snapshot()
        .state()
        .scene(scene_id)
        .expect("scene")
        .items[0]
        .clone();
    assert_eq!(native.transform.position.x, 100.5);
    assert_eq!(native.transform.rotation, 90.0);
    assert_eq!(native.crop.left, 12);
    assert_eq!(
        native.transform.anchor,
        prismcast_core::scene::Anchor::Center
    );

    // Invalid transform fields: bad alignment value → 400; wrong type → 401.
    let d = request(
        &mut stream,
        "SetSceneItemTransform",
        Some(serde_json::json!({
            "sceneName": "Main", "sceneItemId": item_id, "sceneItemTransform": {"alignment": 3}
        })),
    )
    .await;
    assert_code(&d, 400);
    let d = request(
        &mut stream,
        "SetSceneItemTransform",
        Some(serde_json::json!({
            "sceneName": "Main", "sceneItemId": item_id, "sceneItemTransform": {"positionX": "left"}
        })),
    )
    .await;
    assert_code(&d, 401);

    // Unknown sceneItemId → 600.
    let d = request(
        &mut stream,
        "SetSceneItemEnabled",
        Some(
            serde_json::json!({"sceneName": "Main", "sceneItemId": 999, "sceneItemEnabled": true}),
        ),
    )
    .await;
    assert_code(&d, 600);

    // RemoveSceneItem evicts the number: re-resolving it fails with 600.
    let d = request(
        &mut stream,
        "RemoveSceneItem",
        Some(serde_json::json!({"sceneName": "Main", "sceneItemId": item_id})),
    )
    .await;
    assert_ok(&d);
    let d = request(
        &mut stream,
        "RemoveSceneItem",
        Some(serde_json::json!({"sceneName": "Main", "sceneItemId": item_id})),
    )
    .await;
    assert_code(&d, 600);
    let d = request(
        &mut stream,
        "GetSceneItemList",
        Some(serde_json::json!({"sceneName": "Main"})),
    )
    .await;
    assert_eq!(
        d["responseData"]["sceneItems"]
            .as_array()
            .expect("items")
            .len(),
        0
    );

    bed.shutdown().await;
}

#[tokio::test]
async fn scene_item_ids_are_stable_across_z_reorder() {
    let bed = password_bed().await;
    let mut stream = connect_identified(&bed).await;
    let first = setup_scene_with_item(&mut stream, &bed).await;
    let d = request(
        &mut stream,
        "CreateSceneItem",
        Some(serde_json::json!({"sceneName": "Main", "sourceName": "color"})),
    )
    .await;
    let second = d["responseData"]["sceneItemId"].as_u64().expect("id");
    assert_eq!((first, second), (1, 2));

    // Flip the z-order natively.
    let scene_id = bed
        .app
        .snapshot()
        .state()
        .scenes
        .values()
        .next()
        .expect("scene")
        .id;
    let bottom_item: SceneItemId = bed
        .app
        .snapshot()
        .state()
        .scene(scene_id)
        .expect("scene")
        .items[0]
        .id;
    bed.dispatch(Command::SetSceneItemZIndex {
        scene_id,
        item_id: bottom_item,
        z_index: 100,
    })
    .await;

    // The numbers survive the reorder; list order flipped.
    let d = request(
        &mut stream,
        "GetSceneItemList",
        Some(serde_json::json!({"sceneName": "Main"})),
    )
    .await;
    let items = d["responseData"]["sceneItems"].as_array().expect("items");
    assert_eq!(items.len(), 2);
    assert_eq!(
        items[0]["sceneItemId"], second,
        "previously-top item is now bottom"
    );
    assert_eq!(items[1]["sceneItemId"], first);
    assert_eq!(items[0]["sceneItemIndex"], 0);
    assert_eq!(items[1]["sceneItemIndex"], 1);
    bed.shutdown().await;
}

// --- inputs ---

#[tokio::test]
async fn input_family_mute_volume_name() {
    let bed = password_bed().await;
    let source_id = bed.add_source(SourceKind::V4l2Camera, "cam").await;
    let mut stream = connect_identified(&bed).await;

    // GetInputList: obs-shaped entries with adapter kind strings.
    let d = request(&mut stream, "GetInputList", None).await;
    let inputs = d["responseData"]["inputs"].as_array().expect("inputs");
    assert_eq!(inputs.len(), 1);
    assert_eq!(inputs[0]["inputName"], "cam");
    assert_eq!(inputs[0]["inputUuid"], source_id.as_uuid().to_string());
    assert_eq!(inputs[0]["inputKind"], "v4l2_input");
    assert_eq!(inputs[0]["unversionedInputKind"], "v4l2_input");
    // inputKind filter.
    let d = request(
        &mut stream,
        "GetInputList",
        Some(serde_json::json!({"inputKind": "v4l2_input"})),
    )
    .await;
    assert_eq!(
        d["responseData"]["inputs"]
            .as_array()
            .expect("inputs")
            .len(),
        1
    );
    let d = request(
        &mut stream,
        "GetInputList",
        Some(serde_json::json!({"inputKind": "nope"})),
    )
    .await;
    assert_eq!(
        d["responseData"]["inputs"]
            .as_array()
            .expect("inputs")
            .len(),
        0
    );

    // Mute roundtrip.
    let d = request(
        &mut stream,
        "GetInputMute",
        Some(serde_json::json!({"inputName": "cam"})),
    )
    .await;
    assert_eq!(d["responseData"]["inputMuted"], false);
    let d = request(
        &mut stream,
        "SetInputMute",
        Some(serde_json::json!({"inputName": "cam", "inputMuted": true})),
    )
    .await;
    assert_ok(&d);
    let d = request(
        &mut stream,
        "GetInputMute",
        Some(serde_json::json!({"inputName": "cam"})),
    )
    .await;
    assert_eq!(d["responseData"]["inputMuted"], true);
    let d = request(
        &mut stream,
        "ToggleInputMute",
        Some(serde_json::json!({"inputName": "cam"})),
    )
    .await;
    assert_eq!(d["responseData"]["inputMuted"], false);
    assert!(
        !bed.app
            .snapshot()
            .state()
            .audio
            .mixer_state(source_id)
            .muted
    );

    // Volume: mul ↔ db conversion against the core's volume_db.
    let d = request(
        &mut stream,
        "GetInputVolume",
        Some(serde_json::json!({"inputName": "cam"})),
    )
    .await;
    assert_eq!(d["responseData"]["inputVolumeDb"], 0.0);
    assert_eq!(d["responseData"]["inputVolumeMul"], 1.0);
    let d = request(
        &mut stream,
        "SetInputVolume",
        Some(serde_json::json!({"inputName": "cam", "inputVolumeMul": 0.5})),
    )
    .await;
    assert_ok(&d);
    let db = bed
        .app
        .snapshot()
        .state()
        .audio
        .mixer_state(source_id)
        .volume_db;
    assert!((db - -6.0206).abs() < 0.01, "0.5 mul ≈ -6.02 dB, got {db}");
    let d = request(
        &mut stream,
        "GetInputVolume",
        Some(serde_json::json!({"inputName": "cam"})),
    )
    .await;
    let mul = d["responseData"]["inputVolumeMul"].as_f64().expect("mul");
    assert!((mul - 0.5).abs() < 0.001);
    let d = request(
        &mut stream,
        "SetInputVolume",
        Some(serde_json::json!({"inputName": "cam", "inputVolumeDb": -12.0})),
    )
    .await;
    assert_ok(&d);
    assert_eq!(
        bed.app
            .snapshot()
            .state()
            .audio
            .mixer_state(source_id)
            .volume_db,
        -12.0
    );
    // Neither field → 300.
    let d = request(
        &mut stream,
        "SetInputVolume",
        Some(serde_json::json!({"inputName": "cam"})),
    )
    .await;
    assert_code(&d, 300);
    // Unknown input → 600.
    let d = request(
        &mut stream,
        "GetInputMute",
        Some(serde_json::json!({"inputName": "nope"})),
    )
    .await;
    assert_code(&d, 600);

    // SetInputName renames; collision → 601.
    bed.add_source(SourceKind::Color, "color").await;
    let d = request(
        &mut stream,
        "SetInputName",
        Some(serde_json::json!({"inputName": "cam", "newInputName": "cam2"})),
    )
    .await;
    assert_ok(&d);
    let d = request(
        &mut stream,
        "SetInputName",
        Some(serde_json::json!({"inputName": "cam2", "newInputName": "color"})),
    )
    .await;
    assert_code(&d, 601);
    assert_eq!(
        bed.app
            .snapshot()
            .state()
            .source(source_id)
            .expect("source")
            .name,
        "cam2"
    );

    bed.shutdown().await;
}

// --- transitions ---

#[tokio::test]
async fn transition_family() {
    let bed = password_bed().await;
    let mut stream = connect_identified(&bed).await;

    // Default transition is Fade.
    let d = request(&mut stream, "GetCurrentSceneTransition", None).await;
    assert_ok(&d);
    let data = &d["responseData"];
    assert_eq!(data["transitionName"], "Fade");
    assert_eq!(data["transitionKind"], "fade_transition");
    assert_eq!(data["transitionFixed"], false);
    assert_eq!(data["transitionDuration"], 300);

    // Switch by display name and by kind id.
    let d = request(
        &mut stream,
        "SetCurrentSceneTransition",
        Some(serde_json::json!({"transitionName": "Cut"})),
    )
    .await;
    assert_ok(&d);
    let d = request(&mut stream, "GetCurrentSceneTransition", None).await;
    assert_eq!(d["responseData"]["transitionKind"], "cut_transition");
    let d = request(
        &mut stream,
        "SetCurrentSceneTransition",
        Some(serde_json::json!({"transitionName": "slide_transition"})),
    )
    .await;
    assert_ok(&d);
    assert_eq!(
        bed.app.snapshot().state().transition.kind,
        prismcast_core::transition::TransitionKind::Slide
    );
    // Unknown transition → 600.
    let d = request(
        &mut stream,
        "SetCurrentSceneTransition",
        Some(serde_json::json!({"transitionName": "luma_wipe"})),
    )
    .await;
    assert_code(&d, 600);
    bed.shutdown().await;
}

// --- outputs ---

#[tokio::test]
async fn output_family_by_name_with_state_conflicts() {
    let bed = password_bed().await;
    bed.add_output(OutputKind::Recording, "rec").await;
    let mut stream = connect_identified(&bed).await;

    // GetOutputList: obs-shaped entries.
    let d = request(&mut stream, "GetOutputList", None).await;
    let outputs = d["responseData"]["outputs"].as_array().expect("outputs");
    assert_eq!(outputs.len(), 1);
    assert_eq!(outputs[0]["outputName"], "rec");
    assert_eq!(outputs[0]["outputKind"], "recording_output");
    assert_eq!(outputs[0]["outputActive"], false);
    assert_eq!(outputs[0]["outputReconnecting"], false);
    assert_eq!(outputs[0]["outputFlags"]["OBS_OUTPUT_SERVICE"], false);
    assert!(outputs[0]["outputWidth"].is_number());

    // Status of a stopped output.
    let d = request(
        &mut stream,
        "GetOutputStatus",
        Some(serde_json::json!({"outputName": "rec"})),
    )
    .await;
    assert_eq!(d["responseData"]["outputActive"], false);
    for key in [
        "outputTimecode",
        "outputDuration",
        "outputCongestion",
        "outputBytes",
        "outputSkippedFrames",
        "outputTotalFrames",
    ] {
        assert!(d["responseData"].get(key).is_some(), "missing {key}");
    }
    // Unknown output → 600.
    let d = request(
        &mut stream,
        "GetOutputStatus",
        Some(serde_json::json!({"outputName": "nope"})),
    )
    .await;
    assert_code(&d, 600);

    // Stop of a stopped output → 501; start → 100; double start → 500.
    let d = request(
        &mut stream,
        "StopOutput",
        Some(serde_json::json!({"outputName": "rec"})),
    )
    .await;
    assert_code(&d, 501);
    let d = request(
        &mut stream,
        "StartOutput",
        Some(serde_json::json!({"outputName": "rec"})),
    )
    .await;
    assert_ok(&d);
    let d = request(
        &mut stream,
        "StartOutput",
        Some(serde_json::json!({"outputName": "rec"})),
    )
    .await;
    assert_code(&d, 500);
    let d = request(
        &mut stream,
        "GetOutputStatus",
        Some(serde_json::json!({"outputName": "rec"})),
    )
    .await;
    assert_eq!(d["responseData"]["outputActive"], true);

    // Toggle flips and reports the new state. (`Stopping` is terminal until
    // the media engine confirms the drain, which no test double drives — so
    // the toggle-up leg uses a second output.)
    let d = request(
        &mut stream,
        "ToggleOutput",
        Some(serde_json::json!({"outputName": "rec"})),
    )
    .await;
    assert_eq!(d["responseData"]["outputActive"], false);
    bed.add_output(OutputKind::Recording, "rec2").await;
    let d = request(
        &mut stream,
        "ToggleOutput",
        Some(serde_json::json!({"outputName": "rec2"})),
    )
    .await;
    assert_eq!(d["responseData"]["outputActive"], true);

    // Stop succeeds from running.
    let d = request(
        &mut stream,
        "StopOutput",
        Some(serde_json::json!({"outputName": "rec2"})),
    )
    .await;
    assert_ok(&d);
    bed.shutdown().await;
}

#[tokio::test]
async fn stream_record_singletons_resolve_designated_primaries() {
    let bed = password_bed().await;
    // Insertion order: Srt before Rtmp — Rtmp still wins as stream primary.
    bed.add_output(OutputKind::Srt, "srt").await;
    bed.add_output(OutputKind::Rtmp, "rtmp").await;
    bed.add_output(OutputKind::Recording, "rec").await;
    let mut stream = connect_identified(&bed).await;

    let rtmp_id = bed
        .app
        .snapshot()
        .state()
        .outputs
        .values()
        .find(|o| o.kind == OutputKind::Rtmp)
        .map(|o| o.id)
        .expect("rtmp");
    let rec_id = bed
        .app
        .snapshot()
        .state()
        .outputs
        .values()
        .find(|o| o.kind == OutputKind::Recording)
        .map(|o| o.id)
        .expect("rec");

    // GetStreamStatus / GetRecordStatus report the primaries.
    let d = request(&mut stream, "GetStreamStatus", None).await;
    assert_ok(&d);
    assert_eq!(d["responseData"]["outputActive"], false);
    let d = request(&mut stream, "GetRecordStatus", None).await;
    assert_ok(&d);
    assert_eq!(d["responseData"]["outputActive"], false);
    assert_eq!(d["responseData"]["outputPaused"], false);

    // StartStream drives the Rtmp output specifically (not the Srt one).
    let d = request(&mut stream, "StartStream", None).await;
    assert_ok(&d);
    assert_eq!(
        bed.app.snapshot().state().output(rtmp_id).map(|o| o.state),
        Some(OutputState::Starting)
    );
    assert_eq!(
        bed.app
            .snapshot()
            .state()
            .outputs
            .values()
            .find(|o| o.kind == OutputKind::Srt)
            .map(|o| o.state),
        Some(OutputState::Stopped),
        "the fallback is untouched while an Rtmp primary exists"
    );
    // Already running → 500.
    let d = request(&mut stream, "StartStream", None).await;
    assert_code(&d, 500);

    // StartRecord drives the Recording output; ToggleStream stops the stream.
    let d = request(&mut stream, "StartRecord", None).await;
    assert_ok(&d);
    assert_eq!(
        bed.app.snapshot().state().output(rec_id).map(|o| o.state),
        Some(OutputState::Starting)
    );
    let d = request(&mut stream, "ToggleStream", None).await;
    assert_eq!(d["responseData"]["outputActive"], false);
    let d = request(&mut stream, "StopRecord", None).await;
    assert_ok(&d);
    let d = request(&mut stream, "StopRecord", None).await;
    assert_code(&d, 501);
    bed.shutdown().await;
}

#[tokio::test]
async fn absent_primaries_answer_typed_600_and_501() {
    // Only a recording output: no stream primary exists.
    let bed = password_bed().await;
    bed.add_output(OutputKind::Recording, "rec").await;
    let mut stream = connect_identified(&bed).await;

    let d = request(&mut stream, "StartStream", None).await;
    assert_code(&d, 600);
    let d = request(&mut stream, "GetStreamStatus", None).await;
    assert_code(&d, 501);
    let d = request(&mut stream, "StopStream", None).await;
    assert_code(&d, 501);
    let d = request(&mut stream, "ToggleStream", None).await;
    assert_code(&d, 600);
    bed.shutdown().await;

    // No outputs at all: the record primary is absent too.
    let bed = password_bed().await;
    let mut stream = connect_identified(&bed).await;
    let d = request(&mut stream, "StartRecord", None).await;
    assert_code(&d, 600);
    let d = request(&mut stream, "GetRecordStatus", None).await;
    assert_code(&d, 501);
    let d = request(&mut stream, "StopRecord", None).await;
    assert_code(&d, 501);
    bed.shutdown().await;
}

// --- Core Event parity ---

#[tokio::test]
async fn obs_requests_produce_native_core_events() {
    let bed = password_bed().await;
    let scene_a = bed.add_scene("A").await;
    let scene_b = bed.add_scene("B").await;
    let source_id = bed.add_source(SourceKind::Color, "color").await;
    bed.add_output(OutputKind::Recording, "rec").await;
    let output_id = bed
        .app
        .snapshot()
        .state()
        .outputs
        .values()
        .next()
        .map(|o| o.id)
        .expect("output");

    let mut stream = connect_identified(&bed).await;
    let item_number = setup_item_number(&mut stream).await;
    // Subscribe only now, so setup events are not collected below.
    let mut events = bed.app.subscribe(EventFilter::all());

    // Drive one obs request per family the acceptance criteria name.
    let d = request(
        &mut stream,
        "SetCurrentProgramScene",
        Some(serde_json::json!({"sceneName": "B"})),
    )
    .await;
    assert_ok(&d);
    let d = request(
        &mut stream,
        "SetSceneItemEnabled",
        Some(serde_json::json!({"sceneName": "A", "sceneItemId": item_number, "sceneItemEnabled": false})),
    )
    .await;
    assert_ok(&d);
    let d = request(
        &mut stream,
        "SetInputMute",
        Some(serde_json::json!({"inputName": "color", "inputMuted": true})),
    )
    .await;
    assert_ok(&d);
    let d = request(
        &mut stream,
        "StartOutput",
        Some(serde_json::json!({"outputName": "rec"})),
    )
    .await;
    assert_ok(&d);

    // Collect the four expected committed events from the AppHandle stream:
    // exactly what the native protocol's dispatch would have produced.
    let mut seen = Vec::new();
    while seen.len() < 4 {
        match tokio::time::timeout(TIMEOUT, events.recv())
            .await
            .expect("event timed out")
        {
            Some(StreamEvent::Event { event, .. }) => seen.push(event),
            Some(StreamEvent::Lagged { .. }) => continue,
            None => panic!("event stream closed early"),
        }
    }
    assert!(
        seen.iter().any(|e| matches!(e, Event::Scene(SceneEvent::CurrentChanged { scene_id }) if *scene_id == scene_b)),
        "scene switch committed CurrentChanged: {seen:?}"
    );
    assert!(
        seen.iter().any(|e| matches!(e, Event::Scene(SceneEvent::ItemUpdated { scene_id, item }) if *scene_id == scene_a && !item.visible)),
        "scene-item disable committed ItemUpdated(visible=false): {seen:?}"
    );
    assert!(
        seen.iter().any(|e| matches!(e, Event::Audio(AudioEvent::MixerChanged { source_id: sid, state }) if *sid == source_id && state.muted)),
        "input mute committed MixerChanged(muted=true): {seen:?}"
    );
    assert!(
        seen.iter().any(|e| matches!(e, Event::Output(OutputEvent::StateChanged { output_id: oid, state: OutputState::Starting }) if *oid == output_id)),
        "output start committed StateChanged(Starting): {seen:?}"
    );
    bed.shutdown().await;
}

/// Creates a scene item via obs and returns its minted number.
async fn setup_item_number(stream: &mut raw::RawStream) -> u64 {
    let d = request(
        stream,
        "CreateSceneItem",
        Some(serde_json::json!({"sceneName": "A", "sourceName": "color"})),
    )
    .await;
    assert_ok(&d);
    d["responseData"]["sceneItemId"]
        .as_u64()
        .expect("sceneItemId")
}

// --- batches through the real translation path ---

#[tokio::test]
async fn serial_batch_executes_mixed_results_and_halts() {
    let bed = password_bed().await;
    bed.add_scene("S1").await;
    let mut stream = connect_identified(&bed).await;

    // Mixed success/failure, no halt: every request runs, in order.
    let results = batch(
        &mut stream,
        false,
        serde_json::json!([
            {"requestType": "GetVersion"},
            {"requestType": "CreateScene", "requestData": {"sceneName": "S2"}},
            {"requestType": "Sleep", "requestData": {"sleepMillis": 5}},
            {"requestType": "SetCurrentProgramScene", "requestData": {"sceneName": "Nope"}},
            {"requestType": "GetSceneList"},
        ]),
    )
    .await;
    let results = results.as_array().expect("results");
    assert_eq!(results.len(), 5);
    let codes: Vec<u64> = results
        .iter()
        .map(|r| r["requestStatus"]["code"].as_u64().expect("code"))
        .collect();
    assert_eq!(codes, [100, 100, 100, 600, 100]);
    assert_eq!(results[0]["requestType"], "GetVersion");
    assert!(
        results[0]["responseData"]["availableRequests"].is_array(),
        "responseData flows through batches"
    );
    assert!(results[1]["responseData"]["sceneUuid"].is_string());

    // haltOnFailure stops at the first failure: S2 (created above) is the
    // duplicate here, so the batch ends there.
    let results = batch(
        &mut stream,
        true,
        serde_json::json!([
            {"requestType": "GetVersion"},
            {"requestType": "CreateScene", "requestData": {"sceneName": "S2"}},
            {"requestType": "GetSceneList"},
        ]),
    )
    .await;
    let results = results.as_array().expect("results");
    assert_eq!(results.len(), 2, "halt after the 601");
    let codes: Vec<u64> = results
        .iter()
        .map(|r| r["requestStatus"]["code"].as_u64().expect("code"))
        .collect();
    assert_eq!(codes, [100, 601]);

    // The batch mutations really committed.
    let snapshot = bed.app.snapshot();
    let names: Vec<&str> = snapshot
        .state()
        .scenes
        .values()
        .map(|s| s.name.as_str())
        .collect();
    assert_eq!(names, ["S1", "S2"]);
    bed.shutdown().await;
}

// --- permissions are the session's, via the native pivot ---

#[tokio::test]
async fn mutations_inherit_session_permissions() {
    // A read-only token: queries succeed, mutations get 703 (obs has no
    // authorization code group; documented mapping).
    let bed = spawn_bed(AuthConfig::token("reader", vec![Permission::Read])).await;
    bed.add_scene("A").await;
    bed.add_scene("B").await;

    let mut stream = raw::connect(bed.addr).await;
    let _hello = raw::read_value(&mut stream).await;
    raw::write_value(
        &mut stream,
        serde_json::json!({"op": 1, "d": {"rpcVersion": 1, "authentication": "reader"}}),
    )
    .await;
    let identified = raw::read_value(&mut stream).await;
    assert_eq!(identified["op"], 2);

    let d = request(&mut stream, "GetSceneList", None).await;
    assert_ok(&d);
    let d = request(
        &mut stream,
        "SetCurrentProgramScene",
        Some(serde_json::json!({"sceneName": "B"})),
    )
    .await;
    assert_code(&d, 703);
    bed.shutdown().await;
}
