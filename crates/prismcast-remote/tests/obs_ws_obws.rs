//! obs-websocket 5.x adapter: conformance against `obws`, the reference Rust
//! client (OBSWS-001 acceptance item: "conformance test with the `obws`
//! crate: connect with password, GetVersion, list scenes, set program scene,
//! toggle mute").
//!
//! `obws` 0.15 (tokio-websockets transport, JSON) is exercised against a real
//! [`ObsWsServer`] socket exactly as the scripted tungstenite clients in
//! `tests/obs_ws*.rs` are — but through obws's typed API only, so a wire
//! incompatibility surfaces as a handshake failure, a deserialization error,
//! or an API status error.
//!
//! One obws-side adjustment is required and documented here (and in
//! `docs/protocols/obs-websocket-adapter.md`): obws verifies
//! `GetVersion.obsVersion >= 30.2` unless told otherwise, while the adapter
//! reports Prismcast's own crate version as `obsVersion` (there is no OBS
//! build behind it). The test therefore sets
//! `DangerousConnectConfig::skip_studio_version_check`. The
//! `obsWebSocketVersion` check is **not** skipped: the adapter advertises a
//! genuine "5.7.4".

use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use obws::client::{ConnectConfig, DangerousConnectConfig, DEFAULT_BROADCAST_CAPACITY};
use obws::requests::inputs::InputId;
use obws::requests::EventSubscription;

use prismcast_app::{AppHandle, CoreConfig};
use prismcast_core::event::{Event, SceneEvent};
use prismcast_core::id::{SceneId, SourceId};
use prismcast_core::source::SourceKind;
use prismcast_core::state::AppState;
use prismcast_core::Command;
use prismcast_protocol::handshake::Permission;
use prismcast_remote::auth::AuthConfig;
use prismcast_remote::obs_ws::{ObsWsServer, ObsWsServerConfig};

const TIMEOUT: Duration = Duration::from_secs(5);
const PASSWORD: &str = "hunter2";

struct TestBed {
    app: AppHandle,
    server: ObsWsServer,
    addr: SocketAddr,
}

async fn spawn_bed() -> TestBed {
    let app = AppHandle::spawn_with_state(AppState::new(), CoreConfig::default());
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

    /// Native command path (trusted local controller) for test setup and
    /// ground-truth assertions.
    async fn dispatch(&self, command: Command) -> Vec<Event> {
        self.app
            .dispatch(command)
            .await
            .expect("native dispatch")
            .events
    }

    async fn add_scene(&self, name: &str) -> SceneId {
        self.dispatch(Command::AddScene {
            name: name.to_string(),
        })
        .await
        .iter()
        .find_map(|event| match event {
            Event::Scene(SceneEvent::Added { scene_id, .. }) => Some(*scene_id),
            _ => None,
        })
        .expect("scene added")
    }

    async fn add_source(&self, kind: SourceKind, name: &str) -> SourceId {
        self.dispatch(Command::AddSource {
            kind,
            name: name.to_string(),
        })
        .await
        .iter()
        .find_map(|event| match event {
            Event::Source(prismcast_core::SourceEvent::Added { source }) => Some(source.id),
            _ => None,
        })
        .expect("source added")
    }
}

/// Connects an obws client with the password (see the module docs for why
/// `skip_studio_version_check` is set; the websocket-version check stays on).
async fn connect(addr: SocketAddr, password: &str) -> obws::error::Result<obws::Client> {
    obws::Client::connect_with_config(ConnectConfig {
        host: addr.ip().to_string(),
        port: addr.port(),
        dangerous: Some(DangerousConnectConfig {
            skip_studio_version_check: true,
            skip_websocket_version_check: false,
        }),
        password: Some(password),
        event_subscriptions: Some(EventSubscription::NONE),
        broadcast_capacity: DEFAULT_BROADCAST_CAPACITY,
        connect_timeout: TIMEOUT,
    })
    .await
}

#[tokio::test]
async fn obws_handshake_and_get_version() {
    let bed = spawn_bed().await;

    let mut client = connect(bed.addr, PASSWORD)
        .await
        .expect("obws identifies with the obs challenge-response");
    let version = client.general().version().await.expect("GetVersion");
    // `obs_web_socket_version` is a `semver::Version` in obws — it parsed —
    // and matches the adapter's advertised baseline.
    assert_eq!(version.obs_web_socket_version.to_string(), "5.7.4");
    assert_eq!(version.rpc_version, 1, "rpcVersion 1 policy");
    assert_eq!(version.platform, "linux");
    // The advertised set is drift-guarded against the implementation; obws
    // parses it into typed `Vec<String>`.
    for required in [
        "GetVersion",
        "GetSceneList",
        "CreateScene",
        "SetCurrentProgramScene",
        "ToggleInputMute",
    ] {
        assert!(
            version.available_requests.iter().any(|r| r == required),
            "{required} must be advertised: {:?}",
            version.available_requests
        );
    }
    // The advertised obsVersion is Prismcast's own (see module docs).
    assert_eq!(
        version.obs_studio_version.to_string(),
        env!("CARGO_PKG_VERSION")
    );

    client.disconnect().await;
    bed.shutdown().await;
}

#[tokio::test]
async fn obws_wrong_password_fails_handshake_with_4009() {
    let bed = spawn_bed().await;

    let Err(error) = connect(bed.addr, "wrong").await else {
        panic!("wrong password must fail the handshake");
    };
    match error {
        obws::error::Error::Handshake(obws::client::HandshakeError::ConnectionClosed(Some(
            details,
        ))) => {
            assert_eq!(u16::from(details.code), 4009, "AuthenticationFailed");
        }
        other => panic!("expected a 4009 handshake close, got {other}"),
    }

    bed.shutdown().await;
}

#[tokio::test]
async fn obws_scene_list_create_and_program_switch_match_core_state() {
    let bed = spawn_bed().await;
    let main_id = bed.add_scene("Main").await;

    let mut client = connect(bed.addr, PASSWORD).await.expect("connect");

    // GetSceneList: the seeded scene shows up and is the current program scene.
    let scenes = client.scenes().list().await.expect("GetSceneList");
    assert!(
        scenes.scenes.iter().any(|scene| scene.id.name == "Main"),
        "seeded scene listed: {:?}",
        scenes.scenes
    );
    let current = scenes
        .current_program_scene
        .as_ref()
        .expect("a current program scene exists after seeding");
    assert_eq!(current.name, "Main");
    assert_eq!(current.uuid, *main_id.as_uuid());

    // CreateScene through the typed API lands in the core with the returned UUID.
    let created = client.scenes().create("BRB").await.expect("CreateScene");
    let brb_id = bed
        .app
        .snapshot()
        .state()
        .scenes
        .values()
        .find(|scene| scene.name == "BRB")
        .map(|scene| scene.id)
        .expect("core state has the BRB scene");
    assert_eq!(created, *brb_id.as_uuid(), "sceneUuid round-trips");

    // SetCurrentProgramScene switches the core's current scene.
    client
        .scenes()
        .set_current_program_scene("BRB")
        .await
        .expect("SetCurrentProgramScene");
    assert_eq!(
        bed.app.snapshot().state().current_scene,
        Some(brb_id),
        "program scene switched in the core"
    );

    // ...and back, so both directions through obws are exercised.
    client
        .scenes()
        .set_current_program_scene("Main")
        .await
        .expect("SetCurrentProgramScene back");
    assert_eq!(bed.app.snapshot().state().current_scene, Some(main_id));

    client.disconnect().await;
    bed.shutdown().await;
}

#[tokio::test]
async fn obws_input_mute_toggle_matches_mixer_state() {
    let bed = spawn_bed().await;
    let source_id = bed.add_source(SourceKind::Color, "Mic").await;

    let mut client = connect(bed.addr, PASSWORD).await.expect("connect");

    // GetInputList surfaces the seeded source with the adapter's inputKind.
    let inputs = client.inputs().list(None).await.expect("GetInputList");
    let mic = inputs
        .iter()
        .find(|input| input.id.name == "Mic")
        .expect("seeded input listed");
    assert_eq!(mic.kind, "color_source");

    assert!(!client
        .inputs()
        .muted(InputId::Name("Mic"))
        .await
        .expect("GetInputMute"));
    let toggled = client
        .inputs()
        .toggle_mute(InputId::Name("Mic"))
        .await
        .expect("ToggleInputMute");
    assert!(toggled, "inputMutedToggled reports the new state");
    assert!(
        bed.app
            .snapshot()
            .state()
            .audio
            .mixer_state(source_id)
            .muted,
        "the core mixer is muted after the obs toggle"
    );

    let toggled = client
        .inputs()
        .toggle_mute(InputId::Name("Mic"))
        .await
        .expect("ToggleInputMute back");
    assert!(!toggled);
    assert!(
        !bed.app
            .snapshot()
            .state()
            .audio
            .mixer_state(source_id)
            .muted
    );

    client.disconnect().await;
    bed.shutdown().await;
}
