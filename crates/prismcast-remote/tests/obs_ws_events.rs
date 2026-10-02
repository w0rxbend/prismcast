//! obs-websocket 5.x adapter: domain event → obs `Event` (op 5) translation
//! over a real socket (OBSWS-001 event slice). Drives domain changes through
//! [`AppHandle::dispatch`] and asserts the exact `eventType` / `eventIntent`
//! / `eventData` of every emitted event, the bitmask gating of unsubscribed
//! categories, and mid-session `Reidentify` narrowing/widening.
//!
//! Harness modeled on `tests/obs_ws.rs` (same handshake helpers).

use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::{Message, Utf8Bytes};
use tokio_tungstenite::WebSocketStream;

use prismcast_app::{AppHandle, CoreConfig};
use prismcast_core::event::{Event, SceneEvent, SourceEvent};
use prismcast_core::id::{EncoderId, OutputId, SceneId, SceneItemId, SourceId};
use prismcast_core::output::{Output, OutputKind};
use prismcast_core::scene::Transform;
use prismcast_core::source::SourceKind;
use prismcast_core::Command;
use prismcast_protocol::handshake::{AuthChallenge, Permission};
use prismcast_remote::auth::{challenge_response, AuthConfig};
use prismcast_remote::obs_ws::proto::subscription;
use prismcast_remote::obs_ws::{ObsWsServer, ObsWsServerConfig};

const TIMEOUT: Duration = Duration::from_secs(5);
/// How long to listen when asserting that *no* event arrives.
const SILENCE: Duration = Duration::from_millis(300);
const PASSWORD: &str = "hunter2";

type Stream = WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

struct TestBed {
    app: AppHandle,
    server: ObsWsServer,
    addr: SocketAddr,
}

impl TestBed {
    async fn spawn() -> Self {
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
        Self { app, server, addr }
    }

    async fn shutdown(self) {
        self.server.shutdown().await;
        self.app.shutdown().await;
    }
}

// --- wire helpers ---

async fn write_value(stream: &mut Stream, value: Value) {
    stream
        .send(Message::Text(Utf8Bytes::from(value.to_string())))
        .await
        .expect("write");
}

async fn read_value(stream: &mut Stream) -> Value {
    loop {
        match tokio::time::timeout(TIMEOUT, stream.next())
            .await
            .expect("read timed out")
        {
            Some(Ok(Message::Text(text))) => return serde_json::from_str(&text).expect("json"),
            Some(Ok(Message::Ping(_)) | Ok(Message::Pong(_))) => continue,
            other => panic!("expected text frame, got {other:?}"),
        }
    }
}

/// Connects, performs the password handshake, and identifies with the given
/// `eventSubscriptions` mask.
async fn connect_subscribed(addr: SocketAddr, event_subscriptions: Option<u32>) -> Stream {
    let request = format!("ws://{addr}/")
        .into_client_request()
        .expect("request");
    let (mut stream, _) = tokio::time::timeout(TIMEOUT, tokio_tungstenite::connect_async(request))
        .await
        .expect("connect timed out")
        .expect("ws connect");
    let hello = read_value(&mut stream).await;
    assert_eq!(hello["op"], 0, "first message must be Hello: {hello}");
    let challenge: AuthChallenge =
        serde_json::from_value(hello["d"]["authentication"].clone()).expect("challenge");
    let mut d =
        json!({ "rpcVersion": 1, "authentication": challenge_response(PASSWORD, &challenge) });
    if let Some(mask) = event_subscriptions {
        d["eventSubscriptions"] = mask.into();
    }
    write_value(&mut stream, json!({ "op": 1, "d": d })).await;
    let identified = read_value(&mut stream).await;
    assert_eq!(identified["op"], 2, "expected Identified: {identified}");
    stream
}

/// Sends a `Reidentify` with a new mask and asserts the `Identified` answer.
async fn reidentify(stream: &mut Stream, mask: u32) {
    write_value(
        stream,
        json!({ "op": 3, "d": { "eventSubscriptions": mask } }),
    )
    .await;
    let identified = read_value(stream).await;
    assert_eq!(
        identified["op"], 2,
        "Reidentify is answered with Identified"
    );
}

/// Reads the next frame, which must be an `Event` (op 5); returns `d`.
async fn next_event(stream: &mut Stream) -> Value {
    let frame = read_value(stream).await;
    assert_eq!(frame["op"], 5, "expected Event: {frame}");
    frame["d"].clone()
}

/// Sends an obs `Request` (op 6) and returns its `RequestResponse` `d` plus
/// any events (op 5) that arrived interleaved (a request that mutates state
/// publishes its domain events while the response travels the same queue).
async fn request(
    stream: &mut Stream,
    request_type: &str,
    request_id: &str,
    request_data: Value,
) -> (Value, Vec<Value>) {
    write_value(
        stream,
        json!({
            "op": 6,
            "d": {
                "requestType": request_type,
                "requestId": request_id,
                "requestData": request_data,
            },
        }),
    )
    .await;
    let mut events = Vec::new();
    loop {
        let frame = read_value(stream).await;
        match frame["op"].as_u64() {
            Some(5) => events.push(frame["d"].clone()),
            Some(7) => {
                assert_eq!(frame["d"]["requestId"], request_id, "{frame}");
                assert_eq!(frame["d"]["requestType"], request_type, "{frame}");
                return (frame["d"].clone(), events);
            }
            _ => panic!("unexpected frame: {frame}"),
        }
    }
}

/// The next event, preferring ones already drained by [`request`].
async fn next_event_from(pending: &mut Vec<Value>, stream: &mut Stream) -> Value {
    if pending.is_empty() {
        next_event(stream).await
    } else {
        pending.remove(0)
    }
}

/// Asserts the exact shape of one obs event.
fn assert_event(d: &Value, event_type: &str, intent: u32, event_data: Value) {
    assert_eq!(d["eventType"], event_type, "eventType of {d}");
    assert_eq!(d["eventIntent"], intent, "eventIntent of {d}");
    assert_eq!(d["eventData"], event_data, "eventData of {d}");
}

/// Asserts that no message arrives within [`SILENCE`].
async fn assert_silent(stream: &mut Stream) {
    match tokio::time::timeout(SILENCE, stream.next()).await {
        Err(_) => {}
        Ok(frame) => panic!("expected silence, got {frame:?}"),
    }
}

// --- domain drivers ---

async fn add_scene(app: &AppHandle, name: &str) -> SceneId {
    let response = app
        .dispatch(Command::AddScene { name: name.into() })
        .await
        .expect("add scene");
    response
        .events
        .iter()
        .find_map(|event| match event {
            Event::Scene(SceneEvent::Added { scene_id, .. }) => Some(*scene_id),
            _ => None,
        })
        .expect("SceneEvent::Added")
}

async fn add_source(app: &AppHandle, kind: SourceKind, name: &str) -> SourceId {
    let response = app
        .dispatch(Command::AddSource {
            kind,
            name: name.into(),
        })
        .await
        .expect("add source");
    response
        .events
        .iter()
        .find_map(|event| match event {
            Event::Source(SourceEvent::Added { source }) => Some(source.id),
            _ => None,
        })
        .expect("SourceEvent::Added")
}

async fn add_scene_item(app: &AppHandle, scene_id: SceneId, source_id: SourceId) -> SceneItemId {
    let response = app
        .dispatch(Command::AddSceneItem {
            scene_id,
            source_id,
        })
        .await
        .expect("add scene item");
    response
        .events
        .iter()
        .find_map(|event| match event {
            Event::Scene(SceneEvent::ItemAdded { item, .. }) => Some(item.id),
            _ => None,
        })
        .expect("SceneEvent::ItemAdded")
}

async fn add_output(app: &AppHandle, kind: OutputKind, name: &str) -> OutputId {
    let output = Output::new(kind, name, EncoderId::new());
    let output_id = output.id;
    app.dispatch(Command::AddOutput { output })
        .await
        .expect("add output");
    output_id
}

// --- tests ---

#[tokio::test]
async fn scene_lifecycle_events() {
    let bed = TestBed::spawn().await;
    let mut stream = connect_subscribed(bed.addr, Some(subscription::SCENES)).await;

    // First scene becomes program: SceneCreated + CurrentProgramSceneChanged.
    let main = add_scene(&bed.app, "Main").await;
    assert_event(
        &next_event(&mut stream).await,
        "SceneCreated",
        subscription::SCENES,
        json!({"sceneName": "Main", "sceneUuid": main.as_uuid().to_string(), "isGroup": false}),
    );
    assert_event(
        &next_event(&mut stream).await,
        "CurrentProgramSceneChanged",
        subscription::SCENES,
        json!({"sceneName": "Main", "sceneUuid": main.as_uuid().to_string()}),
    );

    let brb = add_scene(&bed.app, "BRB").await;
    assert_event(
        &next_event(&mut stream).await,
        "SceneCreated",
        subscription::SCENES,
        json!({"sceneName": "BRB", "sceneUuid": brb.as_uuid().to_string(), "isGroup": false}),
    );
    assert_silent(&mut stream).await;

    bed.app
        .dispatch(Command::SetCurrentScene { scene_id: brb })
        .await
        .expect("set current");
    assert_event(
        &next_event(&mut stream).await,
        "CurrentProgramSceneChanged",
        subscription::SCENES,
        json!({"sceneName": "BRB", "sceneUuid": brb.as_uuid().to_string()}),
    );

    bed.app
        .dispatch(Command::RenameScene {
            scene_id: main,
            name: "Intro".into(),
        })
        .await
        .expect("rename scene");
    assert_event(
        &next_event(&mut stream).await,
        "SceneNameChanged",
        subscription::SCENES,
        json!({
            "sceneUuid": main.as_uuid().to_string(),
            "oldSceneName": "Main",
            "sceneName": "Intro",
        }),
    );

    // Removing the current scene also moves the program scene.
    bed.app
        .dispatch(Command::RemoveScene { scene_id: brb })
        .await
        .expect("remove scene");
    assert_event(
        &next_event(&mut stream).await,
        "SceneRemoved",
        subscription::SCENES,
        json!({"sceneName": "BRB", "sceneUuid": brb.as_uuid().to_string(), "isGroup": false}),
    );
    assert_event(
        &next_event(&mut stream).await,
        "CurrentProgramSceneChanged",
        subscription::SCENES,
        json!({"sceneName": "Intro", "sceneUuid": main.as_uuid().to_string()}),
    );
    assert_silent(&mut stream).await;

    bed.shutdown().await;
}

#[tokio::test]
async fn scene_item_events_and_enable_state() {
    let bed = TestBed::spawn().await;
    // Scenes and SceneItems both map to the native Scene category, so a
    // SceneItems-only mask also admits the scene lifecycle events.
    let mut stream = connect_subscribed(bed.addr, Some(subscription::SCENE_ITEMS)).await;

    let mic = add_source(&bed.app, SourceKind::Color, "Mic").await;
    assert_silent(&mut stream).await; // Source category: gated out.
    let main = add_scene(&bed.app, "Main").await;
    assert_event(
        &next_event(&mut stream).await,
        "SceneCreated",
        subscription::SCENES,
        json!({"sceneName": "Main", "sceneUuid": main.as_uuid().to_string(), "isGroup": false}),
    );
    assert_event(
        &next_event(&mut stream).await,
        "CurrentProgramSceneChanged",
        subscription::SCENES,
        json!({"sceneName": "Main", "sceneUuid": main.as_uuid().to_string()}),
    );

    let item = add_scene_item(&bed.app, main, mic).await;
    let created = next_event(&mut stream).await;
    assert_eq!(created["eventType"], "SceneItemCreated");
    assert_eq!(created["eventIntent"], subscription::SCENE_ITEMS);
    assert_eq!(
        created["eventData"]["sceneName"],
        json!("Main"),
        "{created}"
    );
    assert_eq!(
        created["eventData"]["sceneUuid"],
        json!(main.as_uuid().to_string()),
    );
    // The source was created while gated out: its name is resolved from the
    // snapshot, not the event stream.
    assert_eq!(created["eventData"]["sourceName"], json!("Mic"));
    assert_eq!(
        created["eventData"]["sourceUuid"],
        json!(mic.as_uuid().to_string()),
    );
    assert_eq!(created["eventData"]["sceneItemIndex"], json!(0));
    let scene_item_id = created["eventData"]["sceneItemId"].clone();
    assert!(scene_item_id.is_number(), "sceneItemId: {created}");

    // Visibility flips emit SceneItemEnableStateChanged carrying the same id.
    bed.app
        .dispatch(Command::SetSceneItemVisible {
            scene_id: main,
            item_id: item,
            visible: false,
        })
        .await
        .expect("hide item");
    assert_event(
        &next_event(&mut stream).await,
        "SceneItemEnableStateChanged",
        subscription::SCENE_ITEMS,
        json!({
            "sceneName": "Main",
            "sceneUuid": main.as_uuid().to_string(),
            "sceneItemId": scene_item_id,
            "sceneItemEnabled": false,
        }),
    );
    bed.app
        .dispatch(Command::SetSceneItemVisible {
            scene_id: main,
            item_id: item,
            visible: true,
        })
        .await
        .expect("show item");
    assert_event(
        &next_event(&mut stream).await,
        "SceneItemEnableStateChanged",
        subscription::SCENE_ITEMS,
        json!({
            "sceneName": "Main",
            "sceneUuid": main.as_uuid().to_string(),
            "sceneItemId": scene_item_id,
            "sceneItemEnabled": true,
        }),
    );

    // A transform change is an ItemUpdated but NOT an enable-state change.
    bed.app
        .dispatch(Command::SetSceneItemTransform {
            scene_id: main,
            item_id: item,
            transform: Transform {
                rotation: 45.0,
                ..Transform::default()
            },
        })
        .await
        .expect("rotate item");
    assert_silent(&mut stream).await;

    bed.app
        .dispatch(Command::RemoveSceneItem {
            scene_id: main,
            item_id: item,
        })
        .await
        .expect("remove item");
    assert_event(
        &next_event(&mut stream).await,
        "SceneItemRemoved",
        subscription::SCENE_ITEMS,
        json!({
            "sceneName": "Main",
            "sceneUuid": main.as_uuid().to_string(),
            "sourceName": "Mic",
            "sourceUuid": mic.as_uuid().to_string(),
            "sceneItemId": scene_item_id,
        }),
    );
    assert_silent(&mut stream).await;

    bed.shutdown().await;
}

#[tokio::test]
async fn input_events_create_rename_mute_volume_remove() {
    let bed = TestBed::spawn().await;
    let mut stream = connect_subscribed(bed.addr, Some(subscription::INPUTS)).await;

    let mic = add_source(&bed.app, SourceKind::Color, "Mic").await;
    assert_event(
        &next_event(&mut stream).await,
        "InputCreated",
        subscription::INPUTS,
        json!({
            "inputName": "Mic",
            "inputUuid": mic.as_uuid().to_string(),
            "inputKind": "color_source",
            "unversionedInputKind": "color_source",
            "inputSettings": {},
            "defaultInputSettings": {},
        }),
    );

    // Mute flips emit only the mute event (mixer diffing, not the full
    // MixerChanged payload).
    bed.app
        .dispatch(Command::SetSourceMuted {
            source_id: mic,
            muted: true,
        })
        .await
        .expect("mute");
    assert_event(
        &next_event(&mut stream).await,
        "InputMuteStateChanged",
        subscription::INPUTS,
        json!({
            "inputName": "Mic",
            "inputUuid": mic.as_uuid().to_string(),
            "inputMuted": true,
        }),
    );
    assert_silent(&mut stream).await;

    // Volume changes emit volumeMul + volumeDb (the core stores dB).
    bed.app
        .dispatch(Command::SetSourceVolume {
            source_id: mic,
            volume_db: -6.0,
        })
        .await
        .expect("volume");
    let volume = next_event(&mut stream).await;
    assert_eq!(volume["eventType"], "InputVolumeChanged");
    assert_eq!(volume["eventIntent"], subscription::INPUTS);
    assert_eq!(volume["eventData"]["inputName"], json!("Mic"));
    assert_eq!(volume["eventData"]["inputVolumeDb"], json!(-6.0));
    let mul = volume["eventData"]["inputVolumeMul"]
        .as_f64()
        .expect("inputVolumeMul number");
    assert!((mul - 0.501_187_233_627_272_2).abs() < 1e-9, "mul {mul}");
    assert_silent(&mut stream).await;

    // A solo change is a MixerChanged without mute/volume deltas: no event.
    bed.app
        .dispatch(Command::SetSourceSolo {
            source_id: mic,
            solo: true,
        })
        .await
        .expect("solo");
    assert_silent(&mut stream).await;

    bed.app
        .dispatch(Command::RenameSource {
            source_id: mic,
            name: "Mic 1".into(),
        })
        .await
        .expect("rename source");
    assert_event(
        &next_event(&mut stream).await,
        "InputNameChanged",
        subscription::INPUTS,
        json!({
            "inputUuid": mic.as_uuid().to_string(),
            "oldInputName": "Mic",
            "inputName": "Mic 1",
        }),
    );

    bed.app
        .dispatch(Command::RemoveSource { source_id: mic })
        .await
        .expect("remove source");
    // The source is gone from the snapshot at emission time; the name comes
    // from the translator's memo.
    assert_event(
        &next_event(&mut stream).await,
        "InputRemoved",
        subscription::INPUTS,
        json!({
            "inputName": "Mic 1",
            "inputUuid": mic.as_uuid().to_string(),
        }),
    );
    assert_silent(&mut stream).await;

    bed.shutdown().await;
}

#[tokio::test]
async fn studio_mode_and_preview_events() {
    let bed = TestBed::spawn().await;
    // Ui maps to the native System category; scene lifecycle events (native
    // Scene) are gated out.
    let mut stream = connect_subscribed(bed.addr, Some(subscription::UI)).await;

    let _a = add_scene(&bed.app, "A").await;
    let b = add_scene(&bed.app, "B").await;
    let c = add_scene(&bed.app, "C").await;
    assert_silent(&mut stream).await;

    // Enabling studio mode picks the first non-program scene as preview.
    bed.app
        .dispatch(Command::SetStudioModeEnabled { enabled: true })
        .await
        .expect("studio on");
    assert_event(
        &next_event(&mut stream).await,
        "StudioModeStateChanged",
        subscription::UI,
        json!({"studioModeEnabled": true}),
    );
    // CurrentPreviewSceneChanged carries the upstream intent (Scenes) even
    // though the native System category admitted it via the Ui bit.
    assert_event(
        &next_event(&mut stream).await,
        "CurrentPreviewSceneChanged",
        subscription::SCENES,
        json!({"sceneName": "B", "sceneUuid": b.as_uuid().to_string()}),
    );

    bed.app
        .dispatch(Command::SetPreviewScene { scene_id: c })
        .await
        .expect("set preview");
    assert_event(
        &next_event(&mut stream).await,
        "CurrentPreviewSceneChanged",
        subscription::SCENES,
        json!({"sceneName": "C", "sceneUuid": c.as_uuid().to_string()}),
    );

    bed.app
        .dispatch(Command::SetStudioModeEnabled { enabled: false })
        .await
        .expect("studio off");
    assert_event(
        &next_event(&mut stream).await,
        "StudioModeStateChanged",
        subscription::UI,
        json!({"studioModeEnabled": false}),
    );
    assert_silent(&mut stream).await;

    bed.shutdown().await;
}

#[tokio::test]
async fn output_state_events_and_stream_record_primaries() {
    let bed = TestBed::spawn().await;
    let mut stream = connect_subscribed(bed.addr, Some(subscription::OUTPUTS)).await;

    // Output creation/removal have no obs counterpart.
    let twitch = add_output(&bed.app, OutputKind::Rtmp, "Twitch").await;
    assert_silent(&mut stream).await;

    // The first Rtmp output is the designated stream primary: its state
    // changes emit the extension OutputStateChanged plus StreamStateChanged.
    bed.app
        .dispatch(Command::StartOutput { output_id: twitch })
        .await
        .expect("start twitch");
    assert_event(
        &next_event(&mut stream).await,
        "OutputStateChanged",
        subscription::OUTPUTS,
        json!({
            "outputName": "Twitch",
            "outputUuid": twitch.as_uuid().to_string(),
            "outputState": "OBS_WEBSOCKET_OUTPUT_STARTING",
        }),
    );
    assert_event(
        &next_event(&mut stream).await,
        "StreamStateChanged",
        subscription::OUTPUTS,
        json!({"outputActive": false, "outputState": "OBS_WEBSOCKET_OUTPUT_STARTING"}),
    );
    assert_silent(&mut stream).await;

    bed.app
        .dispatch(Command::StopOutput { output_id: twitch })
        .await
        .expect("stop twitch");
    assert_event(
        &next_event(&mut stream).await,
        "OutputStateChanged",
        subscription::OUTPUTS,
        json!({
            "outputName": "Twitch",
            "outputUuid": twitch.as_uuid().to_string(),
            "outputState": "OBS_WEBSOCKET_OUTPUT_STOPPING",
        }),
    );
    assert_event(
        &next_event(&mut stream).await,
        "StreamStateChanged",
        subscription::OUTPUTS,
        json!({"outputActive": true, "outputState": "OBS_WEBSOCKET_OUTPUT_STOPPING"}),
    );

    // A second Rtmp output is not the primary: OutputStateChanged only.
    let restream = add_output(&bed.app, OutputKind::Rtmp, "Restream").await;
    bed.app
        .dispatch(Command::StartOutput {
            output_id: restream,
        })
        .await
        .expect("start restream");
    assert_event(
        &next_event(&mut stream).await,
        "OutputStateChanged",
        subscription::OUTPUTS,
        json!({
            "outputName": "Restream",
            "outputUuid": restream.as_uuid().to_string(),
            "outputState": "OBS_WEBSOCKET_OUTPUT_STARTING",
        }),
    );
    assert_silent(&mut stream).await;

    // The first Recording output is the record primary.
    let rec = add_output(&bed.app, OutputKind::Recording, "Rec").await;
    bed.app
        .dispatch(Command::StartOutput { output_id: rec })
        .await
        .expect("start rec");
    assert_event(
        &next_event(&mut stream).await,
        "OutputStateChanged",
        subscription::OUTPUTS,
        json!({
            "outputName": "Rec",
            "outputUuid": rec.as_uuid().to_string(),
            "outputState": "OBS_WEBSOCKET_OUTPUT_STARTING",
        }),
    );
    assert_event(
        &next_event(&mut stream).await,
        "RecordStateChanged",
        subscription::OUTPUTS,
        json!({
            "outputActive": false,
            "outputState": "OBS_WEBSOCKET_OUTPUT_STARTING",
            "outputPath": null,
        }),
    );
    assert_silent(&mut stream).await;

    bed.shutdown().await;
}

#[tokio::test]
async fn primary_fallback_and_non_primary_outputs() {
    let bed = TestBed::spawn().await;
    let mut stream = connect_subscribed(bed.addr, Some(subscription::OUTPUTS)).await;

    // No Rtmp output: the first Srt output becomes the stream primary.
    let srt = add_output(&bed.app, OutputKind::Srt, "SRT").await;
    bed.app
        .dispatch(Command::StartOutput { output_id: srt })
        .await
        .expect("start srt");
    assert_eq!(
        next_event(&mut stream).await["eventType"],
        "OutputStateChanged"
    );
    assert_event(
        &next_event(&mut stream).await,
        "StreamStateChanged",
        subscription::OUTPUTS,
        json!({"outputActive": false, "outputState": "OBS_WEBSOCKET_OUTPUT_STARTING"}),
    );

    // A virtual camera is neither a stream nor a record primary, and with
    // no Recording output there is no record singleton: OutputStateChanged
    // only.
    let vcam = add_output(&bed.app, OutputKind::VirtualCamera, "VCam").await;
    bed.app
        .dispatch(Command::StartOutput { output_id: vcam })
        .await
        .expect("start vcam");
    assert_event(
        &next_event(&mut stream).await,
        "OutputStateChanged",
        subscription::OUTPUTS,
        json!({
            "outputName": "VCam",
            "outputUuid": vcam.as_uuid().to_string(),
            "outputState": "OBS_WEBSOCKET_OUTPUT_STARTING",
        }),
    );
    assert_silent(&mut stream).await;

    bed.shutdown().await;
}

#[tokio::test]
async fn unsubscribed_categories_are_not_delivered() {
    let bed = TestBed::spawn().await;
    let mut stream = connect_subscribed(bed.addr, Some(subscription::SCENES)).await;

    // Input and output changes are gated out entirely.
    let source = add_source(&bed.app, SourceKind::Color, "Mic").await;
    bed.app
        .dispatch(Command::SetSourceMuted {
            source_id: source,
            muted: true,
        })
        .await
        .expect("mute");
    let output = add_output(&bed.app, OutputKind::Rtmp, "Twitch").await;
    bed.app
        .dispatch(Command::StartOutput { output_id: output })
        .await
        .expect("start");
    assert_silent(&mut stream).await;

    // Scene changes still arrive.
    let main = add_scene(&bed.app, "Main").await;
    assert_event(
        &next_event(&mut stream).await,
        "SceneCreated",
        subscription::SCENES,
        json!({"sceneName": "Main", "sceneUuid": main.as_uuid().to_string(), "isGroup": false}),
    );
    assert_eq!(
        next_event(&mut stream).await["eventType"],
        "CurrentProgramSceneChanged"
    );
    assert_silent(&mut stream).await;

    bed.shutdown().await;
}

#[tokio::test]
async fn reidentify_narrows_and_widens_mid_session() {
    let bed = TestBed::spawn().await;
    // Default mask: All (category bits).
    let mut stream = connect_subscribed(bed.addr, None).await;

    let _a = add_scene(&bed.app, "A").await;
    assert_eq!(next_event(&mut stream).await["eventType"], "SceneCreated");
    assert_eq!(
        next_event(&mut stream).await["eventType"],
        "CurrentProgramSceneChanged"
    );

    // Narrow to nothing: no events at all.
    reidentify(&mut stream, subscription::NONE).await;
    let b = add_scene(&bed.app, "B").await;
    let mic = add_source(&bed.app, SourceKind::Color, "Mic").await;
    assert_silent(&mut stream).await;

    // Widen to Inputs only: input events arrive, scene events do not.
    reidentify(&mut stream, subscription::INPUTS).await;
    bed.app
        .dispatch(Command::SetSourceMuted {
            source_id: mic,
            muted: true,
        })
        .await
        .expect("mute");
    assert_event(
        &next_event(&mut stream).await,
        "InputMuteStateChanged",
        subscription::INPUTS,
        json!({
            "inputName": "Mic",
            "inputUuid": mic.as_uuid().to_string(),
            "inputMuted": true,
        }),
    );

    // Widen to Scenes: entities created while unsubscribed were re-seeded
    // from the snapshot, so the removal still resolves the name.
    reidentify(&mut stream, subscription::SCENES).await;
    bed.app
        .dispatch(Command::RemoveScene { scene_id: b })
        .await
        .expect("remove scene");
    assert_event(
        &next_event(&mut stream).await,
        "SceneRemoved",
        subscription::SCENES,
        json!({"sceneName": "B", "sceneUuid": b.as_uuid().to_string(), "isGroup": false}),
    );
    assert_silent(&mut stream).await;

    bed.shutdown().await;
}

/// Numeric `sceneItemId` coherence (ADR-0020 §c): the number minted by the
/// request path (CreateSceneItem/GetSceneItemList responses) is the number
/// the event stream reports, including on the removal event — even though
/// `RemoveSceneItem`'s handler evicts the number eagerly, before the
/// session's event pipe sees the `ItemRemoved` domain event.
#[tokio::test]
async fn scene_item_ids_match_between_requests_and_events() {
    let bed = TestBed::spawn().await;
    let mut stream = connect_subscribed(bed.addr, Some(subscription::ALL)).await;

    let _mic = add_source(&bed.app, SourceKind::Color, "Mic").await;
    let _main = add_scene(&bed.app, "Main").await;
    // Drain the creation events so later assertions start clean.
    assert_eq!(next_event(&mut stream).await["eventType"], "InputCreated");
    assert_eq!(next_event(&mut stream).await["eventType"], "SceneCreated");
    assert_eq!(
        next_event(&mut stream).await["eventType"],
        "CurrentProgramSceneChanged"
    );

    // The request path mints the number.
    let (response, mut pending) = request(
        &mut stream,
        "CreateSceneItem",
        "r1",
        json!({"sceneName": "Main", "sourceName": "Mic"}),
    )
    .await;
    assert_eq!(response["requestStatus"]["code"], 100, "{response}");
    let number = response["responseData"]["sceneItemId"]
        .as_u64()
        .expect("CreateSceneItem responseData.sceneItemId");

    // The creation event (interleaved with or following the response)
    // reports the same number.
    let created = next_event_from(&mut pending, &mut stream).await;
    assert_eq!(created["eventType"], "SceneItemCreated");
    assert_eq!(
        created["eventData"]["sceneItemId"],
        json!(number),
        "event and response agree: {created}"
    );
    assert!(pending.is_empty());

    // Enumeration agrees too.
    let (response, pending) = request(
        &mut stream,
        "GetSceneItemList",
        "r2",
        json!({"sceneName": "Main"}),
    )
    .await;
    assert!(pending.is_empty(), "a query publishes no events");
    assert_eq!(
        response["responseData"]["sceneItems"][0]["sceneItemId"],
        json!(number),
        "{response}"
    );

    // Toggling via the request path emits SceneItemEnableStateChanged with
    // the same number.
    let (response, mut pending) = request(
        &mut stream,
        "SetSceneItemEnabled",
        "r3",
        json!({"sceneName": "Main", "sceneItemId": number, "sceneItemEnabled": false}),
    )
    .await;
    assert_eq!(response["requestStatus"]["code"], 100, "{response}");
    let enable = next_event_from(&mut pending, &mut stream).await;
    assert_eq!(enable["eventType"], "SceneItemEnableStateChanged");
    assert_eq!(enable["eventData"]["sceneItemId"], json!(number));
    assert_eq!(enable["eventData"]["sceneItemEnabled"], json!(false));

    // Removal: the request handler evicts the number eagerly (before the
    // event pipe translates ItemRemoved), yet the removal event still
    // carries it.
    let (response, mut pending) = request(
        &mut stream,
        "RemoveSceneItem",
        "r4",
        json!({"sceneName": "Main", "sceneItemId": number}),
    )
    .await;
    assert_eq!(response["requestStatus"]["code"], 100, "{response}");
    let removed = next_event_from(&mut pending, &mut stream).await;
    assert_eq!(removed["eventType"], "SceneItemRemoved");
    assert_eq!(
        removed["eventData"]["sceneItemId"],
        json!(number),
        "removal event survives eager eviction: {removed}"
    );
    assert_silent(&mut stream).await;

    bed.shutdown().await;
}
