//! Real native tone → owned service → transient app observation lifecycle.
use prismcast_app::{AppHandle, CoreConfig};
use prismcast_core::{audio::TrackMask, Command, Event, SourceEvent, SourceId, SourceKind};
use prismcast_preview::{AudioSession, AudioStatus};
use std::time::Duration;

async fn wait_level(handle: &AppHandle, id: SourceId, check: impl Fn(f32) -> bool) -> f32 {
    let mut meters = handle.subscribe_meters();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(level) = meters.borrow_and_update().levels.get(&id) {
                let peak = level.peak_dbfs[0];
                if check(peak) {
                    return peak;
                }
            }
            meters.changed().await.unwrap();
        }
    })
    .await
    .unwrap()
}

#[test]
fn owned_audio_tone_gain_mute_disable_and_joined_restart() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let handle = AppHandle::spawn(CoreConfig::default());
        let session = AudioSession::start(handle.clone()).await.unwrap();
        let mut status = session.subscribe_status();
        tokio::time::timeout(Duration::from_secs(5), async {
            while *status.borrow_and_update() != AudioStatus::Running {
                status.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        assert!(handle.subscribe_meters().borrow().levels.is_empty());
        let bus_id = handle.snapshot().state().audio.buses[0].id;
        let created = handle
            .dispatch(Command::AddSource {
                kind: SourceKind::TestPattern,
                name: "Explicit tone".into(),
            })
            .await
            .unwrap();
        let id = created
            .events
            .iter()
            .find_map(|event| match event {
                Event::Source(SourceEvent::Added { source }) => Some(source.id),
                _ => None,
            })
            .unwrap();
        handle
            .dispatch(Command::SetSourceSettings {
                source_id: id,
                settings: serde_json::json!({"audio_test": true}),
            })
            .await
            .unwrap();
        handle
            .dispatch(Command::SetAudioRoute {
                source_id: id,
                bus_id,
                tracks: TrackMask::stereo_pair(),
            })
            .await
            .unwrap();
        let unity = wait_level(&handle, id, |level| level > -10.0 && level < -3.0).await;
        let revision = handle.snapshot().revision();
        let mut meters = handle.subscribe_meters();
        meters.changed().await.unwrap();
        assert_eq!(
            handle.snapshot().revision(),
            revision,
            "observations do not commit state"
        );
        handle
            .dispatch(Command::SetSourceVolume {
                source_id: id,
                volume_db: -12.0,
            })
            .await
            .unwrap();
        let quiet = wait_level(&handle, id, |level| level < -15.0 && level > -22.0).await;
        assert!((unity - quiet - 12.0).abs() < 0.5);
        handle
            .dispatch(Command::SetSourceMuted {
                source_id: id,
                muted: true,
            })
            .await
            .unwrap();
        assert_eq!(
            wait_level(&handle, id, |level| level <= -119.0).await,
            -120.0
        );
        handle
            .dispatch(Command::SetSourceSettings {
                source_id: id,
                settings: serde_json::json!({"audio_test":true, "width":0}),
            })
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if matches!(&*status.borrow_and_update(), AudioStatus::Failed(_)) {
                    break;
                }
                status.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        assert!(handle.subscribe_meters().borrow().levels.is_empty());
        handle
            .dispatch(Command::SetSourceSettings {
                source_id: id,
                settings: serde_json::json!({"audio_test":true}),
            })
            .await
            .unwrap();
        assert_eq!(
            wait_level(&handle, id, |level| level <= -119.0).await,
            -120.0
        );
        assert_eq!(*status.borrow(), AudioStatus::Running);
        handle
            .dispatch(Command::SetSourceEnabled {
                source_id: id,
                enabled: false,
            })
            .await
            .unwrap();
        assert!(handle.subscribe_meters().borrow().levels.is_empty());
        session.shutdown().await.unwrap();
        assert_eq!(*status.borrow(), AudioStatus::Stopped);
        // AudioOwner drop revokes its capability before a new attachment.
        let restarted = AudioSession::start(handle.clone()).await.unwrap();
        restarted.shutdown().await.unwrap();
        handle.shutdown().await;
    });
}
