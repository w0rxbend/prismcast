//! Canonical history dispatch, presentation, atomic replay and capture consent.
use prismcast_app::{AppHandle, CoreConfig, HandleError, Permission, Permissions};
use prismcast_core::{
    AppState, CaptureStatus, Command, Error, Source, SourceDimensions, SourceKind,
};
use serde_json::json;
use std::sync::Arc;

fn fixture(config: CoreConfig) -> (AppHandle, prismcast_core::SceneId, prismcast_core::SourceId) {
    let mut state = AppState::new();
    state
        .apply(&Command::AddScene {
            name: "original".into(),
        })
        .unwrap();
    let scene = state.current_scene.unwrap();
    let mut source = Source::new(SourceKind::TestPattern, "tone");
    source.settings = json!({"audio_test":true});
    let id = source.id;
    state.sources.insert(id, source);
    (AppHandle::spawn_with_state(state, config), scene, id)
}

#[tokio::test]
async fn canonical_history_enforces_actual_mixed_scopes_and_labels() {
    let (app, scene_id, source_id) = fixture(CoreConfig::default());
    assert!(!app.snapshot().history().can_undo());
    app.dispatch(Command::Transaction {
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
    })
    .await
    .unwrap();
    let before = app.snapshot();
    for permissions in [
        Permissions::none(),
        Permissions::read_only(),
        Permissions::of([Permission::ControlScenes]),
        Permissions::of([Permission::ControlAudio]),
    ] {
        assert!(matches!(
            app.dispatch_with_permissions(Command::Undo, permissions)
                .await,
            Err(HandleError::Core(Error::Unauthorized(_)))
        ));
        assert!(Arc::ptr_eq(&before, &app.snapshot()));
    }
    let both = Permissions::of([Permission::ControlScenes, Permission::ControlAudio]);
    let undone = app
        .dispatch_with_permissions(Command::Undo, both)
        .await
        .unwrap();
    assert_eq!(undone.label, "undo");
    assert_eq!(undone.events.len(), 2);
    assert_eq!(app.snapshot().scene(scene_id).unwrap().name, "original");
    assert_eq!(
        app.snapshot().history().redo_label.as_deref(),
        Some("transaction")
    );
    let before = app.snapshot();
    assert!(app
        .dispatch_with_permissions(Command::Redo, Permissions::of([Permission::ControlScenes]))
        .await
        .is_err());
    assert!(Arc::ptr_eq(&before, &app.snapshot()));
    assert_eq!(app.redo_with_permissions(both).await.unwrap().label, "redo");
    assert_eq!(app.snapshot().scene(scene_id).unwrap().name, "changed");
    assert!(app.snapshot().history().can_undo());
    assert!(!app.snapshot().history().can_redo());
    app.shutdown().await;
}

#[tokio::test]
async fn group_metadata_updates_preserve_revision_runtime_and_latest_meters() {
    let (app, scene_id, id) = fixture(CoreConfig::default());
    app.dispatch(Command::AddSource {
        kind: SourceKind::PipeWireAudioInput,
        name: "physical".into(),
    })
    .await
    .unwrap();
    let capture_id = app
        .snapshot()
        .sources()
        .find(|source| source.name == "physical")
        .unwrap()
        .id;
    app.dispatch(Command::SetSourceSettings {
        source_id: capture_id,
        settings: json!({"schema_version":1,"target":"node","mode":"input"}),
    })
    .await
    .unwrap();
    app.dispatch(Command::RenameScene {
        scene_id,
        name: "changed".into(),
    })
    .await
    .unwrap();
    let mut owner = app.attach_audio_owner().await.unwrap();
    app.dispatch(Command::AuthorizeSourceCapture {
        source_id: capture_id,
    })
    .await
    .unwrap();
    let request = owner.requests.recv().await.unwrap();
    owner
        .runtime
        .report_capture(capture_id, request.generation, CaptureStatus::Active, None)
        .await
        .unwrap();
    let capture_runtime = app.snapshot().source_runtime(capture_id).unwrap().clone();
    let revision = app.snapshot().revision();
    owner
        .runtime
        .report_levels(revision, id, vec![-6.0], vec![-9.0])
        .await
        .unwrap();
    let meters = app.subscribe_meters();
    let reading = meters.borrow().clone();
    let mut snapshots = app.subscribe_snapshots();
    let prior = app.snapshot();
    app.begin_transaction("gesture").await.unwrap();
    assert!(snapshots.has_changed().unwrap());
    let opened = snapshots.borrow_and_update().clone();
    assert_eq!(opened.revision(), revision);
    assert!(opened.history().group_open);
    assert_eq!(opened.source_runtime(capture_id), Some(&capture_runtime));
    assert!(!opened.history().can_undo());
    assert_eq!(opened.history().undo_label.as_deref(), Some("rename scene"));
    assert!(!prior.history().group_open);
    assert!(Arc::ptr_eq(&reading, &meters.borrow()));
    assert!(app.begin_transaction("nested").await.is_err());
    assert!(Arc::ptr_eq(&opened, &app.snapshot()));
    let other = app.new_controller();
    for history in [Command::Undo, Command::Redo] {
        assert!(other.dispatch(history).await.is_err());
        assert!(Arc::ptr_eq(&opened, &app.snapshot()));
        assert!(Arc::ptr_eq(&reading, &meters.borrow()));
    }
    assert!(other.end_transaction().await.is_err());
    assert!(Arc::ptr_eq(&opened, &app.snapshot()));
    app.end_transaction().await.unwrap();
    assert!(snapshots.has_changed().unwrap());
    assert_eq!(app.snapshot().revision(), revision);
    assert!(app.snapshot().history().can_undo());
    assert!(!app.snapshot().history().group_open);
    assert!(Arc::ptr_eq(&reading, &meters.borrow()));
    assert_eq!(
        app.snapshot().source_runtime(capture_id),
        Some(&capture_runtime)
    );
    owner
        .runtime
        .report_capture_levels(
            revision,
            request.generation,
            capture_id,
            vec![-6.0],
            vec![-9.0],
        )
        .await
        .unwrap();
    assert_eq!(app.snapshot().revision(), revision);

    app.shutdown().await;
}

#[tokio::test]
async fn failed_nested_history_and_failed_replay_preserve_snapshot_and_both_stacks() {
    let (app, scene_id, source_id) = fixture(CoreConfig::default());
    app.dispatch(Command::Transaction {
        commands: vec![
            Command::SetSourceMuted {
                source_id,
                muted: true,
            },
            Command::RenameScene {
                scene_id,
                name: "changed".into(),
            },
        ],
    })
    .await
    .unwrap();
    // Irreversible removal makes the grouped inverse fail after its first member
    // changed scratch state; authoritative state and stack must survive.
    app.dispatch(Command::RemoveSource { source_id })
        .await
        .unwrap();
    let before = app.snapshot();
    for _ in 0..2 {
        assert!(matches!(
            app.dispatch(Command::Undo).await,
            Err(HandleError::Core(Error::NotFound(_)))
        ));
        assert!(Arc::ptr_eq(&before, &app.snapshot()));
    }
    for history in [Command::Undo, Command::Redo] {
        assert!(app
            .dispatch(Command::Transaction {
                commands: vec![
                    Command::RenameScene {
                        scene_id,
                        name: "invalid".into()
                    },
                    Command::Transaction {
                        commands: vec![history]
                    },
                ]
            })
            .await
            .is_err());
        assert!(Arc::ptr_eq(&before, &app.snapshot()));
    }
    assert_eq!(before.scene(scene_id).unwrap().name, "changed");
    assert_eq!(before.history().undo_label.as_deref(), Some("transaction"));
    app.shutdown().await;
}

#[tokio::test]
async fn empty_history_noops_and_capacity_share_the_canonical_path() {
    let (app, scene_id, _) = fixture(CoreConfig {
        undo_capacity: 1,
        ..CoreConfig::default()
    });
    let initial = app.snapshot();
    for history in [Command::Undo, Command::Redo] {
        assert!(app.dispatch(history).await.is_err());
        assert!(Arc::ptr_eq(&initial, &app.snapshot()));
    }
    for name in ["one", "two"] {
        app.dispatch(Command::RenameScene {
            scene_id,
            name: name.into(),
        })
        .await
        .unwrap();
    }
    assert_eq!(app.dispatch(Command::Undo).await.unwrap().label, "undo");
    assert_eq!(app.snapshot().scene(scene_id).unwrap().name, "one");
    assert!(!app.snapshot().history().can_undo());
    // A domain no-op leaves the redo entry intact; compatibility wrappers
    // must consume the same entry and respond with the canonical label.
    app.dispatch(Command::SetSourceEnabled {
        source_id: app.snapshot().sources().next().unwrap().id,
        enabled: true,
    })
    .await
    .unwrap();
    assert!(app.snapshot().history().can_redo());
    assert_eq!(app.redo().await.unwrap().label, "redo");
    assert_eq!(app.undo().await.unwrap().label, "undo");
    app.shutdown().await;
}

#[tokio::test]
async fn capture_settings_and_enable_replay_never_reauthorize_audio_or_video() {
    for kind in [SourceKind::PipeWireAudioInput, SourceKind::PipeWireWindow] {
        let audio = kind == SourceKind::PipeWireAudioInput;
        let mut state = AppState::new();
        let mut source = Source::new(kind, "capture");
        source.settings = if audio {
            json!({"schema_version":1,"target":"old.node","mode":"input"})
        } else {
            json!({"old":true})
        };
        let id = source.id;
        let old_settings = source.settings.clone();
        state.sources.insert(id, source);
        let app = AppHandle::spawn_with_state(state, CoreConfig::default());
        let mut audio_owner = app.attach_audio_owner().await.unwrap();
        let mut video_owner = app.attach_capture_owner().await.unwrap();
        for edit in [
            Command::SetSourceSettings {
                source_id: id,
                settings: if audio {
                    json!({"schema_version":1,"target":"new.node","mode":"input"})
                } else {
                    json!({"new":true})
                },
            },
            Command::SetSourceEnabled {
                source_id: id,
                enabled: false,
            },
        ] {
            app.dispatch(edit).await.unwrap();
            app.dispatch(Command::Undo).await.unwrap();
            assert_eq!(app.snapshot().source(id).unwrap().settings, old_settings);
            assert!(app.snapshot().source(id).unwrap().enabled);
            app.dispatch(Command::AuthorizeSourceCapture { source_id: id })
                .await
                .unwrap();
            let generation = if audio {
                let request = audio_owner.requests.recv().await.unwrap();
                audio_owner
                    .runtime
                    .report_capture(id, request.generation, CaptureStatus::Active, None)
                    .await
                    .unwrap();
                request.generation
            } else {
                let request = video_owner.requests.recv().await.unwrap();
                video_owner
                    .runtime
                    .report(
                        id,
                        request.generation,
                        CaptureStatus::Active,
                        Some(SourceDimensions {
                            width: 640,
                            height: 480,
                        }),
                        None,
                    )
                    .await
                    .unwrap();
                request.generation
            };
            // Explicit authorization preserves the ordinary redo entry.
            assert!(app.snapshot().history().can_redo());
            app.dispatch(Command::Redo).await.unwrap();
            assert!(app.snapshot().source_runtime(id).is_none());
            app.dispatch(Command::Undo).await.unwrap();
            assert!(app.snapshot().source_runtime(id).is_none());
            assert!(audio_owner.requests.try_recv().is_err());
            assert!(video_owner.requests.try_recv().is_err());
            let stale = if audio {
                audio_owner
                    .runtime
                    .report_capture(id, generation, CaptureStatus::Active, None)
                    .await
            } else {
                video_owner
                    .runtime
                    .report(
                        id,
                        generation,
                        CaptureStatus::Active,
                        Some(SourceDimensions {
                            width: 640,
                            height: 480,
                        }),
                        None,
                    )
                    .await
            };
            assert!(stale.is_err());
        }
        app.shutdown().await;
    }
}
