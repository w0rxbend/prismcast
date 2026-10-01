//! CORE-004 integration tests: end-to-end persistence through the store and
//! through the wired core actor, tempdir-based
//! (`docs/architecture/persistence-model.md` §9).

use std::time::Duration;

use prismcast_app::actor::{AppHandle, CoreConfig};
use prismcast_app::persistence::actor::{PersistenceConfig, PersistenceEvent, PersistenceHandle};
use prismcast_app::persistence::envelope::{CollectionSnapshot, SessionState};
use prismcast_app::persistence::error::PersistenceError;
use prismcast_app::persistence::paths::{backup_path, ConfigRoot};
use prismcast_app::persistence::profile::ProfileSnapshot;
use prismcast_app::persistence::store::{ProjectStore, Recovery};
use prismcast_core::audio::TrackMask;
use prismcast_core::id::{EncoderId, SceneId, SourceId};
use prismcast_core::output::{
    EncoderSettings, Output, OutputKind, OutputState, SecretString, Service,
};
use prismcast_core::project::{Profile, VideoConfig};
use prismcast_core::source::SourceKind;
use prismcast_core::state::AppState;
use prismcast_core::Command;

/// Builds a state exercising scenes, sources, items, audio, transitions,
/// studio mode, and outputs.
fn full_state() -> AppState {
    let mut state = AppState::new();
    let apply = |state: &mut AppState, cmd: Command| state.apply(&cmd).expect("apply");

    let cam = match &apply(
        &mut state,
        Command::AddSource {
            kind: SourceKind::V4l2Camera,
            name: "Camera".into(),
        },
    )[0]
    {
        prismcast_core::Event::Source(prismcast_core::SourceEvent::Added { source }) => source.id,
        other => panic!("unexpected {other:?}"),
    };
    let pattern = match &apply(
        &mut state,
        Command::AddSource {
            kind: SourceKind::TestPattern,
            name: "Pattern".into(),
        },
    )[0]
    {
        prismcast_core::Event::Source(prismcast_core::SourceEvent::Added { source }) => source.id,
        other => panic!("unexpected {other:?}"),
    };
    let main = match &apply(
        &mut state,
        Command::AddScene {
            name: "Main".into(),
        },
    )[0]
    {
        prismcast_core::Event::Scene(prismcast_core::SceneEvent::Added { scene_id, .. }) => {
            *scene_id
        }
        other => panic!("unexpected {other:?}"),
    };
    apply(
        &mut state,
        Command::AddSceneItem {
            scene_id: main,
            source_id: cam,
        },
    );
    apply(
        &mut state,
        Command::AddSceneItem {
            scene_id: main,
            source_id: pattern,
        },
    );
    apply(
        &mut state,
        Command::SetSourceVolume {
            source_id: cam,
            volume_db: -6.0,
        },
    );
    let bus = state.audio.buses[0].id;
    apply(
        &mut state,
        Command::SetAudioRoute {
            source_id: cam,
            bus_id: bus,
            tracks: TrackMask::stereo_pair(),
        },
    );
    let intermission = match &apply(
        &mut state,
        Command::AddScene {
            name: "Intermission".into(),
        },
    )[0]
    {
        prismcast_core::Event::Scene(prismcast_core::SceneEvent::Added { scene_id, .. }) => {
            *scene_id
        }
        other => panic!("unexpected {other:?}"),
    };
    apply(
        &mut state,
        Command::SetCurrentScene {
            scene_id: intermission,
        },
    );
    apply(&mut state, Command::SetStudioModeEnabled { enabled: true });
    apply(&mut state, Command::SetPreviewScene { scene_id: main });

    let encoder = EncoderId::new();
    let mut output = Output::new(OutputKind::Rtmp, "Twitch main", encoder);
    output.reconnect_policy.max_retries = 3;
    apply(&mut state, Command::AddOutput { output });
    state
}

fn collection_snapshot(state: &AppState) -> CollectionSnapshot {
    let id = state.active_collection.unwrap();
    CollectionSnapshot {
        collection: prismcast_core::project::SceneCollection {
            id,
            name: state.collections[&id].name.clone(),
            scenes: state.scenes.values().cloned().collect(),
            sources: state.sources.values().cloned().collect(),
            transition: state.transition.clone(),
            audio: state.audio.clone(),
        },
        session: SessionState {
            current_scene: state.current_scene,
            studio_mode: state.studio_mode.clone(),
        },
    }
}

fn profile_snapshot(state: &AppState) -> ProfileSnapshot {
    let id = state.active_profile.unwrap();
    ProfileSnapshot {
        profile: state.profiles[&id].clone(),
        encoders: Vec::new(),
        services: Vec::new(),
        outputs: state.outputs.values().cloned().collect(),
    }
}

#[test]
fn full_state_save_load_identical() {
    let dir = tempfile::tempdir().unwrap();
    let store = ProjectStore::new(ConfigRoot::new(dir.path()));
    let state = full_state();

    let collection = collection_snapshot(&state);
    let saved = store
        .save_collection("dev-stream", &collection, None)
        .expect("save collection");
    let outcome = store
        .load_collection("dev-stream")
        .expect("load collection");
    assert_eq!(outcome.recovery, Recovery::Clean);
    assert!(!outcome.migrated);
    assert_eq!(outcome.value.snapshot, collection);
    assert_eq!(outcome.value.envelope, saved);

    let profile = profile_snapshot(&state);
    store
        .save_profile("twitch-1080p", &profile, None)
        .expect("save profile");
    let outcome = store.load_profile("twitch-1080p").expect("load profile");
    assert_eq!(outcome.recovery, Recovery::Clean);
    let mut expected = profile;
    for output in &mut expected.outputs {
        output.state = OutputState::Stopped;
    }
    assert_eq!(outcome.value.snapshot, expected);

    // Scenes/sources/items/audio/transitions/studio mode all survive.
    let loaded = outcome_collection_scenes(&store);
    assert_eq!(loaded.len(), 2);
}

fn outcome_collection_scenes(store: &ProjectStore) -> Vec<String> {
    store
        .load_collection("dev-stream")
        .unwrap()
        .value
        .snapshot
        .collection
        .scenes
        .iter()
        .map(|s| s.name.clone())
        .collect()
}

#[test]
fn studio_mode_and_current_scene_survive() {
    let dir = tempfile::tempdir().unwrap();
    let store = ProjectStore::new(ConfigRoot::new(dir.path()));
    let state = full_state();
    store
        .save_collection("c", &collection_snapshot(&state), None)
        .unwrap();
    let loaded = store.load_collection("c").unwrap().value.snapshot;
    assert_eq!(loaded.session.current_scene, state.current_scene);
    assert_eq!(loaded.session.studio_mode, state.studio_mode);
}

#[test]
fn corrupted_primary_recovers_from_backup() {
    let dir = tempfile::tempdir().unwrap();
    let store = ProjectStore::new(ConfigRoot::new(dir.path()));
    let state = full_state();
    let snapshot = collection_snapshot(&state);
    store.save_collection("c", &snapshot, None).unwrap();
    // A clean load refreshes the .bak from verified-good content.
    store.load_collection("c").unwrap();
    let primary = dir.path().join("collections/c/collection.json");
    let backup = backup_path(&primary);
    assert!(backup.exists(), ".bak refreshed from verified-good load");

    // Corrupt the primary.
    std::fs::write(&primary, b"{ not json !").unwrap();
    let outcome = store.load_collection("c").unwrap();
    match &outcome.recovery {
        Recovery::FromBackup { reason } => assert!(!reason.is_empty()),
        other => panic!("expected FromBackup, got {other:?}"),
    }
    assert_eq!(outcome.value.snapshot, snapshot);
    // The backup was restored over the primary.
    assert_eq!(
        std::fs::read(&primary).unwrap(),
        std::fs::read(&backup).unwrap()
    );
}

#[test]
fn corrupted_both_fall_back_to_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let store = ProjectStore::new(ConfigRoot::new(dir.path()));
    let state = full_state();
    store
        .save_collection("c", &collection_snapshot(&state), None)
        .unwrap();
    store.load_collection("c").unwrap();

    let primary = dir.path().join("collections/c/collection.json");
    std::fs::write(&primary, b"garbage").unwrap();
    std::fs::write(backup_path(&primary), b"also garbage").unwrap();

    let outcome = store.load_collection("c").unwrap();
    match &outcome.recovery {
        Recovery::Defaults { reason } => assert!(reason.is_some()),
        other => panic!("expected Defaults, got {other:?}"),
    }
    // Fresh defaults: empty default collection.
    assert!(outcome.value.snapshot.collection.scenes.is_empty());
    // Nothing was deleted: the corrupt evidence is still on disk.
    assert_eq!(std::fs::read(&primary).unwrap(), b"garbage");
}

#[test]
fn missing_files_yield_defaults_without_error() {
    let dir = tempfile::tempdir().unwrap();
    let store = ProjectStore::new(ConfigRoot::new(dir.path()));
    let outcome = store.load_collection("never-existed").unwrap();
    assert_eq!(outcome.recovery, Recovery::Defaults { reason: None });
}

#[test]
fn newer_schema_is_typed_error_and_file_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let store = ProjectStore::new(ConfigRoot::new(dir.path()));
    let state = full_state();
    store
        .save_collection("c", &collection_snapshot(&state), None)
        .unwrap();
    let primary = dir.path().join("collections/c/collection.json");
    let mut doc: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&primary).unwrap()).unwrap();
    doc["schemaVersion"] = serde_json::json!(2);
    let bumped = serde_json::to_vec_pretty(&doc).unwrap();
    std::fs::write(&primary, &bumped).unwrap();

    let err = store.load_collection("c").unwrap_err();
    assert!(matches!(
        err,
        PersistenceError::NewerSchema {
            found: 2,
            supported: 1,
            ..
        }
    ));
    // The file is left byte-identical and no fallback/restore happened.
    assert_eq!(std::fs::read(&primary).unwrap(), bumped);
}

#[test]
fn broken_referential_integrity_counts_as_corruption() {
    let dir = tempfile::tempdir().unwrap();
    let store = ProjectStore::new(ConfigRoot::new(dir.path()));
    let state = full_state();
    store
        .save_collection("c", &collection_snapshot(&state), None)
        .unwrap();
    store.load_collection("c").unwrap(); // refresh .bak

    let primary = dir.path().join("collections/c/collection.json");
    let mut doc: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&primary).unwrap()).unwrap();
    // Point a scene item at a source that does not exist.
    doc["scenes"][0]["items"][0]["source_id"] = serde_json::json!(SourceId::new().to_string());
    std::fs::write(&primary, serde_json::to_vec_pretty(&doc).unwrap()).unwrap();

    let outcome = store.load_collection("c").unwrap();
    assert!(matches!(outcome.recovery, Recovery::FromBackup { .. }));
}

#[test]
fn leftover_temp_files_are_reaped_on_load() {
    let dir = tempfile::tempdir().unwrap();
    let store = ProjectStore::new(ConfigRoot::new(dir.path()));
    let state = full_state();
    store
        .save_collection("c", &collection_snapshot(&state), None)
        .unwrap();

    // Simulate a crashed writer: a stale temp file next to the primary.
    let temp = dir.path().join("collections/c/.collection.json.tmp-1-1");
    std::fs::write(&temp, b"partial").unwrap();
    // Backdate it so it counts as from a previous process.
    // (No filetime dep: call the reaper through a load after touching mtime
    // via `touch -d`.)
    let status = std::process::Command::new("touch")
        .arg("-d")
        .arg("2000-01-01")
        .arg(&temp)
        .status()
        .unwrap();
    assert!(status.success());

    let outcome = store.load_collection("c").unwrap();
    assert_eq!(outcome.recovery, Recovery::Clean);
    assert!(!temp.exists(), "stale temp file reaped on load");
}

#[test]
fn crash_mid_write_leaves_old_file_intact() {
    // Simulated kill -9 during the write window: the temp file exists but
    // the rename never happened. The primary must still hold the old,
    // complete content.
    let dir = tempfile::tempdir().unwrap();
    let store = ProjectStore::new(ConfigRoot::new(dir.path()));
    let state = full_state();
    let snapshot = collection_snapshot(&state);
    store.save_collection("c", &snapshot, None).unwrap();
    let primary = dir.path().join("collections/c/collection.json");
    let before = std::fs::read(&primary).unwrap();

    // A half-written temp file from the "crashed" writer.
    let temp = dir.path().join("collections/c/.collection.json.tmp-4242-0");
    std::fs::write(&temp, &before[..before.len() / 2]).unwrap();

    let outcome = store.load_collection("c").unwrap();
    // Temp files of *this* process's lifetime are not reaped by load, but
    // they are ignored: the primary parses fine.
    assert_eq!(outcome.recovery, Recovery::Clean);
    assert_eq!(std::fs::read(&primary).unwrap(), before);
    assert_eq!(outcome.value.snapshot, snapshot);
}

#[test]
fn unknown_fields_survive_file_level_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let store = ProjectStore::new(ConfigRoot::new(dir.path()));
    let state = full_state();
    store
        .save_collection("c", &collection_snapshot(&state), None)
        .unwrap();

    let primary = dir.path().join("collections/c/collection.json");
    let mut doc: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&primary).unwrap()).unwrap();
    doc["x-future-feature"] = serde_json::json!({"enabled": true});
    doc["scenes"][0]["x-future-scene-key"] = serde_json::json!(123);
    std::fs::write(&primary, serde_json::to_vec_pretty(&doc).unwrap()).unwrap();

    // Load (retaining the envelope), then save with modified state.
    let loaded = store.load_collection("c").unwrap();
    assert_eq!(loaded.recovery, Recovery::Clean);
    store
        .save_collection("c", &loaded.value.snapshot, Some(&loaded.value.envelope))
        .unwrap();

    let resaved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&primary).unwrap()).unwrap();
    assert_eq!(
        resaved["x-future-feature"],
        serde_json::json!({"enabled": true})
    );
    assert_eq!(
        resaved["scenes"][0]["x-future-scene-key"],
        serde_json::json!(123)
    );
}

#[cfg(unix)]
#[test]
fn profile_files_are_created_0600() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let store = ProjectStore::new(ConfigRoot::new(dir.path()));
    let mut profile = Profile::new("p", VideoConfig::default());
    profile.settings = serde_json::Value::Null;
    let service_id = prismcast_core::id::ServiceId::new();
    let snapshot = ProfileSnapshot {
        profile,
        encoders: vec![EncoderSettings {
            id: EncoderId::new(),
            codec: "h264".into(),
            bitrate_kbps: 6000,
            keyframe_interval: None,
            settings: serde_json::Value::Null,
        }],
        services: vec![Service {
            id: service_id,
            name: "Twitch".into(),
            url: "rtmps://live.twitch.tv/app".into(),
            key: SecretString::new("live_key_abc"),
            settings: serde_json::Value::Null,
        }],
        outputs: Vec::new(),
    };
    store.save_profile("p", &snapshot, None).unwrap();
    let path = dir.path().join("profiles/p/profile.toml");
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "profile.toml must be 0600 (stream keys)");

    // SecretString roundtrips through the TOML file.
    let loaded = store.load_profile("p").unwrap();
    assert_eq!(
        loaded.value.snapshot.services[0].key,
        SecretString::new("live_key_abc")
    );
}

#[test]
fn pointer_file_selects_active_profile_and_collection() {
    let dir = tempfile::tempdir().unwrap();
    let store = ProjectStore::new(ConfigRoot::new(dir.path()));
    let state = full_state();
    store
        .save_collection("dev-stream", &collection_snapshot(&state), None)
        .unwrap();
    store
        .save_profile("twitch-1080p", &profile_snapshot(&state), None)
        .unwrap();

    let pointer = prismcast_app::persistence::pointer::PointerState {
        active_profile: Some("twitch-1080p".into()),
        active_collection: Some("dev-stream".into()),
    };
    store.save_pointer(&pointer, None).unwrap();

    let loaded = store.load_pointer();
    assert_eq!(loaded.recovery, Recovery::Clean);
    assert_eq!(loaded.value, pointer);

    // Fallback: corrupt pointer recovers trivially to an empty selection.
    std::fs::write(dir.path().join("prismcast.toml"), b"not toml {{{").unwrap();
    let loaded = store.load_pointer();
    assert!(matches!(loaded.recovery, Recovery::Defaults { .. }));
    assert_eq!(
        loaded.value,
        prismcast_app::persistence::pointer::PointerState::default()
    );
    // And the caller can fall back to the first entity on disk.
    assert_eq!(store.list_profile_slugs(), vec!["twitch-1080p".to_string()]);
    assert_eq!(
        store.list_collection_slugs(),
        vec!["dev-stream".to_string()]
    );
}

#[tokio::test]
async fn core_actor_drives_persistence_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = PersistenceConfig::new(ConfigRoot::new(dir.path()));
    config.debounce = Duration::from_millis(50);
    let persistence = PersistenceHandle::spawn(config);
    let mut events = persistence.subscribe();

    let app = AppHandle::spawn_with_persistence(
        AppState::new(),
        CoreConfig::default(),
        persistence.clone(),
    );

    // 100 rapid collection-dirty commands: must coalesce to 1-2 writes.
    let mut scene_id: Option<SceneId> = None;
    for i in 0..10 {
        let response = app
            .dispatch(Command::AddScene {
                name: format!("scene {i}"),
            })
            .await
            .expect("dispatch");
        if scene_id.is_none() {
            scene_id = match &response.events[0] {
                prismcast_core::Event::Scene(prismcast_core::SceneEvent::Added {
                    scene_id,
                    ..
                }) => Some(*scene_id),
                _ => None,
            };
        }
    }
    for i in 0..90 {
        app.dispatch(Command::RenameScene {
            scene_id: scene_id.unwrap(),
            name: format!("renamed {i}"),
        })
        .await
        .expect("dispatch");
    }
    // One profile-dirty command (output graph config is profile data).
    app.dispatch(Command::AddOutput {
        output: Output::new(OutputKind::Recording, "rec", EncoderId::new()),
    })
    .await
    .ok();

    app.shutdown().await; // flushes persistence
    persistence.shutdown().await.expect("shutdown");

    let mut collection_saves = 0;
    let mut profile_saves = 0;
    while let Ok(event) = events.try_recv() {
        match event {
            PersistenceEvent::Saved { path } if path.ends_with("collection.json") => {
                collection_saves += 1
            }
            PersistenceEvent::Saved { path } if path.ends_with("profile.toml") => {
                profile_saves += 1
            }
            _ => {}
        }
    }
    assert!(
        (1..=3).contains(&collection_saves),
        "100 rapid commands coalesced into {collection_saves} collection writes"
    );
    assert!(profile_saves <= 1, "profile writes: {profile_saves}");

    // The persisted collection reflects the final state.
    let store = ProjectStore::new(ConfigRoot::new(dir.path()));
    let outcome = store
        .load_collection("default")
        .expect("load default collection");
    assert_eq!(outcome.value.snapshot.collection.scenes.len(), 10);
    assert_eq!(
        outcome.value.snapshot.collection.scenes[0].name,
        "renamed 89"
    );
}
