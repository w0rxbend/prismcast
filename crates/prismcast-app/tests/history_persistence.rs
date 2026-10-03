//! History replay persists its actual edit families, while history itself is transient.

use std::path::PathBuf;

use prismcast_app::persistence::actor::{PersistenceConfig, PersistenceHandle};
use prismcast_app::persistence::paths::ConfigRoot;
use prismcast_app::persistence::store::ProjectStore;
use prismcast_app::{AppHandle, CoreConfig};
use prismcast_core::output::{Output, OutputKind, ReconnectPolicy};
use prismcast_core::{AppState, Command, EncoderId};

async fn read_saved(root: PathBuf) -> (String, ReconnectPolicy) {
    tokio::task::spawn_blocking(move || {
        let store = ProjectStore::new(ConfigRoot::new(root));
        let collection = store.load_collection("default").unwrap().value.snapshot;
        let profile = store.load_profile("default").unwrap().value.snapshot;
        (
            collection.collection.scenes[0].name.clone(),
            profile.outputs[0].reconnect_policy,
        )
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn canonical_history_replay_persists_both_collection_and_profile_edits() {
    let dir = tempfile::tempdir().unwrap();
    let persistence = PersistenceHandle::spawn(PersistenceConfig::new(ConfigRoot::new(dir.path())));
    let app = AppHandle::spawn_with_persistence(
        AppState::new(),
        CoreConfig::default(),
        persistence.clone(),
    );
    app.dispatch(Command::AddScene {
        name: "Before".into(),
    })
    .await
    .unwrap();
    let scene_id = app.snapshot().current_scene().unwrap();
    let output = Output::new(OutputKind::Recording, "Recording", EncoderId::new());
    let output_id = output.id;
    let original_policy = output.reconnect_policy;
    app.dispatch(Command::AddOutput { output }).await.unwrap();
    persistence.save_now().await.unwrap();
    assert_eq!(
        read_saved(dir.path().to_path_buf()).await,
        ("Before".into(), original_policy)
    );

    let changed_policy = ReconnectPolicy {
        max_retries: 2,
        initial_backoff_ms: 500,
        max_backoff_ms: 1_000,
    };
    app.dispatch(Command::Transaction {
        commands: vec![
            Command::RenameScene {
                scene_id,
                name: "After".into(),
            },
            Command::SetOutputReconnectPolicy {
                output_id,
                policy: changed_policy,
            },
        ],
    })
    .await
    .unwrap();
    persistence.save_now().await.unwrap();
    assert_eq!(
        read_saved(dir.path().to_path_buf()).await,
        ("After".into(), changed_policy)
    );

    assert_eq!(app.dispatch(Command::Undo).await.unwrap().label, "undo");
    persistence.save_now().await.unwrap();
    assert_eq!(
        read_saved(dir.path().to_path_buf()).await,
        ("Before".into(), original_policy)
    );
    assert_eq!(app.dispatch(Command::Redo).await.unwrap().label, "redo");
    persistence.save_now().await.unwrap();
    assert_eq!(
        read_saved(dir.path().to_path_buf()).await,
        ("After".into(), changed_policy)
    );

    // Spawning from the identical working state never restores session history.
    let restored =
        AppHandle::spawn_with_state(app.snapshot().state().clone(), CoreConfig::default());
    assert_eq!(restored.snapshot().state(), app.snapshot().state());
    assert!(!restored.snapshot().history().can_undo());
    assert!(!restored.snapshot().history().can_redo());
    restored.shutdown().await;
    restored.closed().await;
    app.shutdown().await;
    app.closed().await;
    persistence.shutdown().await.unwrap();
}
