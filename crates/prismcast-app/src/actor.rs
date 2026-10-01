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
use crate::undo::{
    bounded_size, validate_json, validate_structure, UndoEntry, UndoLimits, UndoService,
    DEFAULT_UNDO_CAPACITY,
};

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
    /// Byte, member, label and nesting limits for history.
    pub undo_limits: UndoLimits,
}

impl Default for CoreConfig {
    fn default() -> Self {
        Self {
            command_capacity: DEFAULT_COMMAND_CAPACITY,
            event_queue_capacity: crate::broadcaster::DEFAULT_SUBSCRIBER_CAPACITY,
            undo_capacity: DEFAULT_UNDO_CAPACITY,
            undo_limits: UndoLimits::default(),
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

/// Local controller identity. Cloned handles preserve it; explicit forks do not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AppControllerId(uuid::Uuid);
impl AppControllerId {
    fn new() -> Self {
        Self(uuid::Uuid::new_v4())
    }
}

/// A command plus its caller context and reply channel — the unit of work the
/// actor consumes.
#[derive(Debug)]
pub struct CommandEnvelope {
    /// The mutation to apply.
    pub command: Command,
    /// Permissions granted to the caller (session/token/local).
    pub permissions: Permissions,
    /// Identity of the originating controller, independent of permissions.
    pub controller_id: AppControllerId,
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
        permissions: Permissions,
        reply: oneshot::Sender<Result<CommandResponse, Error>>,
    },
    Redo {
        permissions: Permissions,
        reply: oneshot::Sender<Result<CommandResponse, Error>>,
    },
    BeginTransaction {
        controller_id: AppControllerId,
        label: String,
        reply: oneshot::Sender<Result<(), Error>>,
    },
    EndTransaction {
        controller_id: AppControllerId,
        reply: oneshot::Sender<Result<(), Error>>,
    },
    Shutdown,
}

/// Cloneable handle to the running core actor — the single entry point for
/// every controller.
#[derive(Clone)]
pub struct AppHandle {
    controller_id: AppControllerId,
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
            undo: UndoService::with_limits(config.undo_capacity, config.undo_limits),
            group_controller: None,
            undo_limits: config.undo_limits,
            broadcaster: broadcaster.clone(),
            snapshot_tx,
            rx,
            persistence,
        };
        tokio::spawn(actor.run());
        Self {
            controller_id: AppControllerId::new(),
            tx,
            snapshots: snapshot_rx,
            broadcaster,
        }
    }

    /// Creates a distinct controller sharing the actor, snapshots and event stream.
    /// Use this for each remote session; ordinary clones keep group ownership.
    pub fn new_controller(&self) -> Self {
        let mut handle = self.clone();
        handle.controller_id = AppControllerId::new();
        handle
    }

    /// Identity shared by clones of this handle.
    pub fn controller_id(&self) -> AppControllerId {
        self.controller_id
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
                controller_id: self.controller_id,
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
    /// The actor authorizes every operation in the inverse against these permissions.
    pub async fn undo_with_permissions(
        &self,
        permissions: Permissions,
    ) -> Result<CommandResponse, HandleError> {
        check_can_control(permissions, "undo")?;
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(ActorMessage::Undo {
                permissions,
                reply: reply_tx,
            })
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
            .send(ActorMessage::Redo {
                permissions,
                reply: reply_tx,
            })
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
                controller_id: self.controller_id,
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
            .send(ActorMessage::EndTransaction {
                controller_id: self.controller_id,
                reply: reply_tx,
            })
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
    undo_limits: UndoLimits,
    group_controller: Option<AppControllerId>,
    broadcaster: EventBroadcaster,
    snapshot_tx: watch::Sender<Arc<AppSnapshot>>,
    rx: mpsc::Receiver<ActorMessage>,
    persistence: Option<PersistenceHandle>,
}

/// Measures variable payloads before the domain inverse clones them. Transactions
/// replay on a scratch state, preserving the existing atomic inverse semantics.
fn prepare_inverse(
    state: &AppState,
    command: &Command,
    limits: UndoLimits,
) -> Result<Option<Command>, Error> {
    validate_structure(command, limits)?;
    bounded_size(command, limits.retained_bytes)?;
    match command {
        Command::RenameScene { scene_id, .. } => {
            if let Some(scene) = state.scene(*scene_id) {
                bounded_size(&scene.name, limits.retained_bytes)?;
            }
        }
        Command::RenameSource { source_id, .. } => {
            if let Some(source) = state.source(*source_id) {
                bounded_size(&source.name, limits.retained_bytes)?;
            }
        }
        Command::SetSourceSettings { source_id, .. } => {
            if let Some(source) = state.source(*source_id) {
                validate_json(&source.settings, limits)?;
                bounded_size(&source.settings, limits.retained_bytes)?;
            }
        }
        Command::SetTransition { .. } => {
            validate_json(&state.transition.settings, limits)?;
            bounded_size(&state.transition, limits.retained_bytes)?;
        }
        Command::Transaction { commands } => {
            let mut scratch = state.clone();
            let mut inverses = Vec::new();
            let mut bytes = 128usize;
            for command in commands {
                let Some(inverse) = prepare_inverse(&scratch, command, limits)? else {
                    return Ok(None);
                };
                bytes = bytes.saturating_add(bounded_size(&inverse, limits.retained_bytes)?);
                if bytes > limits.retained_bytes {
                    return Err(Error::InvalidInput(
                        "undo history resource limit exceeded".into(),
                    ));
                }
                // A failed transaction has no inverse; authoritative apply reports its error.
                if apply(&mut scratch, command).is_err() {
                    return Ok(None);
                }
                inverses.push(inverse);
            }
            inverses.reverse();
            return Ok(Some(Command::Transaction { commands: inverses }));
        }
        _ => {}
    }
    let inverse = state.inverse(command);
    if let Some(inverse) = &inverse {
        bounded_size(inverse, limits.retained_bytes)?;
    }
    Ok(inverse)
}

impl CoreActor {
    #[instrument(name = "core_actor", skip_all)]
    async fn run(mut self) {
        info!("core actor started");
        while let Some(message) = self.rx.recv().await {
            match message {
                ActorMessage::Command(envelope) => self.handle_command(envelope),
                ActorMessage::Undo { permissions, reply } => {
                    let _ = reply.send(self.handle_undo(permissions));
                }
                ActorMessage::Redo { permissions, reply } => {
                    let _ = reply.send(self.handle_redo(permissions));
                }
                ActorMessage::BeginTransaction {
                    controller_id,
                    label,
                    reply,
                } => {
                    let result = self.undo.begin_transaction(label);
                    if result.is_ok() {
                        self.group_controller = Some(controller_id);
                    }
                    let _ = reply.send(result);
                }
                ActorMessage::EndTransaction {
                    controller_id,
                    reply,
                } => {
                    let result = if self.group_controller != Some(controller_id) {
                        Err(Error::InvalidInput(
                            "controller does not own the open undo group".into(),
                        ))
                    } else {
                        let result = self.undo.end_transaction();
                        if result.is_ok() {
                            self.group_controller = None;
                        }
                        result
                    };
                    let _ = reply.send(result);
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
            controller_id,
            permissions,
            reply,
        } = *envelope;
        debug!(command = command.label(), %permissions, "dispatching command");
        let result = self.apply_authorized(&command, &permissions, controller_id);
        if reply.send(result).is_err() {
            debug!("caller dropped reply channel before command completed");
        }
    }

    fn apply_authorized(
        &mut self,
        command: &Command,
        permissions: &Permissions,
        controller_id: AppControllerId,
    ) -> Result<CommandResponse, Error> {
        // Authorization checkpoint: reject before touching state (ADR-0005).
        validate_structure(command, self.undo_limits)?;
        permissions.check(command)?;
        let label = command.label();
        // Inverse must be computed against the pre-application state (PLAN §59).
        let inverse = prepare_inverse(&self.state, command, self.undo_limits)?;
        let foreign = self
            .group_controller
            .is_some_and(|owner| owner != controller_id);
        if let Err(limit_error) = self.undo.preflight(label, inverse.as_ref(), !foreign) {
            // History saturation must not reject domain-defined no-ops. Input,
            // permission and inverse payload checks already passed. Probe only
            // this exceptional path on a scratch copy; never apply a rejected
            // mutation to authoritative state or change history.
            let mut scratch = self.state.clone();
            match apply(&mut scratch, command) {
                Ok(events) if events.is_empty() => {
                    let events = self.commit(events);
                    return Ok(CommandResponse { label, events });
                }
                _ => return Err(limit_error),
            }
        }
        let events = apply(&mut self.state, command)?;
        if foreign && !events.is_empty() {
            self.undo.end_transaction()?;
            self.group_controller = None;
        }
        if !events.is_empty() {
            self.undo.record(label, inverse);
        }
        let events = self.commit(events);
        self.notify_persistence(command, !events.is_empty());
        Ok(CommandResponse { label, events })
    }

    fn handle_undo(&mut self, permissions: Permissions) -> Result<CommandResponse, Error> {
        if self.undo.in_transaction() {
            return Err(Error::InvalidInput(
                "cannot undo while a transaction group is open".into(),
            ));
        }
        let next = self
            .undo
            .next_undo()
            .ok_or_else(|| Error::InvalidInput("nothing to undo".into()))?;
        validate_structure(&next.inverse, self.undo_limits)?;
        permissions.check(&next.inverse)?;
        let redo_inverse = prepare_inverse(&self.state, &next.inverse, self.undo_limits)?;
        self.undo
            .preflight(&next.label, redo_inverse.as_ref(), false)?;
        let entry = self
            .undo
            .pop_undo()
            .ok_or_else(|| Error::InvalidInput("nothing to undo".into()))?;
        // The redo step is the inverse of the inverse, computed against the
        // post-undo state; if it is not representable the step is simply not
        // redoable (see crate::undo limitations).

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

    fn handle_redo(&mut self, permissions: Permissions) -> Result<CommandResponse, Error> {
        if self.undo.in_transaction() {
            return Err(Error::InvalidInput(
                "cannot redo while a transaction group is open".into(),
            ));
        }
        let next = self
            .undo
            .next_redo()
            .ok_or_else(|| Error::InvalidInput("nothing to redo".into()))?;
        validate_structure(&next.inverse, self.undo_limits)?;
        permissions.check(&next.inverse)?;
        let undo_inverse = prepare_inverse(&self.state, &next.inverse, self.undo_limits)?;
        self.undo
            .preflight(&next.label, undo_inverse.as_ref(), false)?;
        let entry = self
            .undo
            .pop_redo()
            .ok_or_else(|| Error::InvalidInput("nothing to redo".into()))?;

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

    async fn undo_fixture(
        config: CoreConfig,
    ) -> (AppHandle, prismcast_core::SceneId, prismcast_core::SourceId) {
        let handle = AppHandle::spawn(config);
        handle
            .dispatch(Command::AddScene {
                name: "original".into(),
            })
            .await
            .unwrap();
        handle
            .dispatch(Command::AddSource {
                kind: prismcast_core::source::SourceKind::TestPattern,
                name: "source".into(),
            })
            .await
            .unwrap();
        let snapshot = handle.snapshot();
        let scene_id = snapshot.scenes().next().unwrap().id;
        let source_id = snapshot.sources().next().unwrap().id;
        (handle, scene_id, source_id)
    }

    #[tokio::test]
    async fn full_owner_group_accepts_repeated_noop_without_consuming_history() {
        let config = CoreConfig {
            undo_limits: UndoLimits {
                group_members: 2,
                ..UndoLimits::default()
            },
            ..CoreConfig::default()
        };
        let (handle, scene_id, _) = undo_fixture(config).await;
        handle.begin_transaction("one member").await.unwrap();
        let rename = Command::RenameScene {
            scene_id,
            name: "one".into(),
        };
        handle.dispatch(rename.clone()).await.unwrap();
        let mut stream = handle.subscribe(EventFilter::all());
        let response = handle
            .dispatch(rename.clone())
            .await
            .expect("same rename remains successful at group limit");
        assert!(response.events.is_empty());
        // Real mutation is still rejected: the no-op did not free or append members.
        assert!(handle
            .dispatch(Command::RenameScene {
                scene_id,
                name: "two".into()
            })
            .await
            .is_err());
        assert_eq!(handle.snapshot().scene(scene_id).unwrap().name, "one");
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), stream.recv())
                .await
                .is_err()
        );
        handle.end_transaction().await.unwrap();
        handle.undo().await.unwrap();
        assert_eq!(handle.snapshot().scene(scene_id).unwrap().name, "original");
        // A no-op must also preserve a previously available redo step.
        handle
            .dispatch(Command::RenameScene {
                scene_id,
                name: "original".into(),
            })
            .await
            .unwrap();
        handle.redo().await.unwrap();
        assert_eq!(handle.snapshot().scene(scene_id).unwrap().name, "one");
        handle.shutdown().await;
    }

    #[tokio::test]
    async fn restored_oversized_inverse_payload_rejects_before_mutation() {
        let mut state = AppState::new();
        state
            .apply(&Command::AddSource {
                kind: prismcast_core::source::SourceKind::TestPattern,
                name: "restored".into(),
            })
            .unwrap();
        let source_id = *state.sources.keys().next().unwrap();
        state
            .apply(&Command::SetSourceSettings {
                source_id,
                settings: serde_json::json!({"large": "x".repeat(4096)}),
            })
            .unwrap();
        let handle = AppHandle::spawn_with_state(
            state,
            CoreConfig {
                undo_limits: UndoLimits {
                    retained_bytes: 2048,
                    ..UndoLimits::default()
                },
                ..CoreConfig::default()
            },
        );
        let revision = handle.snapshot().revision();
        assert!(handle
            .dispatch(Command::SetSourceSettings {
                source_id,
                settings: serde_json::Value::Null
            })
            .await
            .is_err());
        assert_eq!(handle.snapshot().revision(), revision);
        assert_eq!(
            handle.snapshot().source(source_id).unwrap().settings["large"]
                .as_str()
                .unwrap()
                .len(),
            4096
        );
        handle.shutdown().await;
    }

    #[tokio::test]
    async fn controller_groups_preserve_chronology_and_reject_foreign_end() {
        let (owner, scene_id, _) = undo_fixture(CoreConfig::default()).await;
        let clone = owner.clone();
        let foreign = owner.new_controller();
        assert_eq!(owner.controller_id(), clone.controller_id());
        assert_ne!(owner.controller_id(), foreign.controller_id());
        owner.begin_transaction("owner edits").await.unwrap();
        clone
            .dispatch(Command::RenameScene {
                scene_id,
                name: "owner one".into(),
            })
            .await
            .unwrap();
        assert!(foreign.end_transaction().await.is_err());
        let revision = owner.snapshot().revision();
        assert!(foreign
            .dispatch(Command::RenameScene {
                scene_id: prismcast_core::SceneId::new(),
                name: "bad".into()
            })
            .await
            .is_err());
        assert!(foreign
            .dispatch_with_permissions(
                Command::RenameScene {
                    scene_id,
                    name: "bad".into()
                },
                Permissions::read_only()
            )
            .await
            .is_err());
        assert_eq!(owner.snapshot().revision(), revision);
        // Empty atomic transaction is a successful no-op, not a group boundary.
        foreign
            .dispatch(Command::Transaction { commands: vec![] })
            .await
            .unwrap();
        clone
            .dispatch(Command::RenameScene {
                scene_id,
                name: "owner two".into(),
            })
            .await
            .unwrap();
        foreign
            .dispatch(Command::RenameScene {
                scene_id,
                name: "foreign".into(),
            })
            .await
            .unwrap();
        assert!(owner.end_transaction().await.is_err());
        owner.undo().await.unwrap();
        assert_eq!(owner.snapshot().scene(scene_id).unwrap().name, "owner two");
        foreign.undo().await.unwrap();
        assert_eq!(owner.snapshot().scene(scene_id).unwrap().name, "original");
        owner.redo().await.unwrap();
        assert_eq!(owner.snapshot().scene(scene_id).unwrap().name, "owner two");
        owner.redo().await.unwrap();
        assert_eq!(owner.snapshot().scene(scene_id).unwrap().name, "foreign");
        owner.shutdown().await;
    }

    #[tokio::test]
    async fn mixed_inverse_permissions_are_checked_and_history_survives_denial() {
        let (handle, scene_id, source_id) = undo_fixture(CoreConfig::default()).await;
        let mixed = Command::Transaction {
            commands: vec![
                Command::RenameScene {
                    scene_id,
                    name: "changed".into(),
                },
                Command::SetSourceMuted {
                    source_id,
                    muted: true,
                },
            ],
        };
        handle.dispatch(mixed).await.unwrap();
        let revision = handle.snapshot().revision();
        let scenes = Permissions::from_iter([Permission::ControlScenes]);
        let both = Permissions::from_iter([Permission::ControlScenes, Permission::ControlAudio]);
        assert!(matches!(
            handle.undo_with_permissions(scenes).await,
            Err(HandleError::Core(Error::Unauthorized(_)))
        ));
        assert_eq!(handle.snapshot().revision(), revision);
        handle.undo_with_permissions(both).await.unwrap();
        assert_eq!(handle.snapshot().scene(scene_id).unwrap().name, "original");
        assert!(!handle.snapshot().state().audio.mixer_state(source_id).muted);
        let revision = handle.snapshot().revision();
        assert!(matches!(
            handle.redo_with_permissions(scenes).await,
            Err(HandleError::Core(Error::Unauthorized(_)))
        ));
        assert_eq!(handle.snapshot().revision(), revision);
        handle.redo_with_permissions(both).await.unwrap();
        assert_eq!(handle.snapshot().scene(scene_id).unwrap().name, "changed");
        assert!(handle.snapshot().state().audio.mixer_state(source_id).muted);
        // Mixed domains grouped across separate commands get the same recursive authorization.
        handle.begin_transaction("mixed group").await.unwrap();
        handle
            .dispatch(Command::RenameScene {
                scene_id,
                name: "grouped".into(),
            })
            .await
            .unwrap();
        handle
            .dispatch(Command::SetSourceMuted {
                source_id,
                muted: false,
            })
            .await
            .unwrap();
        handle.end_transaction().await.unwrap();
        assert!(handle.undo_with_permissions(scenes).await.is_err());
        handle.undo_with_permissions(both).await.unwrap();
        assert_eq!(handle.snapshot().scene(scene_id).unwrap().name, "changed");
        assert!(handle.snapshot().state().audio.mixer_state(source_id).muted);
        handle.shutdown().await;
    }

    #[tokio::test]
    async fn group_member_overflow_keeps_state_events_and_explicit_close() {
        let config = CoreConfig {
            undo_limits: UndoLimits {
                group_members: 4,
                ..UndoLimits::default()
            },
            ..CoreConfig::default()
        };
        let (handle, scene_id, _) = undo_fixture(config).await;
        assert!(handle.begin_transaction("x".repeat(257)).await.is_err());
        handle.begin_transaction("bounded").await.unwrap();
        handle
            .dispatch(Command::RenameScene {
                scene_id,
                name: "one".into(),
            })
            .await
            .unwrap();
        // This inverse contains three nodes; adding it to the group exceeds four incl wrapper.
        let nested = Command::Transaction {
            commands: vec![Command::Transaction {
                commands: vec![Command::RenameScene {
                    scene_id,
                    name: "two".into(),
                }],
            }],
        };
        let revision = handle.snapshot().revision();
        let mut stream = handle.subscribe(EventFilter::all());
        assert!(handle.dispatch(nested).await.is_err());
        assert_eq!(handle.snapshot().revision(), revision);
        assert_eq!(handle.snapshot().scene(scene_id).unwrap().name, "one");
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), stream.recv())
                .await
                .is_err()
        );
        handle.end_transaction().await.unwrap();
        handle.undo().await.unwrap();
        assert_eq!(handle.snapshot().scene(scene_id).unwrap().name, "original");
        handle.shutdown().await;
    }

    #[tokio::test]
    async fn byte_and_nested_payload_rejections_preserve_redo_and_groups() {
        let config = CoreConfig {
            undo_limits: UndoLimits {
                retained_bytes: 2500,
                nesting: 4,
                ..UndoLimits::default()
            },
            ..CoreConfig::default()
        };
        let (handle, scene_id, source_id) = undo_fixture(config).await;
        handle
            .dispatch(Command::RenameScene {
                scene_id,
                name: "changed".into(),
            })
            .await
            .unwrap();
        handle.undo().await.unwrap();
        let revision = handle.snapshot().revision();
        assert!(handle
            .new_controller()
            .dispatch(Command::SetSourceSettings {
                source_id,
                settings: serde_json::json!({"large": "x".repeat(4096)})
            })
            .await
            .is_err());
        assert_eq!(handle.snapshot().revision(), revision);
        handle.redo().await.unwrap();
        handle
            .dispatch(Command::SetSourceSettings {
                source_id,
                settings: serde_json::json!({"value": "initial".repeat(50)}),
            })
            .await
            .unwrap();
        handle.begin_transaction("small group").await.unwrap();
        handle
            .dispatch(Command::RenameScene {
                scene_id,
                name: "inside".into(),
            })
            .await
            .unwrap();
        let revision = handle.snapshot().revision();
        let mut nested = serde_json::Value::Null;
        for _ in 0..8 {
            nested = serde_json::Value::Array(vec![nested]);
        }
        assert!(handle
            .dispatch(Command::SetSourceSettings {
                source_id,
                settings: nested
            })
            .await
            .is_err());
        let mut nested_command = Command::RenameScene {
            scene_id,
            name: "deep".into(),
        };
        for _ in 0..8 {
            nested_command = Command::Transaction {
                commands: vec![nested_command],
            };
        }
        assert!(handle.dispatch(nested_command).await.is_err());
        assert_eq!(handle.snapshot().revision(), revision);
        // Retaining successive old settings exhausts the group byte budget.
        handle
            .dispatch(Command::SetSourceSettings {
                source_id,
                settings: serde_json::json!({"value": "a".repeat(350)}),
            })
            .await
            .unwrap();
        handle
            .dispatch(Command::SetSourceSettings {
                source_id,
                settings: serde_json::json!({"value": "b".repeat(350)}),
            })
            .await
            .unwrap();
        let revision = handle.snapshot().revision();
        assert!(handle
            .dispatch(Command::SetSourceSettings {
                source_id,
                settings: serde_json::json!({"value": "c".repeat(350)})
            })
            .await
            .is_err());
        assert_eq!(handle.snapshot().revision(), revision);
        handle.end_transaction().await.unwrap();
        handle.undo().await.unwrap();
        assert_eq!(
            handle.snapshot().source(source_id).unwrap().settings,
            serde_json::json!({"value": "initial".repeat(50)})
        );
        assert_eq!(handle.snapshot().scene(scene_id).unwrap().name, "changed");
        handle.shutdown().await;
    }

    #[tokio::test]
    async fn failed_inverse_application_preserves_history_entry() {
        let (handle, scene_id, _) = undo_fixture(CoreConfig::default()).await;
        handle
            .dispatch(Command::RenameScene {
                scene_id,
                name: "changed".into(),
            })
            .await
            .unwrap();
        handle
            .dispatch(Command::AddScene {
                name: "spare".into(),
            })
            .await
            .unwrap();
        handle
            .dispatch(Command::RemoveScene { scene_id })
            .await
            .unwrap();
        let revision = handle.snapshot().revision();
        for _ in 0..2 {
            assert!(matches!(
                handle.undo().await,
                Err(HandleError::Core(Error::NotFound(_)))
            ));
        }
        assert_eq!(handle.snapshot().revision(), revision);
        handle.shutdown().await;
    }

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
