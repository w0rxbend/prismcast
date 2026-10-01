//! Read-only V4L2 camera discovery through gst::DeviceMonitor (ADR-0019).
//! Discovery never mutates Core state and never authorizes anything; snapshots
//! feed only the UI picker. The monitor thread polls the bus with timed_pop, so
//! hotplug works without a GTK main loop; one-shot listing needs neither the
//! thread nor a main loop. The monitor must show hidden-provider devices:
//! PipeWire's provider hides the native v4l2deviceprovider to dedupe cameras
//! (gst_device_provider_hide_provider), which would otherwise leave the picker
//! empty on exactly the PipeWire desktops Prismcast targets.
use crate::{CaptureError, Result};
use gstreamer::{self as gst, prelude::*};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::sync::watch;

const MAX_FIELD_BYTES: usize = 255;
const MAX_VIDEO_DEVICES: usize = 64;

fn native(error: impl std::fmt::Display) -> CaptureError {
    CaptureError::Native(error.to_string())
}

/// Bounded, validated device identity for the picker. The kernel path is not a
/// stable identity across reboot or replug (ADR-0019).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoDevice {
    path: String,
    display_name: String,
}
impl VideoDevice {
    pub fn new(path: String, display_name: String) -> Result<Self> {
        for value in [&path, &display_name] {
            if value.len() > MAX_FIELD_BYTES || value.chars().any(char::is_control) {
                return Err(CaptureError::Unsupported(
                    "invalid video device metadata".into(),
                ));
            }
        }
        Ok(Self { path, display_name })
    }
    pub fn path(&self) -> &str {
        &self.path
    }
    pub fn display_name(&self) -> &str {
        &self.display_name
    }
}

/// Pure property-map extraction. Only kernel V4L2 nodes (device.api = "v4l2")
/// map to v4l2src-usable devices; PipeWire camera nodes under Video/Source are
/// skipped (ADR-0019 defers PipeWire camera capture). Broken metadata skips the
/// device instead of failing enumeration.
pub fn extract_video_device(fields: &[(&str, &str)]) -> Option<VideoDevice> {
    let field = |key: &str| {
        fields
            .iter()
            .find(|(name, _)| *name == key)
            .map(|(_, value)| *value)
    };
    if field("device.api")? != "v4l2" {
        return None;
    }
    let path = field("device.path")?;
    let name = field("display-name").unwrap_or(path);
    VideoDevice::new(path.into(), name.into()).ok()
}

fn video_device(device: &gst::Device) -> Option<VideoDevice> {
    let properties = device.properties()?;
    let path = properties.get::<String>("device.path").ok()?;
    let api = properties.get::<String>("device.api").ok()?;
    // display-name is a GstDevice object property, not necessarily present in
    // the provider's properties structure.
    let name = properties
        .get::<String>("display-name")
        .ok()
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| device.display_name().to_string());
    let mut fields = vec![("device.path", path.as_str()), ("device.api", api.as_str())];
    if !name.is_empty() {
        fields.push(("display-name", name.as_str()));
    }
    extract_video_device(&fields)
}

fn collect_devices(monitor: &gst::DeviceMonitor) -> Vec<VideoDevice> {
    let mut devices = Vec::new();
    for device in monitor.devices() {
        if devices.len() >= MAX_VIDEO_DEVICES {
            tracing::warn!("video device enumeration capped at {MAX_VIDEO_DEVICES}");
            break;
        }
        if let Some(video) = video_device(&device) {
            devices.push(video);
        }
    }
    devices
}

fn open_monitor() -> Result<gst::DeviceMonitor> {
    gst::init().map_err(native)?;
    let monitor = gst::DeviceMonitor::new();
    if monitor.add_filter(Some("Video/Source"), None).is_none() {
        return Err(native("device monitor rejected Video/Source filter"));
    }
    // PipeWire's provider hides v4l2deviceprovider entries as duplicates;
    // kernel nodes are exactly what v4l2src needs, so show them anyway.
    monitor.set_show_all_devices(true);
    monitor.start().map_err(native)?;
    Ok(monitor)
}

/// One-shot snapshot; standalone, without any running monitor thread.
pub fn list_video_sources() -> Result<Vec<VideoDevice>> {
    let monitor = open_monitor()?;
    let devices = collect_devices(&monitor);
    monitor.stop();
    Ok(devices)
}

/// Running monitor publishing snapshots through a bounded watch channel.
/// Purely informational; carries no Core state.
pub struct VideoDeviceMonitor {
    snapshot: watch::Receiver<Vec<VideoDevice>>,
    shutdown: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl VideoDeviceMonitor {
    pub fn start() -> Result<Self> {
        let (publish, snapshot) = watch::channel::<Vec<VideoDevice>>(Vec::new());
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel::<Result<()>>(1);
        let shutdown = Arc::new(AtomicBool::new(false));
        let flag = shutdown.clone();
        let mut worker = Some(
            std::thread::Builder::new()
                .name("prismcast-device-monitor".into())
                .spawn(move || monitor_worker(publish, ready_tx, flag))
                .map_err(native)?,
        );
        let ready = ready_rx
            .recv_timeout(Duration::from_secs(5))
            .map_err(|_| native("device monitor startup timed out"))?;
        if let Err(error) = ready {
            if let Some(thread) = worker.take() {
                let _ = thread.join();
            }
            return Err(error);
        }
        Ok(Self {
            snapshot,
            shutdown,
            worker,
        })
    }
    pub fn snapshot(&self) -> watch::Receiver<Vec<VideoDevice>> {
        self.snapshot.clone()
    }
    /// Stops the monitor and joins its thread; idempotent.
    pub fn shutdown(&mut self) -> Result<()> {
        self.shutdown.store(true, Ordering::SeqCst);
        if let Some(worker) = self.worker.take() {
            worker
                .join()
                .map_err(|_| native("device monitor worker panicked"))?;
        }
        Ok(())
    }
}
impl Drop for VideoDeviceMonitor {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

fn monitor_worker(
    publish: watch::Sender<Vec<VideoDevice>>,
    ready: std::sync::mpsc::SyncSender<Result<()>>,
    shutdown: Arc<AtomicBool>,
) {
    let monitor = match open_monitor() {
        Ok(monitor) => monitor,
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    let _ = ready.send(Ok(()));
    let bus = monitor.bus();
    publish.send_replace(collect_devices(&monitor));
    // The v4l2 provider runs its own thread and GLib main loop, so polling the
    // monitor bus with timed_pop sees add/remove without any main context here.
    while !shutdown.load(Ordering::SeqCst) {
        let changed = bus
            .timed_pop(gst::ClockTime::from_mseconds(50))
            .is_some_and(|message| {
                matches!(
                    message.view(),
                    gst::MessageView::DeviceAdded(_) | gst::MessageView::DeviceRemoved(_)
                )
            });
        if changed {
            publish.send_replace(collect_devices(&monitor));
        }
    }
    monitor.stop();
    tracing::debug!("video device monitor stopped");
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn video_device_metadata_validation_bounds() {
        let device = VideoDevice::new("/dev/video0".into(), "HD Camera".into()).unwrap();
        assert_eq!(device.path(), "/dev/video0");
        assert_eq!(device.display_name(), "HD Camera");
        let long = "x".repeat(MAX_FIELD_BYTES + 1);
        assert!(VideoDevice::new(long.clone(), "Cam".into()).is_err());
        assert!(VideoDevice::new("/dev/video0".into(), long).is_err());
        assert!(VideoDevice::new("/dev/vid\teo0".into(), "Cam".into()).is_err());
        assert!(VideoDevice::new("/dev/video0".into(), "Ca\nm".into()).is_err());
        assert!(VideoDevice::new(String::new(), String::new()).is_ok());
    }
    #[test]
    fn extraction_requires_v4l2_api_and_bounded_fields() {
        assert!(extract_video_device(&[]).is_none());
        assert!(extract_video_device(&[("device.api", "v4l2")]).is_none());
        assert!(extract_video_device(&[
            ("device.api", "pipewire"),
            ("device.path", "/dev/video0")
        ])
        .is_none());
        assert!(
            extract_video_device(&[("device.api", "v4l2"), ("device.path", "/dev/vid\0eo0")])
                .is_none()
        );
        let device = extract_video_device(&[
            ("device.api", "v4l2"),
            ("device.path", "/dev/video2"),
            ("display-name", "HD Camera"),
        ])
        .unwrap();
        assert_eq!(device.path(), "/dev/video2");
        assert_eq!(device.display_name(), "HD Camera");
        let fallback =
            extract_video_device(&[("device.api", "v4l2"), ("device.path", "/dev/video0")])
                .unwrap();
        assert_eq!(fallback.display_name(), "/dev/video0");
    }
    #[test]
    fn one_shot_listing_is_bounded_and_tolerant() {
        let devices = list_video_sources().unwrap();
        assert!(devices.len() <= MAX_VIDEO_DEVICES);
    }
    #[test]
    fn monitor_publishes_snapshot_and_stops_cleanly() {
        let mut monitor = VideoDeviceMonitor::start().unwrap();
        let snapshot = monitor.snapshot();
        std::thread::sleep(Duration::from_millis(150));
        assert!(snapshot.borrow().len() <= MAX_VIDEO_DEVICES);
        monitor.shutdown().unwrap();
        monitor.shutdown().unwrap();
    }
}
