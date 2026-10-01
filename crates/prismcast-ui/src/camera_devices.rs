//! GTK-local camera discovery delivery. Discovery is a read-only system query
//! (ADR-0019): it never mutates Core state, never emits Core Events and never
//! authorizes anything; results feed only the add-source picker.
//!
//! The one-shot GStreamer enumeration blocks briefly, so it runs on a detached
//! worker thread and the result is delivered back through the default GLib
//! main context — GTK never blocks on device probing.

use gtk::glib;
use prismcast_capture::devices::VideoDevice;

/// Picker row label: display name plus the kernel path for disambiguation.
pub fn device_label(device: &VideoDevice) -> String {
    if device.display_name() == device.path() {
        device.path().to_owned()
    } else {
        format!("{} ({})", device.display_name(), device.path())
    }
}

/// Enumerates cameras off the GTK main thread and delivers the result on the
/// default main context. `send` runs on the GTK main thread; a dropped
/// receiver (dialog closed mid-probe) discards the result silently.
pub fn list(send: impl FnOnce(Result<Vec<VideoDevice>, String>) + 'static) {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let spawned = std::thread::Builder::new()
        .name("prismcast-camera-list".into())
        .spawn(move || {
            let _ = tx.send(
                prismcast_capture::devices::list_video_sources().map_err(|error| error.to_string()),
            );
        });
    if let Err(error) = spawned {
        send(Err(error.to_string()));
        return;
    }
    glib::MainContext::default().spawn_local(async move {
        if let Ok(result) = rx.await {
            send(result);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_label_disambiguates_identical_names_with_paths() {
        let named = VideoDevice::new("/dev/video0".into(), "HD Camera".into()).unwrap();
        assert_eq!(device_label(&named), "HD Camera (/dev/video0)");
        let fallback = VideoDevice::new("/dev/video2".into(), "/dev/video2".into()).unwrap();
        assert_eq!(device_label(&fallback), "/dev/video2");
    }

    #[test]
    fn listing_is_delivered_on_the_main_context_without_a_display() {
        let result = std::rc::Rc::new(std::cell::RefCell::new(None));
        let weak = std::rc::Rc::downgrade(&result);
        list(move |devices| {
            if let Some(result) = weak.upgrade() {
                *result.borrow_mut() = Some(devices);
            }
        });
        let context = glib::MainContext::default();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while result.borrow().is_none() {
            assert!(
                std::time::Instant::now() < deadline,
                "listing never arrived"
            );
            context.iteration(true);
        }
        assert!(result.borrow_mut().take().unwrap().is_ok());
    }
}
