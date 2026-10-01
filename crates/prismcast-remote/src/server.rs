//! The Unix-socket IPC server (IPC-001; ADR-0006, protocol doc §1).
//!
//! [`IpcServer`] binds `$XDG_RUNTIME_DIR/prismcast/control.sock` (or an
//! override path), cleans up stale sockets, enforces `0700`/`0600` filesystem
//! permissions, and spawns one [`crate::session`] task per accepted
//! connection. A single upstream
//! [`EventStream`](prismcast_app::EventStream) per server fans domain events
//! out to all sessions through a bounded `tokio::sync::broadcast` channel, so
//! short-lived sessions never leak broadcaster registrations (the
//! broadcaster has no unsubscribe API).
//!
//! Shutdown is graceful: `shutdown()` signals all sessions (they send a
//! `ServerShutdown` closing notice and exit), stops the accept loop, and
//! removes the socket file. Dropping the server without `shutdown()` aborts
//! the tasks and still removes the socket file.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, watch};
use tokio::task::JoinHandle;
use tracing::{debug, info, info_span, warn, Instrument};

use prismcast_app::broadcaster::StreamEvent;
use prismcast_app::{AppHandle, EventFilter, DEFAULT_SUBSCRIBER_CAPACITY};

use crate::auth::AuthConfig;
use crate::codec::{split_ipc, DEFAULT_MAX_FRAME_SIZE};
use crate::paths::default_socket_path;
use crate::session::{run_session, SessionConfig, SessionContext};

/// Tuning for [`IpcServer`].
#[derive(Debug, Clone)]
pub struct IpcServerConfig {
    /// Socket path override; `None` resolves the default
    /// (`$XDG_RUNTIME_DIR/prismcast/control.sock` or the `/tmp` fallback).
    pub socket_path: Option<PathBuf>,
    /// Authentication policy (default: allow-local with full `Admin`
    /// permissions; see [`crate::auth`]).
    pub auth: AuthConfig,
    /// Maximum frame payload in bytes (bounds per-connection memory).
    pub max_frame_size: usize,
    /// Bound of each session's outbound queue (protocol doc §Backpressure).
    pub outbound_capacity: usize,
    /// Capacity of the server-wide event fan-out channel and of the upstream
    /// broadcaster subscription.
    pub event_queue_capacity: usize,
    /// How long a response enqueue may block before the session is shed with
    /// `SlowConsumer`.
    pub send_timeout: Duration,
    /// Deadline for the client's `Identify` after connect.
    pub handshake_timeout: Duration,
}

impl Default for IpcServerConfig {
    fn default() -> Self {
        Self {
            socket_path: None,
            auth: AuthConfig::allow_local(),
            max_frame_size: DEFAULT_MAX_FRAME_SIZE,
            outbound_capacity: 256,
            event_queue_capacity: DEFAULT_SUBSCRIBER_CAPACITY,
            send_timeout: Duration::from_secs(1),
            handshake_timeout: Duration::from_secs(10),
        }
    }
}

impl IpcServerConfig {
    /// The transport-independent part of the configuration, consumed by the
    /// shared session machinery.
    pub(crate) fn session_config(&self) -> SessionConfig {
        SessionConfig {
            auth: self.auth.clone(),
            outbound_capacity: self.outbound_capacity,
            send_timeout: self.send_timeout,
            handshake_timeout: self.handshake_timeout,
        }
    }
}

/// Errors binding or running the IPC server.
#[derive(Debug, thiserror::Error)]
pub enum IpcError {
    /// A filesystem or socket operation failed.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// A live server already owns the socket (the stale-socket probe
    /// connected successfully).
    #[error("control socket is already in use: {0}")]
    AlreadyInUse(PathBuf),
    /// The configured socket path has no parent directory.
    #[error("socket path has no parent directory: {0}")]
    InvalidPath(PathBuf),
}

/// Server-wide event fan-out: one upstream subscription, N session receivers.
///
/// The `broadcast` channel is bounded; a lagging session receiver gets
/// `Lagged(dropped)` (drop-oldest), which sessions translate into sequence
/// gaps per the protocol's resync contract.
#[derive(Clone)]
pub(crate) struct EventFanout {
    tx: broadcast::Sender<StreamEvent>,
}

impl EventFanout {
    pub(crate) fn spawn(app: &AppHandle, capacity: usize) -> (Self, JoinHandle<()>) {
        let capacity = capacity.max(2);
        let mut stream = app.subscribe_with_capacity(EventFilter::all(), capacity);
        let (tx, _) = broadcast::channel(capacity);
        let fanout_tx = tx.clone();
        let task = tokio::spawn(
            async move {
                while let Some(item) = stream.recv().await {
                    // `SendError` only means "no receivers"; that is fine.
                    drop(fanout_tx.send(item));
                }
                debug!("event fan-out stopped (core actor shut down)");
            }
            .instrument(info_span!("event_fanout")),
        );
        (Self { tx }, task)
    }

    pub(crate) fn subscribe(&self) -> broadcast::Receiver<StreamEvent> {
        self.tx.subscribe()
    }
}

/// The running Unix-socket IPC server.
pub struct IpcServer {
    socket_path: PathBuf,
    accept_task: JoinHandle<()>,
    fanout_task: JoinHandle<()>,
    sessions: Arc<Mutex<Vec<JoinHandle<()>>>>,
    shutdown_tx: watch::Sender<()>,
}

impl IpcServer {
    /// Binds the socket, starts the accept loop and the event fan-out.
    pub async fn bind(app: AppHandle, config: IpcServerConfig) -> Result<Self, IpcError> {
        let socket_path = config
            .socket_path
            .clone()
            .unwrap_or_else(default_socket_path);
        prepare_socket_path(&socket_path).await?;
        let listener = UnixListener::bind(&socket_path)?;
        set_mode(&socket_path, 0o600)?;
        info!(path = %socket_path.display(), "IPC server listening");

        let config = Arc::new(config);
        let (shutdown_tx, shutdown_rx) = watch::channel(());
        let (fanout, fanout_task) = EventFanout::spawn(&app, config.event_queue_capacity);
        let sessions: Arc<Mutex<Vec<JoinHandle<()>>>> = Arc::new(Mutex::new(Vec::new()));
        let accept_task = tokio::spawn(
            accept_loop(listener, app, config, fanout, sessions.clone(), shutdown_rx)
                .instrument(info_span!("ipc_accept")),
        );
        Ok(Self {
            socket_path,
            accept_task,
            fanout_task,
            sessions,
            shutdown_tx,
        })
    }

    /// The path the server is bound to.
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Number of live session tasks (metrics/tests).
    pub fn session_count(&self) -> usize {
        self.lock_sessions()
            .iter()
            .filter(|h| !h.is_finished())
            .count()
    }

    fn lock_sessions(&self) -> MutexGuard<'_, Vec<JoinHandle<()>>> {
        self.sessions.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Graceful shutdown: sessions are signaled (each sends a
    /// `ServerShutdown` closing notice), the accept loop stops, and the
    /// socket file is removed.
    pub async fn shutdown(self) {
        let _ = self.shutdown_tx.send(());
        self.accept_task.abort();
        let sessions: Vec<JoinHandle<()>> = self.lock_sessions().drain(..).collect();
        for mut session in sessions {
            if tokio::time::timeout(Duration::from_secs(2), &mut session)
                .await
                .is_err()
            {
                session.abort();
            }
        }
        self.fanout_task.abort();
        if let Err(error) = std::fs::remove_file(&self.socket_path) {
            debug!(%error, path = %self.socket_path.display(), "socket cleanup");
        }
        info!("IPC server stopped");
    }
}

impl Drop for IpcServer {
    fn drop(&mut self) {
        let _ = self.shutdown_tx.send(());
        self.accept_task.abort();
        self.fanout_task.abort();
        for session in self.lock_sessions().drain(..) {
            session.abort();
        }
        // Best-effort cleanup; the directory itself is left in place.
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

/// Creates the socket directory (`0700`) and removes a stale socket file
/// left behind by a crashed server. A socket that still accepts connections
/// means another live server owns it.
async fn prepare_socket_path(path: &Path) -> Result<(), IpcError> {
    let parent = path
        .parent()
        .ok_or_else(|| IpcError::InvalidPath(path.to_path_buf()))?;
    std::fs::create_dir_all(parent)?;
    set_mode(parent, 0o700)?;
    if path.try_exists()? {
        match UnixStream::connect(path).await {
            Ok(_) => return Err(IpcError::AlreadyInUse(path.to_path_buf())),
            Err(error) => {
                debug!(%error, path = %path.display(), "removing stale socket");
                std::fs::remove_file(path)?;
            }
        }
    }
    Ok(())
}

fn set_mode(path: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

async fn accept_loop(
    listener: UnixListener,
    app: AppHandle,
    config: Arc<IpcServerConfig>,
    fanout: EventFanout,
    sessions: Arc<Mutex<Vec<JoinHandle<()>>>>,
    mut shutdown: watch::Receiver<()>,
) {
    let mut next_connection = 0_u64;
    loop {
        tokio::select! {
            _ = shutdown.changed() => {
                debug!("accept loop shutting down");
                break;
            }
            accepted = listener.accept() => match accepted {
                Ok((stream, _addr)) => {
                    next_connection += 1;
                    debug!(connection_id = next_connection, "accepted connection");
                    let (reader, writer) = split_ipc(stream, config.max_frame_size);
                    let context = SessionContext {
                        app: app.clone(),
                        config: Arc::new(config.session_config()),
                        fanout: fanout.clone(),
                        shutdown: shutdown.clone(),
                        transport: "ipc",
                    };
                    let handle = tokio::spawn(run_session(reader, writer, next_connection, context));
                    let mut guard = sessions.lock().unwrap_or_else(|p| p.into_inner());
                    guard.retain(|h| !h.is_finished());
                    guard.push(handle);
                }
                Err(error) => warn!(%error, "accept failed"),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stale_socket_is_replaced_but_live_socket_is_refused() {
        let dir =
            std::env::temp_dir().join(format!("prismcast-ipc-paths-{}", uuid::Uuid::new_v4()));
        let socket = dir.join("control.sock");

        // A stale file (not a live socket) is removed.
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(&socket, b"stale").expect("stale file");
        prepare_socket_path(&socket).await.expect("prepare");
        assert!(!socket.exists(), "stale socket removed");

        // A live listener is detected.
        let app = AppHandle::spawn(prismcast_app::CoreConfig::default());
        let server = IpcServer::bind(
            app.clone(),
            IpcServerConfig {
                socket_path: Some(socket.clone()),
                ..IpcServerConfig::default()
            },
        )
        .await
        .expect("bind");
        let second = IpcServer::bind(
            app.clone(),
            IpcServerConfig {
                socket_path: Some(socket.clone()),
                ..IpcServerConfig::default()
            },
        )
        .await;
        assert!(matches!(second, Err(IpcError::AlreadyInUse(_))));

        // Directory and socket modes.
        use std::os::unix::fs::PermissionsExt;
        let dir_mode = std::fs::metadata(&dir)
            .expect("dir meta")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(dir_mode, 0o700);
        let sock_mode = std::fs::metadata(&socket)
            .expect("sock meta")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(sock_mode, 0o600);

        server.shutdown().await;
        assert!(!socket.exists(), "socket removed on shutdown");
        app.shutdown().await;
        let _ = std::fs::remove_dir_all(&dir);
    }
}
