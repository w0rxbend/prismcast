//! Explicit, ephemeral portal authorization. No controller or persisted state wiring.
use prismcast_core::SourceId;
use std::{
    future::Future,
    os::fd::OwnedFd,
    pin::Pin,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::sync::{oneshot, watch, Notify, OwnedSemaphorePermit, Semaphore};

pub mod camera;
pub mod devices;
mod portal;
pub mod probe;
pub mod producer;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureKind {
    Monitor,
    Window,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureStatus {
    Authorizing,
    Ready,
    Cancelled,
    Closed,
    Failed(String),
}
#[derive(Debug, Clone, thiserror::Error)]
pub enum CaptureError {
    #[error("capture capacity exhausted or broker shutting down")]
    Busy,
    #[error("capture authorization cancelled")]
    Cancelled,
    #[error("capture authorization timed out")]
    Timeout,
    #[error("portal session closed")]
    Closed,
    #[error("portal capability unsupported: {0}")]
    Unsupported(String),
    #[error("portal rejected access: {0}")]
    Denied(String),
    #[error("portal denied or failed: {0}")]
    Portal(String),
    #[error("native capture failed: {0}")]
    Native(String),
}
pub type Result<T> = std::result::Result<T, CaptureError>;
type PortalFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

/// Timeouts include user selection. Increase authorization for interactive clients.
#[derive(Debug, Clone, Copy)]
pub struct CaptureConfig {
    pub max_leases: usize,
    pub authorization_timeout: Duration,
    pub cleanup_timeout: Duration,
}
impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            max_leases: 4,
            authorization_timeout: Duration::from_secs(120),
            cleanup_timeout: Duration::from_secs(3),
        }
    }
}

/// Retains the remote socket until the native graph is destroyed. Never serialize.
#[derive(Debug)]
pub struct CaptureGrant {
    source_id: SourceId,
    kind: CaptureKind,
    node_id: u32,
    remote: OwnedFd,
    native_use: Arc<NativeUse>,
    _slot: Arc<OwnedSemaphorePermit>,
}
impl CaptureGrant {
    pub fn source_id(&self) -> SourceId {
        self.source_id
    }
    pub fn kind(&self) -> CaptureKind {
        self.kind
    }
    pub fn node_id(&self) -> u32 {
        self.node_id
    }
    pub fn remote(&self) -> &OwnedFd {
        &self.remote
    }
    pub(crate) fn use_native(&self) -> Result<NativeGuard> {
        self.native_use
            .count
            .compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst)
            .map_err(|_| CaptureError::Busy)?;
        Ok(NativeGuard(self.native_use.clone()))
    }
}

#[derive(Debug, Default)]
struct NativeUse {
    count: AtomicUsize,
    idle: Notify,
}
pub(crate) struct NativeGuard(Arc<NativeUse>);
impl Drop for NativeGuard {
    fn drop(&mut self) {
        if self.0.count.fetch_sub(1, Ordering::SeqCst) == 1 {
            self.0.idle.notify_one();
        }
    }
}

/// One worker per admitted lease; dropped callers cannot abandon worker cleanup.
pub struct CaptureBroker {
    slots: Arc<Semaphore>,
    workers: Arc<Semaphore>,
    shutdown: watch::Sender<bool>,
    config: CaptureConfig,
}
impl CaptureBroker {
    pub fn new(config: CaptureConfig) -> Result<Self> {
        if !(1..=16).contains(&config.max_leases)
            || config.authorization_timeout.is_zero()
            || config.cleanup_timeout.is_zero()
            || config.authorization_timeout > Duration::from_secs(3600)
            || config.cleanup_timeout > Duration::from_secs(30)
        {
            return Err(CaptureError::Unsupported("invalid broker limits".into()));
        }
        let (shutdown, _) = watch::channel(false);
        Ok(Self {
            slots: Arc::new(Semaphore::new(config.max_leases)),
            workers: Arc::new(Semaphore::new(config.max_leases)),
            shutdown,
            config,
        })
    }
    /// Explicit invocation can open a portal picker. Does not retry automatically.
    pub fn authorize(&self, source_id: SourceId, kind: CaptureKind) -> Result<PendingCapture> {
        self.begin(
            source_id,
            kind,
            Box::pin(portal::PortalConnection::connect(None)),
        )
    }
    /// Local parent context is ephemeral and never a domain source setting.
    pub fn authorize_with_parent(
        &self,
        source_id: SourceId,
        kind: CaptureKind,
        parent: Option<String>,
    ) -> Result<PendingCapture> {
        let parent = parent
            .map(|parent| {
                if parent.len() > 2048
                    || parent.chars().any(|c| c.is_control() || c.is_whitespace())
                    || parent
                        .split_once(':')
                        .is_none_or(|(_, value)| value.is_empty())
                {
                    return Err(CaptureError::Unsupported(
                        "invalid parent identifier".into(),
                    ));
                }
                parent
                    .parse::<ashpd::WindowIdentifierType>()
                    .map(ashpd::WindowIdentifier::from)
                    .map_err(|_| CaptureError::Unsupported("invalid parent identifier".into()))
            })
            .transpose()?;
        self.begin(
            source_id,
            kind,
            Box::pin(portal::PortalConnection::connect(parent)),
        )
    }
    fn begin<P: Portal + 'static>(
        &self,
        source_id: SourceId,
        kind: CaptureKind,
        connect: Pin<Box<dyn Future<Output = Result<P>> + Send>>,
    ) -> Result<PendingCapture> {
        let runtime = tokio::runtime::Handle::try_current().map_err(|_| {
            CaptureError::Unsupported("authorization requires an entered Tokio runtime".into())
        })?;
        if *self.shutdown.borrow() {
            return Err(CaptureError::Busy);
        }
        let permit = self
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| CaptureError::Busy)?;
        let worker_permit = self
            .workers
            .clone()
            .try_acquire_owned()
            .map_err(|_| CaptureError::Busy)?;
        let permit = Arc::new(permit);
        let (cancel, cancel_rx) = watch::channel(false);
        let (status_tx, status) = watch::channel(CaptureStatus::Authorizing);
        let (reply_tx, reply) = oneshot::channel();
        let (cleaned_tx, cleaned) = watch::channel(None);
        runtime.spawn(worker(
            source_id,
            kind,
            connect,
            self.config,
            permit,
            worker_permit,
            cancel_rx,
            self.shutdown.subscribe(),
            status_tx,
            reply_tx,
            cleaned_tx,
        ));
        Ok(PendingCapture {
            reply,
            cancel,
            status,
            cleaned,
        })
    }
    /// Cancels all workers and waits for cleanup without accepting more requests.
    pub async fn shutdown(&self) -> Result<()> {
        self.shutdown.send_replace(true);
        let permits = u32::try_from(self.config.max_leases).map_err(|_| CaptureError::Busy)?;
        let all = tokio::time::timeout(
            self.config.cleanup_timeout * 3,
            self.workers.clone().acquire_many_owned(permits),
        )
        .await
        .map_err(|_| CaptureError::Timeout)?;
        drop(all.map_err(|_| CaptureError::Busy)?);
        Ok(())
    }
}
impl Drop for CaptureBroker {
    fn drop(&mut self) {
        self.shutdown.send_replace(true);
    }
}

pub struct PendingCapture {
    reply: oneshot::Receiver<Result<CaptureGrant>>,
    cancel: watch::Sender<bool>,
    status: watch::Receiver<CaptureStatus>,
    cleaned: watch::Receiver<Option<Result<()>>>,
}
impl PendingCapture {
    pub fn status(&self) -> watch::Receiver<CaptureStatus> {
        self.status.clone()
    }
    pub fn cancel(&self) {
        self.cancel.send_replace(true);
    }
    pub async fn wait(mut self) -> Result<CaptureLease> {
        let grant = (&mut self.reply)
            .await
            .map_err(|_| CaptureError::Closed)??;
        // Cloning keeps the worker alive after this PendingCapture drops.
        Ok(CaptureLease {
            grant,
            cancel: self.cancel.clone(),
            status: self.status.clone(),
            cleaned: self.cleaned.clone(),
        })
    }
}
// Sender closure is cancellation; no task spawn or async work in Drop.
pub struct CaptureLease {
    grant: CaptureGrant,
    cancel: watch::Sender<bool>,
    status: watch::Receiver<CaptureStatus>,
    cleaned: watch::Receiver<Option<Result<()>>>,
}
impl CaptureLease {
    pub fn grant(&self) -> &CaptureGrant {
        &self.grant
    }
    pub fn status(&self) -> CaptureStatus {
        self.status.borrow().clone()
    }
    pub fn subscribe(&self) -> watch::Receiver<CaptureStatus> {
        self.status.clone()
    }
    /// Destroy native graphs first, then await acknowledgement of session cleanup.
    pub async fn close(self) -> Result<()> {
        self.cancel.send_replace(true);
        let mut cleaned = self.cleaned.clone();
        loop {
            if let Some(result) = cleaned.borrow().clone() {
                return result;
            }
            cleaned.changed().await.map_err(|_| CaptureError::Closed)?;
        }
    }
}

trait Portal: Send + Sync {
    fn create(&mut self, kind: CaptureKind) -> PortalFuture<'_, ()>;
    fn closed(&self) -> watch::Receiver<bool>;
    fn select(&self, kind: CaptureKind) -> PortalFuture<'_, ()>;
    fn start(&self) -> PortalFuture<'_, u32>;
    fn remote(&self) -> PortalFuture<'_, OwnedFd>;
    fn cleanup(&mut self) -> PortalFuture<'_, ()>;
}
async fn cancelled(rx: &mut watch::Receiver<bool>) {
    if *rx.borrow() {
        return;
    }
    let _ = rx.changed().await;
}
#[allow(clippy::too_many_arguments)]
async fn worker<P: Portal>(
    source_id: SourceId,
    kind: CaptureKind,
    connect: Pin<Box<dyn Future<Output = Result<P>> + Send>>,
    config: CaptureConfig,
    permit: Arc<OwnedSemaphorePermit>,
    _worker_permit: OwnedSemaphorePermit,
    mut cancel: watch::Receiver<bool>,
    mut shutdown: watch::Receiver<bool>,
    status: watch::Sender<CaptureStatus>,
    reply: oneshot::Sender<Result<CaptureGrant>>,
    cleaned: watch::Sender<Option<Result<()>>>,
) {
    let native_use = Arc::new(NativeUse::default());
    let deadline = tokio::time::Instant::now() + config.authorization_timeout;
    let connected = tokio::select! { biased;
        _ = cancelled(&mut cancel) => Err(CaptureError::Cancelled),
        _ = cancelled(&mut shutdown) => Err(CaptureError::Cancelled),
        _ = tokio::time::sleep_until(deadline) => Err(CaptureError::Timeout),
        value = connect => value,
    };
    let mut portal = match connected {
        Ok(portal) => portal,
        Err(error) => {
            status.send_replace(terminal(&error));
            let _ = reply.send(Err(error));
            cleaned.send_replace(Some(Ok(())));
            return;
        }
    };
    let created = tokio::select! { biased;
        _ = cancelled(&mut cancel) => Err(CaptureError::Cancelled),
        _ = cancelled(&mut shutdown) => Err(CaptureError::Cancelled),
        _ = tokio::time::sleep_until(deadline) => Err(CaptureError::Timeout),
        result = portal.create(kind) => result,
    };
    let mut closed = portal.closed();
    let authorized = match created {
        Err(error) => Err(error),
        Ok(()) => tokio::select! { biased;
            _ = cancelled(&mut cancel) => Err(CaptureError::Cancelled),
            _ = cancelled(&mut shutdown) => Err(CaptureError::Cancelled),
            _ = cancelled(&mut closed) => Err(CaptureError::Closed),
            _ = tokio::time::sleep_until(deadline) => Err(CaptureError::Timeout),
            result = async {
                portal.select(kind).await?;
                let node_id = portal.start().await?;
                let remote = portal.remote().await?;
                Ok(CaptureGrant { source_id, kind, node_id, remote, native_use: native_use.clone(), _slot:permit.clone() })
            } => result,
        },
    };
    let outcome = match authorized {
        Err(error) => {
            let outcome = terminal(&error);
            let _ = reply.send(Err(error));
            outcome
        }
        Ok(grant) => {
            // Recheck terminal signals before publishing a late successful grant.
            if *closed.borrow()
                || *cancel.borrow()
                || cancel.has_changed().is_err()
                || *shutdown.borrow()
            {
                let error = if *closed.borrow() {
                    CaptureError::Closed
                } else {
                    CaptureError::Cancelled
                };
                let outcome = terminal(&error);
                let _ = reply.send(Err(error));
                outcome
            } else {
                status.send_replace(CaptureStatus::Ready);
                if reply.send(Ok(grant)).is_err() {
                    CaptureStatus::Cancelled
                } else {
                    tokio::select! { biased;
                        _ = cancelled(&mut closed) => CaptureStatus::Closed,
                        _ = cancelled(&mut cancel) => CaptureStatus::Cancelled,
                        _ = cancelled(&mut shutdown) => CaptureStatus::Cancelled,
                    }
                }
            }
        }
    };
    status.send_replace(outcome);
    // Cooperative voluntary stop: native probe sees terminal status and destroys
    // its graph before Session.Close. External portal revocation is inherently immediate.
    let idle = async {
        while native_use.count.load(Ordering::SeqCst) > 0 {
            native_use.idle.notified().await;
        }
    };
    let idle_result = tokio::time::timeout(config.cleanup_timeout, idle).await;
    if idle_result.is_err() {
        tracing::warn!(%source_id, "native graph stop deadline; forcing session revocation with original FD retained by lease");
    }
    let cleanup_result = match tokio::time::timeout(config.cleanup_timeout, portal.cleanup()).await
    {
        Ok(result) => result,
        Err(_) => Err(CaptureError::Timeout),
    };
    if let Err(error) = &cleanup_result {
        tracing::warn!(%source_id, %error, "portal cleanup deadline/error; dedicated connection dropped");
    }
    drop(portal);
    cleaned.send_replace(Some(if idle_result.is_err() {
        Err(CaptureError::Timeout)
    } else {
        cleanup_result
    }));
}
fn terminal(error: &CaptureError) -> CaptureStatus {
    match error {
        CaptureError::Cancelled => CaptureStatus::Cancelled,
        CaptureError::Closed => CaptureStatus::Closed,
        _ => CaptureStatus::Failed(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::Read,
        os::unix::net::UnixStream,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Mutex,
        },
    };
    #[derive(Clone)]
    struct Control {
        events: Arc<Mutex<Vec<&'static str>>>,
        phase: watch::Sender<&'static str>,
        closed: watch::Sender<bool>,
        dropped: Arc<AtomicUsize>,
    }
    struct MockPortal {
        control: Control,
        stall: Option<&'static str>,
        deny: bool,
        early_closed: bool,
        remote: Mutex<Option<OwnedFd>>,
    }
    impl Drop for MockPortal {
        fn drop(&mut self) {
            self.control.dropped.fetch_add(1, Ordering::SeqCst);
        }
    }
    impl MockPortal {
        fn step(&self, phase: &'static str) -> PortalFuture<'_, ()> {
            Box::pin(async move {
                self.control.events.lock().unwrap().push(phase);
                self.control.phase.send_replace(phase);
                if self.stall == Some(phase) {
                    std::future::pending::<()>().await;
                }
                Ok(())
            })
        }
    }
    impl Portal for MockPortal {
        fn create(&mut self, _: CaptureKind) -> PortalFuture<'_, ()> {
            Box::pin(async move {
                self.step("create").await?;
                if self.early_closed {
                    self.control.closed.send_replace(true);
                }
                Ok(())
            })
        }
        fn closed(&self) -> watch::Receiver<bool> {
            self.control.closed.subscribe()
        }
        fn select(&self, _: CaptureKind) -> PortalFuture<'_, ()> {
            self.step("select")
        }
        fn start(&self) -> PortalFuture<'_, u32> {
            Box::pin(async move {
                self.step("start").await?;
                if self.deny {
                    Err(CaptureError::Portal("denied".into()))
                } else {
                    Ok(71)
                }
            })
        }
        fn remote(&self) -> PortalFuture<'_, OwnedFd> {
            Box::pin(async move {
                self.step("remote").await?;
                self.remote
                    .lock()
                    .unwrap()
                    .take()
                    .ok_or(CaptureError::Closed)
            })
        }
        fn cleanup(&mut self) -> PortalFuture<'_, ()> {
            self.step("cleanup")
        }
    }
    fn mock(
        stall: Option<&'static str>,
        deny: bool,
        early_closed: bool,
    ) -> (MockPortal, Control, UnixStream) {
        let (fd, peer) = UnixStream::pair().unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
        let (phase, _) = watch::channel("new");
        let (closed, _) = watch::channel(false);
        let control = Control {
            events: Arc::default(),
            phase,
            closed,
            dropped: Arc::default(),
        };
        (
            MockPortal {
                control: control.clone(),
                stall,
                deny,
                early_closed,
                remote: Mutex::new(Some(fd.into())),
            },
            control,
            peer,
        )
    }
    fn broker() -> CaptureBroker {
        CaptureBroker::new(CaptureConfig {
            max_leases: 1,
            authorization_timeout: Duration::from_secs(2),
            cleanup_timeout: Duration::from_millis(100),
        })
        .unwrap()
    }
    async fn wait_phase(control: &Control, target: &str) {
        let mut phase = control.phase.subscribe();
        tokio::time::timeout(Duration::from_secs(1), async {
            while *phase.borrow() != target {
                phase.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
    }
    fn assert_eof(mut peer: UnixStream) {
        assert_eq!(peer.read(&mut [0]).unwrap(), 0);
    }
    #[test]
    fn authorization_without_runtime_is_typed_error() {
        let broker = broker();
        assert!(matches!(
            broker.authorize(SourceId::new(), CaptureKind::Monitor),
            Err(CaptureError::Unsupported(_))
        ));
    }
    #[tokio::test]
    async fn voluntary_cleanup_waits_for_native_graph_and_ack_reports_failure() {
        let broker = broker();
        let (portal, control, peer) = mock(Some("cleanup"), false, false);
        let lease = broker
            .begin(
                SourceId::new(),
                CaptureKind::Monitor,
                Box::pin(async { Ok(portal) }),
            )
            .unwrap()
            .wait()
            .await
            .unwrap();
        let first = lease.grant().use_native().unwrap();
        assert!(matches!(
            lease.grant().use_native(),
            Err(CaptureError::Busy)
        ));
        drop(first);
        let native = lease.grant().use_native().unwrap();
        assert!(matches!(
            lease.grant().use_native(),
            Err(CaptureError::Busy)
        ));
        let mut status = lease.subscribe();
        lease.cancel.send_replace(true);
        tokio::time::timeout(Duration::from_secs(1), async {
            while *status.borrow() == CaptureStatus::Ready {
                status.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        assert_eq!(*status.borrow(), CaptureStatus::Cancelled);
        assert!(!control.events.lock().unwrap().contains(&"cleanup"));
        drop(native);
        assert!(matches!(lease.close().await, Err(CaptureError::Timeout)));
        broker.shutdown().await.unwrap();
        assert_eof(peer);
    }
    #[tokio::test]
    async fn dropped_wait_future_and_connection_stage_release_slots() {
        let broker = broker();
        let pending = broker
            .begin::<MockPortal>(
                SourceId::new(),
                CaptureKind::Monitor,
                Box::pin(std::future::pending()),
            )
            .unwrap();
        let task = tokio::spawn(pending.wait());
        task.abort();
        let _ = task.await;
        broker.shutdown().await.unwrap();
        assert_eq!(broker.slots.available_permits(), 1);
    }
    #[tokio::test]
    async fn ordered_ready_lease_retains_fd_until_close_and_limits_admission() {
        let broker = broker();
        let (portal, control, peer) = mock(None, false, false);
        let source_id = SourceId::new();
        let lease = broker
            .begin(
                source_id,
                CaptureKind::Window,
                Box::pin(async { Ok(portal) }),
            )
            .unwrap()
            .wait()
            .await
            .unwrap();
        assert_eq!(lease.grant().source_id(), source_id);
        assert_eq!(lease.grant().node_id(), 71);
        assert_eq!(lease.status(), CaptureStatus::Ready);
        assert_eq!(
            *control.events.lock().unwrap(),
            ["create", "select", "start", "remote"]
        );
        assert!(matches!(
            broker.authorize(SourceId::new(), CaptureKind::Monitor),
            Err(CaptureError::Busy)
        ));
        lease.close().await.unwrap();
        broker.shutdown().await.unwrap();
        assert_eq!(
            *control.events.lock().unwrap(),
            ["create", "select", "start", "remote", "cleanup"]
        );
        assert_eq!(control.dropped.load(Ordering::SeqCst), 1);
        assert_eof(peer);
    }
    #[tokio::test]
    async fn revoked_retained_fd_still_counts_against_resource_bound() {
        let broker = broker();
        let (portal, control, peer) = mock(None, false, false);
        let lease = broker
            .begin(
                SourceId::new(),
                CaptureKind::Monitor,
                Box::pin(async { Ok(portal) }),
            )
            .unwrap()
            .wait()
            .await
            .unwrap();
        control.closed.send_replace(true);
        let mut cleaned = lease.cleaned.clone();
        while cleaned.borrow().is_none() {
            cleaned.changed().await.unwrap();
        }
        assert!(matches!(
            broker.authorize(SourceId::new(), CaptureKind::Monitor),
            Err(CaptureError::Busy)
        ));
        // Worker shutdown is independent of a revoked, retained original FD.
        broker.shutdown().await.unwrap();
        assert_eq!(broker.slots.available_permits(), 0);
        drop(lease);
        assert_eof(peer);
        assert_eq!(broker.slots.available_permits(), 1);
    }
    #[tokio::test]
    async fn dropped_authorization_cleans_every_await_stage() {
        for stage in ["create", "select", "start", "remote"] {
            let broker = broker();
            let (portal, control, peer) = mock(Some(stage), false, false);
            let pending = broker
                .begin(
                    SourceId::new(),
                    CaptureKind::Monitor,
                    Box::pin(async { Ok(portal) }),
                )
                .unwrap();
            wait_phase(&control, stage).await;
            drop(pending);
            broker.shutdown().await.unwrap();
            assert_eq!(control.events.lock().unwrap().last(), Some(&"cleanup"));
            assert_eq!(control.dropped.load(Ordering::SeqCst), 1);
            assert_eof(peer);
        }
    }
    #[tokio::test]
    async fn denial_and_early_closure_cannot_publish_ready() {
        for (deny, early_closed) in [(true, false), (false, true)] {
            let broker = broker();
            let (portal, control, peer) = mock(None, deny, early_closed);
            let pending = broker
                .begin(
                    SourceId::new(),
                    CaptureKind::Monitor,
                    Box::pin(async { Ok(portal) }),
                )
                .unwrap();
            assert!(pending.wait().await.is_err());
            broker.shutdown().await.unwrap();
            assert_eof(peer);
            if early_closed {
                assert!(!control.events.lock().unwrap().contains(&"start"));
            }
        }
    }
    #[tokio::test]
    async fn ready_session_revocation_is_terminal_and_cleanup_acknowledged() {
        let broker = broker();
        let (portal, control, peer) = mock(None, false, false);
        let lease = broker
            .begin(
                SourceId::new(),
                CaptureKind::Monitor,
                Box::pin(async { Ok(portal) }),
            )
            .unwrap()
            .wait()
            .await
            .unwrap();
        let mut status = lease.subscribe();
        control.closed.send_replace(true);
        tokio::time::timeout(Duration::from_secs(1), async {
            while *status.borrow() != CaptureStatus::Closed {
                status.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        lease.close().await.unwrap();
        assert_eq!(control.dropped.load(Ordering::SeqCst), 1);
        broker.shutdown().await.unwrap();
        assert_eof(peer);
    }
    #[tokio::test]
    async fn timeout_drops_late_remote_and_releases_slot() {
        let broker = CaptureBroker::new(CaptureConfig {
            authorization_timeout: Duration::from_millis(30),
            ..broker().config
        })
        .unwrap();
        let (portal, control, peer) = mock(Some("remote"), false, false);
        let pending = broker
            .begin(
                SourceId::new(),
                CaptureKind::Monitor,
                Box::pin(async { Ok(portal) }),
            )
            .unwrap();
        assert!(matches!(pending.wait().await, Err(CaptureError::Timeout)));
        broker.shutdown().await.unwrap();
        assert_eof(peer);
        assert_eq!(control.dropped.load(Ordering::SeqCst), 1);
    }
    #[tokio::test]
    async fn closure_while_start_pending_discards_completion() {
        let broker = broker();
        let (portal, control, peer) = mock(Some("start"), false, false);
        let pending = broker
            .begin(
                SourceId::new(),
                CaptureKind::Monitor,
                Box::pin(async { Ok(portal) }),
            )
            .unwrap();
        wait_phase(&control, "start").await;
        control.closed.send_replace(true);
        assert!(matches!(pending.wait().await, Err(CaptureError::Closed)));
        broker.shutdown().await.unwrap();
        assert!(!control.events.lock().unwrap().contains(&"remote"));
        assert_eof(peer);
    }
    #[tokio::test]
    async fn dropped_broker_cancels_authorization_and_active_lease() {
        let broker = broker();
        let (portal, control, peer) = mock(None, false, false);
        let lease = broker
            .begin(
                SourceId::new(),
                CaptureKind::Monitor,
                Box::pin(async { Ok(portal) }),
            )
            .unwrap()
            .wait()
            .await
            .unwrap();
        drop(broker);
        lease.close().await.unwrap();
        assert_eof(peer);
        assert_eq!(control.dropped.load(Ordering::SeqCst), 1);
    }
    #[tokio::test]
    #[ignore = "opens GNOME Wayland portal picker; user must select monitor/window"]
    async fn actual_portal_capture_frames_and_session_teardown() {
        let kind = if std::env::var("PRISMCAST_CAPTURE_KIND").as_deref() == Ok("window") {
            CaptureKind::Window
        } else {
            CaptureKind::Monitor
        };
        let broker = CaptureBroker::new(CaptureConfig::default()).unwrap();
        let lease = broker
            .authorize(SourceId::new(), kind)
            .unwrap()
            .wait()
            .await
            .unwrap();
        // Dedicated OS thread: no native blocking operations on Tokio workers.
        let (lease, evidence) = tokio::task::spawn_blocking(move || {
            let result = probe::capture_frames(&lease, 3, Duration::from_secs(15));
            (lease, result)
        })
        .await
        .unwrap();
        let close_result = lease.close().await;
        let shutdown_result = broker.shutdown().await;
        let evidence = evidence.unwrap();
        close_result.unwrap();
        shutdown_result.unwrap();
        eprintln!("{kind:?} native capture evidence: {evidence:?}");
        assert!(evidence.width > 0 && evidence.height > 0 && evidence.bytes > 0);
        assert_eq!(evidence.buffers, 3);
        assert!(evidence.last_pts_ns.unwrap() > evidence.first_pts_ns.unwrap());
    }
}
