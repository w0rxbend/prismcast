//! Referential integrity validation on load
//! (`docs/architecture/persistence-model.md` §7).
//!
//! Deserialization proves shape, not sense. Violations of the rules below
//! count as **corruption** and drive the `.bak` fallback — except session
//! state (`current_scene`, studio mode), which is expendable and is reset
//! with a warning instead.

use std::collections::HashSet;

use prismcast_core::id::{EncoderId, SceneId, ServiceId, SourceId};

use super::envelope::CollectionSnapshot;
use super::profile::ProfileSnapshot;

/// Validates a loaded collection. Returns warnings (expendable session
/// fix-ups applied by the caller) or a corruption reason.
pub fn validate_collection(
    snapshot: &CollectionSnapshot,
) -> std::result::Result<Vec<String>, String> {
    let collection = &snapshot.collection;
    let mut errors: Vec<String> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();

    // ID uniqueness within each entity class.
    let scene_ids: HashSet<SceneId> =
        collect_ids(collection.scenes.iter().map(|s| s.id), "scene", &mut errors);
    let source_ids: HashSet<SourceId> = collect_ids(
        collection.sources.iter().map(|s| s.id),
        "source",
        &mut errors,
    );

    // Every scene item's source resolves.
    for scene in &collection.scenes {
        for item in &scene.items {
            if !source_ids.contains(&item.source_id) {
                errors.push(format!(
                    "scene '{}' item {} references missing source {}",
                    scene.name, item.id, item.source_id
                ));
            }
        }
    }

    // Scene-source references resolve and are acyclic. Edge scene S → scene
    // T exists when an item of S places a source of kind `Scene(T)`.
    let source_scene: std::collections::HashMap<SourceId, SceneId> = collection
        .sources
        .iter()
        .filter_map(|s| s.scene_reference().map(|target| (s.id, target)))
        .collect();
    for (source, target) in &source_scene {
        if !scene_ids.contains(target) {
            errors.push(format!(
                "scene source {source} references missing scene {target}"
            ));
        }
    }
    {
        let mut edges: std::collections::HashMap<SceneId, Vec<SceneId>> =
            std::collections::HashMap::new();
        for scene in &collection.scenes {
            let targets = scene
                .items
                .iter()
                .filter_map(|item| source_scene.get(&item.source_id).copied())
                .collect();
            edges.insert(scene.id, targets);
        }
        if has_scene_cycle(&edges) {
            errors.push("scene-source references form a cycle".to_string());
        }
    }

    // Audio routes and mixer entries resolve.
    let bus_ids: HashSet<_> = collection.audio.buses.iter().map(|b| b.id).collect();
    collect_ids(
        collection.audio.buses.iter().map(|b| b.id),
        "audio bus",
        &mut errors,
    );
    for route in &collection.audio.routes {
        if !source_ids.contains(&route.source_id) {
            errors.push(format!(
                "audio route references missing source {}",
                route.source_id
            ));
        }
        if !bus_ids.contains(&route.bus_id) {
            errors.push(format!(
                "audio route references missing bus {}",
                route.bus_id
            ));
        }
    }
    for source_id in collection.audio.mixer.keys() {
        if !source_ids.contains(source_id) {
            errors.push(format!("mixer entry references missing source {source_id}"));
        }
    }

    // Session state is expendable: reset dangling references with a warning.
    if let Some(current) = snapshot.session.current_scene {
        if !scene_ids.contains(&current) {
            warnings.push(format!(
                "session current scene {current} is missing; reset to none"
            ));
        }
    }
    if let Some(studio) = &snapshot.session.studio_mode {
        if !scene_ids.contains(&studio.program) || !scene_ids.contains(&studio.preview) {
            warnings.push("studio mode references a missing scene; reset to disabled".to_string());
        }
    }

    if errors.is_empty() {
        Ok(warnings)
    } else {
        Err(errors.join("; "))
    }
}

/// Applies the expendable-session fix-ups that [`validate_collection`]
/// reported: dangling session references are reset rather than corrupting
/// the load.
pub fn repair_session(snapshot: &mut CollectionSnapshot) {
    let scene_ids: HashSet<SceneId> = snapshot.collection.scenes.iter().map(|s| s.id).collect();
    if snapshot
        .session
        .current_scene
        .is_some_and(|id| !scene_ids.contains(&id))
    {
        snapshot.session.current_scene = None;
    }
    if snapshot.session.studio_mode.as_ref().is_some_and(|studio| {
        !scene_ids.contains(&studio.program) || !scene_ids.contains(&studio.preview)
    }) {
        snapshot.session.studio_mode = None;
    }
}

/// Validates a loaded profile: output encoder/service references resolve into
/// the profile's `[[encoders]]`/`[[services]]` — when those registries are
/// present. `AppState` does not yet hold encoder/service registries (tracked
/// follow-up), so files saved from the current app carry empty registries and
/// reference checks activate only when they are non-empty.
pub fn validate_profile(snapshot: &ProfileSnapshot) -> std::result::Result<(), String> {
    let mut errors: Vec<String> = Vec::new();
    collect_ids(snapshot.outputs.iter().map(|o| o.id), "output", &mut errors);
    let encoder_ids: HashSet<EncoderId> = collect_ids(
        snapshot.encoders.iter().map(|e| e.id),
        "encoder",
        &mut errors,
    );
    let service_ids: HashSet<ServiceId> = collect_ids(
        snapshot.services.iter().map(|s| s.id),
        "service",
        &mut errors,
    );

    if !snapshot.encoders.is_empty() {
        for output in &snapshot.outputs {
            if !encoder_ids.contains(&output.video_encoder) {
                errors.push(format!(
                    "output '{}' references missing video encoder {}",
                    output.name, output.video_encoder
                ));
            }
            for encoder in &output.audio_encoders {
                if !encoder_ids.contains(encoder) {
                    errors.push(format!(
                        "output '{}' references missing audio encoder {encoder}",
                        output.name
                    ));
                }
            }
        }
    }
    if !snapshot.services.is_empty() {
        for output in &snapshot.outputs {
            if let Some(service) = output.service {
                if !service_ids.contains(&service) {
                    errors.push(format!(
                        "output '{}' references missing service {service}",
                        output.name
                    ));
                }
            }
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

fn collect_ids<ID: std::hash::Hash + Eq + std::fmt::Display + Copy>(
    ids: impl Iterator<Item = ID>,
    what: &str,
    errors: &mut Vec<String>,
) -> HashSet<ID> {
    let mut seen = HashSet::new();
    for id in ids {
        if !seen.insert(id) {
            errors.push(format!("duplicate {what} id: {id}"));
        }
    }
    seen
}

/// Detects cycles in the scene→scene nesting graph (DFS, three-color).
fn has_scene_cycle(edges: &std::collections::HashMap<SceneId, Vec<SceneId>>) -> bool {
    #[derive(Clone, Copy, PartialEq)]
    enum Mark {
        Visiting,
        Done,
    }
    fn visit(
        node: SceneId,
        edges: &std::collections::HashMap<SceneId, Vec<SceneId>>,
        marks: &mut std::collections::HashMap<SceneId, Mark>,
    ) -> bool {
        match marks.get(&node) {
            Some(Mark::Done) => return false,
            Some(Mark::Visiting) => return true,
            None => {}
        }
        marks.insert(node, Mark::Visiting);
        if let Some(targets) = edges.get(&node) {
            for target in targets {
                if visit(*target, edges, marks) {
                    return true;
                }
            }
        }
        marks.insert(node, Mark::Done);
        false
    }
    let mut marks = std::collections::HashMap::new();
    edges.keys().any(|node| visit(*node, edges, &mut marks))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::envelope::SessionState;
    use prismcast_core::audio::{AudioMixerState, AudioRoute, TrackMask};
    use prismcast_core::project::SceneCollection;
    use prismcast_core::scene::{Scene, SceneItem};
    use prismcast_core::source::{Source, SourceKind};

    fn snapshot_with(collection: SceneCollection) -> CollectionSnapshot {
        CollectionSnapshot {
            collection,
            session: SessionState::default(),
        }
    }

    #[test]
    fn valid_collection_passes() {
        let mut collection = SceneCollection::new("c");
        let source = Source::new(SourceKind::Color, "color");
        let mut scene = Scene::new("s");
        scene.add_item(SceneItem::new(source.id, 0));
        collection.audio.routes.push(AudioRoute {
            source_id: source.id,
            bus_id: collection.audio.buses[0].id,
            tracks: TrackMask::stereo_pair(),
        });
        collection
            .audio
            .mixer
            .insert(source.id, AudioMixerState::default());
        collection.sources.push(source);
        collection.scenes.push(scene);
        let snapshot = snapshot_with(collection);
        assert!(validate_collection(&snapshot).unwrap().is_empty());
    }

    #[test]
    fn dangling_item_source_is_corruption() {
        let mut collection = SceneCollection::new("c");
        let mut scene = Scene::new("s");
        scene.add_item(SceneItem::new(SourceId::new(), 0));
        collection.scenes.push(scene);
        let snapshot = snapshot_with(collection);
        assert!(validate_collection(&snapshot).is_err());
    }

    #[test]
    fn dangling_session_refs_warn_not_corrupt() {
        let mut collection = SceneCollection::new("c");
        collection.scenes.push(Scene::new("s"));
        let mut snapshot = snapshot_with(collection);
        snapshot.session.current_scene = Some(SceneId::new());
        let warnings = validate_collection(&snapshot).unwrap();
        assert_eq!(warnings.len(), 1);
        repair_session(&mut snapshot);
        assert_eq!(snapshot.session.current_scene, None);
    }
}
