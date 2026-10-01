//! Challenge-response authentication over the Unix-socket transport
//! (WS-002, protocol doc §4): per-session `Hello` challenges, the full
//! password handshake, and failure closes — over real IPC connections.

use std::path::PathBuf;
use std::time::Duration;

use tokio::net::UnixStream;
use uuid::Uuid;

use prismcast_app::{AppHandle, CoreConfig};
use prismcast_protocol::handshake::{AuthChallenge, Permission};
use prismcast_protocol::request::RequestKind;
use prismcast_remote::auth::{challenge_response, AuthConfig};
use prismcast_remote::client::{ClientAuth, ClientError, IpcClient, IpcClientConfig};
use prismcast_remote::codec;
use prismcast_remote::{IpcServer, IpcServerConfig};

const TIMEOUT: Duration = Duration::from_secs(5);
const PASSWORD: &str = "hunter2";

struct TestBed {
    app: AppHandle,
    server: IpcServer,
    dir: PathBuf,
    socket: PathBuf,
}

async fn spawn_bed(auth: AuthConfig) -> TestBed {
    let dir = std::env::temp_dir().join(format!("prismcast-ipc-auth-test-{}", Uuid::new_v4()));
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
    async fn client_with(&self, config: IpcClientConfig) -> Result<IpcClient, ClientError> {
        tokio::time::timeout(TIMEOUT, IpcClient::connect_with(&self.socket, config))
            .await
            .expect("connect timed out")
    }

    async fn password_client(&self, password: &str) -> Result<IpcClient, ClientError> {
        self.client_with(IpcClientConfig {
            auth: ClientAuth::Password(password.into()),
            ..IpcClientConfig::default()
        })
        .await
    }

    async fn raw_hello(&self) -> serde_json::Value {
        let mut stream = UnixStream::connect(&self.socket).await.expect("connect");
        let hello = raw::read_value(&mut stream).await;
        assert_eq!(hello["type"], "hello");
        hello
    }

    async fn shutdown(self) {
        self.server.shutdown().await;
        self.app.shutdown().await;
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Raw-frame helpers for handshake-level assertions.
mod raw {
    use super::*;

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

    pub async fn write_value(stream: &mut UnixStream, value: serde_json::Value) {
        let payload = codec::encode(&value).expect("encode");
        codec::write_frame(stream, &payload).await.expect("write");
    }
}

#[tokio::test]
async fn hello_carries_stable_salt_and_per_session_challenge() {
    let bed = spawn_bed(AuthConfig::password(PASSWORD, vec![Permission::Admin])).await;

    let first = bed.raw_hello().await;
    let second = bed.raw_hello().await;
    let salt_a = first["data"]["authentication"]["salt"]
        .as_str()
        .expect("salt");
    let salt_b = second["data"]["authentication"]["salt"]
        .as_str()
        .expect("salt");
    let challenge_a = first["data"]["authentication"]["challenge"]
        .as_str()
        .expect("challenge");
    let challenge_b = second["data"]["authentication"]["challenge"]
        .as_str()
        .expect("challenge");
    assert_eq!(salt_a, salt_b, "salt is stable per server start");
    assert_ne!(challenge_a, challenge_b, "challenge differs per session");
    // 32 raw bytes → 44 base64 chars with padding.
    assert_eq!(salt_a.len(), 44);
    assert_eq!(challenge_a.len(), 44);

    bed.shutdown().await;
}

#[tokio::test]
async fn token_and_allow_local_hellos_carry_no_challenge() {
    let local = spawn_bed(AuthConfig::allow_local()).await;
    let hello = local.raw_hello().await;
    assert!(
        hello["data"].get("authentication").is_none(),
        "allow-local hello must not offer a challenge: {hello}"
    );
    local.shutdown().await;

    let token = spawn_bed(AuthConfig::token("s3cret", vec![Permission::Admin])).await;
    let hello = token.raw_hello().await;
    assert!(
        hello["data"].get("authentication").is_none(),
        "token hello must not offer a challenge: {hello}"
    );
    token.shutdown().await;
}

#[tokio::test]
async fn password_handshake_grants_configured_permissions() {
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
        Err(ClientError::Closed { code, .. }) => assert_eq!(code, 4009),
        Err(other) => panic!("wrong password → expected AuthenticationFailed, got {other}"),
        Ok(_) => panic!("wrong password → expected AuthenticationFailed, got a session"),
    }

    match bed.client_with(IpcClientConfig::default()).await {
        Err(ClientError::Closed { code, .. }) => assert_eq!(code, 4009),
        Err(other) => panic!("missing auth → expected AuthenticationFailed, got {other}"),
        Ok(_) => panic!("missing auth → expected AuthenticationFailed, got a session"),
    }

    match bed
        .client_with(IpcClientConfig {
            auth: ClientAuth::Token(PASSWORD.into()),
            ..IpcClientConfig::default()
        })
        .await
    {
        Err(ClientError::Closed { code, .. }) => assert_eq!(code, 4009),
        Err(other) => panic!("token method → expected AuthenticationFailed, got {other}"),
        Ok(_) => panic!("token method → expected AuthenticationFailed, got a session"),
    }

    bed.shutdown().await;
}

#[tokio::test]
async fn raw_challenge_answer_verifies_and_wrong_answer_closes_4009() {
    let bed = spawn_bed(AuthConfig::password(PASSWORD, vec![Permission::Admin])).await;

    // A hand-rolled answer over raw frames proves the wire shape end-to-end.
    let mut stream = UnixStream::connect(&bed.socket).await.expect("connect");
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

    // An answer computed for another session's challenge must not
    // authenticate this one.
    let mut stream = UnixStream::connect(&bed.socket).await.expect("connect");
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
    let closing = raw::read_value(&mut stream).await;
    assert_eq!(closing["type"], "closing");
    assert_eq!(closing["data"]["code"], 4009);

    bed.shutdown().await;
}

#[tokio::test]
async fn password_client_fails_client_side_when_no_challenge_offered() {
    let bed = spawn_bed(AuthConfig::allow_local()).await;
    match bed.password_client(PASSWORD).await {
        Err(ClientError::Decode(error)) => {
            assert!(error.contains("challenge"), "unexpected error: {error}");
        }
        Err(other) => panic!("expected a client-side decode error, got {other}"),
        Ok(_) => panic!("expected a client-side error, got a session"),
    }
    bed.shutdown().await;
}
