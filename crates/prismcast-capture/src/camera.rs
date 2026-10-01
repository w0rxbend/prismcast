//! Lease-free V4L2 camera sessions (ADR-0019): no portal, picker, lease or FD.
//! The native lifecycle runs on a dedicated OS thread, never a Tokio worker
//! and never spawn_blocking (ADR-0018).
use crate::{
    producer::{CaptureFeed, FrameProducer},
    CaptureError, CaptureStatus, Result,
};
use gstreamer::{self as gst, prelude::*};
use std::time::Duration;
use tokio::sync::watch;

const MAX_PATH_BYTES: usize = 255;
/// Linux errno for a busy device node; the workspace is Linux-only.
const EBUSY: i32 = 16;

fn native(error: impl std::fmt::Display) -> CaptureError {
    CaptureError::Native(error.to_string())
}

/// Absolute path under /dev/, bounded, no control characters or traversal.
pub fn validate_device_path(path: &str) -> Result<()> {
    let Some(node) = path.strip_prefix("/dev/") else {
        return Err(CaptureError::Unsupported(
            "camera device path must be absolute under /dev/".into(),
        ));
    };
    if path.len() > MAX_PATH_BYTES
        || node.is_empty()
        || path.chars().any(char::is_control)
        || node.split('/').any(|part| part.is_empty() || part == "..")
    {
        return Err(CaptureError::Unsupported(
            "invalid camera device path".into(),
        ));
    }
    Ok(())
}

/// Node existence and open-mode checks classify errors before GStreamer does;
/// ADR-0019 prefers these over parsing bus error text, so a missing node maps
/// to a Failed-style Native error, EACCES to Denied and EBUSY to a busy Native.
fn pre_open_check(path: &str) -> Result<()> {
    if let Err(error) = std::fs::metadata(path) {
        return Err(map_node_error(path, &error));
    }
    match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
    {
        Ok(file) => drop(file),
        Err(error) => return Err(map_node_error(path, &error)),
    }
    Ok(())
}
fn map_node_error(path: &str, error: &std::io::Error) -> CaptureError {
    if error.kind() == std::io::ErrorKind::NotFound {
        return CaptureError::Native(format!("camera device {path} does not exist"));
    }
    if error.kind() == std::io::ErrorKind::PermissionDenied {
        return CaptureError::Denied(format!("camera device {path} permission denied"));
    }
    if error.raw_os_error() == Some(EBUSY) {
        return CaptureError::Native(format!("camera device {path} is busy"));
    }
    CaptureError::Native(format!("camera device {path} check failed: {error}"))
}

/// Mirrors probe.rs build_source property validation for the v4l2src factory.
pub fn build_v4l2_source(path: &str) -> Result<gst::Element> {
    validate_device_path(path)?;
    gst::init().map_err(native)?;
    let factory =
        gst::ElementFactory::find("v4l2src").ok_or_else(|| native("v4l2src plugin unavailable"))?;
    let source = factory.create().build().map_err(native)?;
    let property = source
        .find_property("device")
        .ok_or_else(|| native("v4l2src missing device"))?;
    if property.value_type() != String::static_type()
        || !property.flags().contains(gst::glib::ParamFlags::WRITABLE)
    {
        return Err(native("v4l2src incompatible device property"));
    }
    source.set_property("device", path);
    Ok(source)
}

/// Explicit-open camera session. The FrameProducer lives on a dedicated worker
/// thread; close/Drop cancel it, stop the native graph first and only then
/// report cleanup completion. Both block the caller; invoke from the media
/// owner OS thread, never a Tokio worker. Dropping never abandons a running
/// pipeline and there are no automatic reopen loops.
pub struct CameraSession {
    status: watch::Receiver<CaptureStatus>,
    feed: watch::Receiver<Option<CaptureFeed>>,
    cancel: watch::Sender<bool>,
    cleaned: watch::Receiver<Option<Result<()>>>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl CameraSession {
    /// Validates the path, checks node existence and permissions synchronously,
    /// then starts the native producer on the session's dedicated thread.
    pub fn open(path: &str) -> Result<Self> {
        validate_device_path(path)?;
        pre_open_check(path)?;
        let source = build_v4l2_source(path)?;
        Self::spawn(path, source)
    }
    /// Test hook: an injected source (e.g. videotestsrc) skips node checks.
    #[cfg(test)]
    pub(crate) fn open_with_source(path: &str, source: gst::Element) -> Result<Self> {
        validate_device_path(path)?;
        Self::spawn(path, source)
    }
    fn spawn(path: &str, source: gst::Element) -> Result<Self> {
        let (status_tx, status) = watch::channel(CaptureStatus::Authorizing);
        let (feed_tx, feed) = watch::channel(None);
        let (cancel, cancel_rx) = watch::channel(false);
        let (cleaned_tx, cleaned) = watch::channel(None);
        let device = path.to_owned();
        let worker = std::thread::Builder::new()
            .name("prismcast-camera".into())
            .spawn(move || camera_worker(device, source, status_tx, feed_tx, cancel_rx, cleaned_tx))
            .map_err(native)?;
        Ok(Self {
            status,
            feed,
            cancel,
            cleaned,
            worker: Some(worker),
        })
    }
    pub fn status(&self) -> CaptureStatus {
        self.status.borrow().clone()
    }
    pub fn subscribe(&self) -> watch::Receiver<CaptureStatus> {
        self.status.clone()
    }
    /// Present only once the producer reached Ready.
    pub fn feed(&self) -> Option<CaptureFeed> {
        self.feed.borrow().clone()
    }
    pub fn cancel(&self) {
        self.cancel.send_replace(true);
    }
    /// Cancels and joins the worker; returns the cleanup result published only
    /// after the native graph reached NULL.
    pub fn close(mut self) -> Result<()> {
        self.stop()
    }
    fn stop(&mut self) -> Result<()> {
        self.cancel.send_replace(true);
        if let Some(worker) = self.worker.take() {
            if worker.join().is_err() {
                return Err(native("camera worker panicked"));
            }
        }
        self.cleaned.borrow().clone().unwrap_or(Ok(()))
    }
}
impl Drop for CameraSession {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

fn camera_worker(
    device: String,
    source: gst::Element,
    status: watch::Sender<CaptureStatus>,
    feed_out: watch::Sender<Option<CaptureFeed>>,
    cancel: watch::Receiver<bool>,
    cleaned: watch::Sender<Option<Result<()>>>,
) {
    let cancelled =
        |cancel: &watch::Receiver<bool>| *cancel.borrow() || cancel.has_changed().is_err();
    let mut producer = match FrameProducer::start(source) {
        Ok(producer) => producer,
        Err(error) => {
            tracing::warn!(device = %device, %error, "camera open failed");
            status.send_replace(CaptureStatus::Failed(error.to_string()));
            cleaned.send_replace(Some(Err(error)));
            return;
        }
    };
    // A cancel racing startup must never leave a running pipeline behind.
    if cancelled(&cancel) {
        let result = producer.shutdown_native();
        status.send_replace(CaptureStatus::Cancelled);
        cleaned.send_replace(Some(result));
        return;
    }
    let feed = producer.feed();
    feed_out.send_replace(Some(feed.clone()));
    status.send_replace(CaptureStatus::Ready);
    let outcome = loop {
        if cancelled(&cancel) {
            break CaptureStatus::Cancelled;
        }
        if let Some(error) = feed.error() {
            tracing::warn!(device = %device, %error, "camera capture lost");
            break CaptureStatus::Failed(error);
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    // The native graph stops before cleanup completion is signalled.
    let stopped = producer.shutdown_native();
    if let Err(error) = &stopped {
        tracing::warn!(device = %device, %error, "camera native shutdown failed");
    }
    status.send_replace(outcome);
    cleaned.send_replace(Some(stopped));
}

#[cfg(test)]
mod tests {
    use super::*;
    use gstreamer_app::AppSink;
    use std::time::Instant;

    fn test_source() -> gst::Element {
        gst::init().unwrap();
        gst::ElementFactory::make("videotestsrc")
            .property("is-live", true)
            .build()
            .unwrap()
    }
    fn wait_ready(session: &CameraSession) -> CaptureFeed {
        let deadline = Instant::now() + Duration::from_secs(5);
        while session.status() == CaptureStatus::Authorizing {
            assert!(
                Instant::now() < deadline,
                "session stuck: {:?}",
                session.status()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(session.status(), CaptureStatus::Ready);
        session.feed().expect("feed published with Ready")
    }
    #[test]
    fn device_path_validation_table() {
        for path in ["/dev/video0", "/dev/v4l/by-id/usb-camera", "/dev/null"] {
            validate_device_path(path).unwrap();
        }
        let boundary = format!("/dev/{}", "x".repeat(MAX_PATH_BYTES - 5));
        validate_device_path(&boundary).unwrap();
        for path in [
            "",
            "dev/video0",
            "/dev/",
            "/dev//video0",
            "/dev/video0/",
            "/dev/../etc/passwd",
            "/dev/video0\n",
            &format!("/dev/{}", "x".repeat(MAX_PATH_BYTES - 4)),
        ] {
            assert!(validate_device_path(path).is_err(), "{path:?}");
        }
    }
    #[test]
    fn installed_v4l2_properties_build_without_opening_device() {
        gst::init().unwrap();
        let source = build_v4l2_source("/dev/video0").unwrap();
        assert_eq!(source.property::<String>("device"), "/dev/video0");
        assert_eq!(source.current_state(), gst::State::Null);
    }
    #[test]
    fn missing_node_maps_to_failed_style_native_error() {
        match CameraSession::open("/dev/prismcast-missing-camera") {
            Err(CaptureError::Native(message)) => assert!(message.contains("does not exist")),
            Err(other) => panic!("expected missing-node Native error, got {other:?}"),
            Ok(_) => panic!("missing camera node opened successfully"),
        }
    }
    #[test]
    fn node_error_mapping_covers_busy_denied_and_missing() {
        let busy = map_node_error("/dev/video0", &std::io::Error::from_raw_os_error(EBUSY));
        assert!(matches!(busy, CaptureError::Native(message) if message.contains("busy")));
        let denied = map_node_error("/dev/video0", &std::io::Error::from_raw_os_error(13));
        assert!(matches!(denied, CaptureError::Denied(_)));
        let missing = map_node_error("/dev/video0", &std::io::Error::from_raw_os_error(2));
        assert!(
            matches!(missing, CaptureError::Native(message) if message.contains("does not exist"))
        );
    }
    #[test]
    fn unreadable_node_maps_to_denied() {
        use std::os::unix::fs::PermissionsExt;
        let file =
            std::env::temp_dir().join(format!("prismcast-camera-deny-{}", std::process::id()));
        std::fs::write(&file, b"x").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o000)).unwrap();
        let result = pre_open_check(file.to_str().unwrap());
        let _ = std::fs::remove_file(&file);
        match result {
            Err(CaptureError::Denied(_)) => {}
            Ok(()) => {
                eprintln!("privileged environment opened a mode-000 file; EACCES assertion skipped")
            }
            other => panic!("unexpected pre-open outcome {other:?}"),
        }
    }
    #[test]
    fn cancel_before_open_never_leaves_a_running_pipeline() {
        let source = test_source();
        let (status_tx, status) = watch::channel(CaptureStatus::Authorizing);
        let (feed_tx, feed) = watch::channel::<Option<CaptureFeed>>(None);
        let (cancel, cancel_rx) = watch::channel(false);
        cancel.send_replace(true);
        let (cleaned_tx, cleaned) = watch::channel::<Option<Result<()>>>(None);
        camera_worker(
            "/dev/video-test".into(),
            source.clone(),
            status_tx,
            feed_tx,
            cancel_rx,
            cleaned_tx,
        );
        assert_eq!(*status.borrow(), CaptureStatus::Cancelled);
        assert!(feed.borrow().is_none());
        assert!(matches!(&*cleaned.borrow(), Some(Ok(()))));
        assert_eq!(source.current_state(), gst::State::Null);
    }
    #[test]
    fn session_frames_then_close_stops_native_graph_first() {
        let source = test_source();
        let session = CameraSession::open_with_source("/dev/video-test", source.clone()).unwrap();
        let statuses = session.subscribe();
        let feed = wait_ready(&session);
        let deadline = Instant::now() + Duration::from_secs(5);
        while feed.dimensions().is_none() {
            assert!(Instant::now() < deadline, "no negotiated frames");
            std::thread::sleep(Duration::from_millis(10));
        }
        let (width, height) = feed.dimensions().unwrap();
        assert!(width > 0 && height > 0);
        let consumer = feed.consumer().unwrap();
        let sink = AppSink::builder()
            .max_buffers(1)
            .drop(true)
            .sync(false)
            .build();
        let pipeline = gst::Pipeline::new();
        pipeline
            .add_many([consumer.bin.upcast_ref::<gst::Element>(), sink.upcast_ref()])
            .unwrap();
        consumer.bin.link(&sink).unwrap();
        pipeline.set_state(gst::State::Playing).unwrap();
        assert!(sink
            .try_pull_sample(gst::ClockTime::from_seconds(2))
            .is_some());
        assert!(feed.received() > 0);
        pipeline.set_state(gst::State::Null).unwrap();
        drop(pipeline);
        drop(consumer);
        assert_eq!(session.status(), CaptureStatus::Ready);
        session.close().unwrap();
        // Cleanup completion implies the native graph already reached NULL.
        assert_eq!(source.current_state(), gst::State::Null);
        assert!(feed.dimensions().is_none());
        assert_eq!(*statuses.borrow(), CaptureStatus::Cancelled);
    }
    #[test]
    fn mid_run_stream_loss_maps_to_terminal_failed_and_teardown_completes() {
        gst::init().unwrap();
        let source = gst::ElementFactory::make("videotestsrc")
            .property("is-live", true)
            .property("num-buffers", 2i32)
            .build()
            .unwrap();
        let session = CameraSession::open_with_source("/dev/video-test", source.clone()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let terminal = loop {
            match session.status() {
                CaptureStatus::Failed(message) => break message,
                CaptureStatus::Authorizing | CaptureStatus::Ready => {
                    assert!(
                        Instant::now() < deadline,
                        "EOS never surfaced: {:?}",
                        session.status()
                    );
                    std::thread::sleep(Duration::from_millis(10));
                }
                other => panic!("unexpected status {other:?}"),
            }
        };
        assert!(terminal.contains("EOS"), "{terminal}");
        session.close().unwrap();
        assert_eq!(source.current_state(), gst::State::Null);
    }
    #[test]
    #[ignore = "real camera opt-in: PRISMCAST_CAMERA_DEVICE=/dev/videoN cargo test -p prismcast-capture -- --ignored"]
    fn actual_v4l2_camera_frames_and_teardown() {
        let Ok(path) = std::env::var("PRISMCAST_CAMERA_DEVICE") else {
            eprintln!("PRISMCAST_CAMERA_DEVICE unset; no real-camera evidence produced, skipping");
            return;
        };
        gst::init().unwrap();
        let session = CameraSession::open(&path).unwrap();
        let feed = wait_ready(&session);
        let deadline = Instant::now() + Duration::from_secs(15);
        while feed.dimensions().is_none() {
            assert!(Instant::now() < deadline, "camera never negotiated frames");
            std::thread::sleep(Duration::from_millis(25));
        }
        let (width, height) = feed.dimensions().unwrap();
        assert!((1..=8192).contains(&(width as i32)));
        assert!((1..=8192).contains(&(height as i32)));
        let consumer = feed.consumer().unwrap();
        let sink = AppSink::builder()
            .max_buffers(1)
            .drop(true)
            .sync(false)
            .build();
        let pipeline = gst::Pipeline::new();
        pipeline
            .add_many([consumer.bin.upcast_ref::<gst::Element>(), sink.upcast_ref()])
            .unwrap();
        consumer.bin.link(&sink).unwrap();
        pipeline.set_state(gst::State::Playing).unwrap();
        let mut negotiated = None;
        for _ in 0..3 {
            let sample = sink
                .try_pull_sample(gst::ClockTime::from_seconds(5))
                .unwrap();
            let caps = sample.caps().unwrap().structure(0).unwrap().to_owned();
            assert_eq!(caps.name(), "video/x-raw");
            negotiated = Some(caps);
        }
        let caps = negotiated.unwrap();
        assert_eq!(caps.get::<i32>("width").unwrap(), width as i32);
        assert_eq!(caps.get::<i32>("height").unwrap(), height as i32);
        assert!(feed.received() >= 3);
        pipeline.set_state(gst::State::Null).unwrap();
        drop(pipeline);
        drop(consumer);
        session.close().unwrap();
        assert!(feed.dimensions().is_none());
        eprintln!(
            "camera evidence: device={path} {width}x{height} frames={}",
            feed.received()
        );
    }
}
