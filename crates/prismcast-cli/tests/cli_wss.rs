//! CLI-level integration proof (WS-003): the real `prismcast-cli` binary
//! against a real `CoreActor` + `WsServer` terminated with rustls (`wss://`),
//! rooted at a throwaway rcgen CA generated per test into a tempdir (never
//! committed, ADR-0022 §b). Covers `ping` and scene commands over wss with
//! `--tls-ca`, the `--insecure` danger switch with its stderr warning, trust
//! rejection without either flag, and the TLS-flag scoping usage errors.
//! The IPC e2e harness lives in `tests/cli.rs`; the pattern is mirrored here.

use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::process::Command;

use prismcast_app::{AppHandle, CoreConfig};
use prismcast_protocol::handshake::Permission;
use prismcast_protocol::request::RequestKind;
use prismcast_protocol::response::ResponseData;
use prismcast_remote::ws_client::{WsClient, WsClientConfig};
use prismcast_remote::{
    AuthConfig, ClientAuth, ClientTlsConfig, WsServer, WsServerConfig, WsTlsConfig,
};

const BIN: &str = env!("CARGO_BIN_EXE_prismcast-cli");
const TOKEN: &str = "wss-cli-token";

/// An rcgen CA + leaf (SANs: `localhost`, `127.0.0.1`) written to a fresh
/// tempdir; removed on drop. Mirrors the fixture in
/// `prismcast-remote/tests/tls_client.rs`.
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
            std::env::temp_dir().join(format!("prismcast-cli-wss-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create tempdir");

        let ca_key = KeyPair::generate().expect("ca key");
        let mut ca_params = CertificateParams::new(Vec::new()).expect("ca params");
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca_params
            .distinguished_name
            .push(DnType::CommonName, "prismcast cli wss test ca");
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

impl TestBed {
    async fn spawn() -> Self {
        let app = AppHandle::spawn(CoreConfig::default());
        let pki = TestPki::generate();
        let server = WsServer::bind(
            app.clone(),
            WsServerConfig {
                enabled: true,
                bind: SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
                tls: Some(WsTlsConfig {
                    cert_path: pki.cert_path.clone(),
                    key_path: pki.key_path.clone(),
                }),
                auth: AuthConfig::token(TOKEN, vec![Permission::Admin]),
                ..WsServerConfig::default()
            },
        )
        .await
        .expect("bind wss server");
        let addr = server.local_addr();
        Self {
            app,
            server,
            pki,
            addr,
        }
    }

    fn url(&self) -> String {
        format!("wss://{}/", self.addr)
    }

    fn ca_arg(&self) -> String {
        self.pki
            .ca_path
            .to_str()
            .expect("utf-8 ca path")
            .to_string()
    }

    /// Runs the CLI as a subprocess against the wss URL without blocking the
    /// test runtime (the server tasks live on the same runtime).
    async fn cli(&self, args: &[&str]) -> std::process::Output {
        let url = self.url();
        let args: Vec<String> = args.iter().map(|a| (*a).to_string()).collect();
        tokio::task::spawn_blocking(move || {
            let mut command = Command::new(BIN);
            command.arg("--url").arg(&url).args(&args);
            command.output().expect("spawn prismcast-cli")
        })
        .await
        .expect("join")
    }

    /// An in-process protocol client rooted at the test CA, for seeding and
    /// verifying studio state around the CLI runs.
    async fn protocol_client(&self) -> WsClient {
        WsClient::connect_url(
            &self.url(),
            WsClientConfig {
                auth: ClientAuth::Token(TOKEN.to_string()),
                tls: Some(ClientTlsConfig {
                    extra_ca_path: Some(self.pki.ca_path.clone()),
                    danger_accept_invalid_certs: false,
                }),
                ..WsClientConfig::default()
            },
        )
        .await
        .expect("connect protocol client")
    }

    async fn shutdown(self) {
        self.server.shutdown().await;
        self.app.shutdown().await;
    }
}

fn stdout(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stdout).to_string()
}

fn stderr(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).to_string()
}

#[tokio::test]
async fn ping_over_wss_with_tls_ca() {
    let bed = TestBed::spawn().await;
    let output = bed
        .cli(&["--tls-ca", &bed.ca_arg(), "--token", TOKEN, "ping"])
        .await;
    assert!(
        output.status.success(),
        "wss ping failed: {}",
        stderr(&output)
    );
    let text = stdout(&output);
    assert!(text.contains("pong"), "unexpected output: {text}");
    assert!(text.contains("protocol v1"), "unexpected output: {text}");
    bed.shutdown().await;
}

#[tokio::test]
async fn scene_commands_over_wss_with_tls_ca() {
    let bed = TestBed::spawn().await;

    // Seed a scene through the in-process protocol client.
    let mut client = bed.protocol_client().await;
    let scene_id = match client
        .request_data(RequestKind::AddScene {
            name: "Main".into(),
        })
        .await
        .expect("add_scene")
    {
        ResponseData::SceneCreated { scene_id } => scene_id,
        other => panic!("unexpected: {other:?}"),
    };

    let ca = bed.ca_arg();
    let list = bed
        .cli(&["--tls-ca", &ca, "--token", TOKEN, "scene", "list"])
        .await;
    assert!(
        list.status.success(),
        "wss scene list failed: {}",
        stderr(&list)
    );
    let text = stdout(&list);
    assert!(text.contains("Main"), "unexpected output: {text}");

    let switch = bed
        .cli(&["--tls-ca", &ca, "--token", TOKEN, "scene", "switch", "Main"])
        .await;
    assert!(
        switch.status.success(),
        "wss scene switch failed: {}",
        stderr(&switch)
    );

    // The server state reflects the switch made over wss.
    match client
        .request_data(RequestKind::GetSnapshot)
        .await
        .expect("get_snapshot")
    {
        ResponseData::Snapshot { snapshot } => {
            assert_eq!(snapshot.current_scene, Some(scene_id));
        }
        other => panic!("unexpected: {other:?}"),
    }

    client.close().await;
    bed.shutdown().await;
}

#[tokio::test]
async fn insecure_over_wss_succeeds_and_warns_on_stderr() {
    let bed = TestBed::spawn().await;
    let output = bed.cli(&["--insecure", "--token", TOKEN, "ping"]).await;
    assert!(
        output.status.success(),
        "wss ping with --insecure failed: {}",
        stderr(&output)
    );
    assert!(stdout(&output).contains("pong"));
    let err = stderr(&output);
    assert!(
        err.contains("warning") && err.contains("--insecure"),
        "missing --insecure warning on stderr: {err}"
    );
    assert!(!err.contains(TOKEN), "leaked token: {err}");
    bed.shutdown().await;
}

#[tokio::test]
async fn wss_without_tls_ca_or_insecure_fails_verification() {
    // The test CA is not in the system roots: with neither --tls-ca nor
    // --insecure the handshake must fail (exit 1, transport error).
    let bed = TestBed::spawn().await;
    let output = bed.cli(&["--token", TOKEN, "ping"]).await;
    assert_eq!(output.status.code(), Some(1));
    let err = stderr(&output);
    assert!(err.contains("cannot connect"), "unexpected stderr: {err}");
    assert!(!err.contains(TOKEN), "leaked token: {err}");
    bed.shutdown().await;
}

#[tokio::test]
async fn wrong_token_over_wss_is_rejected_without_leaking_secrets() {
    let bed = TestBed::spawn().await;
    let output = bed
        .cli(&[
            "--tls-ca",
            &bed.ca_arg(),
            "--token",
            "not-the-token",
            "ping",
        ])
        .await;
    assert_eq!(output.status.code(), Some(1));
    let err = stderr(&output);
    assert!(err.contains("4009"), "unexpected stderr: {err}");
    assert!(!err.contains("not-the-token"), "leaked token: {err}");
    assert!(!err.contains(TOKEN), "leaked token: {err}");
    bed.shutdown().await;
}

#[test]
fn tls_flags_with_ws_url_are_a_usage_error() {
    // Validation happens before any I/O, so an unreachable URL is fine.
    for tls_flag in ["--tls-ca", "--insecure"] {
        let mut command = Command::new(BIN);
        command.arg("--url").arg("ws://127.0.0.1:1");
        if tls_flag == "--tls-ca" {
            command.arg("--tls-ca").arg("/tmp/ca.pem");
        } else {
            command.arg("--insecure");
        }
        let output = command.arg("ping").output().expect("spawn prismcast-cli");
        assert_eq!(output.status.code(), Some(2), "flag {tls_flag}");
        let err = stderr(&output);
        assert!(
            err.contains(tls_flag),
            "stderr should name {tls_flag}: {err}"
        );
        assert!(err.contains("wss://"), "unexpected stderr: {err}");
    }
}

#[test]
fn url_conflicts_with_socket_flag() {
    let output = Command::new(BIN)
        .arg("--socket")
        .arg("/tmp/control.sock")
        .arg("--url")
        .arg("wss://127.0.0.1:4456")
        .arg("ping")
        .output()
        .expect("spawn prismcast-cli");
    assert_eq!(output.status.code(), Some(2));
    let err = stderr(&output);
    assert!(err.contains("--url"), "unexpected stderr: {err}");
    assert!(err.contains("--socket"), "unexpected stderr: {err}");
}

#[test]
fn tls_flags_without_url_are_a_clap_error() {
    for args in [vec!["--tls-ca", "/tmp/ca.pem"], vec!["--insecure"]] {
        let output = Command::new(BIN)
            .args(&args)
            .arg("ping")
            .output()
            .expect("spawn prismcast-cli");
        assert_eq!(output.status.code(), Some(2), "args {args:?}");
    }
}

#[test]
fn tls_flags_with_socket_flag_are_a_usage_error() {
    // clap's requires = "url" is suppressed because --url conflicts with the
    // present --socket, so the CLI's own validation must reject this.
    for args in [vec!["--tls-ca", "/tmp/ca.pem"], vec!["--insecure"]] {
        let output = Command::new(BIN)
            .arg("--socket")
            .arg("/tmp/control.sock")
            .args(&args)
            .arg("ping")
            .output()
            .expect("spawn prismcast-cli");
        assert_eq!(output.status.code(), Some(2), "args {args:?}");
        let err = stderr(&output);
        assert!(
            err.contains(args[0]),
            "stderr should name {}: {err}",
            args[0]
        );
        assert!(err.contains("wss://"), "unexpected stderr: {err}");
    }
}
