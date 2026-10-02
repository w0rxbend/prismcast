//! Dedicated native audio owner. Persisted selections never authorize capture.
use prismcast_app::{AppHandle, AudioCaptureAuthorizationRequest, AudioOwner};
use prismcast_capture::audio::{resolve_audio_target, AuthorizedAudioTarget};
use prismcast_core::{CaptureGeneration, CaptureStatus, PipeWireAudioSettings, SourceId};
use prismcast_media_gst::GstAudioMixer;
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};
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
/// Dropping requests cancellation; orderly shutdown awaits the native owner join.
pub struct AudioSession {
    cancel: Option<oneshot::Sender<()>>,
    completion: Option<oneshot::Receiver<Result<(), AudioSessionError>>>,
    status: watch::Receiver<AudioStatus>,
}
impl AudioSession {
    pub async fn start(handle: AppHandle) -> Result<Self, AudioSessionError> {
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

/// Private seam tests consent and supervision without a spoofable native grant API.
trait OwnedAudioGraph {
    type Granted: Clone;
    fn authorize(
        &mut self,
        id: SourceId,
        generation: CaptureGeneration,
        settings: &PipeWireAudioSettings,
    ) -> prismcast_core::Result<Self::Granted>;
    fn reconcile(
        &mut self,
        state: &prismcast_core::AppState,
        grants: &[Self::Granted],
    ) -> prismcast_core::Result<()>;
    fn poll(&mut self) -> prismcast_core::Result<Vec<prismcast_audio::SourceMeter>>;
    fn stop(&mut self) -> prismcast_core::Result<()>;
}
impl OwnedAudioGraph for GstAudioMixer {
    type Granted = AuthorizedAudioTarget;
    fn authorize(
        &mut self,
        id: SourceId,
        generation: CaptureGeneration,
        settings: &PipeWireAudioSettings,
    ) -> prismcast_core::Result<Self::Granted> {
        resolve_audio_target(id, generation, settings)
    }
    fn reconcile(
        &mut self,
        state: &prismcast_core::AppState,
        grants: &[Self::Granted],
    ) -> prismcast_core::Result<()> {
        self.reconcile_authorized(state, grants)
    }
    fn poll(&mut self) -> prismcast_core::Result<Vec<prismcast_audio::SourceMeter>> {
        GstAudioMixer::poll(self)
    }
    fn stop(&mut self) -> prismcast_core::Result<()> {
        GstAudioMixer::stop(self)
    }
}
struct Grant<T> {
    native: T,
    generation: CaptureGeneration,
    settings: PipeWireAudioSettings,
    began: Instant,
}
fn valid(
    snapshot: &prismcast_app::AppSnapshot,
    id: SourceId,
    generation: CaptureGeneration,
    settings: &PipeWireAudioSettings,
) -> bool {
    snapshot.source(id).is_some_and(|source| {
        source.enabled
            && PipeWireAudioSettings::from_source(source).is_ok_and(|current| current == *settings)
    }) && snapshot.source_runtime(id).is_some_and(|runtime| {
        runtime.generation == generation
            && matches!(
                runtime.status,
                CaptureStatus::Authorizing | CaptureStatus::Active
            )
    })
}
fn prune_grants<G: OwnedAudioGraph>(
    snapshot: &prismcast_app::AppSnapshot,
    grants: &mut BTreeMap<SourceId, Grant<G::Granted>>,
    mixer: &mut G,
) -> Result<bool, AudioSessionError> {
    let previous = grants.len();
    grants.retain(|id, grant| valid(snapshot, *id, grant.generation, &grant.settings));
    if previous != grants.len() {
        // Stop revoked native capture before any potentially slow discovery.
        mixer.stop()?;
        Ok(true)
    } else {
        Ok(false)
    }
}
/// Await actor ingress without blocking native graph callbacks or shutdown.
fn deliver(
    runtime: &tokio::runtime::Runtime,
    cancel: &mut oneshot::Receiver<()>,
    future: impl std::future::Future<Output = Result<(), prismcast_app::HandleError>>,
) -> Result<bool, AudioSessionError> {
    runtime.block_on(async {
        tokio::select! { biased;
            _ = cancel => Ok(false),
            result = tokio::time::timeout(Duration::from_millis(250), future) => {
                match result { Ok(Ok(())) => {}, Ok(Err(error)) => tracing::debug!(%error,"audio observation rejected"), Err(_) => return Err(AudioSessionError::WorkerGone) }
                Ok(true)
            }
        }
    })
}
fn run(
    owner: AudioOwner,
    cancel: oneshot::Receiver<()>,
    status: watch::Sender<AudioStatus>,
) -> Result<(), AudioSessionError> {
    run_with_graph(owner, cancel, status, GstAudioMixer::new()?)
}
fn run_with_graph<G: OwnedAudioGraph>(
    mut owner: AudioOwner,
    mut cancel: oneshot::Receiver<()>,
    status: watch::Sender<AudioStatus>,
    mut mixer: G,
) -> Result<(), AudioSessionError> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let mut grants = BTreeMap::<SourceId, Grant<G::Granted>>::new();
    let mut pending = None::<AudioCaptureAuthorizationRequest>;
    let mut revision = None;
    let mut next_poll = Instant::now();
    let result = (|| -> Result<(), AudioSessionError> {
        loop {
            if !matches!(cancel.try_recv(), Err(oneshot::error::TryRecvError::Empty)) {
                break;
            }
            let snapshot = owner.snapshots.borrow_and_update().clone();
            if prune_grants(&snapshot, &mut grants, &mut mixer)? {
                revision = None;
            }
            // At most eight retained authorization effects; never synthesize
            // requests from a snapshot, restore, gain or routing change.
            for _ in 0..8 {
                let request = pending.take().or_else(|| owner.requests.try_recv().ok());
                let Some(request) = request else {
                    break;
                };
                if !matches!(cancel.try_recv(), Err(oneshot::error::TryRecvError::Empty)) {
                    return Ok(());
                }
                let current = owner.snapshots.borrow().clone();
                if prune_grants(&current, &mut grants, &mut mixer)? {
                    revision = None;
                }
                if !valid(
                    &current,
                    request.source_id,
                    request.generation,
                    &request.settings,
                ) {
                    continue;
                }
                match mixer.authorize(request.source_id, request.generation, &request.settings) {
                    Ok(native) => {
                        if !matches!(cancel.try_recv(), Err(oneshot::error::TryRecvError::Empty)) {
                            return Ok(());
                        }
                        let current = owner.snapshots.borrow().clone();
                        if prune_grants(&current, &mut grants, &mut mixer)? {
                            revision = None;
                        }
                        if valid(
                            &current,
                            request.source_id,
                            request.generation,
                            &request.settings,
                        ) {
                            grants.insert(
                                request.source_id,
                                Grant {
                                    native,
                                    generation: request.generation,
                                    settings: request.settings,
                                    began: Instant::now(),
                                },
                            );
                            revision = None;
                        }
                    }
                    Err(error) => {
                        let current = owner.snapshots.borrow().clone();
                        if prune_grants(&current, &mut grants, &mut mixer)? {
                            revision = None;
                        }
                        tracing::warn!(source_id=%request.source_id, generation=?request.generation, %error, "audio target resolution failed");
                        if !deliver(
                            &runtime,
                            &mut cancel,
                            owner.runtime.report_capture(
                                request.source_id,
                                request.generation,
                                CaptureStatus::Failed,
                                Some(error.to_string()),
                            ),
                        )? {
                            return Ok(());
                        }
                    }
                }
            }
            let snapshot = owner.snapshots.borrow_and_update().clone();
            if prune_grants(&snapshot, &mut grants, &mut mixer)? {
                revision = None;
            }
            if revision != Some(snapshot.revision()) {
                if !matches!(cancel.try_recv(), Err(oneshot::error::TryRecvError::Empty)) {
                    return Ok(());
                }
                let authorized: Vec<_> =
                    grants.values().map(|grant| grant.native.clone()).collect();
                match mixer.reconcile(snapshot.state(), &authorized) {
                    Ok(()) => {
                        revision = Some(snapshot.revision());
                        status.send_replace(AudioStatus::Running);
                    }
                    Err(error) => {
                        mixer.stop()?;
                        for (id, grant) in std::mem::take(&mut grants) {
                            tracing::warn!(source_id=%id, generation=?grant.generation, %error, "audio capture graph failed; grant revoked");
                            if !deliver(
                                &runtime,
                                &mut cancel,
                                owner.runtime.report_capture(
                                    id,
                                    grant.generation,
                                    CaptureStatus::Failed,
                                    Some(error.to_string()),
                                ),
                            )? {
                                return Ok(());
                            }
                        }
                        let latest = owner.snapshots.borrow().clone();
                        // Runtime-only failure events are handled here. A concurrent
                        // controller edit still needs its own next reconciliation.
                        revision =
                            (latest.state() == snapshot.state()).then_some(latest.revision());
                        status.send_replace(AudioStatus::Failed(error.to_string()));
                    }
                }
            }
            if Instant::now() >= next_poll {
                next_poll = Instant::now() + Duration::from_millis(34);
                match mixer.poll() {
                    Ok(levels) => {
                        for level in levels {
                            if owner.snapshots.borrow().revision() != snapshot.revision() {
                                break;
                            }
                            if let Some(grant) = grants.get(&level.source_id) {
                                let generation = grant.generation;
                                if snapshot
                                    .source_runtime(level.source_id)
                                    .is_some_and(|state| state.status == CaptureStatus::Authorizing)
                                {
                                    // Active proves a real sample. Its Core Event commits a
                                    // revision; defer this batch until next no-op reconcile.
                                    if !deliver(
                                        &runtime,
                                        &mut cancel,
                                        owner.runtime.report_capture(
                                            level.source_id,
                                            generation,
                                            CaptureStatus::Active,
                                            None,
                                        ),
                                    )? {
                                        return Ok(());
                                    }
                                    break;
                                }
                                if !deliver(
                                    &runtime,
                                    &mut cancel,
                                    owner.runtime.report_capture_levels(
                                        snapshot.revision(),
                                        generation,
                                        level.source_id,
                                        level.peak_dbfs,
                                        level.rms_dbfs,
                                    ),
                                )? {
                                    return Ok(());
                                }
                            } else if !deliver(
                                &runtime,
                                &mut cancel,
                                owner.runtime.report_levels(
                                    snapshot.revision(),
                                    level.source_id,
                                    level.peak_dbfs,
                                    level.rms_dbfs,
                                ),
                            )? {
                                return Ok(());
                            }
                        }
                    }
                    Err(error) => {
                        mixer.stop()?;
                        for (id, grant) in std::mem::take(&mut grants) {
                            tracing::warn!(source_id=%id, generation=?grant.generation, %error, "audio capture graph failed; grant revoked");
                            if !deliver(
                                &runtime,
                                &mut cancel,
                                owner.runtime.report_capture(
                                    id,
                                    grant.generation,
                                    CaptureStatus::Failed,
                                    Some(error.to_string()),
                                ),
                            )? {
                                return Ok(());
                            }
                        }
                        if !deliver(&runtime, &mut cancel, owner.runtime.clear_levels())? {
                            return Ok(());
                        }
                        let latest = owner.snapshots.borrow().clone();
                        // Runtime-only failure events are handled here. A concurrent
                        // controller edit still needs its own next reconciliation.
                        revision =
                            (latest.state() == snapshot.state()).then_some(latest.revision());
                        status.send_replace(AudioStatus::Failed(error.to_string()));
                    }
                }
                let now = Instant::now();
                let expired: Vec<_> = grants
                    .iter()
                    .filter(|(id, grant)| {
                        now.duration_since(grant.began) > Duration::from_secs(5)
                            && owner
                                .snapshots
                                .borrow()
                                .source_runtime(**id)
                                .is_some_and(|state| state.status == CaptureStatus::Authorizing)
                    })
                    .map(|(id, _)| *id)
                    .collect();
                for id in expired {
                    if let Some(grant) = grants.remove(&id) {
                        tracing::warn!(source_id=%id, generation=?grant.generation, "audio capture first measurement timed out");
                        if !deliver(
                            &runtime,
                            &mut cancel,
                            owner.runtime.report_capture(
                                id,
                                grant.generation,
                                CaptureStatus::Failed,
                                Some(
                                    "Audio target produced no measurements within five seconds"
                                        .into(),
                                ),
                            ),
                        )? {
                            return Ok(());
                        }
                        revision = None;
                    }
                }
            }
            let delay = next_poll.saturating_duration_since(Instant::now());
            let keep_running = runtime.block_on(async {
                tokio::select! { biased;
                    _ = &mut cancel => false,
                    changed = owner.snapshots.changed() => changed.is_ok(),
                    request = owner.requests.recv() => { pending = request; pending.is_some() },
                    _ = tokio::time::sleep(delay) => true,
                }
            });
            if !keep_running {
                break;
            }
        }
        Ok(())
    })();
    let stopped = mixer.stop().map_err(AudioSessionError::Backend);
    grants.clear();
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
        type Granted = AuthorizedAudioTarget;
        fn authorize(
            &mut self,
            id: SourceId,
            generation: CaptureGeneration,
            settings: &PipeWireAudioSettings,
        ) -> prismcast_core::Result<Self::Granted> {
            resolve_audio_target(id, generation, settings)
        }
        fn reconcile(
            &mut self,
            state: &prismcast_core::AppState,
            grants: &[Self::Granted],
        ) -> prismcast_core::Result<()> {
            self.reconciles.fetch_add(1, Ordering::Relaxed);
            self.native.reconcile_authorized(state, grants)
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
    struct FixtureCaptureGraph {
        native: GstAudioMixer,
        resolves: Arc<AtomicUsize>,
        fail: Arc<AtomicBool>,
        suppress_data: bool,
    }
    impl OwnedAudioGraph for FixtureCaptureGraph {
        type Granted = SourceId;
        fn authorize(
            &mut self,
            id: SourceId,
            _: CaptureGeneration,
            _: &PipeWireAudioSettings,
        ) -> prismcast_core::Result<SourceId> {
            self.resolves.fetch_add(1, Ordering::Relaxed);
            Ok(id)
        }
        fn reconcile(
            &mut self,
            state: &prismcast_core::AppState,
            grants: &[SourceId],
        ) -> prismcast_core::Result<()> {
            // The fixture substitutes a real native diagnostic signal only inside
            // this private test graph; production always resolves PipeWire nodes.
            let mut fixture = state.clone();
            for source in fixture.sources.values_mut() {
                if matches!(
                    source.kind,
                    SourceKind::PipeWireAudioInput | SourceKind::PipeWireAppAudio
                ) {
                    if grants.contains(&source.id) {
                        source.kind = SourceKind::TestPattern;
                        source.settings = serde_json::json!({"audio_test":true});
                    } else {
                        source.enabled = false;
                    }
                }
            }
            self.native.reconcile(&fixture)
        }
        fn poll(&mut self) -> prismcast_core::Result<Vec<prismcast_audio::SourceMeter>> {
            if self.fail.swap(false, Ordering::AcqRel) {
                return Err(prismcast_core::Error::Media(
                    "fixture capture disconnected".into(),
                ));
            }
            let levels = self.native.poll()?;
            Ok(if self.suppress_data {
                Vec::new()
            } else {
                levels
            })
        }
        fn stop(&mut self) -> prismcast_core::Result<()> {
            self.native.stop()
        }
    }
    async fn configured_capture(app: &AppHandle) -> SourceId {
        let response = app
            .dispatch(Command::AddSource {
                kind: SourceKind::PipeWireAudioInput,
                name: "Consent fixture".into(),
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
            settings: serde_json::json!({"schema_version":1,"target":"fixture.mic","mode":"input"}),
        })
        .await
        .unwrap();
        id
    }
    async fn wait_capture(app: &AppHandle, id: SourceId, expected: CaptureStatus) {
        let mut snapshots = app.subscribe_snapshots();
        tokio::time::timeout(Duration::from_secs(7), async {
            loop {
                if snapshots
                    .borrow_and_update()
                    .source_runtime(id)
                    .is_some_and(|state| state.status == expected)
                {
                    break;
                }
                snapshots.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
    }
    async fn wait_capture_meter(app: &AppHandle, id: SourceId) -> f32 {
        let mut meters = app.subscribe_meters();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(level) = meters.borrow_and_update().levels.get(&id) {
                    return level.peak_dbfs[0];
                }
                meters.changed().await.unwrap();
            }
        })
        .await
        .unwrap()
    }
    #[test]
    fn physical_capture_requires_explicit_generation_and_never_reopens_after_failure() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let app =
            runtime.block_on(async { AppHandle::spawn(prismcast_app::CoreConfig::default()) });
        let id = runtime.block_on(configured_capture(&app));
        let owner = runtime.block_on(app.attach_audio_owner()).unwrap();
        let (cancel, cancelled) = oneshot::channel();
        let (status, _) = watch::channel(AudioStatus::Starting);
        let resolves = Arc::new(AtomicUsize::new(0));
        let fail = Arc::new(AtomicBool::new(false));
        let counts = resolves.clone();
        let fault = fail.clone();
        let worker = std::thread::spawn(move || {
            run_with_graph(
                owner,
                cancelled,
                status,
                FixtureCaptureGraph {
                    native: GstAudioMixer::new().unwrap(),
                    resolves: counts,
                    fail: fault,
                    suppress_data: false,
                },
            )
        });
        runtime.block_on(async {
            tokio::time::sleep(Duration::from_millis(100)).await;
            assert_eq!(resolves.load(Ordering::Relaxed), 0); assert!(app.subscribe_meters().borrow().levels.is_empty());
            app.dispatch(Command::AuthorizeSourceCapture { source_id:id }).await.unwrap();
            wait_capture(&app,id,CaptureStatus::Active).await;
            assert!(wait_capture_meter(&app,id).await > -10.0);
            assert_eq!(resolves.load(Ordering::Relaxed),1);
            app.dispatch(Command::SetSourceVolume { source_id:id, volume_db:-12.0 }).await.unwrap();
            assert!(wait_capture_meter(&app,id).await < -15.0); assert_eq!(resolves.load(Ordering::Relaxed),1);
            app.dispatch(Command::SetSourceSettings { source_id:id, settings:serde_json::json!({"schema_version":1,"target":"fixture.replaced","mode":"input"}) }).await.unwrap();
            app.dispatch(Command::SetSourceVolume { source_id:id, volume_db:0.0 }).await.unwrap();
            tokio::time::sleep(Duration::from_millis(100)).await;
            assert!(app.subscribe_meters().borrow().levels.is_empty()); assert_eq!(resolves.load(Ordering::Relaxed),1);
            app.dispatch(Command::AuthorizeSourceCapture { source_id:id }).await.unwrap();
            wait_capture(&app,id,CaptureStatus::Active).await; wait_capture_meter(&app,id).await;
            app.dispatch(Command::SetSourceEnabled { source_id:id, enabled:false }).await.unwrap();
            app.dispatch(Command::SetSourceEnabled { source_id:id, enabled:true }).await.unwrap();
            tokio::time::sleep(Duration::from_millis(100)).await;
            assert!(app.subscribe_meters().borrow().levels.is_empty()); assert_eq!(resolves.load(Ordering::Relaxed),2);
            app.dispatch(Command::AuthorizeSourceCapture { source_id:id }).await.unwrap();
            wait_capture(&app,id,CaptureStatus::Active).await; wait_capture_meter(&app,id).await;
            fail.store(true,Ordering::Release); wait_capture(&app,id,CaptureStatus::Failed).await;
            assert!(app.subscribe_meters().borrow().levels.is_empty());
            app.dispatch(Command::SetSourceVolume { source_id:id, volume_db:-6.0 }).await.unwrap();
            tokio::time::sleep(Duration::from_millis(150)).await;
            assert!(app.subscribe_meters().borrow().levels.is_empty()); assert_eq!(resolves.load(Ordering::Relaxed),3);
            app.dispatch(Command::AuthorizeSourceCapture { source_id:id }).await.unwrap();
            wait_capture(&app,id,CaptureStatus::Active).await; wait_capture_meter(&app,id).await;
            assert_eq!(resolves.load(Ordering::Relaxed),4);
            app.dispatch(Command::RemoveSource { source_id:id }).await.unwrap();
            assert!(app.subscribe_meters().borrow().levels.is_empty());
        });
        cancel.send(()).unwrap();
        worker.join().unwrap().unwrap();
        runtime.block_on(app.shutdown());
    }
    #[test]
    fn capture_without_first_measurement_times_out_without_automatic_reauthorization() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let app =
            runtime.block_on(async { AppHandle::spawn(prismcast_app::CoreConfig::default()) });
        let id = runtime.block_on(configured_capture(&app));
        let owner = runtime.block_on(app.attach_audio_owner()).unwrap();
        let (cancel, cancelled) = oneshot::channel();
        let (status, _) = watch::channel(AudioStatus::Starting);
        let resolves = Arc::new(AtomicUsize::new(0));
        let counts = resolves.clone();
        let worker = std::thread::spawn(move || {
            run_with_graph(
                owner,
                cancelled,
                status,
                FixtureCaptureGraph {
                    native: GstAudioMixer::new().unwrap(),
                    resolves: counts,
                    fail: Arc::new(AtomicBool::new(false)),
                    suppress_data: true,
                },
            )
        });
        runtime.block_on(async {
            app.dispatch(Command::AuthorizeSourceCapture { source_id: id })
                .await
                .unwrap();
            wait_capture(&app, id, CaptureStatus::Failed).await;
            assert!(app
                .snapshot()
                .source_runtime(id)
                .unwrap()
                .message
                .as_deref()
                .unwrap()
                .contains("five seconds"));
            assert!(app.subscribe_meters().borrow().levels.is_empty());
            app.dispatch(Command::SetSourceVolume {
                source_id: id,
                volume_db: -3.0,
            })
            .await
            .unwrap();
            tokio::time::sleep(Duration::from_millis(100)).await;
            assert_eq!(resolves.load(Ordering::Relaxed), 1);
        });
        cancel.send(()).unwrap();
        worker.join().unwrap().unwrap();
        runtime.block_on(app.shutdown());
    }
    struct GatedResolverGraph {
        graph: FixtureCaptureGraph,
        entered: Arc<AtomicBool>,
        release: Arc<AtomicBool>,
        stops: Arc<AtomicUsize>,
    }
    impl OwnedAudioGraph for GatedResolverGraph {
        type Granted = SourceId;
        fn authorize(
            &mut self,
            id: SourceId,
            generation: CaptureGeneration,
            settings: &PipeWireAudioSettings,
        ) -> prismcast_core::Result<SourceId> {
            if self.graph.resolves.load(Ordering::Relaxed) == 1 {
                assert!(
                    self.stops.load(Ordering::Relaxed) > 0,
                    "superseded native capture must stop before slow resolution"
                );
                self.entered.store(true, Ordering::Release);
                let deadline = Instant::now() + Duration::from_secs(3);
                while !self.release.load(Ordering::Acquire) {
                    assert!(Instant::now() < deadline, "resolver gate not released");
                    std::thread::sleep(Duration::from_millis(2));
                }
            }
            self.graph.authorize(id, generation, settings)
        }
        fn reconcile(
            &mut self,
            state: &prismcast_core::AppState,
            grants: &[SourceId],
        ) -> prismcast_core::Result<()> {
            self.graph.reconcile(state, grants)
        }
        fn poll(&mut self) -> prismcast_core::Result<Vec<prismcast_audio::SourceMeter>> {
            self.graph.poll()
        }
        fn stop(&mut self) -> prismcast_core::Result<()> {
            self.stops.fetch_add(1, Ordering::Relaxed);
            self.graph.stop()
        }
    }
    #[test]
    fn superseded_capture_stops_before_resolution_and_stale_resolver_completion_is_discarded() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let app =
            runtime.block_on(async { AppHandle::spawn(prismcast_app::CoreConfig::default()) });
        let id = runtime.block_on(configured_capture(&app));
        let owner = runtime.block_on(app.attach_audio_owner()).unwrap();
        let (cancel, cancelled) = oneshot::channel();
        let (status, _) = watch::channel(AudioStatus::Starting);
        let resolves = Arc::new(AtomicUsize::new(0));
        let entered = Arc::new(AtomicBool::new(false));
        let release = Arc::new(AtomicBool::new(false));
        let stops = Arc::new(AtomicUsize::new(0));
        let counts = resolves.clone();
        let started = entered.clone();
        let gate = release.clone();
        let stopped = stops.clone();
        let worker = std::thread::spawn(move || {
            run_with_graph(
                owner,
                cancelled,
                status,
                GatedResolverGraph {
                    graph: FixtureCaptureGraph {
                        native: GstAudioMixer::new().unwrap(),
                        resolves: counts,
                        fail: Arc::new(AtomicBool::new(false)),
                        suppress_data: false,
                    },
                    entered: started,
                    release: gate,
                    stops: stopped,
                },
            )
        });
        runtime.block_on(async {
            app.dispatch(Command::AuthorizeSourceCapture { source_id: id })
                .await
                .unwrap();
            wait_capture(&app, id, CaptureStatus::Active).await;
            wait_capture_meter(&app, id).await;
            app.dispatch(Command::AuthorizeSourceCapture { source_id: id })
                .await
                .unwrap();
            tokio::time::timeout(Duration::from_secs(2), async {
                while !entered.load(Ordering::Acquire) {
                    tokio::time::sleep(Duration::from_millis(2)).await;
                }
            })
            .await
            .unwrap();
            assert!(stops.load(Ordering::Relaxed) > 0);
            assert!(app.subscribe_meters().borrow().levels.is_empty());
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
            app.dispatch(Command::AuthorizeSourceCapture { source_id: id })
                .await
                .unwrap();
            let latest_generation = app.snapshot().source_runtime(id).unwrap().generation;
            release.store(true, Ordering::Release);
            wait_capture(&app, id, CaptureStatus::Active).await;
            wait_capture_meter(&app, id).await;
            assert_eq!(
                app.snapshot().source_runtime(id).unwrap().generation,
                latest_generation
            );
            assert_eq!(resolves.load(Ordering::Relaxed), 3);
        });
        cancel.send(()).unwrap();
        worker.join().unwrap().unwrap();
        runtime.block_on(app.shutdown());
    }
    struct ConcurrentFaultEdit {
        native: GstAudioMixer,
        app: AppHandle,
        id: SourceId,
        fail: Arc<AtomicBool>,
        edit_on_stop: bool,
    }
    impl OwnedAudioGraph for ConcurrentFaultEdit {
        type Granted = AuthorizedAudioTarget;
        fn authorize(
            &mut self,
            id: SourceId,
            generation: CaptureGeneration,
            settings: &PipeWireAudioSettings,
        ) -> prismcast_core::Result<Self::Granted> {
            resolve_audio_target(id, generation, settings)
        }
        fn reconcile(
            &mut self,
            state: &prismcast_core::AppState,
            grants: &[Self::Granted],
        ) -> prismcast_core::Result<()> {
            self.native.reconcile_authorized(state, grants)
        }
        fn poll(&mut self) -> prismcast_core::Result<Vec<prismcast_audio::SourceMeter>> {
            if self.fail.swap(false, Ordering::AcqRel) {
                self.edit_on_stop = true;
                return Err(prismcast_core::Error::Media(
                    "fault with concurrent controller edit".into(),
                ));
            }
            self.native.poll()
        }
        fn stop(&mut self) -> prismcast_core::Result<()> {
            self.native.stop()?;
            if self.edit_on_stop {
                self.edit_on_stop = false;
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                runtime
                    .block_on(self.app.dispatch(Command::SetSourceVolume {
                        source_id: self.id,
                        volume_db: -6.0,
                    }))
                    .unwrap();
            }
            Ok(())
        }
    }
    #[test]
    fn command_during_fault_cleanup_is_not_marked_reconciled_without_processing() {
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
                    name: "Concurrent tone".into(),
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
                settings: serde_json::json!({"audio_test":true}),
            })
            .await
            .unwrap();
            id
        });
        let owner = runtime.block_on(app.attach_audio_owner()).unwrap();
        let (cancel, cancelled) = oneshot::channel();
        let (status, _) = watch::channel(AudioStatus::Starting);
        let fail = Arc::new(AtomicBool::new(false));
        let fault = fail.clone();
        let controller = app.clone();
        let worker = std::thread::spawn(move || {
            run_with_graph(
                owner,
                cancelled,
                status,
                ConcurrentFaultEdit {
                    native: GstAudioMixer::new().unwrap(),
                    app: controller,
                    id,
                    fail: fault,
                    edit_on_stop: false,
                },
            )
        });
        runtime.block_on(async {
            assert!(wait_capture_meter(&app, id).await > -10.0);
            fail.store(true, Ordering::Release);
            let mut meters = app.subscribe_meters();
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if meters
                        .borrow_and_update()
                        .levels
                        .get(&id)
                        .is_some_and(|level| level.peak_dbfs[0] < -10.0)
                    {
                        break;
                    }
                    meters.changed().await.unwrap();
                }
            })
            .await
            .unwrap();
            assert_eq!(app.snapshot().state().audio.mixer_state(id).volume_db, -6.0);
        });
        cancel.send(()).unwrap();
        worker.join().unwrap().unwrap();
        runtime.block_on(app.shutdown());
    }
}
