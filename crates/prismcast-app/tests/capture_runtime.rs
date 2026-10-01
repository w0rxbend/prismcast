//! Command/effect interleaving, capability ownership and persistence isolation.
use prismcast_app::{AppHandle, CoreConfig, HandleError, Permission, Permissions};
use prismcast_core::{
    AppState, CaptureStatus, Command, Error, Event, Source, SourceDimensions, SourceEvent,
    SourceId, SourceKind,
};

fn fixture() -> (AppHandle, SourceId) {
    let mut state = AppState::new();
    let source = Source::new(SourceKind::PipeWireWindow, "window");
    let source_id = source.id;
    state.sources.insert(source_id, source);
    (
        AppHandle::spawn_with_state(state, CoreConfig::default()),
        source_id,
    )
}
fn pixels() -> Option<SourceDimensions> {
    Some(SourceDimensions {
        width: 6144,
        height: 3456,
    })
}

#[tokio::test]
async fn explicit_effect_publishes_before_delivery_and_reports_actual_caps() {
    let (app, source_id) = fixture();
    assert!(app.snapshot().source_runtime(source_id).is_none());
    assert!(app.authorize_source_capture(source_id, None).await.is_err());
    let mut owner = app.attach_capture_owner().await.unwrap();
    assert!(owner.requests.try_recv().is_err()); // restore/attach never opens a picker
    assert!(app.attach_capture_owner().await.is_err());
    let revision = app.snapshot().revision();
    assert!(matches!(
        app.dispatch_with_permissions(
            Command::AuthorizeSourceCapture { source_id },
            Permissions::from_iter([Permission::ControlAudio])
        )
        .await,
        Err(HandleError::Core(Error::Unauthorized(_)))
    ));
    assert!(app
        .authorize_source_capture(source_id, Some("wayland:".into()))
        .await
        .is_err());
    assert!(app
        .authorize_source_capture(source_id, Some(format!("wayland:{}", "x".repeat(2048))))
        .await
        .is_err());
    assert_eq!(app.snapshot().revision(), revision);
    assert!(owner.requests.try_recv().is_err());
    let response = app
        .authorize_source_capture(source_id, Some("wayland:local-parent".into()))
        .await
        .unwrap();
    assert!(response
        .events
        .iter()
        .any(|e| matches!(e, Event::Source(SourceEvent::RuntimeChanged { .. }))));
    let request = owner.requests.recv().await.unwrap();
    assert_eq!(
        request.parent_window.as_deref(),
        Some("wayland:local-parent")
    );
    assert!(!format!("{request:?}").contains("local-parent"));
    let snapshot = owner.snapshots.borrow().clone();
    assert_eq!(
        snapshot.source_runtime(source_id).unwrap().generation,
        request.generation
    );
    assert_eq!(
        snapshot.source_runtime(source_id).unwrap().status,
        CaptureStatus::Authorizing
    );
    owner
        .runtime
        .report(
            source_id,
            request.generation,
            CaptureStatus::Active,
            pixels(),
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        app.snapshot().source_runtime(source_id).unwrap().dimensions,
        pixels()
    );
    assert!(!serde_json::to_string(app.snapshot().state())
        .unwrap()
        .contains("source_runtime"));
    assert!(!serde_json::to_string(app.snapshot().state())
        .unwrap()
        .contains("local-parent"));
    let restored =
        AppHandle::spawn_with_state(app.snapshot().state().clone(), CoreConfig::default());
    assert!(restored.snapshot().source_runtime(source_id).is_none());
    let mut restored_owner = restored.attach_capture_owner().await.unwrap();
    assert!(restored_owner.requests.try_recv().is_err());
    restored.shutdown().await;
    app.shutdown().await;
}

#[tokio::test]
async fn retry_and_terminal_updates_are_generation_safe_and_caps_checked() {
    let (app, source_id) = fixture();
    let mut owner = app.attach_capture_owner().await.unwrap();
    app.authorize_source_capture(source_id, None).await.unwrap();
    let first = owner.requests.recv().await.unwrap();
    let revision = app.snapshot().revision();
    assert!(owner
        .runtime
        .report(
            source_id,
            first.generation,
            CaptureStatus::Active,
            None,
            None
        )
        .await
        .is_err());
    assert!(owner
        .runtime
        .report(
            source_id,
            first.generation,
            CaptureStatus::Active,
            Some(SourceDimensions {
                width: 0,
                height: 3456
            }),
            None
        )
        .await
        .is_err());
    assert!(owner
        .runtime
        .report(
            source_id,
            first.generation,
            CaptureStatus::Active,
            Some(SourceDimensions {
                width: 8193,
                height: 1
            }),
            None
        )
        .await
        .is_err());
    assert!(owner
        .runtime
        .report(
            source_id,
            first.generation,
            CaptureStatus::Authorizing,
            None,
            None
        )
        .await
        .is_err());
    assert_eq!(app.snapshot().revision(), revision);
    owner
        .runtime
        .report(
            source_id,
            first.generation,
            CaptureStatus::Cancelled,
            None,
            Some("User\ncancelled".repeat(100)),
        )
        .await
        .unwrap();
    let runtime = app.snapshot().source_runtime(source_id).unwrap().clone();
    assert_eq!(runtime.status, CaptureStatus::Cancelled);
    assert!(runtime.message.as_ref().unwrap().len() <= 512);
    assert!(!runtime.message.as_ref().unwrap().contains('\n'));
    assert!(owner
        .runtime
        .report(
            source_id,
            first.generation,
            CaptureStatus::Active,
            pixels(),
            None
        )
        .await
        .is_err());
    app.authorize_source_capture(source_id, None).await.unwrap();
    let retry = owner.requests.recv().await.unwrap();
    assert!(retry.generation > first.generation);
    let revision = app.snapshot().revision();
    assert!(owner
        .runtime
        .report(
            source_id,
            first.generation,
            CaptureStatus::Denied,
            None,
            None
        )
        .await
        .is_err());
    assert_eq!(app.snapshot().revision(), revision);
    owner
        .runtime
        .report(
            source_id,
            retry.generation,
            CaptureStatus::Active,
            pixels(),
            None,
        )
        .await
        .unwrap();
    owner
        .runtime
        .report(
            source_id,
            retry.generation,
            CaptureStatus::Revoked,
            None,
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        app.snapshot().source_runtime(source_id).unwrap().status,
        CaptureStatus::Revoked
    );
    app.shutdown().await;
}

#[tokio::test]
async fn full_or_disconnected_effect_receiver_fails_before_publication() {
    let (app, source_id) = fixture();
    let mut owner = app.attach_capture_owner().await.unwrap();
    for _ in 0..prismcast_app::capture::CAPTURE_CAPACITY {
        app.authorize_source_capture(source_id, None).await.unwrap();
    }
    let before = app.snapshot();
    assert!(app.authorize_source_capture(source_id, None).await.is_err());
    assert_eq!(app.snapshot().revision(), before.revision());
    assert_eq!(
        app.snapshot().source_runtime(source_id),
        before.source_runtime(source_id)
    );
    let latest = before.source_runtime(source_id).unwrap().generation;
    owner.requests.recv().await.unwrap();
    app.authorize_source_capture(source_id, None).await.unwrap();
    assert_eq!(
        app.snapshot()
            .source_runtime(source_id)
            .unwrap()
            .generation
            .value(),
        latest.value() + 1
    );
    let reporter = owner.runtime.clone();
    let generation = app.snapshot().source_runtime(source_id).unwrap().generation;
    drop(owner); // biased actor liveness observation precedes the queued old report
    assert!(matches!(
        reporter
            .report(source_id, generation, CaptureStatus::Active, pixels(), None)
            .await,
        Err(HandleError::Core(Error::Unauthorized(_)))
    ));
    assert_eq!(
        app.snapshot().source_runtime(source_id).unwrap().status,
        CaptureStatus::Failed
    );
    assert!(app.authorize_source_capture(source_id, None).await.is_err());
    let mut new_owner = app.attach_capture_owner().await.unwrap();
    app.authorize_source_capture(source_id, None).await.unwrap();
    let fresh = new_owner.requests.recv().await.unwrap();
    assert!(fresh.generation > generation);
    assert!(reporter
        .report(
            source_id,
            fresh.generation,
            CaptureStatus::Active,
            pixels(),
            None
        )
        .await
        .is_err());
    new_owner
        .runtime
        .report(
            source_id,
            fresh.generation,
            CaptureStatus::Active,
            pixels(),
            None,
        )
        .await
        .unwrap();
    app.shutdown().await;
}

#[tokio::test]
async fn source_disable_settings_removal_cancel_without_a_request_queue_slot() {
    let (app, source_id) = fixture();
    let mut owner = app.attach_capture_owner().await.unwrap();
    app.authorize_source_capture(source_id, None).await.unwrap();
    let first = owner.requests.recv().await.unwrap();
    owner
        .runtime
        .report(
            source_id,
            first.generation,
            CaptureStatus::Active,
            pixels(),
            None,
        )
        .await
        .unwrap();
    for _ in 0..prismcast_app::capture::CAPTURE_CAPACITY {
        app.authorize_source_capture(source_id, None).await.unwrap();
    }
    let generation = app.snapshot().source_runtime(source_id).unwrap().generation;
    app.dispatch(Command::SetSourceEnabled {
        source_id,
        enabled: false,
    })
    .await
    .unwrap();
    assert!(owner.snapshots.borrow().source_runtime(source_id).is_none());
    assert!(owner
        .runtime
        .report(source_id, generation, CaptureStatus::Active, pixels(), None)
        .await
        .is_err());
    assert!(app.authorize_source_capture(source_id, None).await.is_err());
    app.dispatch(Command::SetSourceEnabled {
        source_id,
        enabled: true,
    })
    .await
    .unwrap();
    assert!(app.snapshot().source_runtime(source_id).is_none());
    while owner.requests.try_recv().is_ok() {}
    app.authorize_source_capture(source_id, None).await.unwrap();
    let retry = owner.requests.recv().await.unwrap();
    assert!(retry.generation > generation);
    app.dispatch(Command::SetSourceSettings {
        source_id,
        settings: serde_json::json!({"changed":true}),
    })
    .await
    .unwrap();
    assert!(app.snapshot().source_runtime(source_id).is_none());
    assert!(owner
        .runtime
        .report(
            source_id,
            retry.generation,
            CaptureStatus::Active,
            pixels(),
            None
        )
        .await
        .is_err());
    app.authorize_source_capture(source_id, None).await.unwrap();
    let removed = owner.requests.recv().await.unwrap();
    app.dispatch(Command::RemoveSource { source_id })
        .await
        .unwrap();
    assert!(app.snapshot().source_runtime(source_id).is_none());
    assert!(owner
        .runtime
        .report(
            source_id,
            removed.generation,
            CaptureStatus::Active,
            pixels(),
            None
        )
        .await
        .is_err());
    app.shutdown().await;
}

#[tokio::test]
async fn capture_runtime_does_not_touch_undo_and_survives_profile_and_placement_changes() {
    let (app, source_id) = fixture();
    let mut owner = app.attach_capture_owner().await.unwrap();
    app.dispatch(Command::RenameSource {
        source_id,
        name: "renamed".into(),
    })
    .await
    .unwrap();
    app.undo().await.unwrap();
    app.authorize_source_capture(source_id, None).await.unwrap();
    let request = owner.requests.recv().await.unwrap();
    owner
        .runtime
        .report(
            source_id,
            request.generation,
            CaptureStatus::Active,
            pixels(),
            None,
        )
        .await
        .unwrap();
    app.redo().await.unwrap(); // runtime changes did not invalidate redo
    assert_eq!(app.snapshot().source(source_id).unwrap().name, "renamed");
    app.begin_transaction("rename group").await.unwrap();
    app.dispatch(Command::RenameSource {
        source_id,
        name: "grouped".into(),
    })
    .await
    .unwrap();
    owner
        .runtime
        .report(
            source_id,
            request.generation,
            CaptureStatus::Active,
            Some(SourceDimensions {
                width: 1920,
                height: 1080,
            }),
            None,
        )
        .await
        .unwrap();
    app.end_transaction().await.unwrap();
    app.undo().await.unwrap();
    assert_eq!(app.snapshot().source(source_id).unwrap().name, "renamed");
    app.dispatch(Command::AddScene {
        name: "scene".into(),
    })
    .await
    .unwrap();
    let scene_id = app.snapshot().scenes().next().unwrap().id;
    app.dispatch(Command::AddSceneItem {
        scene_id,
        source_id,
    })
    .await
    .unwrap();
    let item_id = app.snapshot().scene(scene_id).unwrap().items[0].id;
    app.dispatch(Command::SetSceneItemVisible {
        scene_id,
        item_id,
        visible: false,
    })
    .await
    .unwrap();
    let profile = prismcast_core::Profile::new(
        "canvas",
        prismcast_core::VideoConfig {
            width: 640,
            height: 480,
            fps_num: 30,
            fps_den: 1,
        },
    );
    let profile_id = profile.id;
    app.dispatch(Command::AddProfile { profile }).await.unwrap();
    app.dispatch(Command::SelectProfile { profile_id })
        .await
        .unwrap();
    let collection = prismcast_core::SceneCollection::new("pointer only");
    let collection_id = collection.id;
    app.dispatch(Command::AddSceneCollection { collection })
        .await
        .unwrap();
    app.dispatch(Command::SelectSceneCollection { collection_id })
        .await
        .unwrap();
    assert_eq!(
        app.snapshot().source_runtime(source_id).unwrap().generation,
        request.generation
    );
    assert_eq!(
        app.snapshot().source_runtime(source_id).unwrap().status,
        CaptureStatus::Active
    );
    assert!(owner.requests.try_recv().is_err());
    app.shutdown().await;
}

#[tokio::test]
async fn transactional_authorization_is_rejected_atomically_and_never_replayed() {
    let (app, source_id) = fixture();
    let mut owner = app.attach_capture_owner().await.unwrap();
    let revision = app.snapshot().revision();
    let response = app
        .dispatch(Command::Transaction {
            commands: vec![
                Command::RenameSource {
                    source_id,
                    name: "not committed".into(),
                },
                Command::Transaction {
                    commands: vec![Command::AuthorizeSourceCapture { source_id }],
                },
            ],
        })
        .await;
    assert!(response.is_err());
    assert_eq!(app.snapshot().revision(), revision);
    assert_eq!(app.snapshot().source(source_id).unwrap().name, "window");
    assert!(app.snapshot().source_runtime(source_id).is_none());
    assert!(owner.requests.try_recv().is_err());
    assert!(app.undo().await.is_err());
    app.shutdown().await;
}

#[tokio::test]
async fn receiver_close_revokes_capability_even_when_owner_value_remains_alive() {
    let (app, source_id) = fixture();
    let mut owner = app.attach_capture_owner().await.unwrap();
    app.authorize_source_capture(source_id, None).await.unwrap();
    let request = owner.requests.recv().await.unwrap();
    owner.requests.close();
    assert!(matches!(
        owner
            .runtime
            .report(
                source_id,
                request.generation,
                CaptureStatus::Active,
                pixels(),
                None
            )
            .await,
        Err(HandleError::Core(Error::Unauthorized(_)))
    ));
    assert_eq!(
        app.snapshot().source_runtime(source_id).unwrap().status,
        CaptureStatus::Failed
    );
    assert!(app.attach_capture_owner().await.is_ok());
    app.shutdown().await;
}

#[tokio::test]
async fn runtime_observations_are_bounded_and_non_capture_sources_reject_authorization() {
    let (app, source_id) = fixture();
    let mut owner = app.attach_capture_owner().await.unwrap();
    app.authorize_source_capture(source_id, None).await.unwrap();
    let first = owner.requests.recv().await.unwrap();
    owner
        .runtime
        .report(
            source_id,
            first.generation,
            CaptureStatus::Denied,
            None,
            None,
        )
        .await
        .unwrap();
    for i in 1..prismcast_app::capture::CAPTURE_CAPACITY {
        app.dispatch(Command::AddSource {
            kind: SourceKind::PipeWireDisplay,
            name: format!("screen {i}"),
        })
        .await
        .unwrap();
        let next = app
            .snapshot()
            .sources()
            .find(|s| s.name == format!("screen {i}"))
            .unwrap()
            .id;
        app.authorize_source_capture(next, None).await.unwrap();
        owner.requests.recv().await.unwrap();
    }
    app.dispatch(Command::AddSource {
        kind: SourceKind::PipeWireDisplay,
        name: "overflow".into(),
    })
    .await
    .unwrap();
    let overflow = app
        .snapshot()
        .sources()
        .find(|s| s.name == "overflow")
        .unwrap()
        .id;
    let revision = app.snapshot().revision();
    assert!(app.authorize_source_capture(overflow, None).await.is_err());
    assert_eq!(app.snapshot().revision(), revision);
    assert_eq!(
        app.snapshot().source_runtimes().count(),
        prismcast_app::capture::CAPTURE_CAPACITY
    );
    assert!(owner.requests.try_recv().is_err());
    app.dispatch(Command::SetSourceEnabled {
        source_id,
        enabled: false,
    })
    .await
    .unwrap();
    app.authorize_source_capture(overflow, None).await.unwrap();
    owner.requests.recv().await.unwrap();
    app.dispatch(Command::AddSource {
        kind: SourceKind::TestPattern,
        name: "generator".into(),
    })
    .await
    .unwrap();
    let generator = app
        .snapshot()
        .sources()
        .find(|s| s.name == "generator")
        .unwrap()
        .id;
    let revision = app.snapshot().revision();
    assert!(app.authorize_source_capture(generator, None).await.is_err());
    assert_eq!(app.snapshot().revision(), revision);
    app.shutdown().await;
}

#[tokio::test]
async fn v4l2_camera_authorization_reports_and_invalidation_match_portal_sources() {
    let mut state = AppState::new();
    let source = Source::new(SourceKind::V4l2Camera, "camera");
    let source_id = source.id;
    state.sources.insert(source_id, source);
    let app = AppHandle::spawn_with_state(state, CoreConfig::default());
    let mut owner = app.attach_capture_owner().await.unwrap();
    // Lease-free: no portal parent window is ever involved.
    let response = app.authorize_source_capture(source_id, None).await.unwrap();
    assert!(response
        .events
        .iter()
        .any(|e| matches!(e, Event::Source(SourceEvent::RuntimeChanged { .. }))));
    let request = owner.requests.recv().await.unwrap();
    assert_eq!(request.parent_window, None);
    assert_eq!(
        app.snapshot().source_runtime(source_id).unwrap().generation,
        request.generation
    );
    assert_eq!(
        app.snapshot().source_runtime(source_id).unwrap().status,
        CaptureStatus::Authorizing
    );
    owner
        .runtime
        .report(
            source_id,
            request.generation,
            CaptureStatus::Active,
            pixels(),
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        app.snapshot().source_runtime(source_id).unwrap().dimensions,
        pixels()
    );
    // A device change invalidates the active camera runtime (ADR-0017 rule).
    app.dispatch(Command::SetSourceSettings {
        source_id,
        settings: serde_json::json!({"device": "/dev/video1"}),
    })
    .await
    .unwrap();
    assert!(app.snapshot().source_runtime(source_id).is_none());
    assert!(owner
        .runtime
        .report(
            source_id,
            request.generation,
            CaptureStatus::Active,
            pixels(),
            None
        )
        .await
        .is_err());
    // Disable rejects re-authorization; re-enable admits a new generation.
    app.dispatch(Command::SetSourceEnabled {
        source_id,
        enabled: false,
    })
    .await
    .unwrap();
    assert!(app.authorize_source_capture(source_id, None).await.is_err());
    app.dispatch(Command::SetSourceEnabled {
        source_id,
        enabled: true,
    })
    .await
    .unwrap();
    assert!(app.snapshot().source_runtime(source_id).is_none());
    app.authorize_source_capture(source_id, None).await.unwrap();
    let retry = owner.requests.recv().await.unwrap();
    assert!(retry.generation > request.generation);
    // Removal clears the runtime; late reports cannot revive it.
    app.dispatch(Command::RemoveSource { source_id })
        .await
        .unwrap();
    assert!(app.snapshot().source_runtime(source_id).is_none());
    assert!(owner
        .runtime
        .report(
            source_id,
            retry.generation,
            CaptureStatus::Active,
            pixels(),
            None
        )
        .await
        .is_err());
    app.shutdown().await;
}
