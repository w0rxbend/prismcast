//! CLI-level integration proof (IPC-002): the real `prismcast-cli` binary
//! against a real `CoreActor` + `IpcServer` on a tempdir socket. The CLI is
//! spawned as a subprocess — it never touches the core actor from this
//! process (PLAN.md §25, ADR-0006 §6).

use std::path::PathBuf;
use std::process::Command;

use prismcast_app::{AppHandle, CoreConfig};
use prismcast_protocol::handshake::Permission;
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
        Self::spawn_with(AuthConfig::allow_local()).await
    }

    async fn spawn_with(auth: AuthConfig) -> Self {
        let dir = std::env::temp_dir().join(format!("prismcast-cli-test-{}", uuid::Uuid::new_v4()));
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
        self.cli_env(args, &[]).await
    }

    /// Like [`cli`](Self::cli), with extra environment variables for the
    /// subprocess (e.g. `PRISMCAST_PASSWORD`).
    async fn cli_env(&self, args: &[&str], envs: &[(&str, &str)]) -> std::process::Output {
        let socket = self.socket.to_str().expect("utf-8 socket path").to_string();
        let args: Vec<String> = args.iter().map(|a| (*a).to_string()).collect();
        let envs: Vec<(String, String)> = envs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        tokio::task::spawn_blocking(move || {
            let mut command = Command::new(BIN);
            command.arg("--socket").arg(&socket).args(&args);
            for (key, value) in &envs {
                command.env(key, value);
            }
            command.output().expect("spawn prismcast-cli")
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
async fn undo_redo_human_json_and_empty_history_use_real_socket() {
    let bed = TestBed::spawn().await;
    let empty = bed.cli(&["undo"]).await;
    assert_eq!(empty.status.code(), Some(2));
    assert!(stdout(&empty).is_empty());
    assert!(stderr(&empty).contains("InvalidField"));
    let mut client = IpcClient::connect(&bed.socket).await.unwrap();
    let scene = match client
        .request_data(RequestKind::AddScene {
            name: "Original".into(),
        })
        .await
        .unwrap()
    {
        ResponseData::SceneCreated { scene_id } => scene_id,
        other => panic!("{other:?}"),
    };
    client
        .request_data(RequestKind::RenameScene {
            scene_id: scene,
            name: "Edited".into(),
        })
        .await
        .unwrap();
    let undo = bed.cli(&["undo"]).await;
    assert!(undo.status.success(), "{}", stderr(&undo));
    assert_eq!(stdout(&undo), "undo applied\n");
    assert!(stderr(&undo).is_empty());
    assert_eq!(
        bed.app
            .snapshot()
            .scenes()
            .find(|s| *s.id.as_uuid() == scene)
            .unwrap()
            .name,
        "Original"
    );
    let redo = bed.cli(&["--json", "redo"]).await;
    assert!(redo.status.success(), "{}", stderr(&redo));
    assert!(stderr(&redo).is_empty());
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&stdout(&redo)).unwrap(),
        serde_json::json!({"data":"empty"})
    );
    assert_eq!(
        bed.app
            .snapshot()
            .scenes()
            .find(|s| *s.id.as_uuid() == scene)
            .unwrap()
            .name,
        "Edited"
    );
    let empty = bed.cli(&["--json", "redo"]).await;
    assert_eq!(empty.status.code(), Some(2));
    assert!(stdout(&empty).is_empty());
    assert!(stderr(&empty).contains("InvalidField"));
    client.close().await;
    bed.shutdown().await;
}

#[tokio::test]
async fn history_cli_replay_permissions_and_usage_errors_preserve_state() {
    for (permissions, mixed) in [
        (vec![Permission::Read], false),
        (vec![Permission::Read, Permission::ControlAudio], false),
        (vec![Permission::Read, Permission::ControlScenes], true),
    ] {
        let bed = TestBed::spawn_with(AuthConfig::token("history-cli", permissions)).await;
        let local = |request| prismcast_remote::map::command_from_wire(request).unwrap();
        bed.app
            .dispatch(local(RequestKind::AddScene {
                name: "Original".into(),
            }))
            .await
            .unwrap();
        let scene = *bed.app.snapshot().scenes().next().unwrap().id.as_uuid();
        let rename = RequestKind::RenameScene {
            scene_id: scene,
            name: "Edited".into(),
        };
        let edit = if mixed {
            bed.app
                .dispatch(local(RequestKind::AddSource {
                    kind: prismcast_protocol::data::SourceKind::TestPattern,
                    name: "tone".into(),
                }))
                .await
                .unwrap();
            let source = *bed.app.snapshot().sources().next().unwrap().id.as_uuid();
            RequestKind::Transaction {
                commands: vec![
                    rename,
                    RequestKind::SetSourceMuted {
                        source_id: source,
                        muted: true,
                    },
                ],
            }
        } else {
            rename
        };
        bed.app.dispatch(local(edit)).await.unwrap();
        for command in ["undo", "redo"] {
            if command == "redo" {
                bed.app.undo().await.unwrap();
            }
            let before = bed.app.snapshot();
            let output = bed.cli(&["--token", "history-cli", command]).await;
            assert_eq!(output.status.code(), Some(2));
            assert!(stdout(&output).is_empty());
            assert!(stderr(&output).contains("Forbidden"));
            assert!(!stderr(&output).contains("history-cli"));
            assert_eq!(bed.app.snapshot().revision(), before.revision());
            assert_eq!(bed.app.snapshot().state(), before.state());
            assert_eq!(bed.app.snapshot().history(), before.history());
        }
        // Undo/redo accept no target or history payload at the CLI boundary.
        let usage = bed.cli(&["undo", "target-id"]).await;
        assert_eq!(usage.status.code(), Some(2));
        assert!(stderr(&usage).contains("unexpected argument"));
        bed.shutdown().await;
    }
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

/// A socket path that no server listens on, for auth parsing tests that
/// never reach a server.
fn missing_socket() -> String {
    let dir =
        std::env::temp_dir().join(format!("prismcast-cli-test-auth-{}", uuid::Uuid::new_v4()));
    dir.join("control.sock")
        .to_str()
        .expect("utf-8")
        .to_string()
}

#[test]
fn conflicting_token_and_password_flags_are_a_usage_error() {
    let output = Command::new(BIN)
        .arg("--socket")
        .arg(missing_socket())
        .arg("--token")
        .arg("tok-value")
        .arg("--password")
        .arg("pw-value")
        .arg("ping")
        .output()
        .expect("spawn prismcast-cli");
    assert_eq!(output.status.code(), Some(2));
    let err = stderr(&output);
    assert!(err.contains("--token"), "unexpected stderr: {err}");
    assert!(err.contains("--password"), "unexpected stderr: {err}");
    assert!(!err.contains("tok-value"), "leaked token: {err}");
    assert!(!err.contains("pw-value"), "leaked password: {err}");
}

#[test]
fn conflicting_auth_env_vars_are_a_usage_error() {
    let output = Command::new(BIN)
        .arg("--socket")
        .arg(missing_socket())
        .arg("ping")
        .env("PRISMCAST_TOKEN", "tok-value")
        .env("PRISMCAST_PASSWORD", "pw-value")
        .output()
        .expect("spawn prismcast-cli");
    assert_eq!(output.status.code(), Some(2));
    let err = stderr(&output);
    assert!(
        err.contains("mutually exclusive"),
        "unexpected stderr: {err}"
    );
    assert!(!err.contains("tok-value"), "leaked token: {err}");
    assert!(!err.contains("pw-value"), "leaked password: {err}");
}

#[test]
fn password_flag_and_env_mix_with_token_env_conflicts() {
    // The flag and the env fallback feed the same mapping: a token from the
    // environment still conflicts with a --password flag.
    let output = Command::new(BIN)
        .arg("--socket")
        .arg(missing_socket())
        .arg("--password")
        .arg("pw-value")
        .arg("ping")
        .env("PRISMCAST_TOKEN", "tok-value")
        .output()
        .expect("spawn prismcast-cli");
    assert_eq!(output.status.code(), Some(2));
    let err = stderr(&output);
    assert!(
        err.contains("mutually exclusive"),
        "unexpected stderr: {err}"
    );
}

#[test]
fn password_flag_is_accepted() {
    // No server listening: parsing and auth mapping succeeded, the failure
    // is the transport (exit 1), and the password is not echoed anywhere.
    let output = Command::new(BIN)
        .arg("--socket")
        .arg(missing_socket())
        .arg("--password")
        .arg("pw-value")
        .arg("ping")
        .output()
        .expect("spawn prismcast-cli");
    assert_eq!(output.status.code(), Some(1));
    let err = stderr(&output);
    assert!(err.contains("cannot connect"), "unexpected stderr: {err}");
    assert!(!err.contains("pw-value"), "leaked password: {err}");
}

#[test]
fn password_env_is_accepted() {
    let output = Command::new(BIN)
        .arg("--socket")
        .arg(missing_socket())
        .arg("ping")
        .env("PRISMCAST_PASSWORD", "pw-value")
        .output()
        .expect("spawn prismcast-cli");
    assert_eq!(output.status.code(), Some(1));
    let err = stderr(&output);
    assert!(err.contains("cannot connect"), "unexpected stderr: {err}");
    assert!(!err.contains("pw-value"), "leaked password: {err}");
}

// --- WS-002: auth end-to-end over a real socket (protocol doc §4) ---

const CORRECT_PASSWORD: &str = "correct-password";
const WRONG_PASSWORD: &str = "wrong-password";

#[tokio::test]
async fn password_auth_succeeds_with_flag_and_env() {
    let bed = TestBed::spawn_with(AuthConfig::password(
        CORRECT_PASSWORD,
        vec![Permission::Admin],
    ))
    .await;

    let flag = bed.cli(&["--password", CORRECT_PASSWORD, "ping"]).await;
    assert!(
        flag.status.success(),
        "ping with --password failed: {}",
        stderr(&flag)
    );
    assert!(stdout(&flag).contains("pong"));

    let env = bed
        .cli_env(&["ping"], &[("PRISMCAST_PASSWORD", CORRECT_PASSWORD)])
        .await;
    assert!(
        env.status.success(),
        "ping with PRISMCAST_PASSWORD failed: {}",
        stderr(&env)
    );
    assert!(stdout(&env).contains("pong"));

    bed.shutdown().await;
}

#[tokio::test]
async fn wrong_password_is_rejected_without_leaking_secrets() {
    let bed = TestBed::spawn_with(AuthConfig::password(
        CORRECT_PASSWORD,
        vec![Permission::Admin],
    ))
    .await;

    // Wrong password → server closes with AuthenticationFailed (4009); the
    // CLI surfaces it as a transport error (exit 1).
    let output = bed.cli(&["--password", WRONG_PASSWORD, "ping"]).await;
    assert_eq!(output.status.code(), Some(1));
    let err = stderr(&output);
    assert!(err.contains("4009"), "unexpected stderr: {err}");
    assert!(!err.contains(WRONG_PASSWORD), "leaked password: {err}");
    assert!(!err.contains(CORRECT_PASSWORD), "leaked password: {err}");

    // No credentials at all → same rejection.
    let output = bed.cli(&["ping"]).await;
    assert_eq!(output.status.code(), Some(1));
    let err = stderr(&output);
    assert!(err.contains("4009"), "unexpected stderr: {err}");
    assert!(!err.contains(CORRECT_PASSWORD), "leaked password: {err}");

    bed.shutdown().await;
}

#[tokio::test]
async fn token_auth_succeeds_over_socket() {
    let bed = TestBed::spawn_with(AuthConfig::token("cli-token", vec![Permission::Admin])).await;

    let ok = bed.cli(&["--token", "cli-token", "ping"]).await;
    assert!(
        ok.status.success(),
        "ping with --token failed: {}",
        stderr(&ok)
    );
    assert!(stdout(&ok).contains("pong"));

    let wrong = bed.cli(&["--token", "not-the-token", "ping"]).await;
    assert_eq!(wrong.status.code(), Some(1));
    let err = stderr(&wrong);
    assert!(err.contains("4009"), "unexpected stderr: {err}");
    assert!(!err.contains("cli-token"), "leaked token: {err}");
    assert!(!err.contains("not-the-token"), "leaked token: {err}");

    bed.shutdown().await;
}
