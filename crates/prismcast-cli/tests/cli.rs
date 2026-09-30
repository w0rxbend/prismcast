//! CLI-level integration proof (IPC-002): the real `prismcast-cli` binary
//! against a real `CoreActor` + `IpcServer` on a tempdir socket. The CLI is
//! spawned as a subprocess — it never touches the core actor from this
//! process (PLAN.md §25, ADR-0006 §6).

use std::path::PathBuf;
use std::process::Command;

use prismcast_app::{AppHandle, CoreConfig};
use prismcast_protocol::request::RequestKind;
use prismcast_protocol::response::ResponseData;
use prismcast_remote::{AuthConfig, IpcClient, IpcServer, IpcServerConfig};

const BIN: &str = env!("CARGO_BIN_EXE_prismcast-cli");

struct TestBed {
    app: AppHandle,
    server: IpcServer,
    dir: PathBuf,
    socket: PathBuf,
}

impl TestBed {
    async fn spawn() -> Self {
        let dir = std::env::temp_dir().join(format!("prismcast-cli-test-{}", uuid::Uuid::new_v4()));
        let socket = dir.join("control.sock");
        let app = AppHandle::spawn(CoreConfig::default());
        let server = IpcServer::bind(
            app.clone(),
            IpcServerConfig {
                socket_path: Some(socket.clone()),
                auth: AuthConfig::allow_local(),
                ..IpcServerConfig::default()
            },
        )
        .await
        .expect("bind server");
        Self {
            app,
            server,
            dir,
            socket,
        }
    }

    /// Runs the CLI as a subprocess without blocking the test runtime (the
    /// server tasks live on the same runtime).
    async fn cli(&self, args: &[&str]) -> std::process::Output {
        let socket = self.socket.to_str().expect("utf-8 socket path").to_string();
        let args: Vec<String> = args.iter().map(|a| (*a).to_string()).collect();
        tokio::task::spawn_blocking(move || {
            Command::new(BIN)
                .arg("--socket")
                .arg(&socket)
                .args(&args)
                .output()
                .expect("spawn prismcast-cli")
        })
        .await
        .expect("join")
    }

    async fn shutdown(self) {
        self.server.shutdown().await;
        self.app.shutdown().await;
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn stdout(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stdout).to_string()
}

fn stderr(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).to_string()
}

#[tokio::test]
async fn ping_reports_server_version() {
    let bed = TestBed::spawn().await;
    let output = bed.cli(&["ping"]).await;
    assert!(output.status.success(), "ping failed: {}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("pong"), "unexpected output: {text}");
    assert!(text.contains("protocol v1"), "unexpected output: {text}");

    let json = bed.cli(&["--json", "ping"]).await;
    assert!(json.status.success(), "json ping failed: {}", stderr(&json));
    let value: serde_json::Value =
        serde_json::from_str(&stdout(&json)).expect("json output parses");
    assert_eq!(value["data"], "version");
    assert_eq!(value["protocol_version"], 1);
    assert!(value["available_requests"].is_array());

    bed.shutdown().await;
}

#[tokio::test]
async fn status_summarizes_state() {
    let bed = TestBed::spawn().await;
    let output = bed.cli(&["status"]).await;
    assert!(
        output.status.success(),
        "status failed: {}",
        stderr(&output)
    );
    let text = stdout(&output);
    assert!(text.contains("scenes:  0"), "unexpected output: {text}");
    assert!(text.contains("sources: 0"), "unexpected output: {text}");
    assert!(text.contains("outputs: 0"), "unexpected output: {text}");
    bed.shutdown().await;
}

#[tokio::test]
async fn scene_list_and_switch_by_name_and_uuid() {
    let bed = TestBed::spawn().await;

    // Seed two scenes through the protocol client.
    let mut client = IpcClient::connect(&bed.socket).await.expect("connect");
    let main = match client
        .request_data(RequestKind::AddScene {
            name: "Main".into(),
        })
        .await
        .expect("add_scene")
    {
        ResponseData::SceneCreated { scene_id } => scene_id,
        other => panic!("unexpected: {other:?}"),
    };
    let second = match client
        .request_data(RequestKind::AddScene {
            name: "Intermission".into(),
        })
        .await
        .expect("add_scene")
    {
        ResponseData::SceneCreated { scene_id } => scene_id,
        other => panic!("unexpected: {other:?}"),
    };

    let list = bed.cli(&["scene", "list"]).await;
    assert!(
        list.status.success(),
        "scene list failed: {}",
        stderr(&list)
    );
    let text = stdout(&list);
    assert!(text.contains("Main"), "unexpected output: {text}");
    assert!(text.contains("Intermission"), "unexpected output: {text}");
    assert!(
        text.contains(&main.to_string()),
        "unexpected output: {text}"
    );

    // Switch by name, then by UUID.
    let by_name = bed.cli(&["scene", "switch", "Intermission"]).await;
    assert!(
        by_name.status.success(),
        "switch by name failed: {}",
        stderr(&by_name)
    );
    let by_uuid = bed.cli(&["scene", "switch", &main.to_string()]).await;
    assert!(
        by_uuid.status.success(),
        "switch by uuid failed: {}",
        stderr(&by_uuid)
    );

    // The server state reflects the last switch.
    let snapshot = client
        .request_data(RequestKind::GetSnapshot)
        .await
        .expect("get_snapshot");
    match snapshot {
        ResponseData::Snapshot { snapshot } => {
            assert_eq!(snapshot.current_scene, Some(main));
            assert_ne!(snapshot.current_scene, Some(second));
        }
        other => panic!("unexpected: {other:?}"),
    }

    // Unknown scene name → exit code 2 with an error on stderr.
    let unknown = bed.cli(&["scene", "switch", "no-such-scene"]).await;
    assert_eq!(unknown.status.code(), Some(2));
    assert!(stderr(&unknown).contains("no-such-scene"));

    client.close().await;
    bed.shutdown().await;
}

#[tokio::test]
async fn cli_reports_missing_server() {
    let dir = std::env::temp_dir().join(format!(
        "prismcast-cli-test-missing-{}",
        uuid::Uuid::new_v4()
    ));
    let socket = dir.join("control.sock");
    let socket_str = socket.to_str().expect("utf-8");
    let output = Command::new(BIN)
        .arg("--socket")
        .arg(socket_str)
        .arg("ping")
        .output()
        .expect("spawn prismcast-cli");
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("cannot connect"));
}
