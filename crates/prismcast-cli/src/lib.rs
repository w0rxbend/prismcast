//! # prismcast-cli
//!
//! `studioctl`-style command-line controller (binary: `prismcast-cli`),
//! driving the application core over the Unix-socket IPC like any other
//! interface (PLAN.md §25, ADR-0006 §6). The CLI never links the core actor
//! internals and never initializes GTK or GStreamer; it speaks only the
//! native protocol through [`prismcast_remote::IpcClient`].
//!
//! ## Commands
//!
//! - `ping` — handshake + `get_version`.
//! - `status` — snapshot summary (scenes, sources, outputs, current scene).
//! - `scene list` — all scenes with IDs.
//! - `scene switch <uuid-or-name>` — make a scene current; the argument is a
//!   UUID or an exact scene name.
//!
//! Output is human-readable by default; `--json` prints the raw response
//! data. Exit codes: `0` success, `1` transport/protocol failure, `2` the
//! server rejected the request or the command line itself is invalid
//! (clap parse errors and conflicting auth flags).
//!
//! ## Authentication
//!
//! `--token`/`PRISMCAST_TOKEN` selects bearer-token auth and
//! `--password`/`PRISMCAST_PASSWORD` selects SHA-256 challenge-response auth
//! (docs/protocols/native-protocol.md §4). Flags override the environment;
//! setting both methods is a usage error. Secret values are never printed.
//!
//! **Layer: Interfaces.**

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use uuid::Uuid;

use prismcast_protocol::error::WireError;
use prismcast_protocol::request::RequestKind;
use prismcast_protocol::response::ResponseData;
use prismcast_protocol::subscription::SubscriptionSet;
use prismcast_remote::client::{ClientAuth, ClientError, IpcClient, IpcClientConfig};
use prismcast_remote::default_socket_path;

/// Exit code when the CLI could not reach or understand the server.
pub const EXIT_TRANSPORT_ERROR: u8 = 1;
/// Exit code when the server rejected the request (structured error).
pub const EXIT_REQUEST_REJECTED: u8 = 2;

/// Command-line interface definition.
#[derive(Debug, Parser)]
#[command(
    name = "prismcast-cli",
    version,
    about = "Control a running Prismcast studio over IPC"
)]
pub struct Cli {
    /// Path to the control socket
    /// (default: $XDG_RUNTIME_DIR/prismcast/control.sock).
    #[arg(long, global = true, value_name = "PATH")]
    pub socket: Option<PathBuf>,

    /// Machine-readable JSON output.
    #[arg(long, global = true)]
    pub json: bool,

    /// Bearer token for servers configured with token auth
    /// (env: PRISMCAST_TOKEN; the flag wins over the environment).
    #[arg(
        long,
        global = true,
        value_name = "TOKEN",
        env = "PRISMCAST_TOKEN",
        hide_env_values = true
    )]
    pub token: Option<String>,

    /// Password for SHA-256 challenge-response auth
    /// (env: PRISMCAST_PASSWORD; the flag wins over the environment).
    /// Conflicts with `--token`.
    #[arg(
        long,
        global = true,
        value_name = "PASSWORD",
        env = "PRISMCAST_PASSWORD",
        hide_env_values = true
    )]
    pub password: Option<String>,

    #[command(subcommand)]
    pub command: Commands,
}

/// Top-level commands.
#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Handshake and print the server version.
    Ping,
    /// Print a summary of the studio state.
    Status,
    /// Scene operations.
    Scene {
        #[command(subcommand)]
        command: SceneCommands,
    },
}

/// Scene subcommands.
#[derive(Debug, Subcommand)]
pub enum SceneCommands {
    /// List all scenes.
    List,
    /// Switch the current (program) scene by UUID or exact name.
    Switch {
        /// Scene UUID or exact name.
        scene: String,
    },
}

/// CLI failure with its process exit code.
#[derive(Debug, thiserror::Error)]
pub enum CliError {
    /// Transport/protocol failure (exit code 1).
    #[error("{0}")]
    Transport(String),
    /// The server rejected the request (exit code 2).
    #[error("{0} (code {1}, {2:?})")]
    Rejected(String, u16, prismcast_protocol::error::ErrorKind),
    /// Invalid combination of CLI arguments (exit code 2, matching clap).
    /// Never carries secret values — only which flags conflicted.
    #[error("{0}")]
    Usage(String),
}

impl CliError {
    /// The process exit code for this failure.
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Transport(_) => EXIT_TRANSPORT_ERROR,
            Self::Rejected(..) | Self::Usage(_) => EXIT_REQUEST_REJECTED,
        }
    }
}

impl From<ClientError> for CliError {
    fn from(error: ClientError) -> Self {
        match error {
            ClientError::RequestFailed(WireError {
                code,
                kind,
                message,
                ..
            }) => Self::Rejected(message, code, kind),
            other => Self::Transport(other.to_string()),
        }
    }
}

/// Maps the CLI's auth flags onto the client's auth method (WS-002).
/// `token` and `password` are the already-merged flag/env values; setting
/// both is a usage error. The error message names the flags, never the
/// secret values.
fn client_auth(token: Option<String>, password: Option<String>) -> Result<ClientAuth, CliError> {
    match (token, password) {
        (Some(_), Some(_)) => Err(CliError::Usage(
            "--token (PRISMCAST_TOKEN) and --password (PRISMCAST_PASSWORD) are mutually exclusive"
                .to_string(),
        )),
        (Some(token), None) => Ok(ClientAuth::Token(token)),
        (None, Some(password)) => Ok(ClientAuth::Password(password)),
        (None, None) => Ok(ClientAuth::None),
    }
}

/// Runs the parsed CLI to completion.
pub async fn run(cli: Cli) -> Result<(), CliError> {
    let socket = cli.socket.unwrap_or_else(default_socket_path);
    let client_config = IpcClientConfig {
        auth: client_auth(cli.token, cli.password)?,
        // The CLI is a command runner, not an event consumer.
        subscriptions: Some(SubscriptionSet::none()),
        client: Some(prismcast_protocol::handshake::ClientInfo {
            name: "prismcast-cli".to_string(),
            version: Some(env!("CARGO_PKG_VERSION").to_string()),
        }),
        ..IpcClientConfig::default()
    };
    let mut client = IpcClient::connect_with(&socket, client_config)
        .await
        .map_err(|error| {
            CliError::Transport(format!("cannot connect to {}: {error}", socket.display()))
        })?;

    match &cli.command {
        Commands::Ping => {
            let data = client.request_data(RequestKind::GetVersion).await?;
            print_data(&data, cli.json, |value| {
                match value {
                ResponseData::Version {
                    prismcast_version,
                    protocol_version,
                    available_requests,
                } => println!(
                    "pong — prismcast {prismcast_version}, protocol v{protocol_version}, {} request types",
                    available_requests.len()
                ),
                _ => unreachable!("get_version answered by request_data"),
            }
            });
        }
        Commands::Status => {
            let data = client.request_data(RequestKind::GetSnapshot).await?;
            print_data(&data, cli.json, |value| match value {
                ResponseData::Snapshot { snapshot } => {
                    let current = snapshot
                        .current_scene
                        .and_then(|id| {
                            snapshot
                                .scenes
                                .iter()
                                .find(|scene| scene.id == id)
                                .map(|scene| scene.name.clone())
                        })
                        .unwrap_or_else(|| "none".to_string());
                    let running = snapshot
                        .outputs
                        .iter()
                        .filter(|output| {
                            !matches!(
                                output.state,
                                prismcast_protocol::data::OutputState::Stopped
                                    | prismcast_protocol::data::OutputState::Failed
                            )
                        })
                        .count();
                    println!("scenes:  {} (current: {current})", snapshot.scenes.len());
                    println!("sources: {}", snapshot.sources.len());
                    println!("outputs: {} ({running} active)", snapshot.outputs.len());
                    println!(
                        "studio mode: {}",
                        if snapshot.studio_mode.is_some() {
                            "enabled"
                        } else {
                            "disabled"
                        }
                    );
                }
                _ => unreachable!("get_snapshot answered by request_data"),
            });
        }
        Commands::Scene {
            command: SceneCommands::List,
        } => {
            let data = client.request_data(RequestKind::ListScenes).await?;
            print_data(&data, cli.json, |value| match value {
                ResponseData::SceneList { scenes } => {
                    if scenes.is_empty() {
                        println!("no scenes");
                    }
                    for scene in scenes {
                        println!("{}  {}", scene.id, scene.name);
                    }
                }
                _ => unreachable!("list_scenes answered by request_data"),
            });
        }
        Commands::Scene {
            command: SceneCommands::Switch { scene },
        } => {
            let scene_id = match Uuid::parse_str(scene) {
                Ok(uuid) => uuid,
                Err(_) => resolve_scene_by_name(&mut client, scene).await?,
            };
            let data = client
                .request_data(RequestKind::SetCurrentScene { scene_id })
                .await?;
            print_data(&data, cli.json, |_value| {
                println!("switched to scene {scene_id}");
            });
        }
    }
    client.close().await;
    Ok(())
}

/// Resolves an exact scene name to its ID.
async fn resolve_scene_by_name(client: &mut IpcClient, name: &str) -> Result<Uuid, CliError> {
    match client.request_data(RequestKind::ListScenes).await? {
        ResponseData::SceneList { scenes } => scenes
            .iter()
            .find(|scene| scene.name == name)
            .map(|scene| scene.id)
            .ok_or_else(|| {
                CliError::Rejected(
                    format!(
                        "no scene named '{name}' ({} scenes available)",
                        scenes.len()
                    ),
                    prismcast_protocol::error::codes::NOT_FOUND,
                    prismcast_protocol::error::ErrorKind::NotFound,
                )
            }),
        other => Err(CliError::Transport(format!(
            "unexpected list_scenes response: {other:?}"
        ))),
    }
}

/// Prints the response data as JSON when requested, else runs the human
/// formatter.
fn print_data(data: &ResponseData, json: bool, human: impl FnOnce(&ResponseData)) {
    if json {
        match serde_json::to_string_pretty(data) {
            Ok(text) => println!("{text}"),
            Err(error) => eprintln!("cannot serialize response: {error}"),
        }
    } else {
        human(data);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_defaults_to_none() {
        let auth = client_auth(None, None).expect("no auth is valid");
        assert!(matches!(auth, ClientAuth::None));
    }

    #[test]
    fn auth_maps_token() {
        let auth = client_auth(Some("tok".to_string()), None).expect("token is valid");
        match auth {
            ClientAuth::Token(token) => assert_eq!(token, "tok"),
            other => panic!("expected ClientAuth::Token, got {other:?}"),
        }
    }

    #[test]
    fn auth_maps_password() {
        let auth = client_auth(None, Some("s3cret".to_string())).expect("password is valid");
        match auth {
            ClientAuth::Password(password) => assert_eq!(password, "s3cret"),
            other => panic!("expected ClientAuth::Password, got {other:?}"),
        }
    }

    #[test]
    fn auth_conflict_is_usage_error_without_secrets() {
        let error = client_auth(Some("tok-value".to_string()), Some("pw-value".to_string()))
            .expect_err("token + password must conflict");
        assert_eq!(error.exit_code(), 2);
        let message = error.to_string();
        assert!(message.contains("--token"), "message: {message}");
        assert!(message.contains("--password"), "message: {message}");
        assert!(!message.contains("tok-value"), "leaked token: {message}");
        assert!(!message.contains("pw-value"), "leaked password: {message}");
    }

    #[test]
    fn cli_parses_auth_flags() {
        let cli = Cli::try_parse_from([
            "prismcast-cli",
            "--token",
            "tok",
            "--password",
            "pw",
            "ping",
        ])
        .expect("both flags parse (the conflict is detected in client_auth)");
        assert_eq!(cli.token.as_deref(), Some("tok"));
        assert_eq!(cli.password.as_deref(), Some("pw"));
    }
}
