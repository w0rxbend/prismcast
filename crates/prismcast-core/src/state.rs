//! The pure in-memory domain store and the command-application function.
//!
//! [`AppState`] holds all domain state owned by the application core:
//! profiles, scene collections, the working set of scenes/sources/outputs,
//! the audio configuration, the current scene, and studio mode. It is
//! deliberately **not** behind a lock here — PLAN.md §57 puts it inside a core
//! actor; this crate only provides the pure data structure.
//!
//! [`apply`] is the single mutation path (ADR-0005): it validates the command
//! against the current state, mutates, and returns the committed [`Event`]s.
//! It is pure (no I/O, no clocks, no randomness beyond ID generation in
//! `Add*` commands) and deterministic given the same state and command, so all
//! domain logic is headless-testable.
//!
//! ## Delete policy (documented choice)
//!
//! - **Reject** deletions that would silently destroy intentional user
//!   configuration: removing a `Source` referenced by scene items or audio
//!   routes, removing a `Scene` referenced by a scene source or studio mode,
//!   removing the active profile/collection, removing the last scene or the
//!   last audio bus, removing a non-`Stopped` output.
//! - **Cascade with explicit events** only where the cascade is the obvious
//!   meaning of the operation: removing an `AudioBus` removes its routes
//!   (`RouteRemoved` events), and `RemoveScene`/`RemoveSource` drop their
//!   dependent items/mixer entries (a source's orphaned mixer entry resets to
//!   defaults with a `MixerChanged` event before `Removed`).
//!
//! ## Undo (PLAN.md §59)
//!
//! [`AppState::inverse`] reconstructs the inverse of a command from the
//! **pre-application** state: call it before `apply`. Creation and destruction
//! commands (`Add*`/`Remove*`, `DuplicateSceneItem`) are irreversible and
//! return `None`; destructive cascades (`RemoveAudioBus`) would need a full
//! snapshot restore, which the undo service may implement on top.

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use crate::audio::{AudioMixerConfig, AudioMixerState, AudioRoute};
use crate::command::Command;
use crate::error::{Error, Result};
use crate::event::{AudioEvent, Event, OutputEvent, SceneEvent, SourceEvent, SystemEvent};
use crate::id::{OutputId, ProfileId, SceneCollectionId, SceneId, SceneItemId, SourceId};
use crate::output::{Output, OutputState};
use crate::project::{Profile, SceneCollection, StudioMode, VideoConfig};
use crate::scene::{Scene, SceneItem};
use crate::source::{Source, SourceKind};
use crate::transition::Transition;

/// The pure in-memory domain store.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppState {
    /// Known profiles; exactly one is active.
    pub profiles: IndexMap<ProfileId, Profile>,
    /// Known scene collections; exactly one is active.
    pub collections: IndexMap<SceneCollectionId, SceneCollection>,
    /// The active profile.
    pub active_profile: Option<ProfileId>,
    /// The active scene collection.
    pub active_collection: Option<SceneCollectionId>,

    /// Working-set scenes (ordered as in the UI list).
    pub scenes: IndexMap<SceneId, Scene>,
    /// Shared sources referenced by scene items.
    pub sources: IndexMap<SourceId, Source>,
    /// The default transition configuration.
    pub transition: Transition,
    /// Audio buses, routes, and mixer state.
    pub audio: AudioMixerConfig,
    /// The output graph (N independent outputs, ADR-0007).
    pub outputs: IndexMap<OutputId, Output>,

    /// The current (program) scene.
    pub current_scene: Option<SceneId>,
    /// Studio mode state (`None` = disabled).
    pub studio_mode: Option<StudioMode>,
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

impl AppState {
    /// Creates an empty state seeded with a default profile and collection.
    pub fn new() -> Self {
        let profile = Profile::new("Default", VideoConfig::default());
        let collection = SceneCollection::new("Default");
        let mut profiles = IndexMap::new();
        let mut collections = IndexMap::new();
        let active_profile = Some(profile.id);
        let active_collection = Some(collection.id);
        profiles.insert(profile.id, profile);
        collections.insert(collection.id, collection);
        Self {
            profiles,
            collections,
            active_profile,
            active_collection,
            scenes: IndexMap::new(),
            sources: IndexMap::new(),
            transition: Transition::default(),
            audio: AudioMixerConfig::with_master_bus(),
            outputs: IndexMap::new(),
            current_scene: None,
            studio_mode: None,
        }
    }

    /// Returns the scene with the given ID.
    pub fn scene(&self, scene_id: SceneId) -> Option<&Scene> {
        self.scenes.get(&scene_id)
    }

    /// Returns the source with the given ID.
    pub fn source(&self, source_id: SourceId) -> Option<&Source> {
        self.sources.get(&source_id)
    }

    /// Returns the output with the given ID.
    pub fn output(&self, output_id: OutputId) -> Option<&Output> {
        self.outputs.get(&output_id)
    }

    /// Applies a command: validates, mutates, returns committed events.
    pub fn apply(&mut self, command: &Command) -> Result<Vec<Event>> {
        apply(self, command)
    }

    /// Returns the command that undoes `command` given the current
    /// (pre-application) state, or `None` if the command is irreversible
    /// (creations, destructions, duplicates — see module docs).
    ///
    /// Transaction groups invert to the reversed group of per-command
    /// inverses, or `None` if any member is irreversible.
    pub fn inverse(&self, command: &Command) -> Option<Command> {
        match command {
            Command::RenameScene { scene_id, .. } => Some(Command::RenameScene {
                scene_id: *scene_id,
                name: self.scene(*scene_id)?.name.clone(),
            }),
            Command::ReorderScene { scene_id, .. } => {
                let index = self.scenes.get_index_of(scene_id)?;
                Some(Command::ReorderScene {
                    scene_id: *scene_id,
                    new_index: index,
                })
            }
            Command::SetCurrentScene { .. } => Some(Command::SetCurrentScene {
                scene_id: self.current_scene?,
            }),
            Command::SetSceneItemTransform {
                scene_id, item_id, ..
            } => {
                let item = self.scene(*scene_id)?.item(*item_id)?;
                Some(Command::SetSceneItemTransform {
                    scene_id: *scene_id,
                    item_id: *item_id,
                    transform: item.transform,
                })
            }
            Command::SetSceneItemCrop {
                scene_id, item_id, ..
            } => {
                let item = self.scene(*scene_id)?.item(*item_id)?;
                Some(Command::SetSceneItemCrop {
                    scene_id: *scene_id,
                    item_id: *item_id,
                    crop: item.crop,
                })
            }
            Command::SetSceneItemVisible {
                scene_id, item_id, ..
            } => {
                let item = self.scene(*scene_id)?.item(*item_id)?;
                Some(Command::SetSceneItemVisible {
                    scene_id: *scene_id,
                    item_id: *item_id,
                    visible: item.visible,
                })
            }
            Command::SetSceneItemLocked {
                scene_id, item_id, ..
            } => {
                let item = self.scene(*scene_id)?.item(*item_id)?;
                Some(Command::SetSceneItemLocked {
                    scene_id: *scene_id,
                    item_id: *item_id,
                    locked: item.locked,
                })
            }
            Command::SetSceneItemZIndex {
                scene_id, item_id, ..
            } => {
                let item = self.scene(*scene_id)?.item(*item_id)?;
                Some(Command::SetSceneItemZIndex {
                    scene_id: *scene_id,
                    item_id: *item_id,
                    z_index: item.z_index,
                })
            }
            Command::RaiseSceneItem { scene_id, item_id }
            | Command::LowerSceneItem { scene_id, item_id } => {
                // Exact inverse: restore both affected z-indices. The neighbor
                // is whichever item sits adjacent in the current ordering;
                // restoring absolute values is correct even on no-ops.
                let scene = self.scene(*scene_id)?;
                let item = scene.item(*item_id)?;
                let own_z = item.z_index;
                let mut restores = vec![Command::SetSceneItemZIndex {
                    scene_id: *scene_id,
                    item_id: *item_id,
                    z_index: own_z,
                }];
                let pos = scene.items.iter().position(|i| i.id == *item_id)?;
                for neighbor in [pos.wrapping_sub(1), pos + 1] {
                    if let Some(other) = scene.items.get(neighbor) {
                        if other.id != *item_id {
                            restores.push(Command::SetSceneItemZIndex {
                                scene_id: *scene_id,
                                item_id: other.id,
                                z_index: other.z_index,
                            });
                        }
                    }
                }
                Some(Command::Transaction { commands: restores })
            }
            Command::SetSceneItemOpacity {
                scene_id, item_id, ..
            } => {
                let item = self.scene(*scene_id)?.item(*item_id)?;
                Some(Command::SetSceneItemOpacity {
                    scene_id: *scene_id,
                    item_id: *item_id,
                    opacity: item.opacity,
                })
            }
            Command::SetSceneItemBounds {
                scene_id, item_id, ..
            } => {
                let item = self.scene(*scene_id)?.item(*item_id)?;
                Some(Command::SetSceneItemBounds {
                    scene_id: *scene_id,
                    item_id: *item_id,
                    bounds: item.bounds,
                })
            }
            Command::RenameSource { source_id, .. } => Some(Command::RenameSource {
                source_id: *source_id,
                name: self.source(*source_id)?.name.clone(),
            }),
            Command::SetSourceSettings { source_id, .. } => Some(Command::SetSourceSettings {
                source_id: *source_id,
                settings: self.source(*source_id)?.settings.clone(),
            }),
            Command::SetSourceEnabled { source_id, .. } => Some(Command::SetSourceEnabled {
                source_id: *source_id,
                enabled: self.source(*source_id)?.enabled,
            }),
            Command::SetSourceVolume { source_id, .. } => Some(Command::SetSourceVolume {
                source_id: *source_id,
                volume_db: self.audio.mixer_state(*source_id).volume_db,
            }),
            Command::SetSourceMuted { source_id, .. } => Some(Command::SetSourceMuted {
                source_id: *source_id,
                muted: self.audio.mixer_state(*source_id).muted,
            }),
            Command::SetSourceSolo { source_id, .. } => Some(Command::SetSourceSolo {
                source_id: *source_id,
                solo: self.audio.mixer_state(*source_id).solo,
            }),
            Command::SetSourceMonitor { source_id, .. } => Some(Command::SetSourceMonitor {
                source_id: *source_id,
                monitor: self.audio.mixer_state(*source_id).monitor,
            }),
            Command::SetSourceBalance { source_id, .. } => Some(Command::SetSourceBalance {
                source_id: *source_id,
                balance: self.audio.mixer_state(*source_id).balance,
            }),
            Command::SetSourceSyncOffset { source_id, .. } => Some(Command::SetSourceSyncOffset {
                source_id: *source_id,
                sync_offset_ms: self.audio.mixer_state(*source_id).sync_offset_ms,
            }),
            Command::SetAudioRoute {
                source_id, bus_id, ..
            } => {
                match self
                    .audio
                    .routes
                    .iter()
                    .find(|r| r.source_id == *source_id && r.bus_id == *bus_id)
                {
                    Some(route) => Some(Command::SetAudioRoute {
                        source_id: *source_id,
                        bus_id: *bus_id,
                        tracks: route.tracks,
                    }),
                    None => Some(Command::RemoveAudioRoute {
                        source_id: *source_id,
                        bus_id: *bus_id,
                    }),
                }
            }
            Command::RemoveAudioRoute { source_id, bus_id } => {
                let route = self
                    .audio
                    .routes
                    .iter()
                    .find(|r| r.source_id == *source_id && r.bus_id == *bus_id)?;
                Some(Command::SetAudioRoute {
                    source_id: *source_id,
                    bus_id: *bus_id,
                    tracks: route.tracks,
                })
            }
            Command::StartOutput { output_id } => {
                self.output(*output_id)?;
                Some(Command::StopOutput {
                    output_id: *output_id,
                })
            }
            Command::StopOutput { output_id } => {
                self.output(*output_id)?;
                Some(Command::StartOutput {
                    output_id: *output_id,
                })
            }
            Command::SetOutputReconnectPolicy { output_id, .. } => {
                Some(Command::SetOutputReconnectPolicy {
                    output_id: *output_id,
                    policy: self.output(*output_id)?.reconnect_policy,
                })
            }
            Command::SetStudioModeEnabled { .. } => Some(Command::SetStudioModeEnabled {
                enabled: self.studio_mode.is_none(),
            }),
            Command::SetPreviewScene { .. } => Some(Command::SetPreviewScene {
                scene_id: self.studio_mode.as_ref()?.preview,
            }),
            Command::TransitionToProgram | Command::SwapPreviewProgram => {
                self.studio_mode.as_ref()?;
                Some(Command::SwapPreviewProgram)
            }
            Command::SetTransition { .. } => Some(Command::SetTransition {
                transition: self.transition.clone(),
            }),
            Command::SelectProfile { .. } => Some(Command::SelectProfile {
                profile_id: self.active_profile?,
            }),
            Command::SelectSceneCollection { .. } => Some(Command::SelectSceneCollection {
                collection_id: self.active_collection?,
            }),
            Command::Transaction { commands } => {
                let mut inverses = Vec::with_capacity(commands.len());
                // Inverses must be computed against successive pre-states, so
                // replay the group on a scratch copy.
                let mut scratch = self.clone();
                for cmd in commands {
                    inverses.push(scratch.inverse(cmd)?);
                    scratch.apply(cmd).ok()?;
                }
                inverses.reverse();
                Some(Command::Transaction { commands: inverses })
            }
            // Authorization is an external effect, never an undo/replay operation.
            Command::AuthorizeSourceCapture { .. } => None,
            // Irreversible: creations, destructions, duplicates.
            Command::AddScene { .. }
            | Command::RemoveScene { .. }
            | Command::AddSceneItem { .. }
            | Command::RemoveSceneItem { .. }
            | Command::DuplicateSceneItem { .. }
            | Command::AddSource { .. }
            | Command::RemoveSource { .. }
            | Command::AddAudioBus { .. }
            | Command::RemoveAudioBus { .. }
            | Command::AddOutput { .. }
            | Command::RemoveOutput { .. }
            | Command::AddProfile { .. }
            | Command::RemoveProfile { .. }
            | Command::AddSceneCollection { .. }
            | Command::RemoveSceneCollection { .. } => None,
        }
    }
}

/// Applies a command to the state: validates, mutates, returns events.
///
/// Free-function form of [`AppState::apply`] matching the ARCH-002 signature
/// `(state, command) -> (events)`.
pub fn apply(state: &mut AppState, command: &Command) -> Result<Vec<Event>> {
    match command {
        Command::AuthorizeSourceCapture { source_id } => {
            let source = state
                .source(*source_id)
                .ok_or_else(|| Error::NotFound(format!("source {source_id}")))?;
            if !source.enabled
                || !matches!(
                    source.kind,
                    SourceKind::PipeWireDisplay
                        | SourceKind::PipeWireWindow
                        | SourceKind::V4l2Camera
                )
            {
                return Err(Error::InvalidInput(
                    "capture authorization requires an enabled capture source".into(),
                ));
            }
            Ok(vec![Event::Source(
                SourceEvent::CaptureAuthorizationRequested {
                    source_id: *source_id,
                },
            )])
        }
        Command::Transaction { commands } => {
            if contains_capture_authorization(command) {
                return Err(Error::InvalidInput(
                    "capture authorization cannot be inside a transaction".into(),
                ));
            }
            // Atomic: apply to a scratch copy; only commit on full success.
            let mut scratch = state.clone();
            let mut events = Vec::new();
            for cmd in commands {
                events.extend(apply(&mut scratch, cmd)?);
            }
            *state = scratch;
            Ok(events)
        }
        _ => apply_one(state, command),
    }
}

/// External authorization is never replayed through atomic transactions.
pub fn contains_capture_authorization(command: &Command) -> bool {
    match command {
        Command::AuthorizeSourceCapture { .. } => true,
        Command::Transaction { commands } => commands.iter().any(contains_capture_authorization),
        _ => false,
    }
}

fn apply_one(state: &mut AppState, command: &Command) -> Result<Vec<Event>> {
    match command {
        Command::AuthorizeSourceCapture { .. } => Err(Error::InvalidInput(
            "capture authorization requires application dispatch".into(),
        )),
        // Transactions are handled by the public `apply`; nested transactions
        // are flattened by treating them as a sequential group here.
        Command::Transaction { commands } => {
            let mut events = Vec::new();
            for cmd in commands {
                events.extend(apply(state, cmd)?);
            }
            Ok(events)
        }

        // --- Scenes ---
        Command::AddScene { name } => {
            require_non_empty(name, "scene name")?;
            let name = unique_name(name, state.scenes.values().map(|s| s.name.as_str()));
            let scene = Scene::new(name.clone());
            let scene_id = scene.id;
            state.scenes.insert(scene_id, scene);
            let mut events = vec![Event::Scene(SceneEvent::Added { scene_id, name })];
            if state.current_scene.is_none() {
                state.current_scene = Some(scene_id);
                events.push(Event::Scene(SceneEvent::CurrentChanged { scene_id }));
            }
            Ok(events)
        }
        Command::RemoveScene { scene_id } => {
            let scene_id = *scene_id;
            if !state.scenes.contains_key(&scene_id) {
                return Err(not_found("scene", scene_id));
            }
            if state.scenes.len() == 1 {
                return Err(Error::InvalidInput(
                    "cannot remove the last scene".to_string(),
                ));
            }
            if let Some(studio) = &state.studio_mode {
                if studio.program == scene_id || studio.preview == scene_id {
                    return Err(Error::InvalidInput(
                        "scene is used by studio mode".to_string(),
                    ));
                }
            }
            if state
                .sources
                .values()
                .any(|s| s.scene_reference() == Some(scene_id))
            {
                return Err(Error::InvalidInput(
                    "scene is referenced by a scene source".to_string(),
                ));
            }
            state.scenes.shift_remove(&scene_id);
            let mut events = vec![Event::Scene(SceneEvent::Removed { scene_id })];
            if state.current_scene == Some(scene_id) {
                let next = state.scenes.keys().next().copied();
                state.current_scene = next;
                if let Some(next) = next {
                    events.push(Event::Scene(SceneEvent::CurrentChanged { scene_id: next }));
                }
            }
            Ok(events)
        }
        Command::RenameScene { scene_id, name } => {
            require_non_empty(name, "scene name")?;
            let scene_id = *scene_id;
            let scene = state
                .scenes
                .get(&scene_id)
                .ok_or_else(|| not_found("scene", scene_id))?;
            if scene.name == *name {
                return Ok(Vec::new());
            }
            let name = unique_name(
                name,
                state
                    .scenes
                    .values()
                    .filter(|s| s.id != scene_id)
                    .map(|s| s.name.as_str()),
            );
            let scene = state
                .scenes
                .get_mut(&scene_id)
                .ok_or_else(|| not_found("scene", scene_id))?;
            scene.name = name.clone();
            Ok(vec![Event::Scene(SceneEvent::Renamed { scene_id, name })])
        }
        Command::ReorderScene {
            scene_id,
            new_index,
        } => {
            let scene_id = *scene_id;
            let old_index = state
                .scenes
                .get_index_of(&scene_id)
                .ok_or_else(|| not_found("scene", scene_id))?;
            let new_index = (*new_index).min(state.scenes.len().saturating_sub(1));
            if old_index == new_index {
                return Ok(Vec::new());
            }
            state.scenes.move_index(old_index, new_index);
            Ok(vec![Event::Scene(SceneEvent::Reordered)])
        }
        Command::SetCurrentScene { scene_id } => {
            let scene_id = *scene_id;
            if !state.scenes.contains_key(&scene_id) {
                return Err(not_found("scene", scene_id));
            }
            if state.current_scene == Some(scene_id) {
                return Ok(Vec::new());
            }
            state.current_scene = Some(scene_id);
            Ok(vec![Event::Scene(SceneEvent::CurrentChanged { scene_id })])
        }

        // --- Scene items ---
        Command::AddSceneItem {
            scene_id,
            source_id,
        } => {
            let scene_id = *scene_id;
            let source_id = *source_id;
            if !state.sources.contains_key(&source_id) {
                return Err(not_found("source", source_id));
            }
            let scene = state
                .scenes
                .get_mut(&scene_id)
                .ok_or_else(|| not_found("scene", scene_id))?;
            let z_index = scene
                .items
                .iter()
                .map(|i| i.z_index)
                .max()
                .map_or(0, |z| z + 1);
            let item = SceneItem::new(source_id, z_index);
            scene.add_item(item.clone());
            Ok(vec![Event::Scene(SceneEvent::ItemAdded {
                scene_id,
                item: Box::new(item),
            })])
        }
        Command::RemoveSceneItem { scene_id, item_id } => {
            let scene = state
                .scenes
                .get_mut(scene_id)
                .ok_or_else(|| not_found("scene", *scene_id))?;
            let item = scene
                .remove_item(*item_id)
                .ok_or_else(|| not_found("scene item", *item_id))?;
            Ok(vec![Event::Scene(SceneEvent::ItemRemoved {
                scene_id: *scene_id,
                item_id: *item_id,
                source_id: item.source_id,
            })])
        }
        Command::DuplicateSceneItem { scene_id, item_id } => {
            let scene = state
                .scenes
                .get_mut(scene_id)
                .ok_or_else(|| not_found("scene", *scene_id))?;
            let original = scene
                .item(*item_id)
                .ok_or_else(|| not_found("scene item", *item_id))?
                .clone();
            let z_index = scene
                .items
                .iter()
                .map(|i| i.z_index)
                .max()
                .map_or(0, |z| z + 1);
            let mut copy = original;
            copy.id = SceneItemId::new();
            copy.z_index = z_index;
            scene.add_item(copy.clone());
            Ok(vec![Event::Scene(SceneEvent::ItemAdded {
                scene_id: *scene_id,
                item: Box::new(copy),
            })])
        }
        Command::SetSceneItemTransform {
            scene_id,
            item_id,
            transform,
        } => {
            require_finite(
                &[
                    transform.position.x,
                    transform.position.y,
                    transform.scale.x,
                    transform.scale.y,
                    transform.rotation,
                ],
                "transform",
            )?;
            let scene = state
                .scenes
                .get_mut(scene_id)
                .ok_or_else(|| not_found("scene", *scene_id))?;
            let item = editable_item(scene, *item_id)?;
            if item.transform == *transform {
                return Ok(Vec::new());
            }
            item.transform = *transform;
            Ok(vec![item_updated(*scene_id, item)])
        }
        Command::SetSceneItemCrop {
            scene_id,
            item_id,
            crop,
        } => {
            let scene = state
                .scenes
                .get_mut(scene_id)
                .ok_or_else(|| not_found("scene", *scene_id))?;
            let item = editable_item(scene, *item_id)?;
            if item.crop == *crop {
                return Ok(Vec::new());
            }
            item.crop = *crop;
            Ok(vec![item_updated(*scene_id, item)])
        }
        Command::SetSceneItemVisible {
            scene_id,
            item_id,
            visible,
        } => {
            // Visibility is allowed on locked items (OBS behavior: the eye
            // icon stays usable when an item is locked).
            let scene = state
                .scenes
                .get_mut(scene_id)
                .ok_or_else(|| not_found("scene", *scene_id))?;
            let item = scene
                .item_mut(*item_id)
                .ok_or_else(|| not_found("scene item", *item_id))?;
            if item.visible == *visible {
                return Ok(Vec::new());
            }
            item.visible = *visible;
            Ok(vec![item_updated(*scene_id, item)])
        }
        Command::SetSceneItemLocked {
            scene_id,
            item_id,
            locked,
        } => {
            let scene = state
                .scenes
                .get_mut(scene_id)
                .ok_or_else(|| not_found("scene", *scene_id))?;
            let item = scene
                .item_mut(*item_id)
                .ok_or_else(|| not_found("scene item", *item_id))?;
            if item.locked == *locked {
                return Ok(Vec::new());
            }
            item.locked = *locked;
            Ok(vec![item_updated(*scene_id, item)])
        }
        Command::SetSceneItemZIndex {
            scene_id,
            item_id,
            z_index,
        } => {
            let scene_id = *scene_id;
            let scene = state
                .scenes
                .get_mut(&scene_id)
                .ok_or_else(|| not_found("scene", scene_id))?;
            if scene
                .item(*item_id)
                .ok_or_else(|| not_found("scene item", *item_id))?
                .locked
            {
                return Err(locked_err());
            }
            if scene.item(*item_id).map(|i| i.z_index) == Some(*z_index) {
                return Ok(Vec::new());
            }
            scene.set_z_index(*item_id, *z_index);
            let item = scene
                .item(*item_id)
                .ok_or_else(|| not_found("scene item", *item_id))?;
            Ok(vec![item_updated(scene_id, item)])
        }
        Command::RaiseSceneItem { scene_id, item_id } => {
            let scene = state
                .scenes
                .get_mut(scene_id)
                .ok_or_else(|| not_found("scene", *scene_id))?;
            if scene
                .item(*item_id)
                .ok_or_else(|| not_found("scene item", *item_id))?
                .locked
            {
                return Err(locked_err());
            }
            match scene.raise_item(*item_id) {
                // Already on top: a successful no-op.
                None => Ok(Vec::new()),
                Some(_) => {
                    let item = scene
                        .item(*item_id)
                        .ok_or_else(|| not_found("scene item", *item_id))?;
                    Ok(vec![item_updated(*scene_id, item)])
                }
            }
        }
        Command::LowerSceneItem { scene_id, item_id } => {
            let scene = state
                .scenes
                .get_mut(scene_id)
                .ok_or_else(|| not_found("scene", *scene_id))?;
            if scene
                .item(*item_id)
                .ok_or_else(|| not_found("scene item", *item_id))?
                .locked
            {
                return Err(locked_err());
            }
            match scene.lower_item(*item_id) {
                None => Ok(Vec::new()),
                Some(_) => {
                    let item = scene
                        .item(*item_id)
                        .ok_or_else(|| not_found("scene item", *item_id))?;
                    Ok(vec![item_updated(*scene_id, item)])
                }
            }
        }
        Command::SetSceneItemOpacity {
            scene_id,
            item_id,
            opacity,
        } => {
            require_finite(&[*opacity], "opacity")?;
            if !(0.0..=1.0).contains(opacity) {
                return Err(Error::InvalidInput(
                    "opacity must be within [0.0, 1.0]".to_string(),
                ));
            }
            let scene = state
                .scenes
                .get_mut(scene_id)
                .ok_or_else(|| not_found("scene", *scene_id))?;
            let item = editable_item(scene, *item_id)?;
            if item.opacity == *opacity {
                return Ok(Vec::new());
            }
            item.opacity = *opacity;
            Ok(vec![item_updated(*scene_id, item)])
        }
        Command::SetSceneItemBounds {
            scene_id,
            item_id,
            bounds,
        } => {
            require_finite(&[bounds.size.x, bounds.size.y], "bounds")?;
            let scene = state
                .scenes
                .get_mut(scene_id)
                .ok_or_else(|| not_found("scene", *scene_id))?;
            let item = editable_item(scene, *item_id)?;
            if item.bounds == *bounds {
                return Ok(Vec::new());
            }
            item.bounds = *bounds;
            Ok(vec![item_updated(*scene_id, item)])
        }

        // --- Sources ---
        Command::AddSource { kind, name } => {
            require_non_empty(name, "source name")?;
            if let SourceKind::Scene(scene_id) = kind {
                if !state.scenes.contains_key(scene_id) {
                    return Err(not_found("scene", *scene_id));
                }
            }
            let name = unique_name(name, state.sources.values().map(|s| s.name.as_str()));
            let source = Source::new(*kind, name);
            let source_id = source.id;
            state.sources.insert(source_id, source.clone());
            Ok(vec![Event::Source(SourceEvent::Added {
                source: Box::new(source),
            })])
        }
        Command::RemoveSource { source_id } => {
            let source_id = *source_id;
            if !state.sources.contains_key(&source_id) {
                return Err(not_found("source", source_id));
            }
            if state
                .scenes
                .values()
                .any(|s| s.items.iter().any(|i| i.source_id == source_id))
            {
                return Err(Error::InvalidInput(
                    "source is referenced by scene items; remove the items first".to_string(),
                ));
            }
            if state.audio.routes.iter().any(|r| r.source_id == source_id) {
                return Err(Error::InvalidInput(
                    "source has audio routes; remove the routes first".to_string(),
                ));
            }
            state.sources.shift_remove(&source_id);
            let mut events = Vec::new();
            // Cascade: drop the orphaned mixer entry, reporting the reset.
            if state.audio.mixer.shift_remove(&source_id).is_some() {
                events.push(Event::Audio(AudioEvent::MixerChanged {
                    source_id,
                    state: AudioMixerState::default(),
                }));
            }
            events.push(Event::Source(SourceEvent::Removed { source_id }));
            Ok(events)
        }
        Command::RenameSource { source_id, name } => {
            require_non_empty(name, "source name")?;
            let source_id = *source_id;
            let source = state
                .sources
                .get(&source_id)
                .ok_or_else(|| not_found("source", source_id))?;
            if source.name == *name {
                return Ok(Vec::new());
            }
            let name = unique_name(
                name,
                state
                    .sources
                    .values()
                    .filter(|s| s.id != source_id)
                    .map(|s| s.name.as_str()),
            );
            let source = state
                .sources
                .get_mut(&source_id)
                .ok_or_else(|| not_found("source", source_id))?;
            source.name = name.clone();
            Ok(vec![Event::Source(SourceEvent::Renamed {
                source_id,
                name,
            })])
        }
        Command::SetSourceSettings {
            source_id,
            settings,
        } => {
            let source = state
                .sources
                .get_mut(source_id)
                .ok_or_else(|| not_found("source", *source_id))?;
            if source.settings == *settings {
                return Ok(Vec::new());
            }
            source.settings = settings.clone();
            Ok(vec![Event::Source(SourceEvent::SettingsChanged {
                source_id: *source_id,
            })])
        }
        Command::SetSourceEnabled { source_id, enabled } => {
            let source = state
                .sources
                .get_mut(source_id)
                .ok_or_else(|| not_found("source", *source_id))?;
            if source.enabled == *enabled {
                return Ok(Vec::new());
            }
            source.enabled = *enabled;
            Ok(vec![Event::Source(SourceEvent::EnabledChanged {
                source_id: *source_id,
                enabled: *enabled,
            })])
        }

        // --- Audio ---
        Command::SetSourceVolume {
            source_id,
            volume_db,
        } => {
            require_finite(&[*volume_db], "volume_db")?;
            update_mixer(
                state,
                *source_id,
                |m| m.volume_db = *volume_db,
                |m| m.volume_db == *volume_db,
            )
        }
        Command::SetSourceMuted { source_id, muted } => update_mixer(
            state,
            *source_id,
            |m| m.muted = *muted,
            |m| m.muted == *muted,
        ),
        Command::SetSourceSolo { source_id, solo } => {
            update_mixer(state, *source_id, |m| m.solo = *solo, |m| m.solo == *solo)
        }
        Command::SetSourceMonitor { source_id, monitor } => update_mixer(
            state,
            *source_id,
            |m| m.monitor = *monitor,
            |m| m.monitor == *monitor,
        ),
        Command::SetSourceBalance { source_id, balance } => {
            require_finite(&[*balance], "balance")?;
            if !(-1.0..=1.0).contains(balance) {
                return Err(Error::InvalidInput(
                    "balance must be within [-1.0, 1.0]".to_string(),
                ));
            }
            update_mixer(
                state,
                *source_id,
                |m| m.balance = *balance,
                |m| m.balance == *balance,
            )
        }
        Command::SetSourceSyncOffset {
            source_id,
            sync_offset_ms,
        } => update_mixer(
            state,
            *source_id,
            |m| m.sync_offset_ms = *sync_offset_ms,
            |m| m.sync_offset_ms == *sync_offset_ms,
        ),
        Command::AddAudioBus { name } => {
            require_non_empty(name, "audio bus name")?;
            let name = unique_name(name, state.audio.buses.iter().map(|b| b.name.as_str()));
            let bus = crate::audio::AudioBus::new(name.clone());
            let bus_id = bus.id;
            state.audio.buses.push(bus);
            Ok(vec![Event::Audio(AudioEvent::BusAdded { bus_id, name })])
        }
        Command::RemoveAudioBus { bus_id } => {
            let bus_id = *bus_id;
            if !state.audio.buses.iter().any(|b| b.id == bus_id) {
                return Err(not_found("audio bus", bus_id));
            }
            if state.audio.buses.len() == 1 {
                return Err(Error::InvalidInput(
                    "cannot remove the last audio bus".to_string(),
                ));
            }
            // Cascade: routes into this bus are removed with explicit events.
            let removed_routes: Vec<AudioRoute> = state
                .audio
                .routes
                .iter()
                .filter(|r| r.bus_id == bus_id)
                .cloned()
                .collect();
            state.audio.routes.retain(|r| r.bus_id != bus_id);
            state.audio.buses.retain(|b| b.id != bus_id);
            let mut events: Vec<Event> = removed_routes
                .into_iter()
                .map(|r| {
                    Event::Audio(AudioEvent::RouteRemoved {
                        source_id: r.source_id,
                        bus_id,
                    })
                })
                .collect();
            events.push(Event::Audio(AudioEvent::BusRemoved { bus_id }));
            Ok(events)
        }
        Command::SetAudioRoute {
            source_id,
            bus_id,
            tracks,
        } => {
            let source_id = *source_id;
            let bus_id = *bus_id;
            if !state.sources.contains_key(&source_id) {
                return Err(not_found("source", source_id));
            }
            if !state.audio.buses.iter().any(|b| b.id == bus_id) {
                return Err(not_found("audio bus", bus_id));
            }
            if let Some(existing) = state
                .audio
                .routes
                .iter_mut()
                .find(|r| r.source_id == source_id && r.bus_id == bus_id)
            {
                if existing.tracks == *tracks {
                    return Ok(Vec::new());
                }
                existing.tracks = *tracks;
            } else {
                state.audio.routes.push(AudioRoute {
                    source_id,
                    bus_id,
                    tracks: *tracks,
                });
            }
            Ok(vec![Event::Audio(AudioEvent::RouteChanged {
                source_id,
                bus_id,
                tracks: *tracks,
            })])
        }
        Command::RemoveAudioRoute { source_id, bus_id } => {
            let pos = state
                .audio
                .routes
                .iter()
                .position(|r| r.source_id == *source_id && r.bus_id == *bus_id)
                .ok_or_else(|| {
                    Error::NotFound(format!("audio route {} -> {}", source_id, bus_id))
                })?;
            state.audio.routes.remove(pos);
            Ok(vec![Event::Audio(AudioEvent::RouteRemoved {
                source_id: *source_id,
                bus_id: *bus_id,
            })])
        }

        // --- Outputs ---
        Command::AddOutput { output } => {
            if state.outputs.contains_key(&output.id) {
                return Err(Error::InvalidInput(format!(
                    "duplicate output id: {}",
                    output.id
                )));
            }
            let output_id = output.id;
            let name = output.name.clone();
            state.outputs.insert(output_id, output.clone());
            Ok(vec![Event::Output(OutputEvent::Added { output_id, name })])
        }
        Command::RemoveOutput { output_id } => {
            let output_id = *output_id;
            let output = state
                .outputs
                .get(&output_id)
                .ok_or_else(|| not_found("output", output_id))?;
            if output.state != OutputState::Stopped {
                return Err(Error::InvalidInput(
                    "output must be stopped before removal".to_string(),
                ));
            }
            state.outputs.shift_remove(&output_id);
            Ok(vec![Event::Output(OutputEvent::Removed { output_id })])
        }
        Command::StartOutput { output_id } => {
            let output_id = *output_id;
            let output = state
                .outputs
                .get_mut(&output_id)
                .ok_or_else(|| not_found("output", output_id))?;
            match output.state {
                OutputState::Stopped | OutputState::Failed => {
                    output.state = OutputState::Starting;
                    Ok(vec![Event::Output(OutputEvent::StateChanged {
                        output_id,
                        state: OutputState::Starting,
                    })])
                }
                state => Err(Error::InvalidInput(format!(
                    "cannot start output in state {state:?}"
                ))),
            }
        }
        Command::StopOutput { output_id } => {
            let output_id = *output_id;
            let output = state
                .outputs
                .get_mut(&output_id)
                .ok_or_else(|| not_found("output", output_id))?;
            match output.state {
                OutputState::Starting
                | OutputState::Running
                | OutputState::Degraded
                | OutputState::Reconnecting { .. } => {
                    output.state = OutputState::Stopping;
                    Ok(vec![Event::Output(OutputEvent::StateChanged {
                        output_id,
                        state: OutputState::Stopping,
                    })])
                }
                state => Err(Error::InvalidInput(format!(
                    "cannot stop output in state {state:?}"
                ))),
            }
        }
        Command::SetOutputReconnectPolicy { output_id, policy } => {
            let output = state
                .outputs
                .get_mut(output_id)
                .ok_or_else(|| not_found("output", *output_id))?;
            if output.reconnect_policy == *policy {
                return Ok(Vec::new());
            }
            output.reconnect_policy = *policy;
            Ok(vec![Event::Output(OutputEvent::ReconnectPolicyChanged {
                output_id: *output_id,
            })])
        }

        // --- Studio mode ---
        Command::SetStudioModeEnabled { enabled } => {
            let currently = state.studio_mode.is_some();
            if *enabled == currently {
                return Ok(Vec::new());
            }
            if !*enabled {
                state.studio_mode = None;
                return Ok(vec![Event::System(SystemEvent::StudioModeChanged {
                    enabled: false,
                })]);
            }
            if state.scenes.len() < 2 {
                return Err(Error::InvalidInput(
                    "studio mode requires at least two scenes".to_string(),
                ));
            }
            let program = state
                .current_scene
                .ok_or_else(|| Error::InvalidInput("no current scene".to_string()))?;
            let preview = state
                .scenes
                .keys()
                .find(|id| **id != program)
                .copied()
                .ok_or_else(|| Error::InvalidInput("no second scene for preview".to_string()))?;
            state.studio_mode = Some(StudioMode {
                enabled: true,
                program,
                preview,
            });
            Ok(vec![
                Event::System(SystemEvent::StudioModeChanged { enabled: true }),
                Event::System(SystemEvent::PreviewSceneChanged { scene_id: preview }),
            ])
        }
        Command::SetPreviewScene { scene_id } => {
            let scene_id = *scene_id;
            if !state.scenes.contains_key(&scene_id) {
                return Err(not_found("scene", scene_id));
            }
            let studio = state
                .studio_mode
                .as_mut()
                .ok_or_else(|| Error::InvalidInput("studio mode is disabled".to_string()))?;
            if studio.program == scene_id {
                return Err(Error::InvalidInput(
                    "preview scene must differ from the program scene".to_string(),
                ));
            }
            if studio.preview == scene_id {
                return Ok(Vec::new());
            }
            studio.preview = scene_id;
            Ok(vec![Event::System(SystemEvent::PreviewSceneChanged {
                scene_id,
            })])
        }
        Command::TransitionToProgram => {
            let transition = state.transition.clone();
            let studio = state
                .studio_mode
                .as_mut()
                .ok_or_else(|| Error::InvalidInput("studio mode is disabled".to_string()))?;
            let new_program = studio.preview;
            let new_preview = studio.program;
            studio.program = new_program;
            studio.preview = new_preview;
            state.current_scene = Some(new_program);
            let mut events = Vec::new();
            if transition.kind != crate::transition::TransitionKind::Cut {
                events.push(Event::System(SystemEvent::TransitionStarted {
                    kind: transition.kind,
                    duration_ms: transition.duration_ms,
                }));
            }
            events.push(Event::Scene(SceneEvent::CurrentChanged {
                scene_id: new_program,
            }));
            events.push(Event::System(SystemEvent::PreviewSceneChanged {
                scene_id: new_preview,
            }));
            Ok(events)
        }
        Command::SwapPreviewProgram => {
            let studio = state
                .studio_mode
                .as_mut()
                .ok_or_else(|| Error::InvalidInput("studio mode is disabled".to_string()))?;
            std::mem::swap(&mut studio.program, &mut studio.preview);
            let program = studio.program;
            let preview = studio.preview;
            state.current_scene = Some(program);
            Ok(vec![
                Event::Scene(SceneEvent::CurrentChanged { scene_id: program }),
                Event::System(SystemEvent::PreviewSceneChanged { scene_id: preview }),
            ])
        }

        // --- Transitions ---
        Command::SetTransition { transition } => {
            if state.transition == *transition {
                return Ok(Vec::new());
            }
            state.transition = transition.clone();
            Ok(vec![Event::System(SystemEvent::TransitionChanged {
                transition: transition.clone(),
            })])
        }

        // --- Profiles & collections ---
        Command::AddProfile { profile } => {
            if state.profiles.contains_key(&profile.id) {
                return Err(Error::InvalidInput(format!(
                    "duplicate profile id: {}",
                    profile.id
                )));
            }
            let profile_id = profile.id;
            state.profiles.insert(profile_id, profile.clone());
            Ok(vec![Event::System(SystemEvent::ProfileAdded {
                profile_id,
            })])
        }
        Command::RemoveProfile { profile_id } => {
            let profile_id = *profile_id;
            if !state.profiles.contains_key(&profile_id) {
                return Err(not_found("profile", profile_id));
            }
            if state.active_profile == Some(profile_id) {
                return Err(Error::InvalidInput(
                    "cannot remove the active profile".to_string(),
                ));
            }
            state.profiles.shift_remove(&profile_id);
            Ok(vec![Event::System(SystemEvent::ProfileRemoved {
                profile_id,
            })])
        }
        Command::SelectProfile { profile_id } => {
            let profile_id = *profile_id;
            if !state.profiles.contains_key(&profile_id) {
                return Err(not_found("profile", profile_id));
            }
            if state.active_profile == Some(profile_id) {
                return Ok(Vec::new());
            }
            state.active_profile = Some(profile_id);
            Ok(vec![Event::System(SystemEvent::ProfileSelected {
                profile_id,
            })])
        }
        Command::AddSceneCollection { collection } => {
            if state.collections.contains_key(&collection.id) {
                return Err(Error::InvalidInput(format!(
                    "duplicate collection id: {}",
                    collection.id
                )));
            }
            let collection_id = collection.id;
            state.collections.insert(collection_id, collection.clone());
            Ok(vec![Event::System(SystemEvent::CollectionAdded {
                collection_id,
            })])
        }
        Command::RemoveSceneCollection { collection_id } => {
            let collection_id = *collection_id;
            if !state.collections.contains_key(&collection_id) {
                return Err(not_found("scene collection", collection_id));
            }
            if state.active_collection == Some(collection_id) {
                return Err(Error::InvalidInput(
                    "cannot remove the active scene collection".to_string(),
                ));
            }
            state.collections.shift_remove(&collection_id);
            Ok(vec![Event::System(SystemEvent::CollectionRemoved {
                collection_id,
            })])
        }
        Command::SelectSceneCollection { collection_id } => {
            let collection_id = *collection_id;
            if !state.collections.contains_key(&collection_id) {
                return Err(not_found("scene collection", collection_id));
            }
            if state.active_collection == Some(collection_id) {
                return Ok(Vec::new());
            }
            state.active_collection = Some(collection_id);
            Ok(vec![Event::System(SystemEvent::CollectionSelected {
                collection_id,
            })])
        }
    }
}

// --- helpers ---

fn not_found(entity: &str, id: impl std::fmt::Display) -> Error {
    Error::NotFound(format!("{entity} {id}"))
}

fn locked_err() -> Error {
    Error::InvalidInput("scene item is locked".to_string())
}

fn editable_item(scene: &mut Scene, item_id: SceneItemId) -> Result<&mut SceneItem> {
    let item = scene
        .item_mut(item_id)
        .ok_or_else(|| not_found("scene item", item_id))?;
    if item.locked {
        return Err(locked_err());
    }
    Ok(item)
}

fn item_updated(scene_id: SceneId, item: &SceneItem) -> Event {
    Event::Scene(SceneEvent::ItemUpdated {
        scene_id,
        item: Box::new(item.clone()),
    })
}

fn require_non_empty(value: &str, what: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(Error::InvalidInput(format!("{what} must not be empty")));
    }
    Ok(())
}

fn require_finite(values: &[f32], what: &str) -> Result<()> {
    if values.iter().all(|v| v.is_finite()) {
        return Ok(());
    }
    Err(Error::InvalidInput(format!("{what} must be finite")))
}

fn unique_name<'a>(base: &str, existing: impl Iterator<Item = &'a str>) -> String {
    let taken: Vec<&str> = existing.collect();
    if !taken.contains(&base) {
        return base.to_string();
    }
    let mut n = 2u32;
    loop {
        let candidate = format!("{base} ({n})");
        if !taken.contains(&candidate.as_str()) {
            return candidate;
        }
        n += 1;
    }
}

fn update_mixer(
    state: &mut AppState,
    source_id: SourceId,
    mutate: impl Fn(&mut AudioMixerState),
    is_noop: impl Fn(&AudioMixerState) -> bool,
) -> Result<Vec<Event>> {
    if !state.sources.contains_key(&source_id) {
        return Err(not_found("source", source_id));
    }
    let current = state
        .audio
        .mixer
        .get(&source_id)
        .cloned()
        .unwrap_or_default();
    if is_noop(&current) {
        // Keep the map clean: an all-default entry carries no information.
        if current == AudioMixerState::default() {
            state.audio.mixer.shift_remove(&source_id);
        }
        return Ok(Vec::new());
    }
    let mut updated = current;
    mutate(&mut updated);
    // Keep the map clean: an all-default entry carries no information.
    if updated == AudioMixerState::default() {
        state.audio.mixer.shift_remove(&source_id);
    } else {
        state.audio.mixer.insert(source_id, updated.clone());
    }
    Ok(vec![Event::Audio(AudioEvent::MixerChanged {
        source_id,
        state: updated,
    })])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::{MonitorMode, TrackMask};
    use crate::command::Command;
    use crate::id::AudioBusId;
    use crate::output::{EncoderSettings, OutputKind, ReconnectPolicy};
    use crate::scene::{Crop, Transform, Vec2};
    use crate::transition::TransitionKind;

    // --- helpers ---

    fn add_scene(state: &mut AppState, name: &str) -> SceneId {
        let events = state
            .apply(&Command::AddScene { name: name.into() })
            .unwrap();
        match &events[0] {
            Event::Scene(SceneEvent::Added { scene_id, .. }) => *scene_id,
            other => panic!("expected SceneEvent::Added, got {other:?}"),
        }
    }

    fn add_source(state: &mut AppState, kind: SourceKind, name: &str) -> SourceId {
        let events = state
            .apply(&Command::AddSource {
                kind,
                name: name.into(),
            })
            .unwrap();
        match &events[0] {
            Event::Source(SourceEvent::Added { source }) => source.id,
            other => panic!("expected SourceEvent::Added, got {other:?}"),
        }
    }

    fn add_item(state: &mut AppState, scene_id: SceneId, source_id: SourceId) -> SceneItemId {
        let events = state
            .apply(&Command::AddSceneItem {
                scene_id,
                source_id,
            })
            .unwrap();
        match &events[0] {
            Event::Scene(SceneEvent::ItemAdded { item, .. }) => item.id,
            other => panic!("expected SceneEvent::ItemAdded, got {other:?}"),
        }
    }

    fn add_output(state: &mut AppState, kind: OutputKind, name: &str) -> OutputId {
        let output = Output::new(kind, name, crate::id::EncoderId::new());
        let id = output.id;
        state.apply(&Command::AddOutput { output }).unwrap();
        id
    }

    /// PLAN.md §67 milestone domain side: two sources, a scene, compose them,
    /// move/resize/z-order, switch scenes, studio-mode preview→transition→
    /// program, add output, start/stop.
    #[test]
    fn milestone_scenario() {
        let mut state = AppState::new();

        // Create two sources.
        let cam = add_source(&mut state, SourceKind::V4l2Camera, "Camera");
        let pattern = add_source(&mut state, SourceKind::TestPattern, "Pattern");
        assert_eq!(state.sources.len(), 2);

        // Create a scene and compose both sources into it.
        let main = add_scene(&mut state, "Main");
        assert_eq!(state.current_scene, Some(main));
        let cam_item = add_item(&mut state, main, cam);
        let pattern_item = add_item(&mut state, main, pattern);
        assert_eq!(state.scene(main).unwrap().items.len(), 2);
        // New items land on top.
        assert_eq!(state.scene(main).unwrap().items[1].id, pattern_item);

        // Move and resize the camera item.
        let transform = Transform {
            position: Vec2::new(100.0, 50.0),
            scale: Vec2::new(0.5, 0.5),
            rotation: 0.0,
            ..Transform::default()
        };
        state
            .apply(&Command::SetSceneItemTransform {
                scene_id: main,
                item_id: cam_item,
                transform,
            })
            .unwrap();
        assert_eq!(
            state.scene(main).unwrap().item(cam_item).unwrap().transform,
            transform
        );

        // Z-order: raise the camera above the pattern.
        state
            .apply(&Command::RaiseSceneItem {
                scene_id: main,
                item_id: cam_item,
            })
            .unwrap();
        assert_eq!(state.scene(main).unwrap().items[1].id, cam_item);

        // Crop + visibility.
        state
            .apply(&Command::SetSceneItemCrop {
                scene_id: main,
                item_id: pattern_item,
                crop: Crop {
                    left: 10,
                    top: 0,
                    right: 0,
                    bottom: 0,
                },
            })
            .unwrap();
        state
            .apply(&Command::SetSceneItemVisible {
                scene_id: main,
                item_id: pattern_item,
                visible: false,
            })
            .unwrap();
        assert!(
            !state
                .scene(main)
                .unwrap()
                .item(pattern_item)
                .unwrap()
                .visible
        );

        // Switch scenes.
        let intermission = add_scene(&mut state, "Intermission");
        let events = state
            .apply(&Command::SetCurrentScene {
                scene_id: intermission,
            })
            .unwrap();
        assert_eq!(
            events,
            vec![Event::Scene(SceneEvent::CurrentChanged {
                scene_id: intermission
            })]
        );

        // Studio mode: preview → transition → program.
        state
            .apply(&Command::SetStudioModeEnabled { enabled: true })
            .unwrap();
        let studio = state.studio_mode.clone().unwrap();
        assert_eq!(studio.program, intermission);
        state
            .apply(&Command::SetPreviewScene { scene_id: main })
            .unwrap();
        let events = state.apply(&Command::TransitionToProgram).unwrap();
        // Default transition is Fade: TransitionStarted must be emitted.
        assert!(events.iter().any(|e| matches!(
            e,
            Event::System(SystemEvent::TransitionStarted {
                kind: TransitionKind::Fade,
                ..
            })
        )));
        let studio = state.studio_mode.clone().unwrap();
        assert_eq!(studio.program, main);
        assert_eq!(studio.preview, intermission);
        assert_eq!(state.current_scene, Some(main));

        // Cut transition suppresses TransitionStarted.
        state
            .apply(&Command::SetTransition {
                transition: crate::transition::Transition {
                    kind: TransitionKind::Cut,
                    duration_ms: 0,
                    settings: serde_json::Value::Null,
                },
            })
            .unwrap();
        let events = state.apply(&Command::TransitionToProgram).unwrap();
        assert!(!events
            .iter()
            .any(|e| matches!(e, Event::System(SystemEvent::TransitionStarted { .. }))));
        assert_eq!(state.studio_mode.as_ref().unwrap().program, intermission);

        // Add an output, start it, stop it.
        let rec = add_output(&mut state, OutputKind::Recording, "Recording");
        let events = state
            .apply(&Command::StartOutput { output_id: rec })
            .unwrap();
        assert_eq!(
            events,
            vec![Event::Output(OutputEvent::StateChanged {
                output_id: rec,
                state: OutputState::Starting,
            })]
        );
        // Simulate the media layer reporting success.
        state.outputs.get_mut(&rec).unwrap().state = OutputState::Running;
        let events = state
            .apply(&Command::StopOutput { output_id: rec })
            .unwrap();
        assert_eq!(
            events,
            vec![Event::Output(OutputEvent::StateChanged {
                output_id: rec,
                state: OutputState::Stopping,
            })]
        );

        // Audio: volume/mute/route.
        state
            .apply(&Command::SetSourceVolume {
                source_id: cam,
                volume_db: -6.0,
            })
            .unwrap();
        assert_eq!(state.audio.mixer_state(cam).volume_db, -6.0);
        state
            .apply(&Command::SetSourceMuted {
                source_id: cam,
                muted: true,
            })
            .unwrap();
        assert!(state.audio.mixer_state(cam).muted);
        let bus = state.audio.buses[0].id;
        state
            .apply(&Command::SetAudioRoute {
                source_id: cam,
                bus_id: bus,
                tracks: TrackMask::stereo_pair(),
            })
            .unwrap();
        assert_eq!(state.audio.routes.len(), 1);
    }

    // --- error paths ---

    #[test]
    fn missing_ids_are_typed_not_found_errors() {
        let mut state = AppState::new();
        let scene = SceneId::new();
        let source = SourceId::new();
        for cmd in [
            Command::RemoveScene { scene_id: scene },
            Command::SetCurrentScene { scene_id: scene },
            Command::AddSceneItem {
                scene_id: scene,
                source_id: source,
            },
            Command::RemoveSource { source_id: source },
            Command::SetSourceVolume {
                source_id: source,
                volume_db: 0.0,
            },
            Command::StartOutput {
                output_id: OutputId::new(),
            },
            Command::RemoveAudioRoute {
                source_id: source,
                bus_id: AudioBusId::new(),
            },
        ] {
            let err = state.apply(&cmd).unwrap_err();
            assert!(matches!(err, Error::NotFound(_)), "{cmd:?} -> {err:?}");
        }
    }

    #[test]
    fn cannot_remove_referenced_source() {
        let mut state = AppState::new();
        let src = add_source(&mut state, SourceKind::Color, "c");
        let scene = add_scene(&mut state, "s");
        add_item(&mut state, scene, src);
        let err = state
            .apply(&Command::RemoveSource { source_id: src })
            .unwrap_err();
        assert!(matches!(err, Error::InvalidInput(_)));
        // After removing the item, removal succeeds.
        let item_id = state.scene(scene).unwrap().items[0].id;
        state
            .apply(&Command::RemoveSceneItem {
                scene_id: scene,
                item_id,
            })
            .unwrap();
        state
            .apply(&Command::RemoveSource { source_id: src })
            .unwrap();
        assert!(state.sources.is_empty());
    }

    #[test]
    fn cannot_remove_source_with_audio_routes() {
        let mut state = AppState::new();
        let src = add_source(&mut state, SourceKind::PipeWireAudioInput, "mic");
        let bus = state.audio.buses[0].id;
        state
            .apply(&Command::SetAudioRoute {
                source_id: src,
                bus_id: bus,
                tracks: TrackMask::default(),
            })
            .unwrap();
        assert!(state
            .apply(&Command::RemoveSource { source_id: src })
            .is_err());
        state
            .apply(&Command::RemoveAudioRoute {
                source_id: src,
                bus_id: bus,
            })
            .unwrap();
        state
            .apply(&Command::RemoveSource { source_id: src })
            .unwrap();
    }

    #[test]
    fn cannot_remove_last_scene_or_scene_in_studio_mode() {
        let mut state = AppState::new();
        let a = add_scene(&mut state, "a");
        let b = add_scene(&mut state, "b");
        state
            .apply(&Command::SetStudioModeEnabled { enabled: true })
            .unwrap();
        assert!(state.apply(&Command::RemoveScene { scene_id: a }).is_err());
        state
            .apply(&Command::SetStudioModeEnabled { enabled: false })
            .unwrap();
        state.apply(&Command::RemoveScene { scene_id: a }).unwrap();
        // Removing the current scene moves current to the remaining scene.
        assert_eq!(state.current_scene, Some(b));
        assert!(state.apply(&Command::RemoveScene { scene_id: b }).is_err());
    }

    #[test]
    fn output_lifecycle_validation() {
        let mut state = AppState::new();
        let out = add_output(&mut state, OutputKind::Rtmp, "twitch");
        // Stop from Stopped is invalid.
        assert!(state
            .apply(&Command::StopOutput { output_id: out })
            .is_err());
        state
            .apply(&Command::StartOutput { output_id: out })
            .unwrap();
        // Double-start is invalid.
        assert!(state
            .apply(&Command::StartOutput { output_id: out })
            .is_err());
        // Cannot remove a non-stopped output.
        assert!(state
            .apply(&Command::RemoveOutput { output_id: out })
            .is_err());
        // Stop from Starting is legal (abort).
        state
            .apply(&Command::StopOutput { output_id: out })
            .unwrap();
        // Failed outputs can be restarted.
        state.outputs.get_mut(&out).unwrap().state = OutputState::Failed;
        state
            .apply(&Command::StartOutput { output_id: out })
            .unwrap();
        assert_eq!(state.output(out).unwrap().state, OutputState::Starting);
    }

    #[test]
    fn studio_mode_validation() {
        let mut state = AppState::new();
        // Needs two scenes.
        add_scene(&mut state, "only");
        assert!(state
            .apply(&Command::SetStudioModeEnabled { enabled: true })
            .is_err());
        let b = add_scene(&mut state, "second");
        state
            .apply(&Command::SetStudioModeEnabled { enabled: true })
            .unwrap();
        // Preview must differ from program.
        let program = state.studio_mode.as_ref().unwrap().program;
        assert!(state
            .apply(&Command::SetPreviewScene { scene_id: program })
            .is_err());
        state
            .apply(&Command::SetPreviewScene { scene_id: b })
            .unwrap();
        // Preview/transition commands require studio mode.
        state
            .apply(&Command::SetStudioModeEnabled { enabled: false })
            .unwrap();
        assert!(state
            .apply(&Command::SetPreviewScene { scene_id: b })
            .is_err());
        assert!(state.apply(&Command::TransitionToProgram).is_err());
        assert!(state.apply(&Command::SwapPreviewProgram).is_err());
    }

    #[test]
    fn locked_items_reject_edits_but_allow_visibility() {
        let mut state = AppState::new();
        let src = add_source(&mut state, SourceKind::Image, "img");
        let scene = add_scene(&mut state, "s");
        let item = add_item(&mut state, scene, src);
        state
            .apply(&Command::SetSceneItemLocked {
                scene_id: scene,
                item_id: item,
                locked: true,
            })
            .unwrap();
        assert!(state
            .apply(&Command::SetSceneItemTransform {
                scene_id: scene,
                item_id: item,
                transform: Transform::default(),
            })
            .is_err());
        assert!(state
            .apply(&Command::SetSceneItemZIndex {
                scene_id: scene,
                item_id: item,
                z_index: 9
            })
            .is_err());
        state
            .apply(&Command::SetSceneItemVisible {
                scene_id: scene,
                item_id: item,
                visible: false,
            })
            .unwrap();
    }

    #[test]
    fn invalid_numeric_values_rejected() {
        let mut state = AppState::new();
        let src = add_source(&mut state, SourceKind::Text, "t");
        let scene = add_scene(&mut state, "s");
        let item = add_item(&mut state, scene, src);
        let mut bad = Transform::default();
        bad.position.x = f32::NAN;
        assert!(state
            .apply(&Command::SetSceneItemTransform {
                scene_id: scene,
                item_id: item,
                transform: bad,
            })
            .is_err());
        assert!(state
            .apply(&Command::SetSceneItemOpacity {
                scene_id: scene,
                item_id: item,
                opacity: 1.5,
            })
            .is_err());
        assert!(state
            .apply(&Command::SetSourceVolume {
                source_id: src,
                volume_db: f32::INFINITY
            })
            .is_err());
        assert!(state
            .apply(&Command::SetSourceBalance {
                source_id: src,
                balance: -2.0
            })
            .is_err());
    }

    #[test]
    fn names_are_unique_ified() {
        let mut state = AppState::new();
        let a = add_source(&mut state, SourceKind::Color, "cam");
        let b = add_source(&mut state, SourceKind::Color, "cam");
        assert_eq!(state.source(a).unwrap().name, "cam");
        assert_eq!(state.source(b).unwrap().name, "cam (2)");
        // Rename collision is unique-ified too.
        state
            .apply(&Command::RenameSource {
                source_id: b,
                name: "cam".into(),
            })
            .unwrap();
        assert_eq!(state.source(b).unwrap().name, "cam (2)");
        // Empty names rejected.
        assert!(state
            .apply(&Command::AddScene { name: "  ".into() })
            .is_err());
    }

    #[test]
    fn no_op_commands_emit_no_events() {
        let mut state = AppState::new();
        let src = add_source(&mut state, SourceKind::Color, "c");
        let scene = add_scene(&mut state, "s");
        let item = add_item(&mut state, scene, src);
        assert!(state
            .apply(&Command::SetSceneItemVisible {
                scene_id: scene,
                item_id: item,
                visible: true
            })
            .unwrap()
            .is_empty());
        assert!(state
            .apply(&Command::SetCurrentScene { scene_id: scene })
            .unwrap()
            .is_empty());
        assert!(state
            .apply(&Command::SetSourceMuted {
                source_id: src,
                muted: false
            })
            .unwrap()
            .is_empty());
        assert!(state
            .apply(&Command::RenameSource {
                source_id: src,
                name: "c".into()
            })
            .unwrap()
            .is_empty());
        // Default-only mixer entries are not materialized.
        assert!(state.audio.mixer.is_empty());
    }

    #[test]
    fn audio_bus_cascade_removal_emits_route_events() {
        let mut state = AppState::new();
        let src = add_source(&mut state, SourceKind::PipeWireAudioInput, "mic");
        let events = state
            .apply(&Command::AddAudioBus { name: "VOD".into() })
            .unwrap();
        let vod = match events[0] {
            Event::Audio(AudioEvent::BusAdded { bus_id, .. }) => bus_id,
            ref other => panic!("unexpected {other:?}"),
        };
        state
            .apply(&Command::SetAudioRoute {
                source_id: src,
                bus_id: vod,
                tracks: TrackMask::ALL,
            })
            .unwrap();
        let events = state
            .apply(&Command::RemoveAudioBus { bus_id: vod })
            .unwrap();
        assert_eq!(
            events,
            vec![
                Event::Audio(AudioEvent::RouteRemoved {
                    source_id: src,
                    bus_id: vod
                }),
                Event::Audio(AudioEvent::BusRemoved { bus_id: vod }),
            ]
        );
        // The last remaining bus cannot be removed.
        let master = state.audio.buses[0].id;
        assert!(state
            .apply(&Command::RemoveAudioBus { bus_id: master })
            .is_err());
    }

    // --- undo inverses ---

    #[test]
    fn inverse_roundtrips_state() {
        let mut state = AppState::new();
        let src = add_source(&mut state, SourceKind::V4l2Camera, "cam");
        let scene = add_scene(&mut state, "s");
        let item = add_item(&mut state, scene, src);
        let second = add_item(&mut state, scene, src);
        let other = add_scene(&mut state, "other");

        let commands = vec![
            Command::RenameSource {
                source_id: src,
                name: "cam2".into(),
            },
            Command::SetSourceSettings {
                source_id: src,
                settings: serde_json::json!({"fps": 30}),
            },
            Command::SetSourceEnabled {
                source_id: src,
                enabled: false,
            },
            Command::SetSourceVolume {
                source_id: src,
                volume_db: -12.0,
            },
            Command::SetSourceMuted {
                source_id: src,
                muted: true,
            },
            Command::SetSourceMonitor {
                source_id: src,
                monitor: MonitorMode::MonitorOnly,
            },
            Command::SetSceneItemTransform {
                scene_id: scene,
                item_id: item,
                transform: Transform {
                    position: Vec2::new(5.0, 6.0),
                    ..Transform::default()
                },
            },
            Command::SetSceneItemCrop {
                scene_id: scene,
                item_id: item,
                crop: Crop {
                    left: 1,
                    top: 2,
                    right: 3,
                    bottom: 4,
                },
            },
            Command::SetSceneItemVisible {
                scene_id: scene,
                item_id: item,
                visible: false,
            },
            Command::SetSceneItemOpacity {
                scene_id: scene,
                item_id: item,
                opacity: 0.25,
            },
            // Puts `item` (z=0) above `second` (z=1).
            Command::SetSceneItemZIndex {
                scene_id: scene,
                item_id: item,
                z_index: 42,
            },
            Command::SetCurrentScene { scene_id: other },
            Command::SetTransition {
                transition: crate::transition::Transition {
                    kind: TransitionKind::Slide,
                    duration_ms: 500,
                    settings: serde_json::Value::Null,
                },
            },
            // A real swap: `second` (bottom) raised above `item` (top).
            Command::RaiseSceneItem {
                scene_id: scene,
                item_id: second,
            },
            Command::LowerSceneItem {
                scene_id: scene,
                item_id: second,
            },
        ];

        for cmd in commands {
            let before = state.clone();
            let inverse = state
                .inverse(&cmd)
                .unwrap_or_else(|| panic!("no inverse for {cmd:?}"));
            state.apply(&cmd).unwrap();
            state.apply(&inverse).unwrap();
            assert_eq!(
                serde_json::to_string(&state).unwrap(),
                serde_json::to_string(&before).unwrap(),
                "inverse of {cmd:?} did not restore state"
            );
        }
    }

    #[test]
    fn irreversible_commands_have_no_inverse() {
        let mut state = AppState::new();
        let src = add_source(&mut state, SourceKind::Color, "c");
        let scene = add_scene(&mut state, "s");
        let item = add_item(&mut state, scene, src);
        for cmd in [
            Command::AddScene { name: "x".into() },
            Command::RemoveScene { scene_id: scene },
            Command::AddSceneItem {
                scene_id: scene,
                source_id: src,
            },
            Command::RemoveSceneItem {
                scene_id: scene,
                item_id: item,
            },
            Command::DuplicateSceneItem {
                scene_id: scene,
                item_id: item,
            },
            Command::AddSource {
                kind: SourceKind::Color,
                name: "y".into(),
            },
            Command::RemoveSource { source_id: src },
            Command::AddAudioBus { name: "b".into() },
            Command::RemoveAudioBus {
                bus_id: AudioBusId::new(),
            },
            Command::AddOutput {
                output: Output::new(OutputKind::Srt, "s", crate::id::EncoderId::new()),
            },
            Command::RemoveOutput {
                output_id: OutputId::new(),
            },
            Command::AddProfile {
                profile: crate::project::Profile::new("p", VideoConfig::default()),
            },
            Command::RemoveProfile {
                profile_id: ProfileId::new(),
            },
        ] {
            assert!(
                state.inverse(&cmd).is_none(),
                "{cmd:?} should be irreversible"
            );
        }
    }

    #[test]
    fn transaction_is_atomic_and_invertible() {
        let mut state = AppState::new();
        let src = add_source(&mut state, SourceKind::Color, "c");
        let scene = add_scene(&mut state, "s");
        let item = add_item(&mut state, scene, src);
        let before = state.clone();

        // Failing transaction leaves state untouched.
        let failing = Command::Transaction {
            commands: vec![
                Command::SetSourceMuted {
                    source_id: src,
                    muted: true,
                },
                Command::SetSceneItemVisible {
                    scene_id: SceneId::new(), // missing scene -> error
                    item_id: item,
                    visible: false,
                },
            ],
        };
        assert!(state.apply(&failing).is_err());
        assert_eq!(
            serde_json::to_string(&state).unwrap(),
            serde_json::to_string(&before).unwrap()
        );

        // Succeeding transaction applies all and inverts as a group.
        let ok = Command::Transaction {
            commands: vec![
                Command::SetSourceMuted {
                    source_id: src,
                    muted: true,
                },
                Command::SetSceneItemVisible {
                    scene_id: scene,
                    item_id: item,
                    visible: false,
                },
            ],
        };
        let inverse = state.inverse(&ok).unwrap();
        let events = state.apply(&ok).unwrap();
        assert_eq!(events.len(), 2);
        state.apply(&inverse).unwrap();
        assert_eq!(
            serde_json::to_string(&state).unwrap(),
            serde_json::to_string(&before).unwrap()
        );
    }

    #[test]
    fn duplicate_item_gets_fresh_id_and_top_z() {
        let mut state = AppState::new();
        let src = add_source(&mut state, SourceKind::Color, "c");
        let scene = add_scene(&mut state, "s");
        let item = add_item(&mut state, scene, src);
        let events = state
            .apply(&Command::DuplicateSceneItem {
                scene_id: scene,
                item_id: item,
            })
            .unwrap();
        let copy = match &events[0] {
            Event::Scene(SceneEvent::ItemAdded { item, .. }) => item.clone(),
            other => panic!("unexpected {other:?}"),
        };
        assert_ne!(copy.id, item);
        assert_eq!(copy.source_id, src);
        assert_eq!(copy.z_index, 1);
        assert_eq!(state.scene(scene).unwrap().items.len(), 2);
    }

    #[test]
    fn capture_authorization_admission_gates_on_kind_and_enabled() {
        let mut state = AppState::new();
        for kind in [
            SourceKind::PipeWireDisplay,
            SourceKind::PipeWireWindow,
            SourceKind::V4l2Camera,
        ] {
            let id = add_source(&mut state, kind, "capture");
            let events = state
                .apply(&Command::AuthorizeSourceCapture { source_id: id })
                .unwrap();
            assert_eq!(
                events,
                vec![Event::Source(SourceEvent::CaptureAuthorizationRequested {
                    source_id: id
                })]
            );
        }
        // Disabled, non-capture and unknown sources stay rejected.
        let cam = add_source(&mut state, SourceKind::V4l2Camera, "cam");
        state
            .apply(&Command::SetSourceEnabled {
                source_id: cam,
                enabled: false,
            })
            .unwrap();
        assert!(state
            .apply(&Command::AuthorizeSourceCapture { source_id: cam })
            .is_err());
        let generator = add_source(&mut state, SourceKind::TestPattern, "pattern");
        assert!(state
            .apply(&Command::AuthorizeSourceCapture {
                source_id: generator
            })
            .is_err());
        assert!(state
            .apply(&Command::AuthorizeSourceCapture {
                source_id: SourceId::new()
            })
            .is_err());
    }

    #[test]
    fn app_state_serde_roundtrip() {
        let mut state = AppState::new();
        let src = add_source(&mut state, SourceKind::MediaFile, "video");
        let scene = add_scene(&mut state, "Main");
        add_item(&mut state, scene, src);
        add_output(&mut state, OutputKind::Whip, "whip");
        let json = serde_json::to_string(&state).unwrap();
        let back: AppState = serde_json::from_str(&json).unwrap();
        assert_eq!(
            serde_json::to_string(&back).unwrap(),
            json,
            "state must survive a serde roundtrip"
        );
    }

    #[test]
    fn profiles_and_collections_lifecycle() {
        let mut state = AppState::new();
        let default_profile = state.active_profile.unwrap();
        assert!(state
            .apply(&Command::RemoveProfile {
                profile_id: default_profile
            })
            .is_err());

        let profile = crate::project::Profile::new("twitch-1080p", VideoConfig::default());
        let pid = profile.id;
        state.apply(&Command::AddProfile { profile }).unwrap();
        state
            .apply(&Command::SelectProfile { profile_id: pid })
            .unwrap();
        assert_eq!(state.active_profile, Some(pid));
        state
            .apply(&Command::RemoveProfile {
                profile_id: default_profile,
            })
            .unwrap();
        // Duplicate IDs rejected.
        let dup = crate::project::Profile {
            id: pid,
            ..crate::project::Profile::new("dup", VideoConfig::default())
        };
        assert!(state.apply(&Command::AddProfile { profile: dup }).is_err());

        let collection = SceneCollection::new("work");
        let cid = collection.id;
        state
            .apply(&Command::AddSceneCollection { collection })
            .unwrap();
        state
            .apply(&Command::SelectSceneCollection { collection_id: cid })
            .unwrap();
        assert!(state
            .apply(&Command::RemoveSceneCollection { collection_id: cid })
            .is_err());
    }

    #[test]
    fn reorder_scene_moves_and_clamps() {
        let mut state = AppState::new();
        let a = add_scene(&mut state, "a");
        let _b = add_scene(&mut state, "b");
        let c = add_scene(&mut state, "c");
        state
            .apply(&Command::ReorderScene {
                scene_id: a,
                new_index: 99,
            })
            .unwrap();
        let order: Vec<SceneId> = state.scenes.keys().copied().collect();
        assert_eq!(order[2], a);
        assert_eq!(order[0], state.scenes.keys().next().copied().unwrap());
        // Inverse restores position 0.
        let inverse = state.inverse(&Command::ReorderScene {
            scene_id: c,
            new_index: 0,
        });
        assert!(inverse.is_some());
    }

    #[test]
    fn encoder_settings_constructible() {
        // Descriptor types stay usable from the state layer's consumers.
        let enc = EncoderSettings {
            id: crate::id::EncoderId::new(),
            codec: "opus".into(),
            bitrate_kbps: 160,
            keyframe_interval: None,
            settings: serde_json::Value::Null,
        };
        assert_eq!(enc.bitrate_kbps, 160);
        let policy = ReconnectPolicy::default();
        assert!(policy.max_backoff_ms >= policy.initial_backoff_ms);
    }
}
