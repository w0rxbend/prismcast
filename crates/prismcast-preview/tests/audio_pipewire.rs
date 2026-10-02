//! Cross-boundary actual PipeWire capture, using a private hardware-free daemon.
#[path = "../../prismcast-media-gst/tests/common/pipewire.rs"]
mod pipewire;

use prismcast_app::{AppHandle, CoreConfig};
use prismcast_core::{
    audio::TrackMask, CaptureStatus, Command, Event, SourceEvent, SourceId, SourceKind,
};
use prismcast_preview::{AudioSession, AudioStatus};
use std::time::Duration;

async fn measured(handle: &AppHandle, id: SourceId, check: impl Fn(f32) -> bool) -> f32 {
    let mut meters = handle.subscribe_meters();
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if let Some(levels) = meters.borrow_and_update().levels.get(&id) {
                assert_eq!(levels.peak_dbfs.len(), 2);
                assert_eq!(levels.rms_dbfs.len(), 2);
                assert!(levels
                    .peak_dbfs
                    .iter()
                    .chain(&levels.rms_dbfs)
                    .all(|value| value.is_finite()));
                if check(levels.peak_dbfs[0]) {
                    return levels.peak_dbfs[0];
                }
            }
            meters.changed().await.unwrap();
        }
    })
    .await
    .unwrap()
}
async fn capture_state(handle: &AppHandle, id: SourceId, expected: CaptureStatus) {
    let mut snapshots = handle.subscribe_snapshots();
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let snapshot = snapshots.borrow_and_update();
            if let Some(state) = snapshot.source_runtime(id) {
                if state.status == CaptureStatus::Failed && expected != CaptureStatus::Failed {
                    panic!("real capture failed: {:?}", state.message);
                }
                if state.status == expected {
                    assert!(state.dimensions.is_none());
                    break;
                }
            }
            drop(snapshot);
            snapshots.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
}

#[test]
#[ignore = "requires isolated PipeWire/WirePlumber/gst-launch fixture; no hardware; run separately"]
fn actual_pipewire_audio_session_authorization_measurements_invalidation_and_shutdown() {
    if std::env::var_os("PRISMCAST_PRIVATE_AUDIO_FIXTURE").is_none() {
        pipewire::run_isolated(
            "actual_pipewire_audio_session_authorization_measurements_invalidation_and_shutdown",
        );
        return;
    }
    let fixture = pipewire::Fixture::start();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let handle = AppHandle::spawn(CoreConfig::default());
        let session = AudioSession::start(handle.clone()).await.unwrap();
        let status = session.subscribe_status();
        let response = handle
            .dispatch(Command::AddSource {
                kind: SourceKind::PipeWireAudioInput,
                name: "Real isolated input".into(),
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
        let bus_id = handle.snapshot().state().audio.buses[0].id;
        handle
            .dispatch(Command::Transaction {
                commands: vec![
                    Command::SetSourceSettings {
                        source_id: id,
                        settings: serde_json::to_value(pipewire::Fixture::source_settings())
                            .unwrap(),
                    },
                    Command::SetAudioRoute {
                        source_id: id,
                        bus_id,
                        tracks: TrackMask::stereo_pair(),
                    },
                ],
            })
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(handle.snapshot().source_runtime(id).is_none());
        assert!(
            handle.subscribe_meters().borrow().levels.is_empty(),
            "saved target is not consent"
        );
        handle
            .dispatch(Command::AuthorizeSourceCapture { source_id: id })
            .await
            .unwrap();
        let first_generation = handle.snapshot().source_runtime(id).unwrap().generation;
        capture_state(&handle, id, CaptureStatus::Active).await;
        let unity = measured(&handle, id, |peak| peak > -60.0 && peak < 0.0).await;
        let revision = handle.snapshot().revision();
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert_eq!(
            handle.snapshot().revision(),
            revision,
            "measured telemetry does not commit state"
        );
        handle
            .dispatch(Command::SetSourceVolume {
                source_id: id,
                volume_db: -12.0,
            })
            .await
            .unwrap();
        let quiet = measured(&handle, id, |peak| peak < unity - 10.0).await;
        assert!(
            (unity - quiet - 12.0).abs() < 0.5,
            "native captured gain: unity={unity}, quiet={quiet}"
        );
        assert_eq!(
            handle.snapshot().source_runtime(id).unwrap().generation,
            first_generation
        );
        handle
            .dispatch(Command::SetSourceMuted {
                source_id: id,
                muted: true,
            })
            .await
            .unwrap();
        assert_eq!(measured(&handle, id, |peak| peak <= -119.0).await, -120.0);
        handle
            .dispatch(Command::SetSourceEnabled {
                source_id: id,
                enabled: false,
            })
            .await
            .unwrap();
        assert!(handle.subscribe_meters().borrow().levels.is_empty());
        handle
            .dispatch(Command::SetSourceEnabled {
                source_id: id,
                enabled: true,
            })
            .await
            .unwrap();
        handle
            .dispatch(Command::SetSourceMuted {
                source_id: id,
                muted: false,
            })
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(handle.snapshot().source_runtime(id).is_none());
        assert!(
            handle.subscribe_meters().borrow().levels.is_empty(),
            "enable and mixer edits must not reopen capture"
        );
        handle
            .dispatch(Command::AuthorizeSourceCapture { source_id: id })
            .await
            .unwrap();
        let second_generation = handle.snapshot().source_runtime(id).unwrap().generation;
        assert!(second_generation > first_generation);
        capture_state(&handle, id, CaptureStatus::Active).await;
        measured(&handle, id, |peak| (peak - quiet).abs() < 0.5).await;
        session.shutdown().await.unwrap();
        assert_eq!(*status.borrow(), AudioStatus::Stopped);
        capture_state(&handle, id, CaptureStatus::Failed).await;
        assert!(handle.subscribe_meters().borrow().levels.is_empty());
        handle.shutdown().await;
    });
    drop(fixture);
}
