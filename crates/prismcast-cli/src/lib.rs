//! # prismcast-cli
//!
//! `studioctl`-style command-line controller (binary: `prismcast-cli`),
//! driving the application core over the Unix-socket IPC like any other
//! interface (PLAN.md §25, ADR-0006 §6). The CLI never links the core actor
//! internals and never initializes GTK or GStreamer; it speaks only the
//! native protocol through [`prismcast_remote::IpcClient`].
//!
//! ## Transports
//!
//! The default transport is the Unix control socket (`--socket`). `--url
//! ws(s)://host:port` switches to the WebSocket transport (WS-001/WS-003)
//! through [`prismcast_remote::WsClient`] — the same protocol, handshake,
//! and auth, over TCP. For `wss://`, `--tls-ca <PATH>` adds a private CA
//! bundle on top of the system roots and `--insecure` disables certificate
//! verification entirely (warn-logged to stderr; diagnostics only). Both
//! flags are rejected unless the URL uses the `wss://` scheme.
//!
//! ## Commands
//!
//! - `ping` — handshake + `get_version`.
//! - `status` — snapshot summary (scenes, sources, outputs, current scene).
//! - `undo` / `redo` — replay the latest authorized global history entry.
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
use prismcast_remote::tls::ClientTlsConfig;
use prismcast_remote::ws_client::{WsClient, WsClientConfig, WsClientError};

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

    /// Connect over WebSocket instead of the control socket
    /// (`ws://host:port` or `wss://host:port`). Conflicts with `--socket`.
    #[arg(long, global = true, value_name = "URL", conflicts_with = "socket")]
    pub url: Option<String>,

    /// Extra PEM CA bundle to trust for a `wss://` `--url`
    /// (private/self-signed deployments). Requires `--url` with a `wss://`
    /// URL.
    #[arg(long, global = true, value_name = "PATH", requires = "url")]
    pub tls_ca: Option<PathBuf>,

    /// Disable TLS certificate verification for a `wss://` `--url`
    /// (dangerous; diagnostics only — a warning is printed on use).
    /// Requires `--url` with a `wss://` URL.
    #[arg(long, global = true, requires = "url")]
    pub insecure: bool,

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
    /// Undo the latest global history entry (requires its mutation permissions).
    Undo,
    /// Redo the latest undone global history entry (requires its mutation permissions).
    Redo,
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

impl From<WsClientError> for CliError {
    fn from(error: WsClientError) -> Self {
        match error {
            WsClientError::RequestFailed(WireError {
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

/// The connected transport selected by the CLI flags: Unix-socket IPC or
/// WebSocket (`ws://`/`wss://`). Both clients speak the same protocol with
/// near-identical APIs, so dispatch is a thin match.
enum CliClient {
    Ipc(IpcClient),
    Ws(WsClient),
}

impl CliClient {
    async fn request_data(&mut self, kind: RequestKind) -> Result<ResponseData, CliError> {
        match self {
            Self::Ipc(client) => Ok(client.request_data(kind).await?),
            Self::Ws(client) => Ok(client.request_data(kind).await?),
        }
    }

    async fn close(self) {
        match self {
            Self::Ipc(client) => client.close().await,
            Self::Ws(client) => client.close().await,
        }
    }
}

/// The transport selected by the CLI flags, validated before any I/O.
#[derive(Debug)]
enum TransportPlan {
    Ipc {
        socket: PathBuf,
    },
    Ws {
        url: String,
        tls: Option<ClientTlsConfig>,
    },
}

/// Maps the CLI's transport flags onto a connection plan (WS-003).
/// `--tls-ca`/`--insecure` are TLS-only: they are rejected unless `--url`
/// names a `wss://` URL (a `ws://` or IPC server never looks at TLS
/// material, so silently accepting the flags would imply protection that
/// does not exist). Clap's `requires = "url"` covers the bare case, but it
/// is suppressed when `--socket` is present (the required `--url` conflicts
/// with it), so the rule is enforced here in full. The usage error names the
/// flags, never values.
fn transport_plan(cli: &Cli) -> Result<TransportPlan, CliError> {
    let mut tls_flags = Vec::new();
    if cli.tls_ca.is_some() {
        tls_flags.push("--tls-ca");
    }
    if cli.insecure {
        tls_flags.push("--insecure");
    }
    let tls_only_on_wss = |is_wss: bool| {
        if !is_wss && !tls_flags.is_empty() {
            return Err(CliError::Usage(format!(
                "{} only apply to a wss:// --url",
                tls_flags.join(" and ")
            )));
        }
        Ok(())
    };
    match &cli.url {
        None => {
            tls_only_on_wss(false)?;
            Ok(TransportPlan::Ipc {
                socket: cli.socket.clone().unwrap_or_else(default_socket_path),
            })
        }
        Some(url) => {
            let is_wss = url
                .split_once("://")
                .is_some_and(|(scheme, _)| scheme.eq_ignore_ascii_case("wss"));
            tls_only_on_wss(is_wss)?;
            let tls = (cli.tls_ca.is_some() || cli.insecure).then(|| ClientTlsConfig {
                extra_ca_path: cli.tls_ca.clone(),
                danger_accept_invalid_certs: cli.insecure,
            });
            Ok(TransportPlan::Ws {
                url: url.clone(),
                tls,
            })
        }
    }
}

/// Connects according to the plan and performs the protocol handshake.
async fn connect(plan: &TransportPlan, auth: ClientAuth) -> Result<CliClient, CliError> {
    let client_info = prismcast_protocol::handshake::ClientInfo {
        name: "prismcast-cli".to_string(),
        version: Some(env!("CARGO_PKG_VERSION").to_string()),
    };
    match plan {
        TransportPlan::Ipc { socket } => {
            let config = IpcClientConfig {
                auth,
                // The CLI is a command runner, not an event consumer.
                subscriptions: Some(SubscriptionSet::none()),
                client: Some(client_info),
                ..IpcClientConfig::default()
            };
            let client = IpcClient::connect_with(socket, config)
                .await
                .map_err(|error| {
                    CliError::Transport(format!("cannot connect to {}: {error}", socket.display()))
                })?;
            Ok(CliClient::Ipc(client))
        }
        TransportPlan::Ws { url, tls } => {
            if tls
                .as_ref()
                .is_some_and(|tls| tls.danger_accept_invalid_certs)
            {
                eprintln!(
                    "warning: --insecure disables TLS certificate verification; \
                     traffic is encrypted but not authenticated"
                );
            }
            let config = WsClientConfig {
                auth,
                // The CLI is a command runner, not an event consumer.
                subscriptions: Some(SubscriptionSet::none()),
                client: Some(client_info),
                tls: tls.clone(),
                ..WsClientConfig::default()
            };
            let client = WsClient::connect_url(url, config).await.map_err(|error| {
                CliError::Transport(format!("cannot connect to {url}: {error}"))
            })?;
            Ok(CliClient::Ws(client))
        }
    }
}

/// Runs the parsed CLI to completion.
pub async fn run(cli: Cli) -> Result<(), CliError> {
    let plan = transport_plan(&cli)?;
    let auth = client_auth(cli.token, cli.password)?;
    let mut client = connect(&plan, auth).await?;

    match &cli.command {
        Commands::Undo | Commands::Redo => {
            let request = if matches!(cli.command, Commands::Undo) {
                RequestKind::Undo
            } else {
                RequestKind::Redo
            };
            let label = request.tag();
            let data = client.request_data(request).await?;
            if data != ResponseData::Empty {
                return Err(CliError::Transport(format!(
                    "unexpected {label} response data"
                )));
            }
            print_data(&data, cli.json, |_| println!("{label} applied"));
        }
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
async fn resolve_scene_by_name(client: &mut CliClient, name: &str) -> Result<Uuid, CliError> {
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

    // --- WS-003: WebSocket transport flag matrix ---

    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from(args).expect("args parse")
    }

    #[test]
    fn url_conflicts_with_socket() {
        let result = Cli::try_parse_from([
            "prismcast-cli",
            "--socket",
            "/tmp/control.sock",
            "--url",
            "ws://127.0.0.1:4456",
            "ping",
        ]);
        let error = result.expect_err("--url and --socket must conflict");
        assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn tls_ca_requires_url() {
        let result = Cli::try_parse_from(["prismcast-cli", "--tls-ca", "/tmp/ca.pem", "ping"]);
        assert!(result.is_err(), "--tls-ca without --url must not parse");
    }

    #[test]
    fn insecure_requires_url() {
        let result = Cli::try_parse_from(["prismcast-cli", "--insecure", "ping"]);
        assert!(result.is_err(), "--insecure without --url must not parse");
    }

    #[test]
    fn plan_defaults_to_ipc_socket() {
        let cli = parse(&["prismcast-cli", "ping"]);
        match transport_plan(&cli).expect("plan") {
            TransportPlan::Ipc { socket } => assert_eq!(socket, default_socket_path()),
            other => panic!("expected the IPC plan, got {other:?}"),
        }
    }

    #[test]
    fn plan_honours_explicit_socket() {
        let cli = parse(&["prismcast-cli", "--socket", "/tmp/x.sock", "ping"]);
        match transport_plan(&cli).expect("plan") {
            TransportPlan::Ipc { socket } => assert_eq!(socket, PathBuf::from("/tmp/x.sock")),
            other => panic!("expected the IPC plan, got {other:?}"),
        }
    }

    #[test]
    fn plan_ws_plaintext_without_tls() {
        let cli = parse(&["prismcast-cli", "--url", "ws://127.0.0.1:4456", "ping"]);
        match transport_plan(&cli).expect("plan") {
            TransportPlan::Ws { url, tls } => {
                assert_eq!(url, "ws://127.0.0.1:4456");
                assert!(tls.is_none(), "plaintext ws:// carries no TLS config");
            }
            other => panic!("expected the WS plan, got {other:?}"),
        }
    }

    #[test]
    fn plan_wss_defaults_to_native_roots() {
        let cli = parse(&["prismcast-cli", "--url", "wss://studio.local:4456", "ping"]);
        match transport_plan(&cli).expect("plan") {
            TransportPlan::Ws { url, tls } => {
                assert_eq!(url, "wss://studio.local:4456");
                assert!(tls.is_none(), "None = platform native root store");
            }
            other => panic!("expected the WS plan, got {other:?}"),
        }
    }

    #[test]
    fn plan_wss_maps_tls_ca_and_insecure() {
        let cli = parse(&[
            "prismcast-cli",
            "--url",
            "wss://studio.local:4456",
            "--tls-ca",
            "/tmp/ca.pem",
            "--insecure",
            "ping",
        ]);
        match transport_plan(&cli).expect("plan") {
            TransportPlan::Ws { tls, .. } => {
                let tls = tls.expect("tls config");
                assert_eq!(tls.extra_ca_path, Some(PathBuf::from("/tmp/ca.pem")));
                assert!(tls.danger_accept_invalid_certs);
            }
            other => panic!("expected the WS plan, got {other:?}"),
        }
    }

    #[test]
    fn tls_ca_with_ws_url_is_a_usage_error_naming_the_flag() {
        let cli = parse(&[
            "prismcast-cli",
            "--url",
            "ws://127.0.0.1:4456",
            "--tls-ca",
            "/tmp/ca.pem",
            "ping",
        ]);
        let error = transport_plan(&cli).expect_err("--tls-ca needs wss://");
        assert_eq!(error.exit_code(), 2);
        let message = error.to_string();
        assert!(message.contains("--tls-ca"), "message: {message}");
        assert!(message.contains("wss://"), "message: {message}");
    }

    #[test]
    fn insecure_with_ws_url_is_a_usage_error_naming_the_flag() {
        let cli = parse(&[
            "prismcast-cli",
            "--url",
            "ws://127.0.0.1:4456",
            "--insecure",
            "ping",
        ]);
        let error = transport_plan(&cli).expect_err("--insecure needs wss://");
        assert_eq!(error.exit_code(), 2);
        let message = error.to_string();
        assert!(message.contains("--insecure"), "message: {message}");
    }

    #[test]
    fn tls_flags_with_socket_transport_are_a_usage_error() {
        // clap's requires = "url" is suppressed by the --socket conflict, so
        // transport_plan enforces the rule: parse succeeds, the plan fails.
        for tls_args in [&["--tls-ca", "/tmp/ca.pem"][..], &["--insecure"][..]] {
            let cli = Cli::try_parse_from(
                ["prismcast-cli", "--socket", "/tmp/control.sock"]
                    .into_iter()
                    .chain(tls_args.iter().copied())
                    .chain(["ping"]),
            )
            .expect("args parse (clap's requires is suppressed by --socket)");
            let error = transport_plan(&cli).expect_err("TLS flags need a wss:// --url");
            assert_eq!(error.exit_code(), 2);
            let message = error.to_string();
            assert!(message.contains(tls_args[0]), "message: {message}");
            assert!(message.contains("wss://"), "message: {message}");
        }
    }
}
