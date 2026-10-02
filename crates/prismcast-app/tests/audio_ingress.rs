//! Contract tests for the native audio capability and transient meter delivery.

use std::sync::Arc;
use std::time::Duration;

use prismcast_app::{
    category_of, primary_entity, AppHandle, AudioOwner, CoreConfig, EventCategory, EventFilter,
    HandleError, StreamEvent,
};
use prismcast_core::{AppState, Command, Error, Event, MeterEvent, Source, SourceId, SourceKind};
use serde_json::json;
use tokio::time::timeout;

fn tone(name: &str) -> Source {
    let mut source = Source::new(SourceKind::TestPattern, name);
    source.settings = json!({"audio_test": true});
    source
}

async fn fixture() -> (AppHandle, AudioOwner, SourceId) {
    let source = tone("Tone");
    let id = source.id;
    let mut state = AppState::new();
    state.sources.insert(id, source);
    let app = AppHandle::spawn_with_state(state, CoreConfig::default());
    let owner = app.attach_audio_owner().await.unwrap();
    (app, owner, id)
}

async fn report(app: &AppHandle, owner: &AudioOwner, id: SourceId) -> Result<(), HandleError> {
    owner
        .runtime
        .report_levels(
            app.snapshot().revision(),
            id,
            vec![-6.0, -7.0],
            vec![-9.0, -10.0],
        )
        .await
}

#[tokio::test]
async fn owner_is_exclusive_drop_revokes_clones_and_allows_replacement() {
    let (app, owner, id) = fixture().await;
    assert!(matches!(
        app.attach_audio_owner().await,
        Err(HandleError::Core(Error::InvalidInput(_)))
    ));
    let mut meters = app.subscribe_meters();
    report(&app, &owner, id).await.unwrap();
    meters.borrow_and_update();
    let old_reporter = owner.runtime.clone();
    drop(owner);
    timeout(Duration::from_secs(1), meters.changed())
        .await
        .unwrap()
        .unwrap();
    assert!(meters.borrow().levels.is_empty());
    let new_owner = app.attach_audio_owner().await.unwrap();
    assert!(matches!(
        old_reporter
            .report_levels(0, id, vec![-6.0], vec![-9.0])
            .await,
        Err(HandleError::Core(Error::Unauthorized(_)))
    ));
    report(&app, &new_owner, id).await.unwrap();
    app.shutdown().await;
    assert!(meters.borrow().levels.is_empty());
    assert!(matches!(
        report(&app, &new_owner, id).await,
        Err(HandleError::Shutdown)
    ));
}

#[tokio::test]
async fn graph_failure_clear_invalidates_observations_without_state_or_event_churn() {
    let (app, owner, id) = fixture().await;
    let before = app.snapshot();
    let mut snapshots = app.subscribe_snapshots();
    snapshots.borrow_and_update();
    let mut events = app.subscribe(EventFilter::all());
    report(&app, &owner, id).await.unwrap();
    assert!(matches!(
        events.recv().await,
        Some(StreamEvent::Event { seq: 0, .. })
    ));
    let mut meters = app.subscribe_meters();
    meters.borrow_and_update();
    owner.runtime.clear_levels().await.unwrap();
    assert!(meters.has_changed().unwrap());
    assert!(meters.borrow_and_update().levels.is_empty());
    assert!(Arc::ptr_eq(&before, &app.snapshot()));
    assert!(!snapshots.has_changed().unwrap());
    assert!(app.undo().await.is_err());
    // Repeated clears have no watch or event churn, and the owner can recover.
    owner.runtime.clear_levels().await.unwrap();
    assert!(!meters.has_changed().unwrap());
    report(&app, &owner, id).await.unwrap();
    assert!(matches!(
        events.recv().await,
        Some(StreamEvent::Event { seq: 1, .. })
    ));
    let old = owner.runtime.clone();
    drop(owner);
    let new_owner = app.attach_audio_owner().await.unwrap();
    report(&app, &new_owner, id).await.unwrap();
    assert!(matches!(
        old.clear_levels().await,
        Err(HandleError::Core(Error::Unauthorized(_)))
    ));
    assert_eq!(meters.borrow().levels.len(), 1);
    app.shutdown().await;
    assert!(matches!(
        new_owner.runtime.clear_levels().await,
        Err(HandleError::Shutdown)
    ));
}

#[tokio::test]
async fn rejects_invalid_levels_without_observation_or_events() {
    let (app, owner, id) = fixture().await;
    let mut meters = app.subscribe_meters();
    meters.borrow_and_update();
    let invalid = [
        (vec![], vec![]),
        (vec![-6.0], vec![]),
        (vec![-6.0; 9], vec![-9.0; 9]),
        (vec![f32::NAN], vec![-9.0]),
        (vec![-6.0], vec![f32::INFINITY]),
        (vec![f32::NEG_INFINITY], vec![-9.0]),
        (vec![-120.01], vec![-120.0]),
        (vec![-120.0], vec![-120.01]),
    ];
    for (peak, rms) in invalid {
        assert!(matches!(
            owner.runtime.report_levels(0, id, peak, rms).await,
            Err(HandleError::Core(Error::InvalidInput(_)))
        ));
    }
    assert!(!meters.has_changed().unwrap());
    assert!(meters.borrow().levels.is_empty());
    assert_eq!(app.snapshot().revision(), 0);
    // First accepted observation must still receive the first event sequence.
    let mut events = app.subscribe(EventFilter::categories([EventCategory::Meter]));
    owner
        .runtime
        .report_levels(0, id, vec![-120.0; 8], vec![-120.0; 8])
        .await
        .unwrap();
    assert!(matches!(
        events.recv().await,
        Some(StreamEvent::Event { seq: 0, .. })
    ));
    app.shutdown().await;
}

#[tokio::test]
async fn meters_preserve_snapshot_identity_revision_and_undo_redo() {
    let (app, owner, id) = fixture().await;
    app.dispatch(Command::RenameSource {
        source_id: id,
        name: "Renamed".into(),
    })
    .await
    .unwrap();
    let before = app.snapshot();
    let mut snapshots = app.subscribe_snapshots();
    snapshots.borrow_and_update();
    let mut events = app.subscribe(EventFilter::all());
    for _ in 0..3 {
        report(&app, &owner, id).await.unwrap();
    }
    assert!(Arc::ptr_eq(&before, &app.snapshot()));
    assert!(!snapshots.has_changed().unwrap());
    for seq in 1..=3 {
        assert!(matches!(events.recv().await,
            Some(StreamEvent::Event { seq: actual, event: Event::Meter(_) }) if actual == seq));
    }
    app.undo().await.unwrap();
    assert_eq!(app.snapshot().source(id).unwrap().name, "Tone");
    report(&app, &owner, id).await.unwrap();
    app.redo().await.unwrap();
    assert_eq!(app.snapshot().source(id).unwrap().name, "Renamed");
    assert!(app.subscribe_meters().borrow().levels.is_empty());
    app.shutdown().await;
}

#[tokio::test]
async fn revisions_and_source_lifecycle_reject_obsolete_reports_and_clear_watch() {
    let (app, owner, id) = fixture().await;
    let meters = app.subscribe_meters();
    report(&app, &owner, id).await.unwrap();
    let old_revision = app.snapshot().revision();
    app.dispatch(Command::SetSourceVolume {
        source_id: id,
        volume_db: -20.0,
    })
    .await
    .unwrap();
    assert!(meters.borrow().levels.is_empty());
    assert!(owner
        .runtime
        .report_levels(old_revision, id, vec![-6.0], vec![-9.0])
        .await
        .is_err());
    // Future revisions are also invalid.
    assert!(owner
        .runtime
        .report_levels(app.snapshot().revision() + 1, id, vec![-6.0], vec![-9.0])
        .await
        .is_err());
    report(&app, &owner, id).await.unwrap();
    app.dispatch(Command::SetSourceEnabled {
        source_id: id,
        enabled: false,
    })
    .await
    .unwrap();
    assert!(meters.borrow().levels.is_empty());
    assert!(report(&app, &owner, id).await.is_err());
    app.dispatch(Command::SetSourceEnabled {
        source_id: id,
        enabled: true,
    })
    .await
    .unwrap();
    report(&app, &owner, id).await.unwrap();
    app.dispatch(Command::SetSourceSettings {
        source_id: id,
        settings: json!({"audio_test":false}),
    })
    .await
    .unwrap();
    assert!(meters.borrow().levels.is_empty());
    assert!(report(&app, &owner, id).await.is_err());
    app.dispatch(Command::SetSourceSettings {
        source_id: id,
        settings: json!({"audio_test":true}),
    })
    .await
    .unwrap();
    report(&app, &owner, id).await.unwrap();
    app.dispatch(Command::RemoveSource { source_id: id })
        .await
        .unwrap();
    assert!(meters.borrow().levels.is_empty());
    assert!(matches!(
        report(&app, &owner, id).await,
        Err(HandleError::Core(Error::NotFound(_)))
    ));
    app.shutdown().await;
}

#[tokio::test]
async fn only_explicit_enabled_audio_test_patterns_accept_reports() {
    let mut state = AppState::new();
    let mut sources = Vec::new();
    for (kind, settings, enabled) in [
        (SourceKind::TestPattern, json!({}), true),
        (SourceKind::TestPattern, json!({"audio_test":"true"}), true),
        (
            SourceKind::PipeWireAudioInput,
            json!({"audio_test":true}),
            true,
        ),
        (SourceKind::TestPattern, json!({"audio_test":true}), false),
    ] {
        let mut source = Source::new(kind, "Unsupported");
        source.settings = settings;
        source.enabled = enabled;
        sources.push(source.id);
        state.sources.insert(source.id, source);
    }
    let app = AppHandle::spawn_with_state(state, CoreConfig::default());
    let owner = app.attach_audio_owner().await.unwrap();
    for id in sources {
        assert!(matches!(
            report(&app, &owner, id).await,
            Err(HandleError::Core(Error::InvalidInput(_)))
        ));
    }
    app.shutdown().await;
}

#[tokio::test]
async fn source_budget_is_bounded_and_replacements_keep_latest_measurement() {
    let mut state = AppState::new();
    let mut ids = Vec::new();
    for index in 0..33 {
        let source = tone(&format!("Tone {index}"));
        ids.push(source.id);
        state.sources.insert(source.id, source);
    }
    let app = AppHandle::spawn_with_state(state, CoreConfig::default());
    let owner = app.attach_audio_owner().await.unwrap();
    for id in &ids[..32] {
        report(&app, &owner, *id).await.unwrap();
    }
    assert!(matches!(
        report(&app, &owner, ids[32]).await,
        Err(HandleError::Core(Error::InvalidInput(_)))
    ));
    for level in [-5.0, -4.0, -3.0] {
        owner
            .runtime
            .report_levels(0, ids[0], vec![level], vec![level - 3.0])
            .await
            .unwrap();
    }
    let meters = app.subscribe_meters();
    assert_eq!(meters.borrow().levels.len(), 32);
    assert_eq!(meters.borrow().levels[&ids[0]].peak_dbfs, [-3.0]);
    app.dispatch(Command::RemoveSource { source_id: ids[0] })
        .await
        .unwrap();
    report(&app, &owner, ids[32]).await.unwrap();
    assert_eq!(meters.borrow().levels.len(), 1);
    app.shutdown().await;
}

#[tokio::test]
async fn meter_category_and_source_filter_preserve_sequence_and_payload() {
    let (app, owner, id) = fixture().await;
    let mut selected =
        app.subscribe(EventFilter::categories([EventCategory::Meter]).entities([*id.as_uuid()]));
    let mut other_source = app.subscribe(
        EventFilter::categories([EventCategory::Meter]).entities([*SourceId::new().as_uuid()]),
    );
    let mut audio_changes = app.subscribe(EventFilter::categories([EventCategory::Audio]));
    report(&app, &owner, id).await.unwrap();
    let expected = Event::Meter(MeterEvent::Levels {
        source_id: id,
        peak_dbfs: vec![-6.0, -7.0],
        rms_dbfs: vec![-9.0, -10.0],
    });
    assert_eq!(category_of(&expected), EventCategory::Meter);
    assert_eq!(primary_entity(&expected), Some(*id.as_uuid()));
    assert_eq!(
        selected.recv().await,
        Some(StreamEvent::Event {
            seq: 0,
            event: expected.clone()
        })
    );
    let json = serde_json::to_string(&expected).unwrap();
    assert_eq!(serde_json::from_str::<Event>(&json).unwrap(), expected);
    app.shutdown().await;
    assert_eq!(other_source.recv().await, None);
    assert_eq!(audio_changes.recv().await, None);
}
