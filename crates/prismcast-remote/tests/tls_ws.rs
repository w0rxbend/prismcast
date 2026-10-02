//! End-to-end TLS tests for the WebSocket transport (WS-003, ADR-0022): a
//! real `CoreActor` behind a `WsServer` terminating `wss://` with rustls,
//! driven by scripted `tokio-tungstenite` clients rooted at a throwaway
//! rcgen CA. Certificates are generated per test into a tempdir and never
//! committed. Plaintext loopback coverage lives in `tests/ws.rs`, untouched.

use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::CertificateDer;
use rustls::RootCertStore;
use tokio_tungstenite::tungstenite::{Message, Utf8Bytes};
use tokio_tungstenite::{Connector, WebSocketStream};
use uuid::Uuid;

use prismcast_app::{AppHandle, CoreConfig};
use prismcast_protocol::handshake::Permission;
use prismcast_remote::auth::AuthConfig;
use prismcast_remote::tls::{TlsError, WsTlsConfig};
use prismcast_remote::ws::WsError;
use prismcast_remote::{WsServer, WsServerConfig};

const TIMEOUT: Duration = Duration::from_secs(5);
const TOKEN: &str = "s3cret";

/// An rcgen CA + leaf (SANs: `localhost`, `127.0.0.1`) written to a fresh
/// tempdir; removed on drop.
struct TestPki {
    dir: PathBuf,
    cert_path: PathBuf,
    key_path: PathBuf,
    ca_path: PathBuf,
}

impl TestPki {
    fn generate() -> Self {
        use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, KeyPair};

        let dir = std::env::temp_dir().join(format!("prismcast-wss-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create tempdir");

        let ca_key = KeyPair::generate().expect("ca key");
        let mut ca_params = CertificateParams::new(Vec::new()).expect("ca params");
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca_params
            .distinguished_name
            .push(DnType::CommonName, "prismcast wss test ca");
        let ca_cert = ca_params.self_signed(&ca_key).expect("ca cert");

        let leaf_key = KeyPair::generate().expect("leaf key");
        let leaf_params =
            CertificateParams::new(vec!["localhost".to_string(), "127.0.0.1".to_string()])
                .expect("leaf params");
        let leaf_cert = leaf_params
            .signed_by(&leaf_key, &ca_cert, &ca_key)
            .expect("leaf cert");

        let cert_path = dir.join("cert.pem");
        // Leaf-first chain, intermediates after (ADR-0022 §b).
        std::fs::write(&cert_path, format!("{}{}", leaf_cert.pem(), ca_cert.pem()))
            .expect("write chain");
        let key_path = dir.join("key.pem");
        std::fs::write(&key_path, leaf_key.serialize_pem()).expect("write key");
        let ca_path = dir.join("ca.pem");
        std::fs::write(&ca_path, ca_cert.pem()).expect("write ca");
        Self {
            dir,
            cert_path,
            key_path,
            ca_path,
        }
    }

    fn tls_config(&self) -> WsTlsConfig {
        WsTlsConfig {
            cert_path: self.cert_path.clone(),
            key_path: self.key_path.clone(),
        }
    }
}

impl Drop for TestPki {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

struct TestBed {
    app: AppHandle,
    server: WsServer,
    pki: TestPki,
    addr: SocketAddr,
}

async fn spawn_bed(bind: SocketAddr) -> TestBed {
    let app = AppHandle::spawn(CoreConfig::default());
    let pki = TestPki::generate();
    let server = WsServer::bind(
        app.clone(),
        WsServerConfig {
            enabled: true,
            bind,
            tls: Some(pki.tls_config()),
            auth: AuthConfig::token(TOKEN, vec![Permission::Admin]),
            ..WsServerConfig::default()
        },
    )
    .await
    .expect("bind server");
    let addr = server.local_addr();
    TestBed {
        app,
        server,
        pki,
        addr,
    }
}

impl TestBed {
    async fn client(&self) -> RawStream {
        let mut stream = connect_wss(self.addr, &self.pki.ca_path).await;
        identify(&mut stream).await;
        stream
    }

    async fn shutdown(self) {
        self.server.shutdown().await;
        self.app.shutdown().await;
    }
}

type RawStream = WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Connects over `wss://` with a rustls client rooted at the test CA.
async fn connect_wss(addr: SocketAddr, ca_path: &Path) -> RawStream {
    let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_file_iter(ca_path)
        .expect("open ca bundle")
        .collect::<Result<_, _>>()
        .expect("parse ca bundle");
    let mut roots = RootCertStore::empty();
    let (added, _) = roots.add_parsable_certificates(certs);
    assert!(added >= 1, "test CA must be parsable");
    let config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let (stream, _response) = tokio::time::timeout(
        TIMEOUT,
        tokio_tungstenite::connect_async_tls_with_config(
            format!("wss://{addr}/"),
            None,
            false,
            Some(Connector::Rustls(Arc::new(config))),
        ),
    )
    .await
    .expect("connect timed out")
    .expect("wss connect");
    stream
}

/// Reads `hello`, answers `identify` with the token, asserts `identified`.
async fn identify(stream: &mut RawStream) {
    let hello = read_value(stream).await;
    assert_eq!(hello["type"], "hello");
    stream
        .send(Message::Text(Utf8Bytes::from(
            serde_json::json!({
                "type": "identify",
                "data": {
                    "protocol_version": 1,
                    "authentication": {"method": "token", "token": TOKEN},
                }
            })
            .to_string(),
        )))
        .await
        .expect("write identify");
    let identified = read_value(stream).await;
    assert_eq!(identified["type"], "identified");
}

/// Reads the next text frame as a JSON value (skips ping/pong).
async fn read_value(stream: &mut RawStream) -> serde_json::Value {
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

/// Reads frames until the `request_response` for `request_id`, skipping
/// event frames (sessions are subscribed by default, so a mutating client
/// sees its own events interleaved with responses).
async fn read_response(stream: &mut RawStream, request_id: &str) -> serde_json::Value {
    loop {
        let value = read_value(stream).await;
        if value["type"] == "request_response" && value["data"]["request_id"] == request_id {
            return value;
        }
    }
}

#[tokio::test]
async fn wss_full_native_session() {
    let bed = spawn_bed(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))).await;
    let mut a = bed.client().await;
    let mut b = bed.client().await;

    // One request round-trip over wss.
    b.send(Message::Text(Utf8Bytes::from(
        serde_json::json!({
            "type": "request",
            "data": {"request_id": "r-1", "request": "add_scene", "name": "Main"}
        })
        .to_string(),
    )))
    .await
    .expect("write request");
    let response = read_response(&mut b, "r-1").await;
    assert_eq!(response["data"]["status"]["ok"], true);

    // Event delivery over wss: the other session sees the scene event.
    let event = read_value(&mut a).await;
    assert_eq!(event["type"], "event");
    assert_eq!(event["data"]["category"], "scene");

    // Queries work too (same session machinery, TLS framing only).
    b.send(Message::Text(Utf8Bytes::from(
        serde_json::json!({
            "type": "request",
            "data": {"request_id": "r-2", "request": "get_snapshot"}
        })
        .to_string(),
    )))
    .await
    .expect("write request");
    let response = read_response(&mut b, "r-2").await;
    assert_eq!(response["data"]["status"]["ok"], true);

    bed.shutdown().await;
}

#[tokio::test]
async fn plaintext_client_on_tls_port_fails_and_server_keeps_serving() {
    let bed = spawn_bed(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))).await;

    // A plaintext client speaking the WS HTTP upgrade at a wss:// port: the
    // TLS handshake must fail and the connection drop, without a crash.
    let mut plain = tokio::time::timeout(TIMEOUT, tokio::net::TcpStream::connect(bed.addr))
        .await
        .expect("connect timed out")
        .expect("tcp connect");
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    plain
        .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await
        .expect("write plaintext");
    // The server answers the garbage with a TLS fatal alert (a few bytes)
    // and then drops the connection — never a WS upgrade, never a stall.
    let mut buf = [0u8; 64];
    loop {
        match tokio::time::timeout(TIMEOUT, plain.read(&mut buf))
            .await
            .expect("read timed out: server stalled on a plaintext client")
        {
            Ok(0) => break,
            Ok(_) => continue,
            Err(_) => break,
        }
    }

    // The accept loop survived: a good wss client gets a full session —
    // identify plus a request round-trip, after the plaintext failure.
    let mut good = bed.client().await;
    good.send(Message::Text(Utf8Bytes::from(
        serde_json::json!({
            "type": "request",
            "data": {"request_id": "after-bad", "request": "get_version"}
        })
        .to_string(),
    )))
    .await
    .expect("write request");
    let response = read_response(&mut good, "after-bad").await;
    assert_eq!(response["data"]["status"]["ok"], true);
    bed.shutdown().await;
}

#[tokio::test]
async fn non_loopback_bind_without_tls_is_rejected() {
    let app = AppHandle::spawn(CoreConfig::default());
    let result = WsServer::bind(
        app.clone(),
        WsServerConfig {
            enabled: true,
            bind: SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)),
            auth: AuthConfig::token(TOKEN, vec![Permission::Admin]),
            ..WsServerConfig::default()
        },
    )
    .await;
    assert!(matches!(result, Err(WsError::TlsRequired)));

    // The same non-loopback bind with TLS configured is allowed.
    let pki = TestPki::generate();
    let server = WsServer::bind(
        app.clone(),
        WsServerConfig {
            enabled: true,
            bind: SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)),
            tls: Some(pki.tls_config()),
            auth: AuthConfig::token(TOKEN, vec![Permission::Admin]),
            ..WsServerConfig::default()
        },
    )
    .await;
    let server = match server {
        Ok(server) => server,
        Err(error) => panic!("non-loopback bind with TLS must succeed: {error}"),
    };
    server.shutdown().await;
    app.shutdown().await;
}

#[tokio::test]
async fn bad_tls_material_fails_bind_with_typed_error_naming_the_path() {
    let pki = TestPki::generate();
    let app = AppHandle::spawn(CoreConfig::default());

    // Missing key file.
    let missing = pki.dir.join("does-not-exist.pem");
    let result = WsServer::bind(
        app.clone(),
        WsServerConfig {
            enabled: true,
            bind: SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
            tls: Some(WsTlsConfig {
                cert_path: pki.cert_path.clone(),
                key_path: missing.clone(),
            }),
            auth: AuthConfig::token(TOKEN, vec![Permission::Admin]),
            ..WsServerConfig::default()
        },
    )
    .await;
    match result {
        Err(WsError::Tls(TlsError::Io { path, .. })) => assert_eq!(path, missing),
        Err(other) => panic!("expected WsError::Tls(Io) naming the path, got {other}"),
        Ok(_) => panic!("expected bind failure, got a server"),
    }

    // Garbage certificate file.
    let garbage = pki.dir.join("garbage.pem");
    std::fs::write(&garbage, b"not a pem file at all").expect("write garbage");
    let result = WsServer::bind(
        app.clone(),
        WsServerConfig {
            enabled: true,
            bind: SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
            tls: Some(WsTlsConfig {
                cert_path: garbage.clone(),
                key_path: pki.key_path.clone(),
            }),
            auth: AuthConfig::token(TOKEN, vec![Permission::Admin]),
            ..WsServerConfig::default()
        },
    )
    .await;
    match result {
        Err(WsError::Tls(TlsError::EmptyCertChain { path }))
        | Err(WsError::Tls(TlsError::InvalidCert { path, .. })) => assert_eq!(path, garbage),
        Err(other) => panic!("expected typed cert error naming the path, got {other}"),
        Ok(_) => panic!("expected bind failure, got a server"),
    }

    app.shutdown().await;
}
