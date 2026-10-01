//! Capture effects are consumed once. Native mutations stay on the preview OS owner.
//! Portal leases arrive through async broker waits on Tokio; camera sessions
//! (ADR-0019) are lease-free: path validation, node pre-open checks and element
//! construction inside CameraSession::open are near-instant synchronous work, so
//! they run directly on this owner thread and only completion delivery rides the
//! runtime. CameraSession::close blocks like the portal lease close and is
//! invoked with the same owner-thread discipline, never on a Tokio worker.
use prismcast_app::{AppSnapshot, CaptureAuthorizationRequest, CaptureOwner};
use prismcast_capture::camera::CameraSession;
use prismcast_capture::producer::{CaptureFeed, CaptureProducer};
use prismcast_capture::{CaptureBroker, CaptureConfig, CaptureError, CaptureKind, CaptureLease};
use prismcast_core::{CaptureGeneration, CaptureStatus, SourceDimensions, SourceId, SourceKind};
use prismcast_media::{BackendComponent, ComponentState};
use prismcast_media_gst::GstCompositor;
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};
use tokio::{runtime::Runtime, sync::mpsc, task::JoinHandle};

pub(super) struct Completion {
    source_id: SourceId,
    generation: CaptureGeneration,
    result: prismcast_capture::Result<OpenedCapture>,
}
/// One authorized native capture; cameras carry no lease (ADR-0019).
pub(super) enum OpenedCapture {
    Portal(CaptureLease),
    Camera(CameraSession),
}
/// Running native capture behind a uniform feed/status surface.
enum NativeCapture {
    Portal(CaptureProducer),
    Camera(CameraSession),
}
impl NativeCapture {
    fn feed(&self) -> Option<CaptureFeed> {
        match self {
            Self::Portal(producer) => Some(producer.feed()),
            Self::Camera(session) => session.feed(),
        }
    }
}
struct Pending {
    generation: CaptureGeneration,
    task: JoinHandle<()>,
}
#[derive(Clone)]
struct Report {
    generation: CaptureGeneration,
    status: CaptureStatus,
    dimensions: Option<SourceDimensions>,
    message: Option<String>,
}
struct Active {
    generation: CaptureGeneration,
    capture: NativeCapture,
    dimensions: Option<(u32, u32)>,
    started: Instant,
}
pub(super) struct Captures {
    pub owner: CaptureOwner,
    pub completed: mpsc::Receiver<Completion>,
    tx: mpsc::Sender<Completion>,
    broker: CaptureBroker,
    pending: HashMap<SourceId, Pending>,
    active: HashMap<SourceId, Active>,
    reports: HashMap<SourceId, Report>,
    graph_error: Option<String>,
}
fn valid(snapshot: &AppSnapshot, id: SourceId, generation: CaptureGeneration) -> bool {
    snapshot.state().sources.get(&id).is_some_and(|source| {
        source.enabled
            && matches!(
                source.kind,
                SourceKind::PipeWireDisplay | SourceKind::PipeWireWindow | SourceKind::V4l2Camera
            )
    }) && snapshot.source_runtime(id).is_some_and(|runtime| {
        runtime.generation == generation
            && matches!(
                runtime.status,
                CaptureStatus::Authorizing | CaptureStatus::Active
            )
    })
}
impl Captures {
    pub fn new(owner: CaptureOwner) -> prismcast_capture::Result<Self> {
        let (tx, completed) = mpsc::channel(8);
        Ok(Self {
            owner,
            completed,
            tx,
            broker: CaptureBroker::new(CaptureConfig {
                max_leases: 8,
                ..CaptureConfig::default()
            })?,
            pending: HashMap::new(),
            active: HashMap::new(),
            reports: HashMap::new(),
            graph_error: None,
        })
    }
    fn report(
        &mut self,
        runtime: &Runtime,
        id: SourceId,
        generation: CaptureGeneration,
        status: CaptureStatus,
        dimensions: Option<(u32, u32)>,
        message: Option<String>,
    ) {
        let report = Report {
            generation,
            status,
            dimensions: dimensions.map(|(width, height)| SourceDimensions { width, height }),
            message,
        };
        if self.send_report(runtime, id, &report) {
            self.reports.remove(&id);
        } else {
            let latest = self.owner.snapshots.borrow().clone();
            self.reports
                .retain(|id, report| valid(&latest, *id, report.generation));
            if self.reports.len() < 8 || self.reports.contains_key(&id) {
                self.reports.insert(id, report);
            } else {
                tracing::warn!(source_id=%id,"bounded capture report capacity exhausted");
            }
        }
    }
    fn send_report(&self, runtime: &Runtime, id: SourceId, report: &Report) -> bool {
        let result = runtime.block_on(async {
            tokio::time::timeout(
                Duration::from_millis(250),
                self.owner.runtime.report(
                    id,
                    report.generation,
                    report.status,
                    report.dimensions,
                    report.message.clone(),
                ),
            )
            .await
        });
        match result {
            Ok(Ok(())) => true,
            Ok(Err(error)) => {
                tracing::debug!(source_id=%id,%error,"capture report retired/rejected");
                true
            }
            Err(_) => false,
        }
    }
    fn flush_reports(&mut self, snapshot: &AppSnapshot, runtime: &Runtime) {
        let reports = self
            .reports
            .iter()
            .map(|(id, report)| (*id, report.clone()))
            .collect::<Vec<_>>();
        for (id, report) in reports {
            if !valid(snapshot, id, report.generation) || self.send_report(runtime, id, &report) {
                self.reports.remove(&id);
            }
        }
    }
    pub fn graph_error(&self) -> Option<&str> {
        self.graph_error.as_deref()
    }
    fn feeds(&self) -> Vec<(SourceId, CaptureFeed)> {
        self.active
            .iter()
            .filter_map(|(id, active)| {
                let feed = active.capture.feed()?;
                (feed.dimensions().is_some() && feed.error().is_none()).then_some((*id, feed))
            })
            .collect()
    }
    fn sync(&mut self, compositor: &mut GstCompositor) {
        if let Err(error) = compositor.sync_capture_feeds(&self.feeds()) {
            self.graph_error = Some(error.to_string());
            let _ = compositor.stop();
        } else if compositor.state() == ComponentState::Running {
            self.graph_error = None;
        }
    }
    fn retire(&mut self, id: SourceId, runtime: &Runtime, compositor: &mut GstCompositor) {
        if let Some(pending) = self.pending.remove(&id) {
            pending.task.abort();
            let _ = runtime.block_on(pending.task);
        }
        if let Some(active) = self.active.remove(&id) {
            // Consumer NULL/remove barrier precedes stopping the persistent producer.
            self.sync(compositor);
            match active.capture {
                NativeCapture::Portal(mut producer) => {
                    if let Err(error) = producer.shutdown_native() {
                        tracing::warn!(source_id=%id,%error,"capture native stop failed; retaining graph until object destruction");
                    }
                    match producer.take_stopped_lease() {
                        Ok(lease) => {
                            drop(producer);
                            let result = runtime.block_on(lease.close());
                            if let Err(error) = result {
                                tracing::warn!(source_id=%id,%error,"capture lease cleanup failed");
                            }
                        }
                        Err(_) => drop(producer),
                    }
                }
                NativeCapture::Camera(session) => {
                    // Blocking like lease.close above: stops the native graph
                    // and joins the session worker on this owner thread.
                    if let Err(error) = session.close() {
                        tracing::warn!(source_id=%id,%error,"camera session cleanup failed");
                    }
                }
            }
        }
    }
    pub fn prune(
        &mut self,
        snapshot: &AppSnapshot,
        runtime: &Runtime,
        compositor: &mut GstCompositor,
    ) {
        let stale = self
            .pending
            .iter()
            .map(|(id, p)| (*id, p.generation))
            .chain(self.active.iter().map(|(id, a)| (*id, a.generation)))
            .filter_map(|(id, generation)| (!valid(snapshot, id, generation)).then_some(id))
            .collect::<Vec<_>>();
        for id in stale {
            self.retire(id, runtime, compositor);
        }
    }
    pub fn authorize(
        &mut self,
        request: CaptureAuthorizationRequest,
        snapshot: &AppSnapshot,
        runtime: &Runtime,
        compositor: &mut GstCompositor,
    ) {
        if !valid(snapshot, request.source_id, request.generation) {
            return;
        }
        self.retire(request.source_id, runtime, compositor);
        let kind = match snapshot
            .state()
            .sources
            .get(&request.source_id)
            .map(|source| source.kind)
        {
            Some(SourceKind::PipeWireDisplay) => CaptureKind::Monitor,
            Some(SourceKind::PipeWireWindow) => CaptureKind::Window,
            Some(SourceKind::V4l2Camera) => {
                return self.authorize_camera(request, snapshot, runtime);
            }
            _ => return,
        };
        let pending = {
            let _entered = runtime.enter();
            self.broker
                .authorize_with_parent(request.source_id, kind, request.parent_window)
        };
        match pending {
            Ok(pending) => {
                let id = request.source_id;
                let generation = request.generation;
                let tx = self.tx.clone();
                let task = runtime.spawn(async move {
                    let result = pending.wait().await.map(OpenedCapture::Portal);
                    let _ = tx
                        .send(Completion {
                            source_id: id,
                            generation,
                            result,
                        })
                        .await;
                });
                self.pending.insert(
                    id,
                    Pending {
                        generation: request.generation,
                        task,
                    },
                );
            }
            Err(error) => self.report_error(runtime, request.source_id, request.generation, error),
        }
    }
    /// Lease-free camera authorization (ADR-0019). Validation failures and open
    /// errors report synchronously, mirroring the broker error arm; an opened
    /// session still travels through Completion so activation re-checks the
    /// generation guard in complete().
    fn authorize_camera(
        &mut self,
        request: CaptureAuthorizationRequest,
        snapshot: &AppSnapshot,
        runtime: &Runtime,
    ) {
        let id = request.source_id;
        let generation = request.generation;
        let device = snapshot
            .state()
            .sources
            .get(&id)
            .and_then(|source| source.settings.get("device"))
            .and_then(|device| device.as_str())
            .filter(|device| !device.is_empty());
        let Some(device) = device else {
            let error =
                CaptureError::Unsupported("camera source settings lack a \"device\" path".into());
            return self.report_error(runtime, id, generation, error);
        };
        // Synchronous open on this owner thread: validate, node pre-open check
        // and v4l2src construction; the session worker thread takes over after.
        match CameraSession::open(device) {
            Ok(session) => {
                let tx = self.tx.clone();
                let task = runtime.spawn(async move {
                    let _ = tx
                        .send(Completion {
                            source_id: id,
                            generation,
                            result: Ok(OpenedCapture::Camera(session)),
                        })
                        .await;
                });
                self.pending.insert(id, Pending { generation, task });
            }
            Err(error) => self.report_error(runtime, id, generation, error),
        }
    }
    fn report_error(
        &mut self,
        runtime: &Runtime,
        id: SourceId,
        generation: CaptureGeneration,
        error: CaptureError,
    ) {
        let status = match error {
            CaptureError::Cancelled => CaptureStatus::Cancelled,
            CaptureError::Denied(_) => CaptureStatus::Denied,
            CaptureError::Closed => CaptureStatus::Revoked,
            _ => CaptureStatus::Failed,
        };
        self.report(
            runtime,
            id,
            generation,
            status,
            None,
            Some(error.to_string()),
        );
    }
    pub fn complete(
        &mut self,
        completion: Completion,
        snapshot: &AppSnapshot,
        runtime: &Runtime,
        compositor: &mut GstCompositor,
    ) {
        let Completion {
            source_id: id,
            generation,
            result,
        } = completion;
        if self
            .pending
            .get(&id)
            .is_some_and(|pending| pending.generation == generation)
        {
            self.pending.remove(&id);
        }
        if !valid(snapshot, id, generation) {
            match result {
                Ok(OpenedCapture::Portal(lease)) => {
                    let _ = runtime.block_on(lease.close());
                }
                Ok(OpenedCapture::Camera(session)) => {
                    let _ = session.close();
                }
                Err(_) => {}
            }
            return;
        }
        let capture = match result {
            Ok(OpenedCapture::Portal(lease)) => match CaptureProducer::start(lease) {
                Ok(producer) => NativeCapture::Portal(producer),
                Err(error) => {
                    self.report_error(runtime, id, generation, error);
                    self.health(snapshot, runtime, compositor);
                    return;
                }
            },
            Ok(OpenedCapture::Camera(session)) => NativeCapture::Camera(session),
            Err(error) => {
                self.report_error(runtime, id, generation, error);
                self.health(snapshot, runtime, compositor);
                return;
            }
        };
        self.active.insert(
            id,
            Active {
                generation,
                capture,
                dimensions: None,
                started: Instant::now(),
            },
        );
        self.health(snapshot, runtime, compositor);
    }
    pub fn health(
        &mut self,
        snapshot: &AppSnapshot,
        runtime: &Runtime,
        compositor: &mut GstCompositor,
    ) {
        self.prune(snapshot, runtime, compositor);
        self.flush_reports(snapshot, runtime);
        let mut terminal = Vec::new();
        for (id, active) in &mut self.active {
            let closed = CaptureError::Closed.to_string();
            match &active.capture {
                NativeCapture::Portal(producer) => {
                    if producer.status() != prismcast_capture::CaptureStatus::Ready {
                        terminal.push((*id, active.generation, CaptureStatus::Revoked, closed));
                        continue;
                    }
                    if let Some(error) = producer.error() {
                        terminal.push((*id, active.generation, CaptureStatus::Failed, error));
                        continue;
                    }
                }
                NativeCapture::Camera(session) => match session.status() {
                    prismcast_capture::CaptureStatus::Authorizing
                    | prismcast_capture::CaptureStatus::Ready => {}
                    // ADR-0019: losing a device after Active maps to Revoked,
                    // the same outcome report_error gives CaptureError::Closed.
                    prismcast_capture::CaptureStatus::Failed(message)
                        if active.dimensions.is_some() =>
                    {
                        terminal.push((*id, active.generation, CaptureStatus::Revoked, message));
                        continue;
                    }
                    prismcast_capture::CaptureStatus::Failed(message) => {
                        terminal.push((*id, active.generation, CaptureStatus::Failed, message));
                        continue;
                    }
                    prismcast_capture::CaptureStatus::Cancelled
                    | prismcast_capture::CaptureStatus::Closed => {
                        terminal.push((*id, active.generation, CaptureStatus::Revoked, closed));
                        continue;
                    }
                },
            }
            let dimensions = active.capture.feed().and_then(|feed| feed.dimensions());
            if let Some(dimensions) = dimensions {
                if active.dimensions != Some(dimensions) {
                    active.dimensions = Some(dimensions);
                }
            } else if active.started.elapsed() > Duration::from_secs(10) {
                terminal.push((
                    *id,
                    active.generation,
                    CaptureStatus::Failed,
                    "capture negotiated no frame within 10s".to_owned(),
                ));
            }
        }
        for (id, generation, status, message) in terminal {
            self.retire(id, runtime, compositor);
            self.report(runtime, id, generation, status, None, Some(message));
        }
        // Core reports only changed caps/status, preventing a self-generated watch loop.
        let updates = self
            .active
            .iter()
            .filter_map(|(id, active)| {
                active
                    .dimensions
                    .map(|dimensions| (*id, active.generation, dimensions))
            })
            .collect::<Vec<_>>();
        for (id, generation, dimensions) in updates {
            let already = snapshot.source_runtime(id).is_some_and(|state| {
                state.status == CaptureStatus::Active
                    && state.dimensions
                        == Some(SourceDimensions {
                            width: dimensions.0,
                            height: dimensions.1,
                        })
            });
            if !already {
                self.report(
                    runtime,
                    id,
                    generation,
                    CaptureStatus::Active,
                    Some(dimensions),
                    None,
                );
            }
        }
        self.sync(compositor);
    }
    pub fn shutdown(&mut self, runtime: &Runtime, compositor: &mut GstCompositor) {
        let ids = self
            .pending
            .keys()
            .chain(self.active.keys())
            .copied()
            .collect::<Vec<_>>();
        for id in ids {
            self.retire(id, runtime, compositor);
        }
        while let Ok(completion) = self.completed.try_recv() {
            match completion.result {
                Ok(OpenedCapture::Portal(lease)) => {
                    let _ = runtime.block_on(lease.close());
                }
                Ok(OpenedCapture::Camera(session)) => {
                    let _ = session.close();
                }
                Err(_) => {}
            }
        }
        if let Err(error) = runtime.block_on(self.broker.shutdown()) {
            tracing::warn!(%error,"capture broker shutdown failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gstreamer as gst;
    use prismcast_app::{AppHandle, CoreConfig};
    use prismcast_core::{AppState, Command, Source};
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    struct Cancelled(Arc<AtomicBool>);
    impl Drop for Cancelled {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    fn setup() -> (Runtime, AppHandle, Captures, GstCompositor, SourceId) {
        setup_source(SourceKind::PipeWireWindow, serde_json::Value::Null)
    }
    fn setup_source(
        kind: SourceKind,
        settings: serde_json::Value,
    ) -> (Runtime, AppHandle, Captures, GstCompositor, SourceId) {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let mut source = Source::new(kind, "restored");
        source.settings = settings;
        let id = source.id;
        let mut state = AppState::new();
        state.sources.insert(id, source);
        let (handle, owner) = runtime.block_on(async {
            let handle = AppHandle::spawn_with_state(state, CoreConfig::default());
            let owner = handle.attach_capture_owner().await.unwrap();
            (handle, owner)
        });
        gst::init().unwrap();
        let sink = gst::ElementFactory::make("fakesink").build().unwrap();
        (
            runtime,
            handle,
            Captures::new(owner).unwrap(),
            GstCompositor::new(sink).unwrap(),
            id,
        )
    }
    fn request(
        runtime: &Runtime,
        handle: &AppHandle,
        captures: &mut Captures,
        id: SourceId,
    ) -> CaptureAuthorizationRequest {
        runtime
            .block_on(handle.authorize_source_capture(id, None))
            .unwrap();
        runtime.block_on(captures.owner.requests.recv()).unwrap()
    }
    #[test]
    fn restored_state_never_authorizes_and_stale_completion_cannot_revive_retry() {
        let (runtime, handle, mut captures, mut compositor, id) = setup();
        assert!(captures.owner.requests.try_recv().is_err());
        assert!(captures.pending.is_empty());
        assert!(captures.active.is_empty());
        let old = request(&runtime, &handle, &mut captures, id);
        let cancelled = Arc::new(AtomicBool::new(false));
        let guard = Cancelled(cancelled.clone());
        let task = runtime.spawn(async move {
            let _guard = guard;
            std::future::pending::<()>().await;
        });
        captures.pending.insert(
            id,
            Pending {
                generation: old.generation,
                task,
            },
        );
        let fresh = request(&runtime, &handle, &mut captures, id);
        captures.prune(&handle.snapshot(), &runtime, &mut compositor);
        assert!(cancelled.load(Ordering::SeqCst));
        assert!(captures.pending.is_empty());
        captures.complete(
            Completion {
                source_id: id,
                generation: old.generation,
                result: Err(CaptureError::Denied("late".into())),
            },
            &handle.snapshot(),
            &runtime,
            &mut compositor,
        );
        let snapshot = handle.snapshot();
        assert_eq!(
            snapshot.source_runtime(id).unwrap().generation,
            fresh.generation
        );
        assert_eq!(
            snapshot.source_runtime(id).unwrap().status,
            CaptureStatus::Authorizing
        );
        captures.complete(
            Completion {
                source_id: id,
                generation: fresh.generation,
                result: Err(CaptureError::Native("failure\n🦀".repeat(200))),
            },
            &snapshot,
            &runtime,
            &mut compositor,
        );
        let snapshot = handle.snapshot();
        let state = snapshot.source_runtime(id).unwrap();
        assert_eq!(state.status, CaptureStatus::Failed);
        let message = state.message.as_ref().unwrap();
        assert!(message.len() <= 512);
        assert!(!message.chars().any(char::is_control));
        captures.shutdown(&runtime, &mut compositor);
        runtime.block_on(handle.shutdown());
    }
    #[test]
    fn disable_and_remove_retire_generation_before_late_result() {
        let (runtime, handle, mut captures, mut compositor, id) = setup();
        let effect = request(&runtime, &handle, &mut captures, id);
        runtime
            .block_on(handle.dispatch(Command::SetSourceEnabled {
                source_id: id,
                enabled: false,
            }))
            .unwrap();
        captures.complete(
            Completion {
                source_id: id,
                generation: effect.generation,
                result: Err(CaptureError::Cancelled),
            },
            &handle.snapshot(),
            &runtime,
            &mut compositor,
        );
        assert!(handle.snapshot().source_runtime(id).is_none());
        runtime
            .block_on(handle.dispatch(Command::RemoveSource { source_id: id }))
            .unwrap();
        captures.health(&handle.snapshot(), &runtime, &mut compositor);
        assert!(captures.reports.is_empty());
        assert!(captures.active.is_empty());
        captures.shutdown(&runtime, &mut compositor);
        runtime.block_on(handle.shutdown());
    }
    #[test]
    fn camera_authorize_without_device_setting_reports_failed_and_opens_nothing() {
        let (runtime, handle, mut captures, mut compositor, id) =
            setup_source(SourceKind::V4l2Camera, serde_json::Value::Null);
        let request = request(&runtime, &handle, &mut captures, id);
        captures.authorize(request, &handle.snapshot(), &runtime, &mut compositor);
        let snapshot = handle.snapshot();
        let state = snapshot.source_runtime(id).unwrap();
        assert_eq!(state.status, CaptureStatus::Failed);
        assert!(state.message.as_ref().unwrap().contains("device"));
        assert!(captures.pending.is_empty());
        assert!(captures.active.is_empty());
        assert!(captures.completed.try_recv().is_err());
        captures.shutdown(&runtime, &mut compositor);
        runtime.block_on(handle.shutdown());
    }
    #[test]
    fn camera_missing_node_reports_failed_without_pending() {
        let (runtime, handle, mut captures, mut compositor, id) = setup_source(
            SourceKind::V4l2Camera,
            serde_json::json!({"device": "/dev/prismcast-test-missing"}),
        );
        let request = request(&runtime, &handle, &mut captures, id);
        captures.authorize(request, &handle.snapshot(), &runtime, &mut compositor);
        let snapshot = handle.snapshot();
        let state = snapshot.source_runtime(id).unwrap();
        assert_eq!(state.status, CaptureStatus::Failed);
        assert!(state.message.as_ref().unwrap().contains("does not exist"));
        assert!(captures.pending.is_empty());
        assert!(captures.active.is_empty());
        captures.shutdown(&runtime, &mut compositor);
        runtime.block_on(handle.shutdown());
    }
    #[test]
    fn camera_stale_generation_cannot_revive_retry() {
        let (runtime, handle, mut captures, mut compositor, id) = setup_source(
            SourceKind::V4l2Camera,
            serde_json::json!({"device": "/dev/video0"}),
        );
        assert!(captures.pending.is_empty());
        assert!(captures.active.is_empty());
        let old = request(&runtime, &handle, &mut captures, id);
        let cancelled = Arc::new(AtomicBool::new(false));
        let guard = Cancelled(cancelled.clone());
        let task = runtime.spawn(async move {
            let _guard = guard;
            std::future::pending::<()>().await;
        });
        captures.pending.insert(
            id,
            Pending {
                generation: old.generation,
                task,
            },
        );
        let fresh = request(&runtime, &handle, &mut captures, id);
        captures.prune(&handle.snapshot(), &runtime, &mut compositor);
        assert!(cancelled.load(Ordering::SeqCst));
        assert!(captures.pending.is_empty());
        captures.complete(
            Completion {
                source_id: id,
                generation: old.generation,
                result: Err(CaptureError::Denied("late".into())),
            },
            &handle.snapshot(),
            &runtime,
            &mut compositor,
        );
        let snapshot = handle.snapshot();
        assert_eq!(
            snapshot.source_runtime(id).unwrap().generation,
            fresh.generation
        );
        assert_eq!(
            snapshot.source_runtime(id).unwrap().status,
            CaptureStatus::Authorizing
        );
        captures.complete(
            Completion {
                source_id: id,
                generation: fresh.generation,
                result: Err(CaptureError::Native("camera lost".into())),
            },
            &snapshot,
            &runtime,
            &mut compositor,
        );
        assert_eq!(
            handle.snapshot().source_runtime(id).unwrap().status,
            CaptureStatus::Failed
        );
        captures.shutdown(&runtime, &mut compositor);
        runtime.block_on(handle.shutdown());
    }
    #[test]
    fn camera_disable_and_remove_retire_generation_before_late_result() {
        let (runtime, handle, mut captures, mut compositor, id) = setup_source(
            SourceKind::V4l2Camera,
            serde_json::json!({"device": "/dev/video0"}),
        );
        let effect = request(&runtime, &handle, &mut captures, id);
        runtime
            .block_on(handle.dispatch(Command::SetSourceEnabled {
                source_id: id,
                enabled: false,
            }))
            .unwrap();
        captures.complete(
            Completion {
                source_id: id,
                generation: effect.generation,
                result: Err(CaptureError::Cancelled),
            },
            &handle.snapshot(),
            &runtime,
            &mut compositor,
        );
        assert!(handle.snapshot().source_runtime(id).is_none());
        runtime
            .block_on(handle.dispatch(Command::RemoveSource { source_id: id }))
            .unwrap();
        captures.health(&handle.snapshot(), &runtime, &mut compositor);
        assert!(captures.reports.is_empty());
        assert!(captures.active.is_empty());
        captures.shutdown(&runtime, &mut compositor);
        runtime.block_on(handle.shutdown());
    }
    #[test]
    fn camera_settings_change_clears_runtime_and_prune_retires_pending() {
        let (runtime, handle, mut captures, mut compositor, id) = setup_source(
            SourceKind::V4l2Camera,
            serde_json::json!({"device": "/dev/video0"}),
        );
        let effect = request(&runtime, &handle, &mut captures, id);
        let cancelled = Arc::new(AtomicBool::new(false));
        let guard = Cancelled(cancelled.clone());
        let task = runtime.spawn(async move {
            let _guard = guard;
            std::future::pending::<()>().await;
        });
        captures.pending.insert(
            id,
            Pending {
                generation: effect.generation,
                task,
            },
        );
        runtime
            .block_on(handle.dispatch(Command::SetSourceSettings {
                source_id: id,
                settings: serde_json::json!({"device": "/dev/video2"}),
            }))
            .unwrap();
        assert!(handle.snapshot().source_runtime(id).is_none());
        captures.prune(&handle.snapshot(), &runtime, &mut compositor);
        assert!(cancelled.load(Ordering::SeqCst));
        assert!(captures.pending.is_empty());
        captures.shutdown(&runtime, &mut compositor);
        runtime.block_on(handle.shutdown());
    }
}
