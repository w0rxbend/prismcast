//! End-to-end tests for the WebSocket transport (WS-001): a real
//! `CoreActor` behind a real `WsServer` on loopback TCP, driven by the
//! `WsClient` and by raw `tokio-tungstenite` clients for
//! protocol-violation cases. Mirrors `tests/ipc.rs` — both transports share
//! the same session machinery.

use std::net::{Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::{Message, Utf8Bytes};
use tokio_tungstenite::WebSocketStream;
use uuid::Uuid;

use prismcast_app::{AppHandle, CoreConfig};
use prismcast_protocol::batch::{BatchRequest, RequestBatch};
use prismcast_protocol::error::ErrorKind;
use prismcast_protocol::event::WireEvent;
use prismcast_protocol::handshake::Permission;
use prismcast_protocol::request::RequestKind;
use prismcast_protocol::response::ResponseData;
use prismcast_protocol::subscription::{EventCategory, Subscription, SubscriptionSet};
use prismcast_remote::auth::AuthConfig;
use prismcast_remote::ws_client::{WsClient, WsClientConfig, WsClientError};
use prismcast_remote::{WsServer, WsServerConfig};

const TIMEOUT: Duration = Duration::from_secs(5);
const TOKEN: &str = "s3cret";

struct TestBed {
    app: AppHandle,
    server: WsServer,
    addr: SocketAddr,
}

async fn spawn_bed(auth: AuthConfig) -> TestBed {
    let app = AppHandle::spawn(CoreConfig::default());
    let server = WsServer::bind(
        app.clone(),
        WsServerConfig {
            enabled: true,
            bind: SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
            auth,
            ..WsServerConfig::default()
        },
    )
    .await
    .expect("bind server");
    let addr = server.local_addr();
    TestBed { app, server, addr }
}

impl TestBed {
    async fn client(&self) -> WsClient {
        self.client_with(WsClientConfig {
            token: Some(TOKEN.into()),
            ..WsClientConfig::default()
        })
        .await
        .expect("connect")
    }

    async fn client_with(&self, config: WsClientConfig) -> Result<WsClient, WsClientError> {
        tokio::time::timeout(TIMEOUT, WsClient::connect_with(self.addr, config))
            .await
            .expect("connect timed out")
    }

    async fn add_scene(client: &mut WsClient, name: &str) -> Uuid {
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
    }
}

/// Raw-frame helpers over a bare tungstenite client.
mod raw {
    use super::*;

    pub type RawStream = WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

    pub async fn connect(addr: SocketAddr) -> RawStream {
        let (stream, _response) = tokio::time::timeout(
            TIMEOUT,
            tokio_tungstenite::connect_async(format!("ws://{addr}/")),
        )
        .await
        .expect("connect timed out")
        .expect("ws connect");
        stream
    }

    /// Connects, reads `hello`, sends `identify` with the token, and asserts
    /// `identified`.
    pub async fn identify(stream: &mut RawStream) {
        let hello = read_value(stream).await;
        assert_eq!(hello["type"], "hello");
        write_value(
            stream,
            serde_json::json!({
                "type": "identify",
                "data": {
                    "protocol_version": 1,
                    "authentication": {"method": "token", "token": TOKEN},
                }
            }),
        )
        .await;
        let identified = read_value(stream).await;
        assert_eq!(identified["type"], "identified");
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

#[tokio::test]
async fn handshake_identify_with_token() {
    let bed = spawn_bed(AuthConfig::token(TOKEN, vec![Permission::Admin])).await;
    let client = bed.client().await;
    assert_eq!(client.negotiated_protocol_version, 1);
    assert!(client.permissions.contains(&Permission::Admin));
    client.close().await;
    bed.shutdown().await;
}

#[tokio::test]
async fn wrong_or_missing_token_closes_with_auth_failed() {
    let bed = spawn_bed(AuthConfig::token(TOKEN, vec![Permission::Read])).await;

    let wrong = bed
        .client_with(WsClientConfig {
            token: Some("wrong".into()),
            ..WsClientConfig::default()
        })
        .await;
    match wrong {
        Err(WsClientError::Closed { code, .. }) => assert_eq!(code, 4009),
        Err(other) => panic!("expected auth failure, got {other}"),
        Ok(_) => panic!("expected auth failure, got a session"),
    }

    let missing = bed.client_with(WsClientConfig::default()).await;
    assert!(matches!(
        missing,
        Err(WsClientError::Closed { code: 4009, .. })
    ));

    // A token session with [read] can query but not mutate (800 forbidden).
    let mut client = bed
        .client_with(WsClientConfig {
            token: Some(TOKEN.into()),
            ..WsClientConfig::default()
        })
        .await
        .expect("connect");
    assert_eq!(client.permissions, vec![Permission::Read]);
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

#[tokio::test]
async fn unsupported_protocol_version_closes_with_4010() {
    let bed = spawn_bed(AuthConfig::token(TOKEN, vec![Permission::Admin])).await;
    let result = bed
        .client_with(WsClientConfig {
            // Below the server's minimum supported version.
            protocol_version: 0,
            token: Some(TOKEN.into()),
            ..WsClientConfig::default()
        })
        .await;
    assert!(matches!(
        result,
        Err(WsClientError::Closed { code: 4010, .. })
    ));
    bed.shutdown().await;
}

#[tokio::test]
async fn request_roundtrip_scene_reflected_in_snapshot() {
    let bed = spawn_bed(AuthConfig::token(TOKEN, vec![Permission::Admin])).await;
    let mut client = bed.client().await;

    let main = TestBed::add_scene(&mut client, "Main").await;
    let snapshot = client
        .request_data(RequestKind::GetSnapshot)
        .await
        .expect("get_snapshot");
    match snapshot {
        ResponseData::Snapshot { snapshot } => {
            assert_eq!(snapshot.scenes.len(), 1);
            assert_eq!(snapshot.scenes[0].id, main);
            assert_eq!(snapshot.scenes[0].name, "Main");
            assert_eq!(snapshot.current_scene, Some(main));
        }
        other => panic!("unexpected: {other:?}"),
    }

    client.close().await;
    bed.shutdown().await;
}

#[tokio::test]
async fn event_subscription_delivery_with_seq() {
    let bed = spawn_bed(AuthConfig::token(TOKEN, vec![Permission::Admin])).await;
    // Default subscriptions (all standard categories) for both clients.
    let mut a = bed.client().await;
    let mut b = bed.client().await;

    let scene = TestBed::add_scene(&mut b, "Main").await;

    let first = tokio::time::timeout(TIMEOUT, a.next_event())
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
    let second = tokio::time::timeout(TIMEOUT, a.next_event())
        .await
        .expect("event timed out")
        .expect("event");
    assert_eq!(second.seq, 1);
    assert!(matches!(
        &second.event,
        WireEvent::Scene(prismcast_protocol::event::SceneEvent::CurrentChanged { scene_id })
        if *scene_id == scene
    ));

    a.close().await;
    b.close().await;
    bed.shutdown().await;
}

#[tokio::test]
async fn throttle_coalesces_rapid_state_events() {
    let bed = spawn_bed(AuthConfig::token(TOKEN, vec![Permission::Admin])).await;
    let mut a = bed
        .client_with(WsClientConfig {
            token: Some(TOKEN.into()),
            subscriptions: Some(SubscriptionSet {
                entries: vec![Subscription {
                    category: EventCategory::Scene,
                    entity_ids: Vec::new(),
                    throttle_ms: Some(200),
                }],
            }),
            ..WsClientConfig::default()
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

    // ...the following same-key events coalesce into the latest one,
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

    let none = tokio::time::timeout(Duration::from_millis(500), a.next_event()).await;
    assert!(none.is_err(), "coalesced events must not be delivered");

    a.close().await;
    b.close().await;
    bed.shutdown().await;
}

#[tokio::test]
async fn batch_request_executes_serially() {
    let bed = spawn_bed(AuthConfig::token(TOKEN, vec![Permission::Admin])).await;
    let mut client = bed.client().await;

    let response = client
        .request_batch(RequestBatch {
            request_id: "batch-1".into(),
            halt_on_failure: false,
            requests: vec![
                BatchRequest {
                    request_id: Some("m-1".into()),
                    kind: RequestKind::AddScene { name: "A".into() },
                },
                BatchRequest {
                    request_id: Some("m-2".into()),
                    kind: RequestKind::AddScene { name: "B".into() },
                },
                BatchRequest {
                    request_id: Some("m-3".into()),
                    kind: RequestKind::ListScenes,
                },
            ],
        })
        .await
        .expect("batch");

    assert_eq!(response.request_id, "batch-1");
    assert_eq!(response.results.len(), 3);
    assert!(response.results.iter().all(|r| r.status.ok));
    assert_eq!(response.results[0].request_id.as_deref(), Some("m-1"));
    assert_eq!(response.results[2].request_type, "list_scenes");
    match &response.results[2].data {
        Some(ResponseData::SceneList { scenes }) => {
            let names: Vec<&str> = scenes.iter().map(|s| s.name.as_str()).collect();
            assert_eq!(names, ["A", "B"]);
        }
        other => panic!("unexpected: {other:?}"),
    }

    client.close().await;
    bed.shutdown().await;
}

#[tokio::test]
async fn disabled_by_default_nothing_binds() {
    let app = AppHandle::spawn(CoreConfig::default());
    // Pick an address that is free, then prove the disabled server does not
    // take it.
    let probe = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("probe");
    let addr = probe.local_addr().expect("addr");
    drop(probe);

    let none = WsServer::bind_if_enabled(
        app.clone(),
        WsServerConfig {
            bind: addr,
            ..WsServerConfig::default()
        },
    )
    .await
    .expect("bind_if_enabled");
    assert!(none.is_none(), "disabled server must not bind");
    assert!(
        tokio::net::TcpStream::connect(addr).await.is_err(),
        "connecting to the address must be refused: nothing was bound"
    );

    // `bind` on a disabled config is a hard error, not a silent no-op.
    let result = WsServer::bind(app.clone(), WsServerConfig::default()).await;
    assert!(matches!(result, Err(prismcast_remote::WsError::Disabled)));

    app.shutdown().await;
}

#[tokio::test]
async fn oversized_message_closes_with_decode_error() {
    let app = AppHandle::spawn(CoreConfig::default());
    let server = WsServer::bind(
        app.clone(),
        WsServerConfig {
            enabled: true,
            bind: SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
            auth: AuthConfig::token(TOKEN, vec![Permission::Admin]),
            max_message_size: 1024,
            ..WsServerConfig::default()
        },
    )
    .await
    .expect("bind");

    let mut stream = raw::connect(server.local_addr()).await;
    let hello = raw::read_value(&mut stream).await;
    assert_eq!(hello["type"], "hello");

    // A text frame larger than the configured limit.
    let big = format!(
        "{{\"type\":\"identify\",\"data\":{{\"protocol_version\":1,\"padding\":\"{}\"}}}}",
        "x".repeat(2048)
    );
    stream
        .send(Message::Text(Utf8Bytes::from(big)))
        .await
        .expect("write");
    let (code, _reason) = raw::read_close(&mut stream).await;
    assert_eq!(code, 4002, "oversized message → MessageDecodeError");

    server.shutdown().await;
    app.shutdown().await;
}

#[tokio::test]
async fn binary_frame_closes_with_decode_error() {
    let bed = spawn_bed(AuthConfig::token(TOKEN, vec![Permission::Admin])).await;
    let mut stream = raw::connect(bed.addr).await;
    raw::identify(&mut stream).await;

    // Binary frames are reserved for the future prismcast.msgpack
    // subprotocol; v1 speaks JSON text only (protocol doc §1).
    stream
        .send(Message::Binary(vec![0xde, 0xad].into()))
        .await
        .expect("write");
    let (code, reason) = raw::read_close(&mut stream).await;
    assert_eq!(code, 4002);
    assert!(reason.contains("msgpack") || reason.contains("binary"));

    bed.shutdown().await;
}

#[tokio::test]
async fn unknown_request_type_gets_202_error_response() {
    let bed = spawn_bed(AuthConfig::token(TOKEN, vec![Permission::Admin])).await;
    let mut stream = raw::connect(bed.addr).await;
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
async fn request_before_identify_closes_with_not_identified() {
    let bed = spawn_bed(AuthConfig::token(TOKEN, vec![Permission::Admin])).await;
    let mut stream = raw::connect(bed.addr).await;
    let _hello = raw::read_value(&mut stream).await;
    raw::write_value(
        &mut stream,
        serde_json::json!({
            "type": "request",
            "data": {"request_id": "r-1", "request": "get_version"}
        }),
    )
    .await;
    let (code, _reason) = raw::read_close(&mut stream).await;
    assert_eq!(code, 4007);

    bed.shutdown().await;
}
