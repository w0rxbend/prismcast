//! Immutable, cheaply-shareable state snapshots (PLAN.md §57, ADR-0005).
//!
//! The core actor publishes a fresh [`AppSnapshot`] after every applied
//! command. Snapshots are shared as `Arc<AppSnapshot>`, so reads never touch
//! the actor's command queue and never block writers (PLAN.md §57: "Snapshots
//! can be shared as immutable `Arc<AppSnapshot>`"; explicitly *not* a giant
//! `Arc<Mutex<AppState>>`).
//!
//! ## Strategy: clone-on-publish
//!
//! `AppSnapshot` owns a full clone of [`AppState`] taken at publish time.
//! This is a deliberate choice:
//!
//! - The domain working set is small (tens of scenes/sources/outputs — not
//!   media frames), so a structural clone is microseconds-cheap and allocation
//!   light relative to command latency budgets (PLAN.md §56: IPC p95 < 10 ms).
//! - Readers get a *consistent* view of the whole state with zero locking
//!   against writers: an `Arc` swap is the only synchronization.
//! - No persistent/immutable data-structure dependency is pulled in.
//!
//! If profiling ever shows clone cost mattering, the internals can switch to a
//! persistent structure without changing the public read API.

use prismcast_core::SourceRuntime;
use std::collections::HashMap;
use std::sync::Arc;

use prismcast_core::id::{OutputId, SceneId, SourceId};
use prismcast_core::output::Output;
use prismcast_core::scene::Scene;
use prismcast_core::source::Source;
use prismcast_core::state::AppState;

/// An immutable, point-in-time view of the application state.
///
/// Published by the core actor after every applied command; `revision` is
/// strictly increasing and lets controllers detect missed updates (pair with
/// the event stream's sequence numbers for gap detection).
#[derive(Debug, Clone)]
pub struct AppSnapshot {
    revision: u64,
    state: AppState,
    runtime: HashMap<SourceId, SourceRuntime>,
}

impl AppSnapshot {
    /// Wraps a state clone at the given revision. Called by the core actor.
    pub(crate) fn new(revision: u64, state: AppState) -> Arc<Self> {
        Self::with_runtime(revision, state, HashMap::new())
    }

    pub(crate) fn with_runtime(
        revision: u64,
        state: AppState,
        runtime: HashMap<SourceId, SourceRuntime>,
    ) -> Arc<Self> {
        Arc::new(Self {
            revision,
            state,
            runtime,
        })
    }
    /// Transient capture observation, absent until explicit authorization.
    pub fn source_runtime(&self, source_id: SourceId) -> Option<&SourceRuntime> {
        self.runtime.get(&source_id)
    }
    /// Current bounded runtime observations, independent of persisted state.
    pub fn source_runtimes(&self) -> impl Iterator<Item = (SourceId, &SourceRuntime)> {
        self.runtime.iter().map(|(id, value)| (*id, value))
    }

    /// Monotonically increasing publish counter (0 = initial state).
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// The full domain state view.
    pub fn state(&self) -> &AppState {
        &self.state
    }

    /// All working-set scenes, in UI list order.
    pub fn scenes(&self) -> impl Iterator<Item = &Scene> {
        self.state.scenes.values()
    }

    /// Returns the scene with the given ID, if present.
    pub fn scene(&self, scene_id: SceneId) -> Option<&Scene> {
        self.state.scene(scene_id)
    }

    /// All shared sources.
    pub fn sources(&self) -> impl Iterator<Item = &Source> {
        self.state.sources.values()
    }

    /// Returns the source with the given ID, if present.
    pub fn source(&self, source_id: SourceId) -> Option<&Source> {
        self.state.source(source_id)
    }

    /// All configured outputs.
    pub fn outputs(&self) -> impl Iterator<Item = &Output> {
        self.state.outputs.values()
    }

    /// Returns the output with the given ID, if present.
    pub fn output(&self, output_id: OutputId) -> Option<&Output> {
        self.state.output(output_id)
    }

    /// The current (program) scene, if any.
    pub fn current_scene(&self) -> Option<SceneId> {
        self.state.current_scene
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use prismcast_core::Command;

    #[test]
    fn snapshot_reflects_state_at_publish_time() {
        let mut state = AppState::new();
        let first = AppSnapshot::new(0, state.clone());
        let events = state
            .apply(&Command::AddScene {
                name: "Main".into(),
            })
            .expect("apply add scene");
        let scene_id = match &events[0] {
            prismcast_core::Event::Scene(prismcast_core::SceneEvent::Added {
                scene_id, ..
            }) => *scene_id,
            other => panic!("unexpected event: {other:?}"),
        };
        let second = AppSnapshot::new(1, state);

        assert_eq!(first.revision(), 0);
        assert_eq!(first.scenes().count(), 0);
        assert_eq!(second.revision(), 1);
        assert!(second.scene(scene_id).is_some());
        // The earlier snapshot is unaffected by later mutation.
        assert_eq!(first.scenes().count(), 0);
    }

    #[test]
    fn accessors_delegate_to_state() {
        let mut state = AppState::new();
        state
            .apply(&Command::AddScene { name: "A".into() })
            .expect("apply");
        let snapshot = AppSnapshot::new(1, state);
        let scene = snapshot.scenes().next().expect("one scene");
        assert_eq!(scene.name, "A");
        assert_eq!(
            snapshot.scene(scene.id).map(|s| &s.name),
            Some(&"A".to_string())
        );
        assert_eq!(snapshot.sources().count(), 0);
        assert_eq!(snapshot.outputs().count(), 0);
        // The first scene becomes current on creation.
        assert_eq!(snapshot.current_scene(), Some(scene.id));
    }
}
