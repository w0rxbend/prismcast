//! Canonical history commands through both actual native transports.
use prismcast_app::{AppHandle, CoreConfig};
use prismcast_core::{Command, SceneId, SourceKind};
use prismcast_protocol::{
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
        let dir = std::env::temp_dir().join(format!("prismcast-history-{}", Uuid::new_v4()));
        let auth = AuthConfig::token("history-test", permissions);
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
                    token: Some("history-test".into()),
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
                    token: Some("history-test".into()),
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
    async fn seed_scene(&self) -> SceneId {
        self.app
            .dispatch(Command::AddScene {
                name: "Original".into(),
            })
            .await
            .unwrap();
        self.app.snapshot().state().current_scene.unwrap()
    }
    async fn unchanged_error(&mut self, request: RequestKind, kind: ErrorKind) {
        let before = self.app.snapshot();
        let response = self.client.request(request).await;
        assert!(!response.status.ok);
        assert_eq!(response.status.error.unwrap().kind, kind);
        assert!(response.data.is_none());
        let after = self.app.snapshot();
        assert_eq!(before.revision(), after.revision());
        assert_eq!(before.state(), after.state());
        assert_eq!(before.history(), after.history());
    }
}

#[tokio::test]
async fn canonical_history_events_and_snapshots_match_over_unix_and_websocket() {
    for ws in [false, true] {
        let mut bed = Bed::spawn(ws, vec![Permission::Read, Permission::ControlScenes]).await;
        if let ResponseData::Version {
            available_requests, ..
        } = bed.client.success(RequestKind::GetVersion).await
        {
            assert!(available_requests.iter().any(|s| s == "undo"));
            assert!(available_requests.iter().any(|s| s == "redo"));
        } else {
            panic!("version response");
        }
        let scene = match bed
            .client
            .success(RequestKind::AddScene {
                name: "Original".into(),
            })
            .await
        {
            ResponseData::SceneCreated { scene_id } => scene_id,
            other => panic!("{other:?}"),
        };
        assert_eq!(bed.client.event().await.seq, 0);
        assert_eq!(bed.client.event().await.seq, 1);
        for (index, request, name) in [
            (
                2,
                RequestKind::RenameScene {
                    scene_id: scene,
                    name: "Edited".into(),
                },
                "Edited",
            ),
            (3, RequestKind::Undo, "Original"),
            (4, RequestKind::Redo, "Edited"),
        ] {
            let tag = request.tag();
            let response = bed.client.request(request).await;
            assert_eq!(response.request_type, tag);
            assert!(response.status.ok);
            assert_eq!(response.data, Some(ResponseData::Empty));
            let event = bed.client.event().await;
            assert_eq!(event.seq, index);
            assert_eq!(event.category, EventCategory::Scene);
            assert!(
                matches!(event.event,WireEvent::Scene(SceneEvent::Renamed{scene_id,name:ref actual}) if scene_id==scene && actual==name)
            );
            let snapshot = bed.client.success(RequestKind::GetSnapshot).await;
            match snapshot {
                ResponseData::Snapshot { snapshot } => assert_eq!(
                    snapshot.scenes.iter().find(|s| s.id == scene).unwrap().name,
                    name
                ),
                other => panic!("{other:?}"),
            }
        }
        bed.shutdown().await;
    }
}

#[tokio::test]
async fn rejected_replay_preserves_both_stacks_over_unix_and_websocket() {
    for ws in [false, true] {
        for (permissions, mixed) in [
            (vec![Permission::Read], false),
            (vec![Permission::Read, Permission::ControlAudio], false),
            (vec![Permission::Read, Permission::ControlScenes], true),
        ] {
            let mut bed = Bed::spawn(ws, permissions).await;
            let scene = bed.seed_scene().await;
            let rename = Command::RenameScene {
                scene_id: scene,
                name: "Edited".into(),
            };
            let edit = if mixed {
                bed.app
                    .dispatch(Command::AddSource {
                        kind: SourceKind::TestPattern,
                        name: "tone".into(),
                    })
                    .await
                    .unwrap();
                let source = bed
                    .app
                    .snapshot()
                    .state()
                    .sources
                    .values()
                    .next()
                    .unwrap()
                    .id;
                Command::Transaction {
                    commands: vec![
                        rename,
                        Command::SetSourceMuted {
                            source_id: source,
                            muted: true,
                        },
                    ],
                }
            } else {
                rename
            };
            bed.app.dispatch(edit).await.unwrap();
            bed.unchanged_error(RequestKind::Undo, ErrorKind::Forbidden)
                .await;
            bed.app.undo().await.unwrap();
            bed.unchanged_error(RequestKind::Redo, ErrorKind::Forbidden)
                .await;
            bed.app.redo().await.unwrap();
            assert_eq!(
                bed.app.snapshot().state().scene(scene).unwrap().name,
                "Edited"
            );
            bed.shutdown().await;
        }
    }
}

#[tokio::test]
async fn empty_open_group_and_atomic_history_rejections_are_unchanged_on_both_transports() {
    for ws in [false, true] {
        let mut bed = Bed::spawn(ws, vec![Permission::Admin]).await;
        bed.unchanged_error(RequestKind::Undo, ErrorKind::InvalidField)
            .await;
        bed.unchanged_error(RequestKind::Redo, ErrorKind::InvalidField)
            .await;
        let scene = bed.seed_scene().await;
        bed.app
            .dispatch(Command::RenameScene {
                scene_id: scene,
                name: "Edited".into(),
            })
            .await
            .unwrap();
        bed.app
            .begin_transaction("open local gesture")
            .await
            .unwrap();
        bed.unchanged_error(RequestKind::Undo, ErrorKind::InvalidField)
            .await;
        bed.unchanged_error(RequestKind::Redo, ErrorKind::InvalidField)
            .await;
        bed.app.end_transaction().await.unwrap();
        for forbidden in [
            RequestKind::Undo,
            RequestKind::Redo,
            RequestKind::Transaction {
                commands: vec![RequestKind::Undo],
            },
        ] {
            bed.unchanged_error(
                RequestKind::Transaction {
                    commands: vec![
                        RequestKind::RenameScene {
                            scene_id: *scene.as_uuid(),
                            name: "must not commit".into(),
                        },
                        forbidden,
                    ],
                },
                ErrorKind::InvalidRequest,
            )
            .await;
        }
        assert!(bed.client.request(RequestKind::Undo).await.status.ok);
        assert_eq!(
            bed.app.snapshot().state().scene(scene).unwrap().name,
            "Original"
        );
        bed.shutdown().await;
    }
}
