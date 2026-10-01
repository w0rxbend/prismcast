//! Challenge-response authentication over the WebSocket transport (WS-002,
//! protocol doc §4): `WsServer::bind` accepts a password policy (and still
//! rejects allow-local), `Hello` carries a per-session challenge, and the
//! full handshake plus failure closes run over real loopback connections.
//! Mirrors `tests/auth_ipc.rs` — both transports share the same session
//! machinery.

use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::{Message, Utf8Bytes};
use tokio_tungstenite::WebSocketStream;

use prismcast_app::{AppHandle, CoreConfig};
use prismcast_protocol::handshake::{AuthChallenge, Permission};
use prismcast_protocol::request::RequestKind;
use prismcast_remote::auth::{challenge_response, AuthConfig};
use prismcast_remote::ws_client::{WsClient, WsClientConfig, WsClientError};
use prismcast_remote::{ClientAuth, WsError, WsServer, WsServerConfig};

const TIMEOUT: Duration = Duration::from_secs(5);
const PASSWORD: &str = "hunter2";

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
    async fn client_with(&self, config: WsClientConfig) -> Result<WsClient, WsClientError> {
        tokio::time::timeout(TIMEOUT, WsClient::connect_with(self.addr, config))
            .await
            .expect("connect timed out")
    }

    async fn password_client(&self, password: &str) -> Result<WsClient, WsClientError> {
        self.client_with(WsClientConfig {
            auth: ClientAuth::Password(password.into()),
            ..WsClientConfig::default()
        })
        .await
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
async fn bind_accepts_password_and_rejects_allow_local() {
    // `spawn_bed` with a password policy is itself the acceptance proof.
    let bed = spawn_bed(AuthConfig::password(PASSWORD, vec![Permission::Admin])).await;
    assert!(bed.server.session_count() == 0);

    let app = AppHandle::spawn(CoreConfig::default());
    let result = WsServer::bind(
        app.clone(),
        WsServerConfig {
            enabled: true,
            bind: SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
            auth: AuthConfig::allow_local(),
            ..WsServerConfig::default()
        },
    )
    .await;
    assert!(matches!(result, Err(WsError::AuthRequired)));

    app.shutdown().await;
    bed.shutdown().await;
}

#[tokio::test]
async fn hello_carries_stable_salt_and_per_session_challenge() {
    let bed = spawn_bed(AuthConfig::password(PASSWORD, vec![Permission::Admin])).await;

    let mut first = raw::connect(bed.addr).await;
    let hello_a = raw::read_value(&mut first).await;
    let mut second = raw::connect(bed.addr).await;
    let hello_b = raw::read_value(&mut second).await;
    assert_eq!(hello_a["type"], "hello");
    let salt_a = hello_a["data"]["authentication"]["salt"]
        .as_str()
        .expect("salt");
    let salt_b = hello_b["data"]["authentication"]["salt"]
        .as_str()
        .expect("salt");
    let challenge_a = hello_a["data"]["authentication"]["challenge"]
        .as_str()
        .expect("challenge");
    let challenge_b = hello_b["data"]["authentication"]["challenge"]
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
    let bed = spawn_bed(AuthConfig::token("s3cret", vec![Permission::Admin])).await;
    let mut stream = raw::connect(bed.addr).await;
    let hello = raw::read_value(&mut stream).await;
    assert!(
        hello["data"].get("authentication").is_none(),
        "token hello must not offer a challenge: {hello}"
    );
    bed.shutdown().await;
}

#[tokio::test]
async fn password_handshake_succeeds_over_ws_client() {
    let bed = spawn_bed(AuthConfig::password(PASSWORD, vec![Permission::Read])).await;
    let mut client = bed
        .password_client(PASSWORD)
        .await
        .expect("password handshake");
    assert_eq!(client.permissions, vec![Permission::Read]);
    client
        .request_data(RequestKind::GetVersion)
        .await
        .expect("get_version");
    client.close().await;
    bed.shutdown().await;
}

#[tokio::test]
async fn wrong_password_missing_auth_and_token_method_close_4009() {
    let bed = spawn_bed(AuthConfig::password(PASSWORD, vec![Permission::Admin])).await;

    match bed.password_client("wrong").await {
        Err(WsClientError::Closed { code, .. }) => assert_eq!(code, 4009),
        Err(other) => panic!("wrong password → expected AuthenticationFailed, got {other}"),
        Ok(_) => panic!("wrong password → expected AuthenticationFailed, got a session"),
    }

    match bed.client_with(WsClientConfig::default()).await {
        Err(WsClientError::Closed { code, .. }) => assert_eq!(code, 4009),
        Err(other) => panic!("missing auth → expected AuthenticationFailed, got {other}"),
        Ok(_) => panic!("missing auth → expected AuthenticationFailed, got a session"),
    }

    match bed
        .client_with(WsClientConfig {
            auth: ClientAuth::Token(PASSWORD.into()),
            ..WsClientConfig::default()
        })
        .await
    {
        Err(WsClientError::Closed { code, .. }) => assert_eq!(code, 4009),
        Err(other) => panic!("token method → expected AuthenticationFailed, got {other}"),
        Ok(_) => panic!("token method → expected AuthenticationFailed, got a session"),
    }

    bed.shutdown().await;
}

#[tokio::test]
async fn raw_challenge_answer_verifies_and_wrong_answer_closes_4009() {
    let bed = spawn_bed(AuthConfig::password(PASSWORD, vec![Permission::Admin])).await;

    let mut stream = raw::connect(bed.addr).await;
    let hello = raw::read_value(&mut stream).await;
    let challenge: AuthChallenge =
        serde_json::from_value(hello["data"]["authentication"].clone()).expect("challenge");
    raw::write_value(
        &mut stream,
        serde_json::json!({
            "type": "identify",
            "data": {
                "protocol_version": 1,
                "authentication": {
                    "method": "challenge",
                    "response": challenge_response(PASSWORD, &challenge),
                },
            }
        }),
    )
    .await;
    let identified = raw::read_value(&mut stream).await;
    assert_eq!(identified["type"], "identified");

    // A replayed answer bound to another session's challenge is refused.
    let mut stream = raw::connect(bed.addr).await;
    let _hello = raw::read_value(&mut stream).await;
    raw::write_value(
        &mut stream,
        serde_json::json!({
            "type": "identify",
            "data": {
                "protocol_version": 1,
                "authentication": {
                    "method": "challenge",
                    "response": challenge_response(PASSWORD, &challenge),
                },
            }
        }),
    )
    .await;
    let (code, _reason) = raw::read_close(&mut stream).await;
    assert_eq!(code, 4009);

    bed.shutdown().await;
}

#[tokio::test]
async fn password_client_fails_client_side_when_no_challenge_offered() {
    // Only reachable against a non-prismcast server: `WsServer::bind` refuses
    // challenge-less policies, so drive the check against a token bed whose
    // hello carries no challenge.
    let bed = spawn_bed(AuthConfig::token("s3cret", vec![Permission::Admin])).await;
    match bed.password_client(PASSWORD).await {
        Err(WsClientError::Decode(error)) => {
            assert!(error.contains("challenge"), "unexpected error: {error}");
        }
        Err(other) => panic!("expected a client-side decode error, got {other}"),
        Ok(_) => panic!("expected a client-side error, got a session"),
    }
    bed.shutdown().await;
}
