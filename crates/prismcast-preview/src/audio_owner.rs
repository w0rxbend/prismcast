//! Dedicated native audio owner. GTK never enters this thread's graph.
use prismcast_app::{AppHandle, AudioOwner};
use prismcast_media_gst::GstAudioMixer;
use std::time::{Duration, Instant};
use tokio::sync::{oneshot, watch};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AudioStatus {
    Starting,
    Running,
    Failed(String),
    Stopped,
}

#[derive(Debug, thiserror::Error)]
pub enum AudioSessionError {
    #[error("audio owner attachment failed: {0}")]
    Attach(#[from] prismcast_app::HandleError),
    #[error("audio owner thread failed: {0}")]
    Thread(#[from] std::io::Error),
    #[error("audio graph failed: {0}")]
    Backend(#[from] prismcast_core::Error),
    #[error("audio owner exited unexpectedly")]
    WorkerGone,
    #[error("audio owner panicked")]
    WorkerPanic,
}

/// Cancellation is requested on drop; orderly shutdown awaits the owner join.
pub struct AudioSession {
    cancel: Option<oneshot::Sender<()>>,
    completion: Option<oneshot::Receiver<Result<(), AudioSessionError>>>,
    status: watch::Receiver<AudioStatus>,
}
impl AudioSession {
    pub async fn start(handle: AppHandle) -> Result<Self, AudioSessionError> {
        // Attachment is asynchronous, before any native graph is constructed.
        let owner = handle.attach_audio_owner().await?;
        let (cancel, cancelled) = oneshot::channel();
        let (status_tx, status) = watch::channel(AudioStatus::Starting);
        let (completed, completion) = oneshot::channel();
        let thread_status = status_tx.clone();
        let worker = std::thread::Builder::new()
            .name("prismcast-audio-owner".into())
            .spawn(move || run(owner, cancelled, thread_status))?;
        std::thread::Builder::new()
            .name("prismcast-audio-reaper".into())
            .spawn(move || {
                let result = worker.join().unwrap_or(Err(AudioSessionError::WorkerPanic));
                status_tx.send_replace(match &result {
                    Ok(()) => AudioStatus::Stopped,
                    Err(error) => AudioStatus::Failed(error.to_string()),
                });
                let _ = completed.send(result);
            })?;
        Ok(Self {
            cancel: Some(cancel),
            completion: Some(completion),
            status,
        })
    }
    pub fn subscribe_status(&self) -> watch::Receiver<AudioStatus> {
        self.status.clone()
    }
    pub async fn shutdown(mut self) -> Result<(), AudioSessionError> {
        if let Some(cancel) = self.cancel.take() {
            let _ = cancel.send(());
        }
        self.completion
            .take()
            .ok_or(AudioSessionError::WorkerGone)?
            .await
            .map_err(|_| AudioSessionError::WorkerGone)?
    }
}
impl Drop for AudioSession {
    fn drop(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            let _ = cancel.send(());
        }
    }
}

/// Private supervision seam; native graph APIs remain outside GTK.
trait OwnedAudioGraph {
    fn reconcile(&mut self, state: &prismcast_core::AppState) -> prismcast_core::Result<()>;
    fn poll(&mut self) -> prismcast_core::Result<Vec<prismcast_audio::SourceMeter>>;
    fn stop(&mut self) -> prismcast_core::Result<()>;
}
impl OwnedAudioGraph for GstAudioMixer {
    fn reconcile(&mut self, state: &prismcast_core::AppState) -> prismcast_core::Result<()> {
        GstAudioMixer::reconcile(self, state)
    }
    fn poll(&mut self) -> prismcast_core::Result<Vec<prismcast_audio::SourceMeter>> {
        GstAudioMixer::poll(self)
    }
    fn stop(&mut self) -> prismcast_core::Result<()> {
        GstAudioMixer::stop(self)
    }
}
fn run(
    owner: AudioOwner,
    cancel: oneshot::Receiver<()>,
    status: watch::Sender<AudioStatus>,
) -> Result<(), AudioSessionError> {
    run_with_graph(owner, cancel, status, GstAudioMixer::new()?)
}
fn run_with_graph(
    mut owner: AudioOwner,
    mut cancel: oneshot::Receiver<()>,
    status: watch::Sender<AudioStatus>,
    mut mixer: impl OwnedAudioGraph,
) -> Result<(), AudioSessionError> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let mut revision = None;
    let mut next_poll = Instant::now();
    let result = (|| -> Result<(), AudioSessionError> {
        loop {
            let snapshot = owner.snapshots.borrow_and_update().clone();
            if revision != Some(snapshot.revision()) {
                match mixer.reconcile(snapshot.state()) {
                    Ok(()) => {
                        revision = Some(snapshot.revision());
                        status.send_replace(AudioStatus::Running);
                    }
                    Err(error) => {
                        mixer.stop()?;
                        // Mark this failed revision handled; retry on the next command.
                        revision = Some(snapshot.revision());
                        status.send_replace(AudioStatus::Failed(error.to_string()));
                    }
                }
            }
            if Instant::now() >= next_poll {
                next_poll = Instant::now() + Duration::from_millis(34);
                match mixer.poll() {
                    Ok(levels) => {
                        for level in levels {
                            // Recheck the latest snapshot before crossing into the actor;
                            // the actor repeats this check atomically on ingress.
                            if owner.snapshots.borrow().revision() != snapshot.revision() {
                                break;
                            }
                            let keep_running = runtime.block_on(async {
                                let report = owner.runtime.report_levels(snapshot.revision(), level.source_id, level.peak_dbfs, level.rms_dbfs);
                                tokio::select! {
                                    biased;
                                    _ = &mut cancel => false,
                                    outcome = tokio::time::timeout(Duration::from_millis(100), report) => {
                                        match outcome {
                                            Ok(Ok(())) => {}
                                            Ok(Err(error)) => tracing::debug!(source_id=%level.source_id, %error, "audio observation rejected"),
                                            Err(_) => tracing::debug!(source_id=%level.source_id, "audio observation timed out"),
                                        }
                                        true
                                    }
                                }
                            });
                            if !keep_running {
                                return Ok(());
                            }
                        }
                    }
                    Err(error) => {
                        mixer.stop()?;
                        let cleared = runtime.block_on(async {
                            tokio::select! {
                                biased;
                                _ = &mut cancel => Ok(false),
                                outcome = tokio::time::timeout(Duration::from_millis(100), owner.runtime.clear_levels()) => {
                                    outcome.map_err(|_| AudioSessionError::WorkerGone)?.map_err(AudioSessionError::Attach)?;
                                    Ok::<bool, AudioSessionError>(true)
                                }
                            }
                        })?;
                        if !cleared {
                            return Ok(());
                        }
                        // Polling the stopped graph yields no data. A later
                        // command revision retries once through reconcile.
                        status.send_replace(AudioStatus::Failed(error.to_string()));
                    }
                }
            }
            let delay = next_poll.saturating_duration_since(Instant::now());
            let keep_running = runtime.block_on(async {
                tokio::select! {
                    biased;
                    _ = &mut cancel => false,
                    changed = owner.snapshots.changed() => changed.is_ok(),
                    _ = tokio::time::sleep(delay) => true,
                }
            });
            if !keep_running {
                break;
            }
        }
        Ok(())
    })();
    // Native NULL teardown precedes capability revocation and completion.
    let stopped = mixer.stop().map_err(AudioSessionError::Backend);
    drop(mixer);
    drop(owner);
    result.and(stopped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use prismcast_core::{Command, Event, SourceEvent, SourceKind};
    use std::sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    };

    struct FailOnce {
        native: GstAudioMixer,
        fail: Arc<AtomicBool>,
        reconciles: Arc<AtomicUsize>,
    }
    impl OwnedAudioGraph for FailOnce {
        fn reconcile(&mut self, state: &prismcast_core::AppState) -> prismcast_core::Result<()> {
            self.reconciles.fetch_add(1, Ordering::Relaxed);
            self.native.reconcile(state)
        }
        fn poll(&mut self) -> prismcast_core::Result<Vec<prismcast_audio::SourceMeter>> {
            if self.fail.swap(false, Ordering::AcqRel) {
                return Err(prismcast_core::Error::Media(
                    "injected terminal bus failure".into(),
                ));
            }
            self.native.poll()
        }
        fn stop(&mut self) -> prismcast_core::Result<()> {
            self.native.stop()
        }
    }
    #[test]
    fn poll_failure_clears_existing_measurements_without_revision_and_recovers_on_command() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let app =
            runtime.block_on(async { AppHandle::spawn(prismcast_app::CoreConfig::default()) });
        let id = runtime.block_on(async {
            let response = app
                .dispatch(Command::AddSource {
                    kind: SourceKind::TestPattern,
                    name: "Supervised tone".into(),
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
            app.dispatch(Command::SetSourceSettings {
                source_id: id,
                settings: serde_json::json!({"audio_test": true}),
            })
            .await
            .unwrap();
            id
        });
        let owner = runtime.block_on(app.attach_audio_owner()).unwrap();
        let (cancel, cancelled) = oneshot::channel();
        let (status_tx, mut status) = watch::channel(AudioStatus::Starting);
        let fail = Arc::new(AtomicBool::new(false));
        let reconciles = Arc::new(AtomicUsize::new(0));
        let thread_fail = fail.clone();
        let thread_reconciles = reconciles.clone();
        let worker = std::thread::spawn(move || {
            run_with_graph(
                owner,
                cancelled,
                status_tx,
                FailOnce {
                    native: GstAudioMixer::new().unwrap(),
                    fail: thread_fail,
                    reconciles: thread_reconciles,
                },
            )
        });
        runtime.block_on(async {
            let mut meters = app.subscribe_meters();
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if meters.borrow_and_update().levels.contains_key(&id) { break; }
                    meters.changed().await.unwrap();
                }
            }).await.unwrap();
            let revision = app.snapshot().revision();
            fail.store(true, Ordering::Release);
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if matches!(&*status.borrow_and_update(), AudioStatus::Failed(message) if message.contains("injected terminal")) { break; }
                    status.changed().await.unwrap();
                }
            }).await.unwrap();
            assert!(app.subscribe_meters().borrow().levels.is_empty());
            assert_eq!(app.snapshot().revision(), revision);
            let count = reconciles.load(Ordering::Relaxed);
            tokio::time::sleep(Duration::from_millis(150)).await;
            assert_eq!(reconciles.load(Ordering::Relaxed), count, "timer never reopens failed graph");
            assert!(app.subscribe_meters().borrow().levels.is_empty());
            app.dispatch(Command::SetSourceVolume { source_id: id, volume_db: -6.0 }).await.unwrap();
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if let Some(level) = meters.borrow_and_update().levels.get(&id) {
                        assert!(level.peak_dbfs[0] > -14.0 && level.peak_dbfs[0] < -10.0);
                        break;
                    }
                    meters.changed().await.unwrap();
                }
            }).await.unwrap();
            assert_eq!(*status.borrow(), AudioStatus::Running);
        });
        cancel.send(()).unwrap();
        worker.join().unwrap().unwrap();
        runtime.block_on(app.shutdown());
    }
}
