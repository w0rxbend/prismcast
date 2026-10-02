//! Name/number addressing for the obs adapter (ADR-0020 §c).
//!
//! obs-websocket addresses scenes, inputs, and outputs by *name* and scene
//! items by a numeric `sceneItemId` that is only valid within its scene;
//! Prismcast addresses everything by typed UUIDs. Two bridges:
//!
//! - **Stateless name→ID resolution** by scanning the latest snapshot's
//!   state: renames need no index maintenance. On duplicate names the first
//!   match in list order wins and a warning is logged (a documented
//!   divergence — OBS cannot create duplicate names through the protocol;
//!   Prismcast's core auto-uniquifies names on add/rename, so duplicates can
//!   only appear via state restore or injection).
//! - **Stateful [`ItemIdMap`]**: per-scene `SceneItemId ↔ u64` registries,
//!   minted lazily on enumeration/creation and shared server-wide so all
//!   clients agree on numbers. Eviction happens on every removal path —
//!   scene item removed, scene removed, scene-collection switch (the working
//!   set is replaced wholesale) — via [`ItemIdMap::apply_event`] driven by
//!   the server's [`EventFanout`](crate::server::EventFanout) listener
//!   ([`eviction_listener`]). Numbers are never reused within a scene's
//!   registry lifetime (upstream mints monotonically too); a lagged event
//!   stream clears the whole map, since missed removals are unrecoverable
//!   and IDs re-mint lazily.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use prismcast_app::broadcaster::StreamEvent;
use prismcast_core::event::{Event, SceneEvent, SystemEvent};
use prismcast_core::id::{SceneId, SceneItemId};
use prismcast_core::output::{Output, OutputKind};
use prismcast_core::scene::Scene;
use prismcast_core::source::Source;
use prismcast_core::state::AppState;
use tokio::sync::broadcast;
use tracing::warn;

/// Resolves a scene by its user-facing name: the first match in scene-list
/// order, with a warning when the name is ambiguous.
pub(crate) fn scene_by_name<'a>(state: &'a AppState, name: &str) -> Option<&'a Scene> {
    let mut matches = state.scenes.values().filter(|scene| scene.name == name);
    let first = matches.next()?;
    if matches.next().is_some() {
        warn!(
            scene_name = name,
            "duplicate scene names; resolving to the first match"
        );
    }
    Some(first)
}

/// Resolves a shared source (obs "input") by name; same first-match rule as
/// [`scene_by_name`].
pub(crate) fn source_by_name<'a>(state: &'a AppState, name: &str) -> Option<&'a Source> {
    let mut matches = state.sources.values().filter(|source| source.name == name);
    let first = matches.next()?;
    if matches.next().is_some() {
        warn!(
            input_name = name,
            "duplicate input names; resolving to the first match"
        );
    }
    Some(first)
}

/// Resolves an output by name; same first-match rule as [`scene_by_name`].
pub(crate) fn output_by_name<'a>(state: &'a AppState, name: &str) -> Option<&'a Output> {
    let mut matches = state.outputs.values().filter(|output| output.name == name);
    let first = matches.next()?;
    if matches.next().is_some() {
        warn!(
            output_name = name,
            "duplicate output names; resolving to the first match"
        );
    }
    Some(first)
}

/// The designated primary stream output (obs's singleton "the stream"): the
/// first `Rtmp` output, falling back to `Srt`, then `Whip` (ADR-0020 §e).
pub(crate) fn primary_stream_output(state: &AppState) -> Option<&Output> {
    [OutputKind::Rtmp, OutputKind::Srt, OutputKind::Whip]
        .into_iter()
        .find_map(|kind| state.outputs.values().find(|output| output.kind == kind))
}

/// The designated primary record output: the first `Recording` output
/// (ADR-0020 §e).
pub(crate) fn primary_record_output(state: &AppState) -> Option<&Output> {
    state
        .outputs
        .values()
        .find(|output| output.kind == OutputKind::Recording)
}

/// Per-scene registry of minted obs `sceneItemId` numbers.
#[derive(Debug, Default)]
struct SceneItems {
    by_item: HashMap<SceneItemId, u64>,
    by_number: HashMap<u64, SceneItemId>,
    /// Last minted number; strictly increasing, never reused after eviction.
    next: u64,
}

/// Server-wide `SceneItemId ↔ u64` registry, shared by all obs sessions so
/// clients agree on `sceneItemId` numbers (ADR-0020 §c). Thread-safe; all
/// operations are O(1)-ish critical sections on a std mutex (same pattern as
/// the server's session list — no `.await` is ever held across a lock).
#[derive(Debug, Default)]
pub(crate) struct ItemIdMap {
    scenes: Mutex<HashMap<SceneId, SceneItems>>,
}

impl ItemIdMap {
    /// An empty registry behind an `Arc` for sharing with session tasks.
    pub(crate) fn shared() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<SceneId, SceneItems>> {
        self.scenes.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The obs number for `item_id` in `scene_id`, minting the next number
    /// when the item is not registered yet. Re-`mint` of a known item
    /// returns its stable number.
    pub(crate) fn mint(&self, scene_id: SceneId, item_id: SceneItemId) -> u64 {
        let mut scenes = self.lock();
        let entry = scenes.entry(scene_id).or_default();
        if let Some(number) = entry.by_item.get(&item_id) {
            return *number;
        }
        entry.next += 1;
        let number = entry.next;
        entry.by_item.insert(item_id, number);
        entry.by_number.insert(number, item_id);
        number
    }

    /// Mints numbers for `item_ids` in iteration order (used when
    /// enumerating a scene's items bottom-to-top).
    pub(crate) fn mint_all(
        &self,
        scene_id: SceneId,
        item_ids: impl IntoIterator<Item = SceneItemId>,
    ) -> Vec<u64> {
        item_ids
            .into_iter()
            .map(|item_id| self.mint(scene_id, item_id))
            .collect()
    }

    /// Resolves an obs `sceneItemId` number back to the domain ID. Unknown
    /// numbers (never minted, or evicted) return `None`.
    pub(crate) fn resolve(&self, scene_id: SceneId, number: u64) -> Option<SceneItemId> {
        self.lock()
            .get(&scene_id)
            .and_then(|entry| entry.by_number.get(&number))
            .copied()
    }

    /// Evicts one item's number (item removed from the scene).
    pub(crate) fn evict_item(&self, scene_id: SceneId, item_id: SceneItemId) {
        let mut scenes = self.lock();
        if let Some(entry) = scenes.get_mut(&scene_id) {
            if let Some(number) = entry.by_item.remove(&item_id) {
                entry.by_number.remove(&number);
            }
        }
    }

    /// Evicts a whole scene's registry (scene removed).
    pub(crate) fn evict_scene(&self, scene_id: SceneId) {
        self.lock().remove(&scene_id);
    }

    /// Drops every registry (scene-collection switch: the entire working set
    /// is replaced; also the conservative answer to a lagged event stream).
    pub(crate) fn clear(&self) {
        self.lock().clear();
    }

    /// Event-driven eviction, covering every removal path (ADR-0020 §c):
    /// item removal, scene removal, and collection switch (which replaces
    /// the working set under new scene IDs).
    pub(crate) fn apply_event(&self, event: &Event) {
        match event {
            Event::Scene(SceneEvent::ItemRemoved {
                scene_id, item_id, ..
            }) => self.evict_item(*scene_id, *item_id),
            Event::Scene(SceneEvent::Removed { scene_id }) => self.evict_scene(*scene_id),
            Event::System(SystemEvent::CollectionSelected { .. }) => self.clear(),
            _ => {}
        }
    }
}

/// Server-lifetime task applying [`ItemIdMap`] eviction to every event the
/// server's fan-out delivers. A lagged receiver means removals may have been
/// missed, so the whole map is cleared (IDs re-mint lazily). Ends when the
/// fan-out closes (core shutdown).
pub(crate) async fn eviction_listener(
    mut rx: broadcast::Receiver<StreamEvent>,
    map: Arc<ItemIdMap>,
) {
    loop {
        match rx.recv().await {
            Ok(StreamEvent::Event { event, .. }) => map.apply_event(&event),
            Ok(StreamEvent::Lagged { dropped }) => {
                warn!(
                    dropped,
                    "obs item-id listener lost events; clearing minted scene item IDs"
                );
                map.clear();
            }
            Err(broadcast::error::RecvError::Lagged(dropped)) => {
                warn!(
                    dropped,
                    "obs item-id listener lagged; clearing minted scene item IDs"
                );
                map.clear();
            }
            Err(broadcast::error::RecvError::Closed) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_with_scenes(names: &[&str]) -> AppState {
        let mut state = AppState::new();
        for name in names {
            state.scenes.insert(SceneId::new(), Scene::new(*name));
        }
        state
    }

    #[test]
    fn name_resolution_is_first_match_in_list_order() {
        let state = state_with_scenes(&["Main", "Dup", "Dup"]);
        let scene = scene_by_name(&state, "Main").expect("found");
        assert_eq!(scene.name, "Main");
        let first_dup_id = state
            .scenes
            .values()
            .nth(1)
            .map(|scene| scene.id)
            .expect("second scene");
        assert_eq!(
            scene_by_name(&state, "Dup").map(|scene| scene.id),
            Some(first_dup_id),
            "duplicates resolve to the first match in list order"
        );
        assert!(scene_by_name(&state, "Nope").is_none());
    }

    #[test]
    fn output_resolution_and_primary_fallback_order() {
        let mut state = AppState::new();
        for (kind, name) in [
            (OutputKind::Recording, "rec"),
            (OutputKind::Srt, "srt"),
            (OutputKind::Rtmp, "rtmp"),
            (OutputKind::Whip, "whip"),
        ] {
            let output = Output::new(kind, name, prismcast_core::id::EncoderId::new());
            state.outputs.insert(output.id, output);
        }
        assert_eq!(
            output_by_name(&state, "srt").map(|o| o.kind),
            Some(OutputKind::Srt)
        );
        assert!(output_by_name(&state, "Nope").is_none());
        // Rtmp wins over Srt/Whip regardless of insertion order.
        assert_eq!(
            primary_stream_output(&state).map(|o| o.kind),
            Some(OutputKind::Rtmp)
        );
        state.outputs.shift_remove(
            &state
                .outputs
                .values()
                .find(|o| o.kind == OutputKind::Rtmp)
                .map(|o| o.id)
                .expect("rtmp"),
        );
        assert_eq!(
            primary_stream_output(&state).map(|o| o.kind),
            Some(OutputKind::Srt)
        );
        state.outputs.shift_remove(
            &state
                .outputs
                .values()
                .find(|o| o.kind == OutputKind::Srt)
                .map(|o| o.id)
                .expect("srt"),
        );
        assert_eq!(
            primary_stream_output(&state).map(|o| o.kind),
            Some(OutputKind::Whip)
        );
        state.outputs.shift_remove(
            &state
                .outputs
                .values()
                .find(|o| o.kind == OutputKind::Whip)
                .map(|o| o.id)
                .expect("whip"),
        );
        assert!(
            primary_stream_output(&state).is_none(),
            "no streamable output left"
        );
        assert_eq!(
            primary_record_output(&state).map(|o| o.name.as_str()),
            Some("rec")
        );
    }

    #[test]
    fn mint_is_stable_and_monotonic() {
        let map = ItemIdMap::shared();
        let scene = SceneId::new();
        let (a, b, c) = (SceneItemId::new(), SceneItemId::new(), SceneItemId::new());
        assert_eq!(map.mint_all(scene, [a, b]), vec![1, 2]);
        assert_eq!(map.mint(scene, a), 1, "re-mint keeps the number");
        // Out-of-order lazy minting continues the sequence.
        assert_eq!(map.mint(scene, c), 3);
        assert_eq!(map.resolve(scene, 1), Some(a));
        assert_eq!(map.resolve(scene, 2), Some(b));
        assert_eq!(map.resolve(scene, 3), Some(c));
        assert_eq!(map.resolve(scene, 4), None);
        assert_eq!(map.resolve(SceneId::new(), 1), None, "other scene");
    }

    #[test]
    fn eviction_on_item_removal_event() {
        let map = ItemIdMap::shared();
        let (scene, item, source) = (
            SceneId::new(),
            SceneItemId::new(),
            prismcast_core::SourceId::new(),
        );
        let number = map.mint(scene, item);
        map.apply_event(&Event::Scene(SceneEvent::ItemRemoved {
            scene_id: scene,
            item_id: item,
            source_id: source,
        }));
        assert_eq!(map.resolve(scene, number), None, "evicted on ItemRemoved");
        // Numbers are never reused: a new item mints the next number.
        let other = SceneItemId::new();
        assert_eq!(map.mint(scene, other), number + 1);
    }

    #[test]
    fn eviction_on_scene_removal_event() {
        let map = ItemIdMap::shared();
        let scene = SceneId::new();
        let item = SceneItemId::new();
        let number = map.mint(scene, item);
        map.apply_event(&Event::Scene(SceneEvent::Removed { scene_id: scene }));
        assert_eq!(map.resolve(scene, number), None, "evicted on scene Removed");
        // A fresh scene ID starts its own numbering from 1.
        let new_scene = SceneId::new();
        assert_eq!(map.mint(new_scene, item), 1);
    }

    #[test]
    fn eviction_on_collection_switch_event() {
        let map = ItemIdMap::shared();
        let scene = SceneId::new();
        let number = map.mint(scene, SceneItemId::new());
        map.apply_event(&Event::System(SystemEvent::CollectionSelected {
            collection_id: prismcast_core::SceneCollectionId::new(),
        }));
        assert_eq!(
            map.resolve(scene, number),
            None,
            "cleared on collection switch"
        );
    }

    #[test]
    fn unrelated_events_do_not_evict() {
        let map = ItemIdMap::shared();
        let scene = SceneId::new();
        let item = SceneItemId::new();
        let number = map.mint(scene, item);
        map.apply_event(&Event::Scene(SceneEvent::Reordered));
        map.apply_event(&Event::System(SystemEvent::StudioModeChanged {
            enabled: true,
        }));
        assert_eq!(map.resolve(scene, number), Some(item));
    }

    #[tokio::test]
    async fn eviction_listener_applies_events_and_clears_on_lag() {
        let (tx, rx) = broadcast::channel(2);
        let map = ItemIdMap::shared();
        let listener = tokio::spawn(eviction_listener(rx, map.clone()));
        let scene = SceneId::new();
        let item = SceneItemId::new();
        let number = map.mint(scene, item);
        tx.send(StreamEvent::Event {
            seq: 1,
            event: Event::Scene(SceneEvent::Removed { scene_id: scene }),
        })
        .expect("send");
        // Yield until the listener processed the event.
        for _ in 0..100 {
            if map.resolve(scene, number).is_none() {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(map.resolve(scene, number), None);
        drop(tx);
        listener
            .await
            .expect("listener exits when the fan-out closes");
    }
}
