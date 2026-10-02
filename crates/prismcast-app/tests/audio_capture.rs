//! Explicit capture consent, frozen effects and native-generation meter ingress.

use prismcast_app::{
    AppHandle, AudioCaptureAuthorizationRequest, AudioOwner, CoreConfig, HandleError, Permission,
    Permissions,
};
use prismcast_core::{
    AppState, CaptureGeneration, CaptureStatus, Command, Error, Event, PipeWireAudioMode, Source,
    SourceDimensions, SourceEvent, SourceId, SourceKind,
};
use serde_json::json;
use std::sync::Arc;

fn source(kind: SourceKind, mode: &str, name: &str) -> Source {
    let mut source = Source::new(kind, name);
    source.settings = json!({"schema_version":1,"target":format!("selected.{name}"),"mode":mode});
    source
}

fn fixture() -> (AppHandle, SourceId, SourceId) {
    let mut state = AppState::new();
    let mic = source(SourceKind::PipeWireAudioInput, "input", "microphone");
    let video = Source::new(SourceKind::PipeWireWindow, "window");
    let ids = (mic.id, video.id);
    state.sources.insert(mic.id, mic);
    state.sources.insert(video.id, video);
    (
        AppHandle::spawn_with_state(state, CoreConfig::default()),
        ids.0,
        ids.1,
    )
}

async fn authorize(
    app: &AppHandle,
    owner: &mut AudioOwner,
    id: SourceId,
) -> AudioCaptureAuthorizationRequest {
    app.dispatch(Command::AuthorizeSourceCapture { source_id: id })
        .await
        .unwrap();
    owner.requests.recv().await.unwrap()
}

async fn active(app: &AppHandle, owner: &mut AudioOwner, id: SourceId) -> CaptureGeneration {
    let request = authorize(app, owner, id).await;
    owner
        .runtime
        .report_capture(id, request.generation, CaptureStatus::Active, None)
        .await
        .unwrap();
    request.generation
}

async fn levels(
    app: &AppHandle,
    owner: &AudioOwner,
    id: SourceId,
    generation: CaptureGeneration,
) -> Result<(), HandleError> {
    owner
        .runtime
        .report_capture_levels(
            app.snapshot().revision(),
            generation,
            id,
            vec![-6.0, -6.0],
            vec![-9.0, -9.0],
        )
        .await
}

#[tokio::test]
async fn explicit_audio_commands_route_frozen_settings_and_preserve_permission_policy() {
    let (app, id, _) = fixture();
    let mut video = app.attach_capture_owner().await.unwrap();
    assert!(app
        .dispatch(Command::AuthorizeSourceCapture { source_id: id })
        .await
        .is_err());
    let mut owner = app.attach_audio_owner().await.unwrap();
    assert!(owner.requests.try_recv().is_err());
    assert!(video.requests.try_recv().is_err());
    let revision = app.snapshot().revision();
    assert!(matches!(
        app.dispatch_with_permissions(
            Command::AuthorizeSourceCapture { source_id: id },
            Permissions::from_iter([Permission::ControlAudio])
        )
        .await,
        Err(HandleError::Core(Error::Unauthorized(_)))
    ));
    assert_eq!(app.snapshot().revision(), revision);
    assert!(owner.requests.try_recv().is_err());
    app.dispatch_with_permissions(
        Command::AuthorizeSourceCapture { source_id: id },
        Permissions::from_iter([Permission::ControlScenes]),
    )
    .await
    .unwrap();
    let request = owner.requests.recv().await.unwrap();
    assert_eq!(request.settings.target, "selected.microphone");
    assert_eq!(request.settings.mode, PipeWireAudioMode::Input);
    let snapshot = owner.snapshots.borrow().clone();
    assert_eq!(
        snapshot.source_runtime(id).unwrap().generation,
        request.generation
    );
    assert_eq!(
        snapshot.source_runtime(id).unwrap().status,
        CaptureStatus::Authorizing
    );
    assert!(video.requests.try_recv().is_err());
    owner
        .runtime
        .report_capture(id, request.generation, CaptureStatus::Active, None)
        .await
        .unwrap();
    assert_eq!(app.snapshot().source_runtime(id).unwrap().dimensions, None);
    assert!(video
        .runtime
        .report(
            id,
            request.generation,
            CaptureStatus::Active,
            Some(SourceDimensions {
                width: 64,
                height: 64
            }),
            None
        )
        .await
        .is_err());
    app.shutdown().await;
}

#[tokio::test]
async fn microphone_sink_monitor_and_application_modes_use_existing_source_kinds() {
    let mut state = AppState::new();
    let mut ids = Vec::new();
    for (kind, mode) in [
        (SourceKind::PipeWireAudioInput, "input"),
        (SourceKind::PipeWireAudioInput, "output"),
        (SourceKind::PipeWireAppAudio, "application"),
    ] {
        let source = source(kind, mode, mode);
        ids.push(source.id);
        state.sources.insert(source.id, source);
    }
    let app = AppHandle::spawn_with_state(state, CoreConfig::default());
    let mut owner = app.attach_audio_owner().await.unwrap();
    for id in ids {
        let generation = active(&app, &mut owner, id).await;
        levels(&app, &owner, id, generation).await.unwrap();
        assert_eq!(app.snapshot().source_runtime(id).unwrap().dimensions, None);
    }
    app.shutdown().await;
}

#[tokio::test]
async fn invalid_settings_and_disabled_sources_publish_neither_runtime_nor_effect() {
    let (app, id, _) = fixture();
    let mut owner = app.attach_audio_owner().await.unwrap();
    for settings in [
        json!({}),
        json!({"schema_version":2,"target":"node","mode":"input"}),
        json!({"schema_version":1,"target":"","mode":"input"}),
        json!({"schema_version":1,"target":"node","mode":"application"}),
        json!({"schema_version":1,"target":"node","mode":"input","object_serial":42}),
    ] {
        app.dispatch(Command::SetSourceSettings {
            source_id: id,
            settings,
        })
        .await
        .unwrap();
        let before = app.snapshot();
        assert!(app
            .dispatch(Command::AuthorizeSourceCapture { source_id: id })
            .await
            .is_err());
        assert!(Arc::ptr_eq(&before, &app.snapshot()));
        assert!(app.snapshot().source_runtime(id).is_none());
        assert!(owner.requests.try_recv().is_err());
    }
    app.dispatch(Command::SetSourceSettings {
        source_id: id,
        settings: json!({"schema_version":1,"target":"node","mode":"input"}),
    })
    .await
    .unwrap();
    app.dispatch(Command::SetSourceEnabled {
        source_id: id,
        enabled: false,
    })
    .await
    .unwrap();
    assert!(app
        .dispatch(Command::AuthorizeSourceCapture { source_id: id })
        .await
        .is_err());
    assert!(owner.requests.try_recv().is_err());
    app.shutdown().await;
}

#[tokio::test]
async fn physical_meter_ingress_requires_active_generation_and_exact_revision() {
    let (app, id, _) = fixture();
    let mut owner = app.attach_audio_owner().await.unwrap();
    assert!(levels(&app, &owner, id, CaptureGeneration::new(1))
        .await
        .is_err());
    let first = authorize(&app, &mut owner, id).await;
    assert!(levels(&app, &owner, id, first.generation).await.is_err());
    assert!(owner
        .runtime
        .report_capture(id, first.generation, CaptureStatus::Authorizing, None)
        .await
        .is_err());
    owner
        .runtime
        .report_capture(id, first.generation, CaptureStatus::Active, None)
        .await
        .unwrap();
    let before = app.snapshot();
    assert!(owner
        .runtime
        .report_levels(before.revision(), id, vec![-6.0], vec![-9.0])
        .await
        .is_err());
    levels(&app, &owner, id, first.generation).await.unwrap();
    assert!(Arc::ptr_eq(&before, &app.snapshot()));
    app.dispatch(Command::SetSourceVolume {
        source_id: id,
        volume_db: -12.0,
    })
    .await
    .unwrap();
    assert!(app.subscribe_meters().borrow().levels.is_empty());
    assert!(owner
        .runtime
        .report_capture_levels(
            before.revision(),
            first.generation,
            id,
            vec![-6.0],
            vec![-9.0]
        )
        .await
        .is_err());
    levels(&app, &owner, id, first.generation).await.unwrap();
    let retry = authorize(&app, &mut owner, id).await;
    assert!(retry.generation > first.generation);
    assert!(levels(&app, &owner, id, first.generation).await.is_err());
    assert!(levels(&app, &owner, id, retry.generation).await.is_err());
    assert!(owner
        .runtime
        .report_capture(id, first.generation, CaptureStatus::Active, None)
        .await
        .is_err());
    owner
        .runtime
        .report_capture(id, retry.generation, CaptureStatus::Active, None)
        .await
        .unwrap();
    levels(&app, &owner, id, retry.generation).await.unwrap();
    owner
        .runtime
        .report_capture(
            id,
            retry.generation,
            CaptureStatus::Revoked,
            Some("node\ndisappeared".repeat(100)),
        )
        .await
        .unwrap();
    let runtime = app.snapshot().source_runtime(id).unwrap().clone();
    assert!(runtime.message.as_ref().unwrap().len() <= 512);
    assert!(!runtime.message.as_ref().unwrap().contains('\n'));
    assert!(levels(&app, &owner, id, retry.generation).await.is_err());
    assert!(owner
        .runtime
        .report_capture(id, retry.generation, CaptureStatus::Active, None)
        .await
        .is_err());
    app.shutdown().await;
}

#[tokio::test]
async fn restored_settings_enable_and_mixer_commands_never_authorize() {
    let (app, id, _) = fixture();
    let mut owner = app.attach_audio_owner().await.unwrap();
    let generation = active(&app, &mut owner, id).await;
    levels(&app, &owner, id, generation).await.unwrap();
    for command in [
        Command::SetSourceMuted {
            source_id: id,
            muted: true,
        },
        Command::SetSourceVolume {
            source_id: id,
            volume_db: -20.0,
        },
        Command::SetSourceSolo {
            source_id: id,
            solo: true,
        },
    ] {
        app.dispatch(command).await.unwrap();
        assert_eq!(
            app.snapshot().source_runtime(id).unwrap().generation,
            generation
        );
        assert!(owner.requests.try_recv().is_err());
    }
    app.dispatch(Command::SetSourceSettings {
        source_id: id,
        settings: json!({"schema_version":1,"target":"changed.target","mode":"input"}),
    })
    .await
    .unwrap();
    assert!(app.snapshot().source_runtime(id).is_none());
    assert!(levels(&app, &owner, id, generation).await.is_err());
    assert!(owner.requests.try_recv().is_err());
    let request = authorize(&app, &mut owner, id).await;
    assert_eq!(request.settings.target, "changed.target");
    app.dispatch(Command::SetSourceSettings {
        source_id: id,
        settings: json!({"schema_version":1,"target":"next.target","mode":"input"}),
    })
    .await
    .unwrap();
    assert_eq!(
        request.settings.target, "changed.target",
        "effect keeps its original target"
    );
    assert!(owner
        .runtime
        .report_capture(id, request.generation, CaptureStatus::Active, None)
        .await
        .is_err());
    let generation = active(&app, &mut owner, id).await;
    app.dispatch(Command::SetSourceEnabled {
        source_id: id,
        enabled: false,
    })
    .await
    .unwrap();
    app.dispatch(Command::SetSourceEnabled {
        source_id: id,
        enabled: true,
    })
    .await
    .unwrap();
    assert!(app.snapshot().source_runtime(id).is_none());
    assert!(owner.requests.try_recv().is_err());
    assert!(levels(&app, &owner, id, generation).await.is_err());
    let restored =
        AppHandle::spawn_with_state(app.snapshot().state().clone(), CoreConfig::default());
    let mut restored_owner = restored.attach_audio_owner().await.unwrap();
    assert!(restored.snapshot().source_runtime(id).is_none());
    assert!(restored_owner.requests.try_recv().is_err());
    app.dispatch(Command::RemoveSource { source_id: id })
        .await
        .unwrap();
    assert!(app.snapshot().source_runtime(id).is_none());
    assert!(owner
        .runtime
        .report_capture(id, generation, CaptureStatus::Active, None)
        .await
        .is_err());
    restored.shutdown().await;
    app.shutdown().await;
}

#[tokio::test]
async fn owner_loss_and_receiver_close_are_isolated_by_capture_family() {
    let (app, id, video_id) = fixture();
    let mut owner = app.attach_audio_owner().await.unwrap();
    let mut video = app.attach_capture_owner().await.unwrap();
    let generation = active(&app, &mut owner, id).await;
    app.dispatch(Command::AuthorizeSourceCapture {
        source_id: video_id,
    })
    .await
    .unwrap();
    let video_request = video.requests.recv().await.unwrap();
    video
        .runtime
        .report(
            video_id,
            video_request.generation,
            CaptureStatus::Active,
            Some(SourceDimensions {
                width: 64,
                height: 64,
            }),
            None,
        )
        .await
        .unwrap();
    assert!(owner
        .runtime
        .report_capture(
            video_id,
            video_request.generation,
            CaptureStatus::Active,
            None
        )
        .await
        .is_err());
    levels(&app, &owner, id, generation).await.unwrap();
    owner.requests.close();
    assert!(matches!(
        owner
            .runtime
            .report_capture(id, generation, CaptureStatus::Active, None)
            .await,
        Err(HandleError::Core(Error::Unauthorized(_)))
    ));
    assert_eq!(
        app.snapshot().source_runtime(id).unwrap().status,
        CaptureStatus::Failed
    );
    assert_eq!(
        app.snapshot().source_runtime(video_id).unwrap().status,
        CaptureStatus::Active
    );
    assert!(app.subscribe_meters().borrow().levels.is_empty());
    let mut replacement = app.attach_audio_owner().await.unwrap();
    let generation = active(&app, &mut replacement, id).await;
    let video_reporter = video.runtime.clone();
    drop(video);
    assert!(video_reporter
        .report(
            video_id,
            video_request.generation,
            CaptureStatus::Active,
            Some(SourceDimensions {
                width: 64,
                height: 64
            }),
            None
        )
        .await
        .is_err());
    assert_eq!(
        app.snapshot().source_runtime(video_id).unwrap().status,
        CaptureStatus::Failed
    );
    assert_eq!(
        app.snapshot().source_runtime(id).unwrap().status,
        CaptureStatus::Active
    );
    levels(&app, &replacement, id, generation).await.unwrap();
    assert!(owner.runtime.clear_levels().await.is_err());
    assert_eq!(app.subscribe_meters().borrow().levels.len(), 1);
    app.shutdown().await;
}

#[tokio::test]
async fn shared_capacity_never_evicts_live_captures_and_failed_admission_is_atomic() {
    let mut state = AppState::new();
    let mut ids = Vec::new();
    for index in 0..9 {
        let source = source(
            SourceKind::PipeWireAudioInput,
            "input",
            &format!("mic{index}"),
        );
        ids.push(source.id);
        state.sources.insert(source.id, source);
    }
    let app = AppHandle::spawn_with_state(state, CoreConfig::default());
    let mut owner = app.attach_audio_owner().await.unwrap();
    let mut generations = Vec::new();
    for id in &ids[..8] {
        generations.push(active(&app, &mut owner, *id).await);
    }
    let before = app.snapshot();
    assert!(app
        .dispatch(Command::AuthorizeSourceCapture { source_id: ids[8] })
        .await
        .is_err());
    assert!(Arc::ptr_eq(&before, &app.snapshot()));
    assert!(owner.requests.try_recv().is_err());
    // A retry of an existing slot is admitted even when every slot is live.
    let request = authorize(&app, &mut owner, ids[7]).await;
    assert!(request.generation > generations[7]);
    assert_eq!(app.snapshot().source_runtimes().count(), 8);
    app.shutdown().await;
}

#[tokio::test]
async fn runtime_budget_is_shared_between_video_and_audio_owners() {
    let (app, id, video_id) = fixture();
    let mut audio = app.attach_audio_owner().await.unwrap();
    let mut video = app.attach_capture_owner().await.unwrap();
    active(&app, &mut audio, id).await;
    app.dispatch(Command::AuthorizeSourceCapture {
        source_id: video_id,
    })
    .await
    .unwrap();
    video.requests.recv().await.unwrap();
    for index in 0..6 {
        let response = app
            .dispatch(Command::AddSource {
                kind: SourceKind::PipeWireWindow,
                name: format!("Other window {index}"),
            })
            .await
            .unwrap();
        let id = response
            .events
            .iter()
            .find_map(|event| match event {
                Event::Source(SourceEvent::Added { source }) => Some(source.id),
                _ => None,
            })
            .unwrap();
        app.dispatch(Command::AuthorizeSourceCapture { source_id: id })
            .await
            .unwrap();
        video.requests.recv().await.unwrap();
    }
    let response = app
        .dispatch(Command::AddSource {
            kind: SourceKind::PipeWireAppAudio,
            name: "Extra application".into(),
        })
        .await
        .unwrap();
    let extra = response
        .events
        .iter()
        .find_map(|event| match event {
            Event::Source(SourceEvent::Added { source }) => Some(source.id),
            _ => None,
        })
        .unwrap();
    app.dispatch(Command::SetSourceSettings {
        source_id: extra,
        settings: json!({"schema_version":1,"target":"chosen.application","mode":"application"}),
    })
    .await
    .unwrap();
    let before = app.snapshot();
    assert!(app
        .dispatch(Command::AuthorizeSourceCapture { source_id: extra })
        .await
        .is_err());
    assert!(Arc::ptr_eq(&before, &app.snapshot()));
    assert_eq!(app.snapshot().source_runtimes().count(), 8);
    assert!(audio.requests.try_recv().is_err());
    assert!(video.requests.try_recv().is_err());
    app.shutdown().await;
}

#[tokio::test]
async fn oldest_terminal_retirement_waits_for_effect_admission_and_is_observable() {
    let mut state = AppState::new();
    let mut ids = Vec::new();
    for index in 0..9 {
        let source = source(
            SourceKind::PipeWireAudioInput,
            "input",
            &format!("mic{index}"),
        );
        ids.push(source.id);
        state.sources.insert(source.id, source);
    }
    let app = AppHandle::spawn_with_state(state, CoreConfig::default());
    let mut owner = app.attach_audio_owner().await.unwrap();
    let mut generations = Vec::new();
    for id in &ids[..8] {
        let request = authorize(&app, &mut owner, *id).await;
        generations.push(request.generation);
        owner
            .runtime
            .report_capture(*id, request.generation, CaptureStatus::Denied, None)
            .await
            .unwrap();
    }
    // Saturate the effect receiver using same-slot explicit retries. Leave the
    // oldest terminal entry untouched so it is the planned retirement victim.
    for _ in 0..8 {
        app.dispatch(Command::AuthorizeSourceCapture { source_id: ids[7] })
            .await
            .unwrap();
    }
    let before = app.snapshot();
    assert!(app
        .dispatch(Command::AuthorizeSourceCapture { source_id: ids[8] })
        .await
        .is_err());
    assert!(Arc::ptr_eq(&before, &app.snapshot()));
    assert!(app.snapshot().source_runtime(ids[0]).is_some());
    owner.requests.recv().await.unwrap();
    let response = app
        .dispatch(Command::AuthorizeSourceCapture { source_id: ids[8] })
        .await
        .unwrap();
    assert!(response.events.iter().any(|event| matches!(event,Event::Source(SourceEvent::RuntimeChanged{source_id,runtime:None}) if *source_id == ids[0])));
    assert!(app.snapshot().source_runtime(ids[0]).is_none());
    assert!(app.snapshot().source_runtime(ids[1]).is_some());
    assert!(app.snapshot().source_runtime(ids[8]).is_some());
    assert_eq!(app.snapshot().source_runtimes().count(), 8);
    assert_eq!(
        app.snapshot()
            .source_runtime(ids[8])
            .unwrap()
            .generation
            .value(),
        before.source_runtime(ids[7]).unwrap().generation.value() + 1
    );
    assert!(owner
        .runtime
        .report_capture(ids[0], generations[0], CaptureStatus::Active, None)
        .await
        .is_err());
    app.shutdown().await;
}
