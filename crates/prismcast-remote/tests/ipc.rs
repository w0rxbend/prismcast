//! End-to-end proof of the layered architecture (IPC-001): a real
//! `CoreActor` behind a real `IpcServer` on a tempdir socket, driven by the
//! shared `IpcClient` and by raw-frame clients for protocol-violation cases.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use tokio::net::UnixStream;
use uuid::Uuid;

use prismcast_app::{AppHandle, CoreConfig};
use prismcast_protocol::error::ErrorKind;
use prismcast_protocol::event::WireEvent;
use prismcast_protocol::handshake::Permission;
use prismcast_protocol::request::RequestKind;
use prismcast_protocol::response::ResponseData;
use prismcast_protocol::subscription::{EventCategory, Subscription, SubscriptionSet};
use prismcast_remote::auth::AuthConfig;
use prismcast_remote::client::{ClientError, IpcClient, IpcClientConfig};
use prismcast_remote::codec;
use prismcast_remote::{IpcServer, IpcServerConfig};

const TIMEOUT: Duration = Duration::from_secs(5);

struct TestBed {
    app: AppHandle,
    server: IpcServer,
    dir: PathBuf,
    socket: PathBuf,
}

async fn spawn_bed(auth: AuthConfig) -> TestBed {
    let dir = std::env::temp_dir().join(format!("prismcast-ipc-test-{}", Uuid::new_v4()));
    let socket = dir.join("control.sock");
    let app = AppHandle::spawn(CoreConfig::default());
    let server = IpcServer::bind(
        app.clone(),
        IpcServerConfig {
            socket_path: Some(socket.clone()),
            auth,
            ..IpcServerConfig::default()
        },
    )
    .await
    .expect("bind server");
    TestBed {
        app,
        server,
        dir,
        socket,
    }
}

impl TestBed {
    async fn client(&self) -> IpcClient {
        tokio::time::timeout(TIMEOUT, IpcClient::connect(&self.socket))
            .await
            .expect("connect timed out")
            .expect("connect")
    }

    async fn client_with(&self, config: IpcClientConfig) -> Result<IpcClient, ClientError> {
        tokio::time::timeout(TIMEOUT, IpcClient::connect_with(&self.socket, config))
            .await
            .expect("connect timed out")
    }

    async fn add_scene(client: &mut IpcClient, name: &str) -> Uuid {
        match client
            .request_data(RequestKind::AddScene { name: name.into() })
            .await
            .expect("add_scene")
        {
            ResponseData::SceneCreated { scene_id } => scene_id,
            other => panic!("unexpected add_scene response: {other:?}"),
        }
    }

    async fn shutdown(self) {
        self.server.shutdown().await;
        self.app.shutdown().await;
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[tokio::test]
async fn ping_status_and_snapshot_roundtrip() {
    let bed = spawn_bed(AuthConfig::allow_local()).await;
    let mut client = bed.client().await;
    assert_eq!(client.negotiated_protocol_version, 1);
    assert!(client.permissions.contains(&Permission::Admin));

    let version = client
        .request_data(RequestKind::GetVersion)
        .await
        .expect("get_version");
    match version {
        ResponseData::Version {
            prismcast_version,
            protocol_version,
            available_requests,
        } => {
            assert_eq!(prismcast_version, env!("CARGO_PKG_VERSION"));
            assert_eq!(protocol_version, 1);
            assert!(available_requests.contains(&"add_scene".to_string()));
            assert!(available_requests.contains(&"get_snapshot".to_string()));
        }
        other => panic!("unexpected: {other:?}"),
    }

    let snapshot = client
        .request_data(RequestKind::GetSnapshot)
        .await
        .expect("get_snapshot");
    match snapshot {
        ResponseData::Snapshot { snapshot } => {
            assert!(snapshot.scenes.is_empty());
            assert!(snapshot.current_scene.is_none());
            // Fresh state is seeded with one profile and one collection.
            assert_eq!(snapshot.profiles.len(), 1);
            assert_eq!(snapshot.collections.len(), 1);
        }
        other => panic!("unexpected: {other:?}"),
    }

    client.close().await;
    bed.shutdown().await;
}

#[tokio::test]
async fn scene_commands_and_queries_roundtrip() {
    let bed = spawn_bed(AuthConfig::allow_local()).await;
    let mut client = bed.client().await;

    let main = TestBed::add_scene(&mut client, "Main").await;
    let intermission = TestBed::add_scene(&mut client, "Intermission").await;

    // First scene becomes current automatically; switch to the second.
    client
        .request_data(RequestKind::SetCurrentScene {
            scene_id: intermission,
        })
        .await
        .expect("set_current_scene");

    let scenes = client
        .request_data(RequestKind::ListScenes)
        .await
        .expect("list_scenes");
    match scenes {
        ResponseData::SceneList { scenes } => {
            let names: Vec<&str> = scenes.iter().map(|s| s.name.as_str()).collect();
            assert_eq!(names, ["Main", "Intermission"]);
        }
        other => panic!("unexpected: {other:?}"),
    }

    let scene = client
        .request_data(RequestKind::GetScene { scene_id: main })
        .await
        .expect("get_scene");
    match scene {
        ResponseData::Scene { scene } => assert_eq!(scene.name, "Main"),
        other => panic!("unexpected: {other:?}"),
    }

    let snapshot = client
        .request_data(RequestKind::GetSnapshot)
        .await
        .expect("get_snapshot");
    match snapshot {
        ResponseData::Snapshot { snapshot } => {
            assert_eq!(snapshot.current_scene, Some(intermission))
        }
        other => panic!("unexpected: {other:?}"),
    }

    // A missing entity is a structured 600 not-found error.
    let missing = client
        .request(RequestKind::GetScene {
            scene_id: Uuid::new_v4(),
        })
        .await
        .expect("get_scene missing");
    assert!(!missing.status.ok);
    let error = missing.status.error.expect("error payload");
    assert_eq!(error.code, 600);
    assert_eq!(error.kind, ErrorKind::NotFound);
    assert_eq!(error.field.as_deref(), Some("scene_id"));

    client.close().await;
    bed.shutdown().await;
}

#[tokio::test]
async fn events_reach_both_subscribed_clients() {
    let bed = spawn_bed(AuthConfig::allow_local()).await;
    // Default subscriptions (all standard categories) for both clients.
    let mut a = bed.client().await;
    let mut b = bed.client().await;

    let scene = TestBed::add_scene(&mut b, "Main").await;

    for client in [&mut a, &mut b] {
        let first = tokio::time::timeout(TIMEOUT, client.next_event())
            .await
            .expect("event timed out")
            .expect("event");
        assert_eq!(first.seq, 0);
        assert_eq!(first.category, EventCategory::Scene);
        assert!(matches!(
            &first.event,
            WireEvent::Scene(prismcast_protocol::event::SceneEvent::Added { scene_id, name })
            if *scene_id == scene && name == "Main"
        ));
        let second = tokio::time::timeout(TIMEOUT, client.next_event())
            .await
            .expect("event timed out")
            .expect("event");
        assert_eq!(second.seq, 1);
        assert!(matches!(
            &second.event,
            WireEvent::Scene(prismcast_protocol::event::SceneEvent::CurrentChanged { scene_id })
            if *scene_id == scene
        ));
    }

    a.close().await;
    b.close().await;
    bed.shutdown().await;
}

#[tokio::test]
async fn update_subscriptions_filters_categories_and_entities() {
    let bed = spawn_bed(AuthConfig::allow_local()).await;
    let mut a = bed.client().await;
    let mut b = bed.client().await;

    // Restrict A to the scene category only.
    let applied = a
        .update_subscriptions(SubscriptionSet {
            entries: vec![Subscription::category(EventCategory::Scene)],
        })
        .await
        .expect("update_subscriptions");
    assert_eq!(applied.entries.len(), 1);
    assert_eq!(applied.entries[0].category, EventCategory::Scene);

    // A source event must not reach A; the following scene event must.
    b.request_data(RequestKind::AddSource {
        kind: prismcast_protocol::data::SourceKind::Color,
        name: "bg".into(),
    })
    .await
    .expect("add_source");
    let scene = TestBed::add_scene(&mut b, "Main").await;

    let event = tokio::time::timeout(TIMEOUT, a.next_event())
        .await
        .expect("event timed out")
        .expect("event");
    assert!(
        matches!(
            &event.event,
            WireEvent::Scene(prismcast_protocol::event::SceneEvent::Added { scene_id, .. })
            if *scene_id == scene
        ),
        "first event must be the scene (source events are filtered): {event:?}"
    );

    // Drain the follow-up CurrentChanged (first scene becomes current).
    let follow_up = tokio::time::timeout(TIMEOUT, a.next_event())
        .await
        .expect("event timed out")
        .expect("event");
    assert!(matches!(
        &follow_up.event,
        WireEvent::Scene(prismcast_protocol::event::SceneEvent::CurrentChanged { .. })
    ));

    // Entity filter: subscribe A to a *different* scene only — no events.
    let other = Uuid::new_v4();
    let applied = a
        .update_subscriptions(SubscriptionSet {
            entries: vec![Subscription {
                category: EventCategory::Scene,
                entity_ids: vec![other],
                throttle_ms: None,
            }],
        })
        .await
        .expect("update_subscriptions");
    assert_eq!(applied.entries[0].entity_ids, vec![other]);
    TestBed::add_scene(&mut b, "Second").await;
    let none = tokio::time::timeout(Duration::from_millis(500), a.next_event()).await;
    assert!(none.is_err(), "entity-filtered session must stay silent");

    // An invalid set is rejected with 901 and the previous set stays active.
    let response = a
        .request(RequestKind::UpdateSubscriptions {
            subscriptions: SubscriptionSet {
                entries: vec![
                    Subscription::category(EventCategory::Scene),
                    Subscription::category(EventCategory::Scene),
                ],
            },
        })
        .await
        .expect("request");
    assert!(!response.status.ok);
    let error = response.status.error.expect("error");
    assert_eq!(error.code, 901);
    assert_eq!(error.kind, ErrorKind::InvalidSubscription);
    let current = a
        .request_data(RequestKind::GetSubscriptions)
        .await
        .expect("get_subscriptions");
    match current {
        ResponseData::Subscriptions { subscriptions } => {
            assert_eq!(subscriptions.entries.len(), 1);
            assert_eq!(subscriptions.entries[0].entity_ids, vec![other]);
        }
        other => panic!("unexpected: {other:?}"),
    }

    a.close().await;
    b.close().await;
    bed.shutdown().await;
}

#[tokio::test]
async fn throttle_coalesces_rapid_state_events() {
    let bed = spawn_bed(AuthConfig::allow_local()).await;
    let mut a = bed
        .client_with(IpcClientConfig {
            subscriptions: Some(SubscriptionSet {
                entries: vec![Subscription {
                    category: EventCategory::Scene,
                    entity_ids: Vec::new(),
                    throttle_ms: Some(200),
                }],
            }),
            ..IpcClientConfig::default()
        })
        .await
        .expect("connect");
    let mut b = bed.client().await;

    let scene = TestBed::add_scene(&mut b, "Main").await;
    b.request_data(RequestKind::RenameScene {
        scene_id: scene,
        name: "Renamed A".into(),
    })
    .await
    .expect("rename");
    b.request_data(RequestKind::RenameScene {
        scene_id: scene,
        name: "Renamed B".into(),
    })
    .await
    .expect("rename");

    // First event of the window is delivered immediately...
    let started = Instant::now();
    let first = tokio::time::timeout(TIMEOUT, a.next_event())
        .await
        .expect("event timed out")
        .expect("event");
    assert_eq!(first.seq, 0);
    assert!(matches!(
        &first.event,
        WireEvent::Scene(prismcast_protocol::event::SceneEvent::Added { .. })
    ));

    // ...the three following same-key events coalesce into the latest one,
    // delivered when the window expires.
    let second = tokio::time::timeout(TIMEOUT, a.next_event())
        .await
        .expect("event timed out")
        .expect("event");
    let elapsed = started.elapsed();
    assert_eq!(second.seq, 1);
    assert!(
        matches!(
            &second.event,
            WireEvent::Scene(prismcast_protocol::event::SceneEvent::Renamed { name, .. })
            if name == "Renamed B"
        ),
        "coalescing keeps the latest state: {second:?}"
    );
    assert!(
        elapsed >= Duration::from_millis(150),
        "throttled flush must wait out the 200ms window (took {elapsed:?})"
    );

    // The intermediate states were coalesced away: nothing else arrives.
    let none = tokio::time::timeout(Duration::from_millis(500), a.next_event()).await;
    assert!(none.is_err(), "coalesced events must not be delivered");

    a.close().await;
    b.close().await;
    bed.shutdown().await;
}

#[tokio::test]
async fn token_auth_grants_configured_permissions() {
    let bed = spawn_bed(AuthConfig::token("s3cret", vec![Permission::Read])).await;

    // Wrong token → closed with AuthenticationFailed (4009).
    let wrong = bed
        .client_with(IpcClientConfig {
            token: Some("wrong".into()),
            ..IpcClientConfig::default()
        })
        .await;
    match wrong {
        Err(ClientError::Closed { code, .. }) => assert_eq!(code, 4009),
        Err(other) => panic!("expected auth failure, got {other}"),
        Ok(_) => panic!("expected auth failure, got a session"),
    }

    // No token at all → same.
    let missing = bed.client_with(IpcClientConfig::default()).await;
    assert!(matches!(
        missing,
        Err(ClientError::Closed { code: 4009, .. })
    ));

    // Correct token → session with exactly [read].
    let mut client = bed
        .client_with(IpcClientConfig {
            token: Some("s3cret".into()),
            ..IpcClientConfig::default()
        })
        .await
        .expect("connect");
    assert_eq!(client.permissions, vec![Permission::Read]);

    // Queries are allowed; mutations are rejected with 800 forbidden.
    client
        .request_data(RequestKind::ListScenes)
        .await
        .expect("list_scenes");
    let rejected = client
        .request(RequestKind::AddScene {
            name: "nope".into(),
        })
        .await
        .expect("request");
    assert!(!rejected.status.ok);
    let error = rejected.status.error.expect("error");
    assert_eq!(error.code, 800);
    assert_eq!(error.kind, ErrorKind::Forbidden);

    client.close().await;
    bed.shutdown().await;
}

/// Raw-frame helpers for protocol-violation tests.
mod raw {
    use super::*;

    /// Performs the handshake with raw frames; returns after `identified`.
    pub async fn identify(stream: &mut UnixStream) {
        let _hello = read_value(stream).await;
        write_value(
            stream,
            serde_json::json!({"type": "identify", "data": {"protocol_version": 1}}),
        )
        .await;
        let identified = read_value(stream).await;
        assert_eq!(identified["type"], "identified");
    }

    pub async fn write_value(stream: &mut UnixStream, value: serde_json::Value) {
        let payload = codec::encode(&value).expect("encode");
        codec::write_frame(stream, &payload).await.expect("write");
    }

    pub async fn read_value(stream: &mut UnixStream) -> serde_json::Value {
        let payload = tokio::time::timeout(
            TIMEOUT,
            codec::read_frame(stream, codec::DEFAULT_MAX_FRAME_SIZE),
        )
        .await
        .expect("read timed out")
        .expect("read frame")
        .expect("frame present");
        codec::decode_value(&payload).expect("decode")
    }
}

#[tokio::test]
async fn unknown_request_type_gets_202_error_response() {
    let bed = spawn_bed(AuthConfig::allow_local()).await;
    let mut stream = UnixStream::connect(&bed.socket).await.expect("connect");
    raw::identify(&mut stream).await;

    raw::write_value(
        &mut stream,
        serde_json::json!({
            "type": "request",
            "data": {"request_id": "r-1", "request": "teleport"}
        }),
    )
    .await;
    let response = raw::read_value(&mut stream).await;
    assert_eq!(response["type"], "request_response");
    assert_eq!(response["data"]["request_id"], "r-1");
    assert_eq!(response["data"]["request_type"], "teleport");
    assert_eq!(response["data"]["status"]["ok"], false);
    assert_eq!(response["data"]["status"]["error"]["code"], 202);

    bed.shutdown().await;
}

#[tokio::test]
async fn oversized_frame_closes_with_decode_error() {
    let bed = spawn_bed(AuthConfig::allow_local()).await;
    let mut stream = UnixStream::connect(&bed.socket).await.expect("connect");
    let _hello = raw::read_value(&mut stream).await;

    // A frame header announcing more than the limit: rejected before the
    // payload is read, with a closing notice carrying 4002.
    tokio::io::AsyncWriteExt::write_all(
        &mut stream,
        &(codec::DEFAULT_MAX_FRAME_SIZE as u32 + 1).to_be_bytes(),
    )
    .await
    .expect("write prefix");
    let closing = raw::read_value(&mut stream).await;
    assert_eq!(closing["type"], "closing");
    assert_eq!(closing["data"]["code"], 4002);

    bed.shutdown().await;
}

#[tokio::test]
async fn request_before_identify_closes_with_not_identified() {
    let bed = spawn_bed(AuthConfig::allow_local()).await;
    let mut stream = UnixStream::connect(&bed.socket).await.expect("connect");
    let _hello = raw::read_value(&mut stream).await;
    raw::write_value(
        &mut stream,
        serde_json::json!({
            "type": "request",
            "data": {"request_id": "r-1", "request": "get_version"}
        }),
    )
    .await;
    let closing = raw::read_value(&mut stream).await;
    assert_eq!(closing["type"], "closing");
    assert_eq!(closing["data"]["code"], 4007);

    bed.shutdown().await;
}
