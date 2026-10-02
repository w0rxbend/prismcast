//! Client-side `wss://` tests for [`WsClient`] (WS-003, ADR-0022): the native
//! WS client against a rustls-terminated `WsServer`, rooted at a throwaway
//! rcgen CA — a full session over TLS, trust rejection of the test CA, the
//! warn-logged danger switch, `ws://`/bogus-scheme URL handling, and the
//! token/password auth matrix over `wss`. Certificates are generated per test
//! into a tempdir and never committed. Server-side TLS coverage lives in
//! `tests/tls_ws.rs`; the plaintext auth matrix in `tests/auth_ws.rs`.

use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::time::Duration;

use uuid::Uuid;

use prismcast_app::{AppHandle, CoreConfig};
use prismcast_protocol::handshake::Permission;
use prismcast_protocol::request::RequestKind;
use prismcast_protocol::response::ResponseData;
use prismcast_protocol::subscription::EventCategory;
use prismcast_remote::auth::AuthConfig;
use prismcast_remote::tls::{ClientTlsConfig, WsTlsConfig};
use prismcast_remote::ws_client::{WsClient, WsClientConfig, WsClientError};
use prismcast_remote::{ClientAuth, WsServer, WsServerConfig};

const TIMEOUT: Duration = Duration::from_secs(5);
const TOKEN: &str = "s3cret";
const PASSWORD: &str = "hunter2";

/// An rcgen CA + leaf (SANs: `localhost`, `127.0.0.1`) written to a fresh
/// tempdir; removed on drop. Mirrors the fixture in `tests/tls_ws.rs`.
struct TestPki {
    dir: PathBuf,
    cert_path: PathBuf,
    key_path: PathBuf,
    ca_path: PathBuf,
}

impl TestPki {
    fn generate() -> Self {
        use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, KeyPair};

        let dir =
            std::env::temp_dir().join(format!("prismcast-wss-client-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create tempdir");

        let ca_key = KeyPair::generate().expect("ca key");
        let mut ca_params = CertificateParams::new(Vec::new()).expect("ca params");
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca_params
            .distinguished_name
            .push(DnType::CommonName, "prismcast wss client test ca");
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
    scheme: &'static str,
}

async fn spawn_bed(auth: AuthConfig, with_tls: bool) -> TestBed {
    let app = AppHandle::spawn(CoreConfig::default());
    let pki = TestPki::generate();
    let server = WsServer::bind(
        app.clone(),
        WsServerConfig {
            enabled: true,
            bind: SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
            tls: with_tls.then(|| pki.tls_config()),
            auth,
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
        scheme: if with_tls { "wss" } else { "ws" },
    }
}

async fn spawn_tls_bed(auth: AuthConfig) -> TestBed {
    spawn_bed(auth, true).await
}

impl TestBed {
    fn url(&self) -> String {
        format!("{}://{}/", self.scheme, self.addr)
    }

    /// A client config rooted at the test CA with the given credential.
    fn client_config(&self, auth: ClientAuth) -> WsClientConfig {
        WsClientConfig {
            auth,
            tls: Some(ClientTlsConfig {
                extra_ca_path: Some(self.pki.ca_path.clone()),
                danger_accept_invalid_certs: false,
            }),
            ..WsClientConfig::default()
        }
    }

    async fn connect(&self, config: WsClientConfig) -> Result<WsClient, WsClientError> {
        tokio::time::timeout(TIMEOUT, WsClient::connect_url(&self.url(), config))
            .await
            .expect("connect timed out")
    }

    async fn token_client(&self) -> WsClient {
        self.connect(self.client_config(ClientAuth::Token(TOKEN.into())))
            .await
            .expect("token client")
    }

    async fn shutdown(self) {
        self.server.shutdown().await;
        self.app.shutdown().await;
    }
}

#[tokio::test]
async fn wss_full_session_with_extra_ca() {
    let bed = spawn_tls_bed(AuthConfig::token(TOKEN, vec![Permission::Admin])).await;
    let mut a = bed.token_client().await;
    let mut b = bed.token_client().await;
    assert_eq!(b.permissions, vec![Permission::Admin]);

    // One request round-trip over wss.
    let response = b
        .request(RequestKind::AddScene {
            name: "Main".to_string(),
        })
        .await
        .expect("add_scene");
    assert!(response.status.ok);

    // Event delivery over wss: the other session sees the scene event.
    let event = tokio::time::timeout(TIMEOUT, a.next_event())
        .await
        .expect("event timed out")
        .expect("event");
    assert_eq!(event.category, EventCategory::Scene);

    // Queries work too (same session machinery, TLS framing only).
    match b.request_data(RequestKind::GetVersion).await {
        Ok(ResponseData::Version { .. }) => {}
        other => panic!("expected version data, got {other:?}"),
    }

    b.close().await;
    a.close().await;
    bed.shutdown().await;
}

#[tokio::test]
async fn wss_native_roots_reject_test_ca() {
    let bed = spawn_tls_bed(AuthConfig::token(TOKEN, vec![Permission::Admin])).await;

    // `tls: None` verifies against the platform's native root store, which
    // does not contain the throwaway test CA: the handshake must fail with a
    // typed certificate error before any protocol frame is exchanged.
    let result = bed
        .connect(WsClientConfig {
            auth: ClientAuth::Token(TOKEN.into()),
            tls: None,
            ..WsClientConfig::default()
        })
        .await;
    match result {
        Err(WsClientError::WebSocket(error)) => {
            assert!(
                error.to_string().contains("certificate"),
                "expected a certificate verification failure, got {error}"
            );
        }
        Err(other) => panic!("expected a TLS certificate failure, got {other}"),
        Ok(_) => panic!("untrusted test CA must not verify against native roots"),
    }

    // The reject left the server healthy.
    let good = bed.token_client().await;
    good.close().await;
    bed.shutdown().await;
}

#[tokio::test]
async fn wss_danger_accept_invalid_certs_connects() {
    let bed = spawn_tls_bed(AuthConfig::token(TOKEN, vec![Permission::Admin])).await;
    let mut client = bed
        .connect(WsClientConfig {
            auth: ClientAuth::Token(TOKEN.into()),
            tls: Some(ClientTlsConfig {
                extra_ca_path: None,
                danger_accept_invalid_certs: true,
            }),
            ..WsClientConfig::default()
        })
        .await
        .expect("danger-mode client must connect to the untrusted test CA");
    client
        .request_data(RequestKind::GetVersion)
        .await
        .expect("get_version over danger-mode wss");
    client.close().await;
    bed.shutdown().await;
}

#[tokio::test]
async fn ws_scheme_ignores_tls_config_and_bogus_scheme_is_typed() {
    let bed = spawn_bed(AuthConfig::token(TOKEN, vec![Permission::Admin]), false).await;

    // `ws://` connects in plaintext; a `tls` config is ignored entirely —
    // here it points at a nonexistent CA bundle, which would fail connector
    // construction if it were consulted.
    let mut client = bed
        .connect(WsClientConfig {
            auth: ClientAuth::Token(TOKEN.into()),
            tls: Some(ClientTlsConfig {
                extra_ca_path: Some(bed.pki.dir.join("does-not-exist.pem")),
                danger_accept_invalid_certs: false,
            }),
            ..WsClientConfig::default()
        })
        .await
        .expect("ws:// connect must ignore the tls config");
    client
        .request_data(RequestKind::GetVersion)
        .await
        .expect("get_version over ws");
    client.close().await;

    // A bogus scheme is rejected before any network I/O.
    let result =
        WsClient::connect_url(&format!("http://{}/", bed.addr), WsClientConfig::default()).await;
    match result {
        Err(WsClientError::Url { scheme }) => assert_eq!(scheme, "http"),
        Err(other) => panic!("expected a typed Url error, got {other}"),
        Ok(_) => panic!("http:// scheme must be rejected"),
    }

    bed.shutdown().await;
}

#[tokio::test]
async fn wss_token_auth_matrix() {
    let bed = spawn_tls_bed(AuthConfig::token(TOKEN, vec![Permission::Admin])).await;

    // Good token: full session.
    let client = bed.token_client().await;
    client.close().await;

    // Wrong token: close 4009 (AuthenticationFailed, protocol doc §4).
    match bed
        .connect(bed.client_config(ClientAuth::Token("wrong".into())))
        .await
    {
        Err(WsClientError::Closed { code, .. }) => assert_eq!(code, 4009),
        Err(other) => panic!("wrong token → expected AuthenticationFailed, got {other}"),
        Ok(_) => panic!("wrong token → expected AuthenticationFailed, got a session"),
    }

    // Missing credential: close 4009.
    match bed.connect(bed.client_config(ClientAuth::None)).await {
        Err(WsClientError::Closed { code, .. }) => assert_eq!(code, 4009),
        Err(other) => panic!("missing auth → expected AuthenticationFailed, got {other}"),
        Ok(_) => panic!("missing auth → expected AuthenticationFailed, got a session"),
    }

    bed.shutdown().await;
}

#[tokio::test]
async fn wss_password_auth_matrix() {
    let bed = spawn_tls_bed(AuthConfig::password(PASSWORD, vec![Permission::Read])).await;

    // Good password: challenge-response over wss.
    let mut client = bed
        .connect(bed.client_config(ClientAuth::Password(PASSWORD.into())))
        .await
        .expect("password handshake over wss");
    assert_eq!(client.permissions, vec![Permission::Read]);
    client
        .request_data(RequestKind::GetVersion)
        .await
        .expect("get_version");
    client.close().await;

    // Wrong password: close 4009.
    match bed
        .connect(bed.client_config(ClientAuth::Password("wrong".into())))
        .await
    {
        Err(WsClientError::Closed { code, .. }) => assert_eq!(code, 4009),
        Err(other) => panic!("wrong password → expected AuthenticationFailed, got {other}"),
        Ok(_) => panic!("wrong password → expected AuthenticationFailed, got a session"),
    }

    // Token method against a password policy: close 4009.
    match bed
        .connect(bed.client_config(ClientAuth::Token(PASSWORD.into())))
        .await
    {
        Err(WsClientError::Closed { code, .. }) => assert_eq!(code, 4009),
        Err(other) => panic!("token method → expected AuthenticationFailed, got {other}"),
        Ok(_) => panic!("token method → expected AuthenticationFailed, got a session"),
    }

    bed.shutdown().await;
}
