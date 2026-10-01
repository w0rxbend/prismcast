//! The persistence actor: the only component that opens files under the
//! config root (PLAN §57, ADR-0008 §5; persistence-model §8).
//!
//! The core actor classifies every applied command ([`dirty_class`]) and
//! forwards whole-aggregate snapshots over a bounded channel. Dirty marks are
//! debounced (trailing edge, capped by a max delay) so a 100-command drag
//! transaction produces one write. Writes run in `tokio::task::spawn_blocking`
//! — fsync is blocking syscall work and never happens on runtime threads.
//!
//! ## Backpressure
//!
//! The command→actor channel is bounded. A full channel drops the *new* dirty
//! mark with a warning: whole-aggregate snapshots make this self-healing,
//! because any later snapshot contains all prior state, and the shutdown
//! flush is awaited. The core actor never blocks on persistence.
//!
//! ## Shutdown
//!
//! [`PersistenceHandle::shutdown`] performs a final awaited flush; the app
//! does not exit with pending dirty state.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use tokio::sync::{broadcast, mpsc, oneshot};
use tracing::{debug, error, info, instrument, warn};

use prismcast_core::command::Command;
use prismcast_core::id::{ProfileId, SceneCollectionId};
use prismcast_core::project::Profile;
use prismcast_core::state::AppState;

use super::envelope::{CollectionFileV1, CollectionSnapshot, SessionState};
use super::error::PersistenceError;
use super::paths::{unique_slug, ConfigRoot};
use super::pointer::{PointerDocument, PointerState};
use super::profile::{ProfileDocument, ProfileSnapshot};
use super::store::ProjectStore;

/// Default debounce quiet period before a dirty mark is written.
pub const DEFAULT_DEBOUNCE: Duration = Duration::from_millis(500);
/// Default cap on how long a dirty mark may pend while changes keep coming.
pub const DEFAULT_MAX_DELAY: Duration = Duration::from_secs(5);
/// Default bound of the command→persistence channel.
pub const DEFAULT_CHANNEL_CAPACITY: usize = 16;

/// Which file families a command touches.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DirtyClass {
    /// The active scene collection (`collections/<slug>/collection.json`).
    pub collection: bool,
    /// The active profile (`profiles/<slug>/profile.toml`).
    pub profile: bool,
    /// The pointer file (`prismcast.toml`).
    pub pointer: bool,
}

impl DirtyClass {
    /// Runtime-only change; nothing is written.
    pub const VOLATILE: Self = Self {
        collection: false,
        profile: false,
        pointer: false,
    };
    /// Collection content changed.
    pub const COLLECTION: Self = Self {
        collection: true,
        profile: false,
        pointer: false,
    };
    /// Profile content changed.
    pub const PROFILE: Self = Self {
        collection: false,
        profile: true,
        pointer: false,
    };

    /// Whether nothing needs saving.
    pub fn is_volatile(self) -> bool {
        self == Self::VOLATILE
    }

    fn merge(self, other: Self) -> Self {
        Self {
            collection: self.collection || other.collection,
            profile: self.profile || other.profile,
            pointer: self.pointer || other.pointer,
        }
    }
}

/// Classifies a command into the file families it dirties. Exhaustive over
/// all 49 [`Command`] variants — a new variant fails to compile until it is
/// classified.
///
/// Rules (persistence-model §8):
/// - scenes/sources/items/transition/audio → collection;
/// - session state (current scene, studio mode) → collection (it lives in
///   `collection.json`);
/// - output graph configuration → profile;
/// - output lifecycle (`StartOutput`/`StopOutput`) → volatile (`Output.state`
///   is never persisted);
/// - add/select of profiles/collections → pointer file.
///
/// `RemoveProfile`/`RemoveSceneCollection` are classified volatile: file
/// deletion of removed entities is a deliberate follow-up (their directories
/// are kept as orphans rather than deleted under the user's feet).
pub fn dirty_class(command: &Command) -> DirtyClass {
    match command {
        // --- Scenes ---
        Command::AddScene { .. }
        | Command::RemoveScene { .. }
        | Command::RenameScene { .. }
        | Command::ReorderScene { .. }
        | Command::SetCurrentScene { .. }
        // --- Scene items ---
        | Command::AddSceneItem { .. }
        | Command::RemoveSceneItem { .. }
        | Command::DuplicateSceneItem { .. }
        | Command::SetSceneItemTransform { .. }
        | Command::SetSceneItemCrop { .. }
        | Command::SetSceneItemVisible { .. }
        | Command::SetSceneItemLocked { .. }
        | Command::SetSceneItemZIndex { .. }
        | Command::RaiseSceneItem { .. }
        | Command::LowerSceneItem { .. }
        | Command::SetSceneItemOpacity { .. }
        | Command::SetSceneItemBounds { .. }
        // --- Sources ---
        | Command::AddSource { .. }
        | Command::RemoveSource { .. }
        | Command::RenameSource { .. }
        | Command::SetSourceSettings { .. }
        | Command::SetSourceEnabled { .. }
        // --- Audio ---
        | Command::SetSourceVolume { .. }
        | Command::SetSourceMuted { .. }
        | Command::SetSourceSolo { .. }
        | Command::SetSourceMonitor { .. }
        | Command::SetSourceBalance { .. }
        | Command::SetSourceSyncOffset { .. }
        | Command::AddAudioBus { .. }
        | Command::RemoveAudioBus { .. }
        | Command::SetAudioRoute { .. }
        | Command::RemoveAudioRoute { .. }
        // --- Studio mode & transitions (session lives in collection.json) ---
        | Command::SetStudioModeEnabled { .. }
        | Command::SetPreviewScene { .. }
        | Command::TransitionToProgram
        | Command::SwapPreviewProgram
        | Command::SetTransition { .. } => DirtyClass::COLLECTION,

        // --- Outputs: configuration is profile data, lifecycle is volatile ---
        Command::AddOutput { .. }
        | Command::RemoveOutput { .. }
        | Command::SetOutputReconnectPolicy { .. } => DirtyClass::PROFILE,
        Command::StartOutput { .. } | Command::StopOutput { .. } => DirtyClass::VOLATILE,

        // --- Profiles & collections registries ---
        Command::AddProfile { .. } => DirtyClass {
            collection: false,
            profile: true,
            pointer: true,
        },
        Command::SelectProfile { .. } => DirtyClass {
            collection: false,
            profile: false,
            pointer: true,
        },
        Command::AddSceneCollection { .. } => DirtyClass {
            collection: false,
            profile: false,
            pointer: true,
        },
        Command::SelectSceneCollection { .. } => DirtyClass {
            collection: true,
            profile: false,
            pointer: true,
        },
        // File deletion for removed entities is a follow-up; directories are
        // kept as orphans.
        Command::RemoveProfile { .. } | Command::RemoveSceneCollection { .. } => {
            DirtyClass::VOLATILE
        }

        Command::Transaction { commands } => commands
            .iter()
            .fold(DirtyClass::VOLATILE, |acc, cmd| acc.merge(dirty_class(cmd))),
    }
}

/// Whole-aggregate dirty marks sent from the core actor to the persistence
/// actor. Only the parts a command touched are filled.
#[derive(Debug, Default)]
pub struct DirtyMarks {
    /// Latest active-collection snapshot, if collection-dirty.
    pub collection: Option<CollectionSnapshot>,
    /// Latest active-profile snapshot, if profile-dirty.
    pub profile: Option<ProfileSnapshot>,
    /// Latest pointer selection, if pointer-dirty.
    pub pointer: Option<PointerSelection>,
}

impl DirtyMarks {
    fn is_empty(&self) -> bool {
        self.collection.is_none() && self.profile.is_none() && self.pointer.is_none()
    }

    fn absorb(&mut self, other: DirtyMarks) {
        if other.collection.is_some() {
            self.collection = other.collection;
        }
        if other.profile.is_some() {
            self.profile = other.profile;
        }
        if other.pointer.is_some() {
            self.pointer = other.pointer;
        }
    }
}

/// The pointer selection as the core actor sees it: IDs plus names (slugs are
/// resolved by the persistence actor, which owns the ID ↔ directory map).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PointerSelection {
    /// Active profile ID and name.
    pub active_profile: Option<(ProfileId, String)>,
    /// Active collection ID and name.
    pub active_collection: Option<(SceneCollectionId, String)>,
}

/// Events emitted by the persistence actor (saved/recovered/failed), for
/// observers and tests. A future `SystemEvent::PersistenceRecovered` core
/// variant will surface these to controllers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PersistenceEvent {
    /// A file was written.
    Saved {
        /// The written file.
        path: PathBuf,
    },
    /// A write failed (logged; the dirty mark is retried on the next change).
    WriteFailed {
        /// The file that could not be written.
        path: PathBuf,
        /// The typed error, rendered.
        error: String,
    },
}

/// Tuning for the persistence actor.
#[derive(Debug, Clone)]
pub struct PersistenceConfig {
    /// The config root (injectable for tests).
    pub root: ConfigRoot,
    /// Debounce quiet period.
    pub debounce: Duration,
    /// Cap on pending time while changes keep arriving.
    pub max_delay: Duration,
    /// Bound of the command→persistence channel.
    pub channel_capacity: usize,
}

impl PersistenceConfig {
    /// Config for a given root with default tuning.
    pub fn new(root: ConfigRoot) -> Self {
        Self {
            root,
            debounce: DEFAULT_DEBOUNCE,
            max_delay: DEFAULT_MAX_DELAY,
            channel_capacity: DEFAULT_CHANNEL_CAPACITY,
        }
    }
}

enum PersistMsg {
    Dirty(Box<DirtyMarks>),
    Flush(oneshot::Sender<Result<(), PersistenceError>>),
    Shutdown(oneshot::Sender<Result<(), PersistenceError>>),
}

/// Cloneable handle to the running persistence actor.
#[derive(Clone)]
pub struct PersistenceHandle {
    tx: mpsc::Sender<PersistMsg>,
    events: broadcast::Sender<PersistenceEvent>,
}

impl PersistenceHandle {
    /// Spawns the persistence actor. Must be called from within a Tokio
    /// runtime.
    pub fn spawn(config: PersistenceConfig) -> Self {
        let (tx, rx) = mpsc::channel(config.channel_capacity.max(1));
        let (events, _) = broadcast::channel(64);
        let actor = PersistenceActor {
            store: ProjectStore::new(config.root),
            debounce: config.debounce,
            max_delay: config.max_delay,
            rx,
            events: events.clone(),
            profile_slugs: HashMap::new(),
            collection_slugs: HashMap::new(),
            retained_collection: None,
            retained_profile: None,
            retained_pointer: None,
        };
        tokio::spawn(actor.run());
        Self { tx, events }
    }

    /// Non-blocking notification from the core actor's command path: an
    /// applied command plus the post-apply state. Extracts the dirty
    /// aggregates and queues them; on a full channel the mark is dropped with
    /// a warning (self-healing, see module docs). Never blocks the caller.
    pub fn command_applied(&self, command: &Command, state: &AppState) {
        let class = dirty_class(command);
        if class.is_volatile() {
            return;
        }
        let mut marks = DirtyMarks::default();
        if class.collection {
            marks.collection = collection_snapshot(state);
        }
        if class.profile {
            marks.profile = Some(profile_snapshot(state));
        }
        if class.pointer {
            marks.pointer = Some(pointer_selection(state));
        }
        if marks.is_empty() {
            return;
        }
        if let Err(err) = self.tx.try_send(PersistMsg::Dirty(Box::new(marks))) {
            warn!(
                %err,
                "persistence channel full or closed; dirty mark coalesced/dropped"
            );
        }
    }

    /// Forces an immediate write of all pending dirty state and resolves when
    /// the writes finished. This is the save-on-shutdown / explicit-save path.
    pub async fn save_now(&self) -> Result<(), PersistenceError> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(PersistMsg::Flush(tx))
            .await
            .map_err(|_| PersistenceError::Serialize {
                what: "persistence actor",
                reason: "actor is shut down".to_string(),
            })?;
        rx.await.map_err(|_| PersistenceError::Serialize {
            what: "persistence actor",
            reason: "actor dropped the flush reply".to_string(),
        })?
    }

    /// Alias of [`save_now`](Self::save_now), for the shutdown call site.
    pub async fn flush(&self) -> Result<(), PersistenceError> {
        self.save_now().await
    }

    /// Flushes pending writes and stops the actor.
    pub async fn shutdown(&self) -> Result<(), PersistenceError> {
        let (tx, rx) = oneshot::channel();
        if self.tx.send(PersistMsg::Shutdown(tx)).await.is_err() {
            return Ok(()); // Already stopped — the desired end state.
        }
        rx.await.unwrap_or(Ok(()))
    }

    /// Subscribes to persistence events (saved/failed).
    pub fn subscribe(&self) -> broadcast::Receiver<PersistenceEvent> {
        self.events.subscribe()
    }
}

/// Extracts the active collection plus session state from the app state.
fn collection_snapshot(state: &AppState) -> Option<CollectionSnapshot> {
    let id = state.active_collection?;
    let name = state.collections.get(&id)?.name.clone();
    Some(CollectionSnapshot {
        collection: prismcast_core::project::SceneCollection {
            id,
            name,
            scenes: state.scenes.values().cloned().collect(),
            sources: state.sources.values().cloned().collect(),
            transition: state.transition.clone(),
            audio: state.audio.clone(),
        },
        session: SessionState {
            current_scene: state.current_scene,
            studio_mode: state.studio_mode.clone(),
        },
    })
}

/// Extracts the active profile plus the output graph.
///
/// Note: `AppState` holds outputs in a flat map, so the whole working set is
/// attributed to the active profile (persistence-model §3 follow-up).
fn profile_snapshot(state: &AppState) -> ProfileSnapshot {
    let profile = state
        .active_profile
        .and_then(|id| state.profiles.get(&id))
        .cloned()
        .unwrap_or_else(|| {
            Profile::new("Default", prismcast_core::project::VideoConfig::default())
        });
    ProfileSnapshot {
        profile,
        // AppState does not yet hold encoder/service registries; they load
        // from file but have no domain home yet (tracked follow-up).
        encoders: Vec::new(),
        services: Vec::new(),
        outputs: state.outputs.values().cloned().collect(),
    }
}

fn pointer_selection(state: &AppState) -> PointerSelection {
    PointerSelection {
        active_profile: state
            .active_profile
            .and_then(|id| state.profiles.get(&id))
            .map(|p| (p.id, p.name.clone())),
        active_collection: state
            .active_collection
            .and_then(|id| state.collections.get(&id))
            .map(|c| (c.id, c.name.clone())),
    }
}

struct PersistenceActor {
    store: ProjectStore,
    debounce: Duration,
    max_delay: Duration,
    rx: mpsc::Receiver<PersistMsg>,
    events: broadcast::Sender<PersistenceEvent>,
    /// ID ↔ directory slug maps (the actor owns them, persistence-model §8).
    profile_slugs: HashMap<ProfileId, String>,
    collection_slugs: HashMap<SceneCollectionId, String>,
    /// Retained envelopes/documents of the last load or save, so unknown
    /// fields survive a load → save cycle.
    retained_collection: Option<(SceneCollectionId, CollectionFileV1)>,
    retained_profile: Option<(ProfileId, ProfileDocument)>,
    retained_pointer: Option<PointerDocument>,
}

impl PersistenceActor {
    #[instrument(name = "persistence_actor", skip_all)]
    async fn run(mut self) {
        info!(root = %self.store.root().root().display(), "persistence actor started");
        let mut pending = DirtyMarks::default();
        let mut first_pending: Option<Instant> = None;
        let mut deadline: Option<Instant> = None;

        loop {
            // `async move` copies the (Copy) deadline so the timer future
            // never borrows state the select arms mutate.
            let timer = async move {
                match deadline {
                    Some(deadline) => tokio::time::sleep_until(deadline.into()).await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                message = self.rx.recv() => {
                    match message {
                        Some(PersistMsg::Dirty(marks)) => {
                            pending.absorb(*marks);
                            let now = Instant::now();
                            let first = *first_pending.get_or_insert(now);
                            deadline = Some((now + self.debounce).min(first + self.max_delay));
                        }
                        Some(PersistMsg::Flush(reply)) => {
                            let result = self.write_pending(&mut pending).await;
                            first_pending = None;
                            deadline = None;
                            let _ = reply.send(result);
                        }
                        Some(PersistMsg::Shutdown(reply)) => {
                            let result = self.write_pending(&mut pending).await;
                            let _ = reply.send(result);
                            break;
                        }
                        None => {
                            // All handles dropped: flush and stop.
                            if !pending.is_empty() {
                                let _ = self.write_pending(&mut pending).await;
                            }
                            break;
                        }
                    }
                }
                () = timer => {
                    debug!("debounce quiet period elapsed; writing pending state");
                    let _ = self.write_pending(&mut pending).await;
                    first_pending = None;
                    deadline = None;
                }
            }
        }
        info!("persistence actor stopped");
    }

    /// Writes every pending dirty part, one file at a time, off the runtime.
    async fn write_pending(&mut self, pending: &mut DirtyMarks) -> Result<(), PersistenceError> {
        let mut first_error: Option<PersistenceError> = None;

        if let Some(snapshot) = pending.collection.take() {
            let slug = self.collection_slug(snapshot.collection.id, &snapshot.collection.name);
            let retained = self
                .retained_collection
                .as_ref()
                .filter(|(id, _)| *id == snapshot.collection.id)
                .map(|(_, envelope)| envelope.clone());
            let store = self.store.clone();
            let slug_owned = slug.clone();
            let result = tokio::task::spawn_blocking(move || {
                store.save_collection(&slug_owned, &snapshot, retained.as_ref())
            })
            .await;
            match result {
                Ok(Ok(envelope)) => {
                    let id = envelope.id;
                    self.emit(PersistenceEvent::Saved {
                        path: self.store.root().collection_file(&slug),
                    });
                    self.retained_collection = Some((id, envelope));
                }
                Ok(Err(err)) => {
                    self.write_failed(self.store.root().collection_file(&slug), &err);
                    first_error.get_or_insert(err);
                }
                Err(join) => {
                    error!(%join, "collection write task panicked");
                }
            }
        }

        if let Some(snapshot) = pending.profile.take() {
            let slug = self.profile_slug(snapshot.profile.id, &snapshot.profile.name);
            let retained = self
                .retained_profile
                .as_ref()
                .filter(|(id, _)| *id == snapshot.profile.id)
                .map(|(_, doc)| doc.clone());
            let store = self.store.clone();
            let slug_owned = slug.clone();
            let result = tokio::task::spawn_blocking(move || {
                store.save_profile(&slug_owned, &snapshot, retained.as_ref())
            })
            .await;
            match result {
                Ok(Ok(document)) => {
                    let id = document.snapshot().profile.id;
                    self.emit(PersistenceEvent::Saved {
                        path: self.store.root().profile_file(&slug),
                    });
                    self.retained_profile = Some((id, document));
                }
                Ok(Err(err)) => {
                    self.write_failed(self.store.root().profile_file(&slug), &err);
                    first_error.get_or_insert(err);
                }
                Err(join) => {
                    error!(%join, "profile write task panicked");
                }
            }
        }

        if let Some(selection) = pending.pointer.take() {
            let state = PointerState {
                active_profile: selection
                    .active_profile
                    .map(|(id, name)| self.profile_slug(id, &name)),
                active_collection: selection
                    .active_collection
                    .map(|(id, name)| self.collection_slug(id, &name)),
            };
            let retained = self.retained_pointer.clone();
            let store = self.store.clone();
            let result =
                tokio::task::spawn_blocking(move || store.save_pointer(&state, retained.as_ref()))
                    .await;
            let path = self.store.root().pointer_file();
            match result {
                Ok(Ok(document)) => {
                    self.emit(PersistenceEvent::Saved { path });
                    self.retained_pointer = Some(document);
                }
                Ok(Err(err)) => {
                    self.write_failed(path, &err);
                    first_error.get_or_insert(err);
                }
                Err(join) => {
                    error!(%join, "pointer write task panicked");
                }
            }
        }

        match first_error {
            Some(err) => Err(err),
            None => Ok(()),
        }
    }

    fn write_failed(&self, path: PathBuf, err: &PersistenceError) {
        error!(path = %path.display(), %err, "persistence write failed");
        self.emit(PersistenceEvent::WriteFailed {
            path,
            error: err.to_string(),
        });
    }

    fn emit(&self, event: PersistenceEvent) {
        // No subscribers is normal; lagged subscribers simply miss events.
        let _ = self.events.send(event);
    }

    fn profile_slug(&mut self, id: ProfileId, name: &str) -> String {
        self.profile_slugs
            .entry(id)
            .or_insert_with(|| unique_slug(name, self.store.list_profile_slugs().iter()))
            .clone()
    }

    fn collection_slug(&mut self, id: SceneCollectionId, name: &str) -> String {
        self.collection_slugs
            .entry(id)
            .or_insert_with(|| unique_slug(name, self.store.list_collection_slugs().iter()))
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use prismcast_core::audio::{MonitorMode, TrackMask};
    use prismcast_core::id::{AudioBusId, OutputId, SceneId, SceneItemId, SourceId};
    use prismcast_core::output::{Output, OutputKind, ReconnectPolicy};
    use prismcast_core::project::SceneCollection;
    use prismcast_core::scene::{Bounds, Crop, Transform};
    use prismcast_core::source::SourceKind;
    use prismcast_core::transition::Transition;

    /// Exercises every one of the 49 Command variants: the match in
    /// `dirty_class` is exhaustive, so this test failing to compile is the
    /// signal that a new variant was added without classification.
    #[test]
    fn every_variant_is_classified() {
        let scene = SceneId::new();
        let item = SceneItemId::new();
        let source = SourceId::new();
        let bus = AudioBusId::new();
        let output = OutputId::new();
        let commands = vec![
            Command::AddScene { name: "s".into() },
            Command::RemoveScene { scene_id: scene },
            Command::RenameScene {
                scene_id: scene,
                name: "s".into(),
            },
            Command::ReorderScene {
                scene_id: scene,
                new_index: 0,
            },
            Command::SetCurrentScene { scene_id: scene },
            Command::AddSceneItem {
                scene_id: scene,
                source_id: source,
            },
            Command::RemoveSceneItem {
                scene_id: scene,
                item_id: item,
            },
            Command::DuplicateSceneItem {
                scene_id: scene,
                item_id: item,
            },
            Command::SetSceneItemTransform {
                scene_id: scene,
                item_id: item,
                transform: Transform::default(),
            },
            Command::SetSceneItemCrop {
                scene_id: scene,
                item_id: item,
                crop: Crop::default(),
            },
            Command::SetSceneItemVisible {
                scene_id: scene,
                item_id: item,
                visible: true,
            },
            Command::SetSceneItemLocked {
                scene_id: scene,
                item_id: item,
                locked: false,
            },
            Command::SetSceneItemZIndex {
                scene_id: scene,
                item_id: item,
                z_index: 0,
            },
            Command::RaiseSceneItem {
                scene_id: scene,
                item_id: item,
            },
            Command::LowerSceneItem {
                scene_id: scene,
                item_id: item,
            },
            Command::SetSceneItemOpacity {
                scene_id: scene,
                item_id: item,
                opacity: 1.0,
            },
            Command::SetSceneItemBounds {
                scene_id: scene,
                item_id: item,
                bounds: Bounds::default(),
            },
            Command::AddSource {
                kind: SourceKind::Color,
                name: "c".into(),
            },
            Command::RemoveSource { source_id: source },
            Command::RenameSource {
                source_id: source,
                name: "c".into(),
            },
            Command::SetSourceSettings {
                source_id: source,
                settings: serde_json::Value::Null,
            },
            Command::SetSourceEnabled {
                source_id: source,
                enabled: true,
            },
            Command::SetSourceVolume {
                source_id: source,
                volume_db: 0.0,
            },
            Command::SetSourceMuted {
                source_id: source,
                muted: false,
            },
            Command::SetSourceSolo {
                source_id: source,
                solo: false,
            },
            Command::SetSourceMonitor {
                source_id: source,
                monitor: MonitorMode::Off,
            },
            Command::SetSourceBalance {
                source_id: source,
                balance: 0.0,
            },
            Command::SetSourceSyncOffset {
                source_id: source,
                sync_offset_ms: 0,
            },
            Command::AddAudioBus { name: "b".into() },
            Command::RemoveAudioBus { bus_id: bus },
            Command::SetAudioRoute {
                source_id: source,
                bus_id: bus,
                tracks: TrackMask::default(),
            },
            Command::RemoveAudioRoute {
                source_id: source,
                bus_id: bus,
            },
            Command::AddOutput {
                output: Output::new(
                    OutputKind::Recording,
                    "r",
                    prismcast_core::id::EncoderId::new(),
                ),
            },
            Command::RemoveOutput { output_id: output },
            Command::StartOutput { output_id: output },
            Command::StopOutput { output_id: output },
            Command::SetOutputReconnectPolicy {
                output_id: output,
                policy: ReconnectPolicy::default(),
            },
            Command::SetStudioModeEnabled { enabled: true },
            Command::SetPreviewScene { scene_id: scene },
            Command::TransitionToProgram,
            Command::SwapPreviewProgram,
            Command::SetTransition {
                transition: Transition::default(),
            },
            Command::AddProfile {
                profile: Profile::new("p", prismcast_core::project::VideoConfig::default()),
            },
            Command::RemoveProfile {
                profile_id: ProfileId::new(),
            },
            Command::SelectProfile {
                profile_id: ProfileId::new(),
            },
            Command::AddSceneCollection {
                collection: SceneCollection::new("c"),
            },
            Command::RemoveSceneCollection {
                collection_id: SceneCollectionId::new(),
            },
            Command::SelectSceneCollection {
                collection_id: SceneCollectionId::new(),
            },
            Command::Transaction {
                commands: vec![Command::TransitionToProgram],
            },
        ];
        assert_eq!(commands.len(), 49);
        for command in &commands {
            let _ = dirty_class(command);
        }
    }

    #[test]
    fn classification_rules() {
        let scene = SceneId::new();
        let source = SourceId::new();
        assert!(dirty_class(&Command::AddScene { name: "s".into() }).collection);
        assert!(dirty_class(&Command::SetCurrentScene { scene_id: scene }).collection);
        assert!(
            dirty_class(&Command::SetSourceMuted {
                source_id: source,
                muted: true
            })
            .collection
        );
        assert!(dirty_class(&Command::TransitionToProgram).collection);

        let output = OutputId::new();
        assert!(
            dirty_class(&Command::SetOutputReconnectPolicy {
                output_id: output,
                policy: ReconnectPolicy::default(),
            })
            .profile
        );
        assert!(dirty_class(&Command::StartOutput { output_id: output }).is_volatile());
        assert!(dirty_class(&Command::StopOutput { output_id: output }).is_volatile());

        let pointer = dirty_class(&Command::SelectProfile {
            profile_id: ProfileId::new(),
        });
        assert!(pointer.pointer && !pointer.profile && !pointer.collection);

        let transaction = dirty_class(&Command::Transaction {
            commands: vec![
                Command::StartOutput { output_id: output },
                Command::SetCurrentScene { scene_id: scene },
            ],
        });
        assert!(transaction.collection && !transaction.profile);
    }

    #[tokio::test]
    async fn debounce_coalesces_rapid_marks() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = PersistenceConfig::new(ConfigRoot::new(dir.path()));
        config.debounce = Duration::from_millis(500);
        let handle = PersistenceHandle::spawn(config);
        let mut events = handle.subscribe();

        let state = AppState::new();
        let command = Command::AddScene { name: "s".into() };
        for _ in 0..100 {
            handle.command_applied(&command, &state);
        }
        handle.save_now().await.expect("flush");
        handle.shutdown().await.expect("shutdown");

        let mut saved = 0;
        while let Ok(event) = events.try_recv() {
            if matches!(event, PersistenceEvent::Saved { .. }) {
                saved += 1;
            }
        }
        assert!(
            (1..=2).contains(&saved),
            "100 rapid marks -> {saved} writes"
        );
    }

    #[tokio::test]
    async fn save_now_writes_pending_immediately() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = PersistenceConfig::new(ConfigRoot::new(dir.path()));
        config.debounce = Duration::from_secs(3600); // debounce must not gate save_now
        let handle = PersistenceHandle::spawn(config);

        let state = AppState::new();
        handle.command_applied(&Command::AddScene { name: "s".into() }, &state);
        handle.save_now().await.expect("save_now");

        let file = dir.path().join("collections/default/collection.json");
        assert!(file.exists(), "collection file written: {file:?}");
        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn shutdown_flushes_pending() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = PersistenceConfig::new(ConfigRoot::new(dir.path()));
        config.debounce = Duration::from_secs(3600);
        let handle = PersistenceHandle::spawn(config);

        let state = AppState::new();
        handle.command_applied(&Command::AddScene { name: "s".into() }, &state);
        handle.shutdown().await.expect("shutdown flushes");
        assert!(dir
            .path()
            .join("collections/default/collection.json")
            .exists());
    }
}
