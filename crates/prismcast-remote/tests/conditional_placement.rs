//! Atomic conditional scene placement edits (ADR-0026, CORE-007) through both
//! actual native transports: real Unix sockets and loopback WebSockets.
use prismcast_app::{AppHandle, CoreConfig};
use prismcast_core::{Command, SourceKind};
use prismcast_protocol::{
    data::{Anchor, PlacementExpectation, StateSnapshot, Transform, Vec2},
    error::ErrorKind,
    event::{SceneEvent, WireEvent},
    handshake::Permission,
    request::RequestKind,
    response::{RequestResponse, ResponseData},
    subscription::{EventCategory, Subscription, SubscriptionSet},
};
use prismcast_remote::{
    AuthConfig, IpcClient, IpcClientConfig, IpcServer, IpcServerConfig, WsClient, WsClientConfig,
    WsServer, WsServerConfig,
};
use std::{
    net::{Ipv4Addr, SocketAddr},
    path::PathBuf,
    time::Duration,
};
use uuid::Uuid;

const TIMEOUT: Duration = Duration::from_secs(3);

enum Client {
    Ipc(IpcClient),
    Ws(WsClient),
}

impl Client {
    async fn request(&mut self, kind: RequestKind) -> RequestResponse {
        match self {
            Self::Ipc(c) => c.request(kind).await.unwrap(),
            Self::Ws(c) => c.request(kind).await.unwrap(),
        }
    }
    async fn success(&mut self, kind: RequestKind) -> ResponseData {
        let response = self.request(kind).await;
        assert!(response.status.ok, "{:?}", response.status);
        response.data.unwrap()
    }
    async fn event(&mut self) -> prismcast_protocol::event::EventMessage {
        tokio::time::timeout(TIMEOUT, async {
            match self {
                Self::Ipc(c) => c.next_event().await.unwrap(),
                Self::Ws(c) => c.next_event().await.unwrap(),
            }
        })
        .await
        .unwrap()
    }
    async fn close(self) {
        match self {
            Self::Ipc(c) => c.close().await,
            Self::Ws(c) => c.close().await,
        }
    }
}

enum Server {
    Ipc(IpcServer),
    Ws(WsServer),
}

struct Bed {
    app: AppHandle,
    server: Server,
    client: Client,
    dir: PathBuf,
}

impl Bed {
    async fn spawn(ws: bool, permissions: Vec<Permission>) -> Self {
        let app = AppHandle::spawn(CoreConfig::default());
        let dir = std::env::temp_dir().join(format!("prismcast-conditional-{}", Uuid::new_v4()));
        let auth = AuthConfig::token("conditional-test", permissions);
        let subscriptions = Some(SubscriptionSet {
            entries: vec![Subscription {
                category: EventCategory::Scene,
                entity_ids: vec![],
                throttle_ms: None,
            }],
        });
        let (server, client) = if ws {
            let server = WsServer::bind(
                app.clone(),
                WsServerConfig {
                    enabled: true,
                    bind: SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
                    auth,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
            let client = WsClient::connect_with(
                server.local_addr(),
                WsClientConfig {
                    token: Some("conditional-test".into()),
                    subscriptions,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
            (Server::Ws(server), Client::Ws(client))
        } else {
            let socket = dir.join("control.sock");
            let server = IpcServer::bind(
                app.clone(),
                IpcServerConfig {
                    socket_path: Some(socket.clone()),
                    auth,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
            let client = IpcClient::connect_with(
                socket,
                IpcClientConfig {
                    token: Some("conditional-test".into()),
                    subscriptions,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
            (Server::Ipc(server), Client::Ipc(client))
        };
        Self {
            app,
            server,
            client,
            dir,
        }
    }

    async fn shutdown(self) {
        self.client.close().await;
        match self.server {
            Server::Ipc(s) => s.shutdown().await,
            Server::Ws(s) => s.shutdown().await,
        }
        self.app.shutdown().await;
        let _ = std::fs::remove_dir_all(self.dir);
    }

    async fn snapshot(&mut self) -> StateSnapshot {
        match self.client.success(RequestKind::GetSnapshot).await {
            ResponseData::Snapshot { snapshot } => *snapshot,
            other => panic!("{other:?}"),
        }
    }

    /// Seeds scene + source + item over the wire and drains the pending scene
    /// events (`added`, `current_changed`, `item_added`).
    async fn seed_via_wire(&mut self) -> (Uuid, Uuid, Uuid) {
        let scene = match self
            .client
            .success(RequestKind::AddScene {
                name: "Scene".into(),
            })
            .await
        {
            ResponseData::SceneCreated { scene_id } => scene_id,
            other => panic!("{other:?}"),
        };
        let source = match self
            .client
            .success(RequestKind::AddSource {
                kind: prismcast_protocol::data::SourceKind::TestPattern,
                name: "pattern".into(),
            })
            .await
        {
            ResponseData::SourceCreated { source_id } => source_id,
            other => panic!("{other:?}"),
        };
        let item = match self
            .client
            .success(RequestKind::AddSceneItem {
                scene_id: scene,
                source_id: source,
            })
            .await
        {
            ResponseData::SceneItemCreated { item_id } => item_id,
            other => panic!("{other:?}"),
        };
        for expected in ["added", "current_changed", "item_added"] {
            let event = self.client.event().await;
            let tag = match &event.event {
                WireEvent::Scene(SceneEvent::Added { .. }) => "added",
                WireEvent::Scene(SceneEvent::CurrentChanged { .. }) => "current_changed",
                WireEvent::Scene(SceneEvent::ItemAdded { .. }) => "item_added",
                other => panic!("expected {expected}, got {other:?}"),
            };
            assert_eq!(tag, expected);
        }
        (scene, source, item)
    }
}

/// Builds the placement expectation entirely from the wire snapshot: the
/// item's transform/crop/bounds/locked, `current_scene`, the active profile's
/// id and video configuration, and the item source's runtime dimensions
/// (`None` in these tests — there is no capture runtime, and `None` expects
/// absent dimensions).
fn expectation_from_snapshot(
    snapshot: &StateSnapshot,
    scene_id: Uuid,
    item_id: Uuid,
) -> PlacementExpectation {
    let scene = snapshot
        .scenes
        .iter()
        .find(|s| s.id == scene_id)
        .unwrap_or_else(|| panic!("scene {scene_id} in snapshot"));
    let item = scene
        .items
        .iter()
        .find(|i| i.id == item_id)
        .unwrap_or_else(|| panic!("item {item_id} in snapshot"));
    let active_profile = snapshot.active_profile.expect("active profile");
    let video = snapshot
        .profiles
        .iter()
        .find(|p| p.id == active_profile)
        .expect("active profile in snapshot")
        .video;
    let source_dimensions = snapshot
        .source_runtime
        .iter()
        .find(|entry| entry.source_id == item.source_id)
        .and_then(|entry| entry.runtime.dimensions);
    PlacementExpectation {
        current_scene: snapshot.current_scene.expect("current scene"),
        active_profile,
        video,
        transform: item.transform,
        crop: item.crop,
        bounds: item.bounds,
        locked: item.locked,
        source_dimensions,
    }
}

fn moved_transform() -> Transform {
    Transform {
        position: Vec2 { x: 100.0, y: 50.0 },
        scale: Vec2 { x: 1.0, y: 1.0 },
        rotation: 15.0,
        anchor: Anchor::TopLeft,
    }
}

fn item_transform(snapshot: &StateSnapshot, scene_id: Uuid, item_id: Uuid) -> Transform {
    snapshot
        .scenes
        .iter()
        .find(|s| s.id == scene_id)
        .and_then(|s| s.items.iter().find(|i| i.id == item_id))
        .expect("item in snapshot")
        .transform
}

#[tokio::test]
async fn conditional_transform_success_over_unix_and_websocket() {
    for ws in [false, true] {
        let mut bed = Bed::spawn(ws, vec![Permission::Read, Permission::ControlScenes]).await;
        if let ResponseData::Version {
            available_requests, ..
        } = bed.client.success(RequestKind::GetVersion).await
        {
            assert!(available_requests
                .iter()
                .any(|s| s == "set_scene_item_transform_if"));
        } else {
            panic!("version response");
        }
        let (scene, _source, item) = bed.seed_via_wire().await;
        let expect = expectation_from_snapshot(&bed.snapshot().await, scene, item);
        let transform = moved_transform();
        let response = bed
            .client
            .request(RequestKind::SetSceneItemTransformIf {
                scene_id: scene,
                item_id: item,
                transform,
                expect,
            })
            .await;
        assert_eq!(response.request_type, "set_scene_item_transform_if");
        assert!(response.status.ok, "{:?}", response.status);
        assert_eq!(response.data, Some(ResponseData::Empty));
        // An ordinary item_updated event carries the committed transform.
        let event = bed.client.event().await;
        assert_eq!(event.category, EventCategory::Scene);
        match event.event {
            WireEvent::Scene(SceneEvent::ItemUpdated {
                scene_id,
                item: updated,
            }) => {
                assert_eq!(scene_id, scene);
                assert_eq!(updated.id, item);
                assert_eq!(updated.transform, transform);
            }
            other => panic!("expected item_updated, got {other:?}"),
        }
        // The snapshot reflects the change.
        let snapshot = bed.snapshot().await;
        assert_eq!(item_transform(&snapshot, scene, item), transform);
        bed.shutdown().await;
    }
}

#[tokio::test]
async fn stale_conditional_transform_conflicts_over_unix_and_websocket() {
    for ws in [false, true] {
        let mut bed = Bed::spawn(ws, vec![Permission::Read, Permission::ControlScenes]).await;
        let (scene, _source, item) = bed.seed_via_wire().await;
        let expect = expectation_from_snapshot(&bed.snapshot().await, scene, item);
        // A concurrent unconditional edit commits newer state.
        let newer = Transform {
            position: Vec2 { x: 5.0, y: 5.0 },
            scale: Vec2 { x: 1.0, y: 1.0 },
            rotation: 0.0,
            anchor: Anchor::TopLeft,
        };
        bed.client
            .success(RequestKind::SetSceneItemTransform {
                scene_id: scene,
                item_id: item,
                transform: newer,
            })
            .await;
        bed.client.event().await; // item_updated for the unconditional edit
                                  // The conditional edit computed from the stale basis is rejected.
        let response = bed
            .client
            .request(RequestKind::SetSceneItemTransformIf {
                scene_id: scene,
                item_id: item,
                transform: moved_transform(),
                expect,
            })
            .await;
        assert!(!response.status.ok);
        let error = response.status.error.expect("error status");
        assert_eq!(error.kind, ErrorKind::StateConflict);
        assert_eq!(error.code, 500);
        assert_eq!(error.field.as_deref(), Some("expect.transform"));
        assert!(response.data.is_none());
        // The newer value survived; the rejection changed nothing.
        let snapshot = bed.snapshot().await;
        assert_eq!(item_transform(&snapshot, scene, item), newer);
        bed.shutdown().await;
    }
}

#[tokio::test]
async fn conditional_transform_requires_control_scenes_over_unix_and_websocket() {
    for ws in [false, true] {
        let mut bed = Bed::spawn(ws, vec![Permission::Read]).await;
        // Seed directly through the core: the read-only session cannot mutate.
        let scene = bed
            .app
            .dispatch(Command::AddScene {
                name: "Scene".into(),
            })
            .await
            .unwrap();
        let scene_id = scene
            .events
            .iter()
            .find_map(|e| match e {
                prismcast_core::Event::Scene(prismcast_core::event::SceneEvent::Added {
                    scene_id,
                    ..
                }) => Some(*scene_id.as_uuid()),
                _ => None,
            })
            .expect("scene id");
        let source = bed
            .app
            .dispatch(Command::AddSource {
                kind: SourceKind::TestPattern,
                name: "pattern".into(),
            })
            .await
            .unwrap();
        let source_id = source
            .events
            .iter()
            .find_map(|e| match e {
                prismcast_core::Event::Source(prismcast_core::event::SourceEvent::Added {
                    source,
                }) => Some(*source.id.as_uuid()),
                _ => None,
            })
            .expect("source id");
        bed.app
            .dispatch(Command::AddSceneItem {
                scene_id: prismcast_core::SceneId::from(scene_id),
                source_id: prismcast_core::SourceId::from(source_id),
            })
            .await
            .unwrap();
        let snapshot = bed.snapshot().await;
        let item = snapshot
            .scenes
            .iter()
            .find(|s| s.id == scene_id)
            .and_then(|s| s.items.first())
            .expect("seeded item")
            .id;
        // Drain the seeded scene events so the stream stays aligned.
        for _ in 0..3 {
            bed.client.event().await;
        }
        let expect = expectation_from_snapshot(&snapshot, scene_id, item);
        let original = item_transform(&snapshot, scene_id, item);
        let response = bed
            .client
            .request(RequestKind::SetSceneItemTransformIf {
                scene_id,
                item_id: item,
                transform: moved_transform(),
                expect,
            })
            .await;
        assert!(!response.status.ok);
        let error = response.status.error.expect("error status");
        assert_eq!(error.kind, ErrorKind::Forbidden);
        assert_eq!(error.code, 800);
        // State is untouched.
        let snapshot = bed.snapshot().await;
        assert_eq!(item_transform(&snapshot, scene_id, item), original);
        bed.shutdown().await;
    }
}

#[tokio::test]
async fn conditional_transform_is_rejected_as_transaction_member_over_unix_and_websocket() {
    for ws in [false, true] {
        let mut bed = Bed::spawn(ws, vec![Permission::Read, Permission::ControlScenes]).await;
        let (scene, _source, item) = bed.seed_via_wire().await;
        let snapshot = bed.snapshot().await;
        let expect = expectation_from_snapshot(&snapshot, scene, item);
        let original = item_transform(&snapshot, scene, item);
        let response = bed
            .client
            .request(RequestKind::Transaction {
                commands: vec![RequestKind::SetSceneItemTransformIf {
                    scene_id: scene,
                    item_id: item,
                    transform: moved_transform(),
                    expect,
                }],
            })
            .await;
        assert!(!response.status.ok);
        let error = response.status.error.expect("error status");
        assert_eq!(error.kind, ErrorKind::InvalidRequest);
        assert_eq!(error.code, 200);
        // Structural rejection happens before any member executes.
        let snapshot = bed.snapshot().await;
        assert_eq!(item_transform(&snapshot, scene, item), original);
        bed.shutdown().await;
    }
}
