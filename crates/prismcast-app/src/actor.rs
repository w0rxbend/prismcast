//! The core actor: sole owner of [`AppState`] (CORE-001; PLAN.md §57,
//! ADR-0005).
//!
//! All mutations flow through a bounded `mpsc` channel as
//! [`CommandEnvelope`]s. The actor applies commands sequentially via
//! [`prismcast_core::apply`], assigns each committed event a global sequence
//! number, publishes events to the [`EventBroadcaster`], publishes a fresh
//! [`AppSnapshot`], and records undo inverses in the [`UndoService`].
//!
//! [`AppHandle`] is the **only** way in — cloneable, cheap, and safe to hold
//! by GTK, CLI, WebSocket, IPC, and web-UI controllers alike (PLAN.md §76's
//! interchangeable controllers). Read paths ([`AppHandle::snapshot`],
//! [`AppHandle::query`], subscriptions) never touch the command queue.
//!
//! ## Shutdown
//!
//! Graceful shutdown is a queue message ([`AppHandle::shutdown`]): because the
//! channel is FIFO, every command enqueued before it completes first; later
//! senders observe [`HandleError::Shutdown`]. Dropping every `AppHandle` also
//! stops the actor. On exit the actor closes all event streams; subscribers
//! drain queued events, then see `None`.
//!
//! ## Undo/redo
//!
//! [`AppHandle::undo`] / [`AppHandle::redo`] are actor messages (not
//! [`Command`]s — they are meta-operations over the undo service, see
//! [`crate::undo`]), but the inverse commands they apply go through the same
//! apply → events → snapshot pipeline as any other command.

use std::sync::Arc;

use tokio::sync::{mpsc, oneshot, watch};
use tracing::{debug, info, instrument, warn};

use prismcast_core::command::Command;
use prismcast_core::error::Error;
use prismcast_core::event::Event;
use prismcast_core::state::{apply, AppState};

use crate::broadcaster::{EventBroadcaster, EventFilter, EventStream};
use crate::dispatch::{Permissions, Query, QueryResponse};
use crate::persistence::PersistenceHandle;
use crate::snapshot::AppSnapshot;
use crate::undo::{UndoEntry, UndoService, DEFAULT_UNDO_CAPACITY};

/// Default capacity of the command channel.
pub const DEFAULT_COMMAND_CAPACITY: usize = 64;

/// Tuning for the core actor.
#[derive(Debug, Clone, Copy)]
pub struct CoreConfig {
    /// Bound of the command channel; senders await when the actor is behind.
    pub command_capacity: usize,
    /// Default per-subscriber event queue capacity (see
    /// [`crate::broadcaster`]).
    pub event_queue_capacity: usize,
    /// Maximum undo-stack depth.
    pub undo_capacity: usize,
}

impl Default for CoreConfig {
    fn default() -> Self {
        Self {
            command_capacity: DEFAULT_COMMAND_CAPACITY,
            event_queue_capacity: crate::broadcaster::DEFAULT_SUBSCRIBER_CAPACITY,
            undo_capacity: DEFAULT_UNDO_CAPACITY,
        }
    }
}

/// Successful command reply: an events summary (PLAN.md §20).
#[derive(Debug, Clone)]
pub struct CommandResponse {
    /// Human-readable label of the applied command (or the undo/redo step).
    pub label: &'static str,
    /// Events committed by the command, in apply order, each already
    /// broadcast to subscribers.
    pub events: Vec<Event>,
}

/// A command plus its caller context and reply channel — the unit of work the
/// actor consumes.
#[derive(Debug)]
pub struct CommandEnvelope {
    /// The mutation to apply.
    pub command: Command,
    /// Permissions granted to the caller (session/token/local).
    pub permissions: Permissions,
    /// Typed reply: `Ok(CommandResponse)` or the core [`Error`] (validation,
    /// unauthorized, not-found, ...).
    pub reply: oneshot::Sender<Result<CommandResponse, Error>>,
}

/// Errors talking to the actor (transport-level). Domain rejections arrive as
/// [`HandleError::Core`] wrapping the core [`Error`].
#[derive(Debug, thiserror::Error)]
pub enum HandleError {
    /// The actor has shut down (or the queue is closed).
    #[error("core actor is shut down")]
    Shutdown,
    /// The command/query was rejected: unauthorized, invalid, not found, ...
    #[error(transparent)]
    Core(#[from] Error),
}

enum ActorMessage {
    Command(Box<CommandEnvelope>),
    Undo {
        reply: oneshot::Sender<Result<CommandResponse, Error>>,
    },
    Redo {
        reply: oneshot::Sender<Result<CommandResponse, Error>>,
    },
    BeginTransaction {
        label: String,
        reply: oneshot::Sender<Result<(), Error>>,
    },
    EndTransaction {
        reply: oneshot::Sender<Result<(), Error>>,
    },
    Shutdown,
}

/// Cloneable handle to the running core actor — the single entry point for
/// every controller.
#[derive(Clone)]
pub struct AppHandle {
    tx: mpsc::Sender<ActorMessage>,
    snapshots: watch::Receiver<Arc<AppSnapshot>>,
    broadcaster: EventBroadcaster,
}

impl AppHandle {
    /// Spawns the core actor with a fresh default [`AppState`].
    ///
    /// Must be called from within a Tokio runtime.
    pub fn spawn(config: CoreConfig) -> Self {
        Self::spawn_with_state(AppState::new(), config)
    }

    /// Spawns the core actor around an existing state (persistence restore).
    pub fn spawn_with_state(state: AppState, config: CoreConfig) -> Self {
        Self::spawn_inner(state, config, None)
    }

    /// Spawns the core actor with persistence wired in: every applied command
    /// notifies the persistence actor (non-blocking; see
    /// [`PersistenceHandle::command_applied`]), and shutdown performs a final
    /// awaited flush so no dirty state is lost.
    pub fn spawn_with_persistence(
        state: AppState,
        config: CoreConfig,
        persistence: PersistenceHandle,
    ) -> Self {
        Self::spawn_inner(state, config, Some(persistence))
    }

    fn spawn_inner(
        state: AppState,
        config: CoreConfig,
        persistence: Option<PersistenceHandle>,
    ) -> Self {
        let (tx, rx) = mpsc::channel(config.command_capacity.max(1));
        let (snapshot_tx, snapshot_rx) = watch::channel(AppSnapshot::new(0, state.clone()));
        let broadcaster = EventBroadcaster::new(config.event_queue_capacity);
        let actor = CoreActor {
            state,
            revision: 0,
            next_seq: 0,
            undo: UndoService::new(config.undo_capacity),
            broadcaster: broadcaster.clone(),
            snapshot_tx,
            rx,
            persistence,
        };
        tokio::spawn(actor.run());
        Self {
            tx,
            snapshots: snapshot_rx,
            broadcaster,
        }
    }

    /// Dispatches a command as a trusted local controller
    /// ([`Permissions::admin`]). Remote adapters must use
    /// [`dispatch_with_permissions`](Self::dispatch_with_permissions) with the
    /// session's permissions instead.
    pub async fn dispatch(&self, command: Command) -> Result<CommandResponse, HandleError> {
        self.dispatch_with_permissions(command, Permissions::admin())
            .await
    }

    /// Dispatches a command with an explicit permission set. The actor checks
    /// authorization before touching state; failures arrive as
    /// [`HandleError::Core`]`(`[`Error::Unauthorized`]`)`.
    pub async fn dispatch_with_permissions(
        &self,
        command: Command,
        permissions: Permissions,
    ) -> Result<CommandResponse, HandleError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(ActorMessage::Command(Box::new(CommandEnvelope {
                command,
                permissions,
                reply: reply_tx,
            })))
            .await
            .map_err(|_| HandleError::Shutdown)?;
        reply_rx
            .await
            .map_err(|_| HandleError::Shutdown)?
            .map_err(HandleError::Core)
    }

    /// Undoes the last undoable step (trusted local controller).
    pub async fn undo(&self) -> Result<CommandResponse, HandleError> {
        self.undo_with_permissions(Permissions::admin()).await
    }

    /// Undoes the last undoable step with an explicit permission set.
    /// Interim authz: any controlling (non-read-only) caller may undo — see
    /// [`crate::undo`].
    pub async fn undo_with_permissions(
        &self,
        permissions: Permissions,
    ) -> Result<CommandResponse, HandleError> {
        check_can_control(permissions, "undo")?;
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(ActorMessage::Undo { reply: reply_tx })
            .await
            .map_err(|_| HandleError::Shutdown)?;
        reply_rx
            .await
            .map_err(|_| HandleError::Shutdown)?
            .map_err(HandleError::Core)
    }

    /// Redoes the last undone step (trusted local controller).
    pub async fn redo(&self) -> Result<CommandResponse, HandleError> {
        self.redo_with_permissions(Permissions::admin()).await
    }

    /// Redoes the last undone step with an explicit permission set.
    pub async fn redo_with_permissions(
        &self,
        permissions: Permissions,
    ) -> Result<CommandResponse, HandleError> {
        check_can_control(permissions, "redo")?;
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(ActorMessage::Redo { reply: reply_tx })
            .await
            .map_err(|_| HandleError::Shutdown)?;
        reply_rx
            .await
            .map_err(|_| HandleError::Shutdown)?
            .map_err(HandleError::Core)
    }

    /// Opens an undo transaction group (trusted local controller). Commands
    /// applied until [`end_transaction`](Self::end_transaction) become one
    /// undo entry (PLAN.md §59 drag-gesture grouping).
    pub async fn begin_transaction(&self, label: impl Into<String>) -> Result<(), HandleError> {
        self.begin_transaction_with_permissions(label, Permissions::admin())
            .await
    }

    /// Permission-checked variant of [`begin_transaction`](Self::begin_transaction).
    pub async fn begin_transaction_with_permissions(
        &self,
        label: impl Into<String>,
        permissions: Permissions,
    ) -> Result<(), HandleError> {
        check_can_control(permissions, "begin transaction")?;
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(ActorMessage::BeginTransaction {
                label: label.into(),
                reply: reply_tx,
            })
            .await
            .map_err(|_| HandleError::Shutdown)?;
        reply_rx
            .await
            .map_err(|_| HandleError::Shutdown)?
            .map_err(HandleError::Core)
    }

    /// Closes the open undo transaction group (trusted local controller).
    pub async fn end_transaction(&self) -> Result<(), HandleError> {
        self.end_transaction_with_permissions(Permissions::admin())
            .await
    }

    /// Permission-checked variant of [`end_transaction`](Self::end_transaction).
    pub async fn end_transaction_with_permissions(
        &self,
        permissions: Permissions,
    ) -> Result<(), HandleError> {
        check_can_control(permissions, "end transaction")?;
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(ActorMessage::EndTransaction { reply: reply_tx })
            .await
            .map_err(|_| HandleError::Shutdown)?;
        reply_rx
            .await
            .map_err(|_| HandleError::Shutdown)?
            .map_err(HandleError::Core)
    }

    /// The latest immutable snapshot. Never touches the command queue and
    /// never blocks the actor (an `Arc` clone out of a watch cell).
    ///
    /// This is the trusted local read path; remote adapters must go through
    /// [`query`](Self::query) with session permissions.
    pub fn snapshot(&self) -> Arc<AppSnapshot> {
        self.snapshots.borrow().clone()
    }

    /// A watch receiver for snapshot changes (reactive UIs). Only the latest
    /// snapshot is retained; pair with the event stream for gap-free
    /// incremental updates.
    pub fn subscribe_snapshots(&self) -> watch::Receiver<Arc<AppSnapshot>> {
        self.snapshots.clone()
    }

    /// Subscribes to the event stream with the default queue capacity.
    pub fn subscribe(&self, filter: EventFilter) -> EventStream {
        self.broadcaster.subscribe(filter)
    }

    /// Subscribes to the event stream with an explicit per-subscriber queue
    /// capacity (see the slow-consumer policy in [`crate::broadcaster`]).
    pub fn subscribe_with_capacity(&self, filter: EventFilter, capacity: usize) -> EventStream {
        self.broadcaster.subscribe_with_capacity(filter, capacity)
    }

    /// Runs a read-only query against the latest snapshot. Requires
    /// [`crate::dispatch::Permission::Read`]; never queues to the actor.
    pub fn query(&self, query: &Query, permissions: Permissions) -> Result<QueryResponse, Error> {
        let required = query.required_permission();
        if !permissions.grants(required) {
            return Err(Error::Unauthorized(format!("query requires {required:?}")));
        }
        Ok(query.resolve(&self.snapshot()))
    }

    /// Initiates graceful shutdown: commands already queued complete, then the
    /// actor stops and all event streams close.
    pub async fn shutdown(&self) {
        if self.tx.send(ActorMessage::Shutdown).await.is_err() {
            // Already shut down — that is the desired end state.
            return;
        }
        self.closed().await;
    }

    /// Resolves once the actor task has exited.
    pub async fn closed(&self) {
        self.tx.closed().await;
    }
}

fn check_can_control(permissions: Permissions, operation: &str) -> Result<(), HandleError> {
    if permissions.can_control() {
        Ok(())
    } else {
        Err(HandleError::Core(Error::Unauthorized(format!(
            "{operation} requires at least one control permission"
        ))))
    }
}

/// The actor itself. Owns all mutable application state; runs on the Tokio
/// runtime as one task (PLAN.md §57 core actor).
struct CoreActor {
    state: AppState,
    revision: u64,
    next_seq: u64,
    undo: UndoService,
    broadcaster: EventBroadcaster,
    snapshot_tx: watch::Sender<Arc<AppSnapshot>>,
    rx: mpsc::Receiver<ActorMessage>,
    persistence: Option<PersistenceHandle>,
}

impl CoreActor {
    #[instrument(name = "core_actor", skip_all)]
    async fn run(mut self) {
        info!("core actor started");
        while let Some(message) = self.rx.recv().await {
            match message {
                ActorMessage::Command(envelope) => self.handle_command(envelope),
                ActorMessage::Undo { reply } => {
                    let _ = reply.send(self.handle_undo());
                }
                ActorMessage::Redo { reply } => {
                    let _ = reply.send(self.handle_redo());
                }
                ActorMessage::BeginTransaction { label, reply } => {
                    let _ = reply.send(self.undo.begin_transaction(label));
                }
                ActorMessage::EndTransaction { reply } => {
                    let _ = reply.send(self.undo.end_transaction());
                }
                ActorMessage::Shutdown => {
                    // Save-on-shutdown: flush pending dirty state before
                    // stopping. FIFO ordering guarantees every Dirty mark
                    // queued by earlier commands is written.
                    if let Some(persistence) = &self.persistence {
                        if let Err(error) = persistence.flush().await {
                            warn!(%error, "persistence flush on shutdown failed");
                        }
                    }
                    break;
                }
            }
        }
        self.broadcaster.close_all();
        info!("core actor stopped");
    }

    fn handle_command(&mut self, envelope: Box<CommandEnvelope>) {
        let CommandEnvelope {
            command,
            permissions,
            reply,
        } = *envelope;
        debug!(command = command.label(), %permissions, "dispatching command");
        let result = self.apply_authorized(&command, &permissions);
        if reply.send(result).is_err() {
            debug!("caller dropped reply channel before command completed");
        }
    }

    fn apply_authorized(
        &mut self,
        command: &Command,
        permissions: &Permissions,
    ) -> Result<CommandResponse, Error> {
        // Authorization checkpoint: reject before touching state (ADR-0005).
        permissions.check(command)?;
        let label = command.label();
        // Inverse must be computed against the pre-application state (PLAN §59).
        let inverse = self.state.inverse(command);
        let events = apply(&mut self.state, command)?;
        self.undo.record(label, inverse);
        let events = self.commit(events);
        self.notify_persistence(command, !events.is_empty());
        Ok(CommandResponse { label, events })
    }

    fn handle_undo(&mut self) -> Result<CommandResponse, Error> {
        if self.undo.in_transaction() {
            return Err(Error::InvalidInput(
                "cannot undo while a transaction group is open".into(),
            ));
        }
        let entry = self
            .undo
            .pop_undo()
            .ok_or_else(|| Error::InvalidInput("nothing to undo".into()))?;
        // The redo step is the inverse of the inverse, computed against the
        // post-undo state; if it is not representable the step is simply not
        // redoable (see crate::undo limitations).
        let redo_inverse = self.state.inverse(&entry.inverse);
        let label = entry.inverse.label();
        match apply(&mut self.state, &entry.inverse) {
            Ok(events) => {
                if let Some(inverse) = redo_inverse {
                    self.undo.push_redo(UndoEntry {
                        label: entry.label,
                        inverse,
                    });
                }
                let events = self.commit(events);
                self.notify_persistence(&entry.inverse, !events.is_empty());
                Ok(CommandResponse { label, events })
            }
            Err(error) => {
                warn!(step = entry.label, %error, "undo failed; entry restored to stack");
                self.undo.push_undo_back(entry);
                Err(error)
            }
        }
    }

    fn handle_redo(&mut self) -> Result<CommandResponse, Error> {
        if self.undo.in_transaction() {
            return Err(Error::InvalidInput(
                "cannot redo while a transaction group is open".into(),
            ));
        }
        let entry = self
            .undo
            .pop_redo()
            .ok_or_else(|| Error::InvalidInput("nothing to redo".into()))?;
        let undo_inverse = self.state.inverse(&entry.inverse);
        let label = entry.inverse.label();
        match apply(&mut self.state, &entry.inverse) {
            Ok(events) => {
                if let Some(inverse) = undo_inverse {
                    self.undo.push_undo_back(UndoEntry {
                        label: entry.label,
                        inverse,
                    });
                }
                let events = self.commit(events);
                self.notify_persistence(&entry.inverse, !events.is_empty());
                Ok(CommandResponse { label, events })
            }
            Err(error) => {
                warn!(step = entry.label, %error, "redo failed; entry restored to stack");
                self.undo.push_redo(entry);
                Err(error)
            }
        }
    }

    /// Notifies the persistence actor of an applied command (CORE-004).
    /// Skipped for no-op applies (a command that emitted no events changed
    /// nothing, so there is nothing to save). Never blocks: the handle
    /// `try_send`s whole-aggregate snapshots over a bounded channel.
    fn notify_persistence(&self, command: &Command, changed: bool) {
        if let (Some(persistence), true) = (&self.persistence, changed) {
            persistence.command_applied(command, &self.state);
        }
    }

    /// Broadcasts committed events (with sequence numbers) and publishes the
    /// post-command snapshot. Called only after a successful apply.
    fn commit(&mut self, events: Vec<Event>) -> Vec<Event> {
        for event in &events {
            self.broadcaster.publish(self.next_seq, event);
            self.next_seq += 1;
        }
        self.revision += 1;
        // Receivers that lag see only the latest snapshot — by design.
        if self
            .snapshot_tx
            .send(AppSnapshot::new(self.revision, self.state.clone()))
            .is_err()
        {
            debug!("no snapshot receivers");
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::broadcaster::StreamEvent;
    use crate::dispatch::Permission;
    use prismcast_core::{Command, SceneEvent};

    #[tokio::test]
    async fn dispatch_applies_and_replies_with_events() {
        let handle = AppHandle::spawn(CoreConfig::default());
        let response = handle
            .dispatch(Command::AddScene {
                name: "Main".into(),
            })
            .await
            .expect("dispatch");
        assert_eq!(response.label, "add scene");
        // First scene also becomes current: Added + CurrentChanged.
        assert!(matches!(
            response.events.as_slice(),
            [
                Event::Scene(SceneEvent::Added { .. }),
                Event::Scene(SceneEvent::CurrentChanged { .. })
            ]
        ));
        assert_eq!(handle.snapshot().revision(), 1);
        handle.shutdown().await;
    }

    #[tokio::test]
    async fn unauthorized_command_is_rejected_before_state_access() {
        let handle = AppHandle::spawn(CoreConfig::default());
        let mut stream = handle.subscribe(EventFilter::all());

        let error = handle
            .dispatch_with_permissions(
                Command::SetStudioModeEnabled { enabled: true },
                Permissions::read_only(),
            )
            .await
            .expect_err("must be rejected");
        assert!(matches!(error, HandleError::Core(Error::Unauthorized(_))));

        // No state mutation, no snapshot publish, no event.
        assert_eq!(handle.snapshot().revision(), 0);
        assert!(handle.snapshot().state().studio_mode.is_none());
        handle.shutdown().await;
        assert_eq!(stream.recv().await, None);
    }

    #[tokio::test]
    async fn shutdown_lets_queued_commands_finish() {
        let handle = AppHandle::spawn(CoreConfig::default());
        let h2 = handle.clone();
        let dispatched = tokio::spawn(async move {
            h2.dispatch(Command::AddScene {
                name: "queued".into(),
            })
            .await
        });
        let h3 = handle.clone();
        let shutdown = tokio::spawn(async move { h3.shutdown().await });
        let response = dispatched
            .await
            .expect("join")
            .expect("queued command completes");
        assert_eq!(response.label, "add scene");
        shutdown.await.expect("join");
        // After shutdown, dispatch fails with Shutdown.
        let error = handle
            .dispatch(Command::AddScene {
                name: "late".into(),
            })
            .await
            .expect_err("actor is gone");
        assert!(matches!(error, HandleError::Shutdown));
    }

    #[tokio::test]
    async fn undo_without_control_permission_is_rejected() {
        let handle = AppHandle::spawn(CoreConfig::default());
        let error = handle
            .undo_with_permissions(Permissions::read_only())
            .await
            .expect_err("read-only cannot undo");
        assert!(matches!(error, HandleError::Core(Error::Unauthorized(_))));

        let control = Permissions::from_iter([Permission::ControlAudio]);
        let error = handle
            .undo_with_permissions(control)
            .await
            .expect_err("nothing to undo yet");
        assert!(matches!(error, HandleError::Core(Error::InvalidInput(_))));
        handle.shutdown().await;
    }

    #[tokio::test]
    async fn events_carry_monotonic_sequence_numbers() {
        let handle = AppHandle::spawn(CoreConfig::default());
        let mut stream = handle.subscribe(EventFilter::all());
        handle
            .dispatch(Command::AddScene { name: "a".into() })
            .await
            .expect("dispatch");
        handle
            .dispatch(Command::AddScene { name: "b".into() })
            .await
            .expect("dispatch");
        let first = stream.recv().await.expect("event");
        let second = stream.recv().await.expect("event");
        match (first, second) {
            (StreamEvent::Event { seq: a, .. }, StreamEvent::Event { seq: b, .. }) => {
                assert_eq!((a, b), (0, 1))
            }
            other => panic!("unexpected {other:?}"),
        }
        handle.shutdown().await;
    }
}
