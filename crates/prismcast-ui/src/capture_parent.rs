//! GTK-thread ownership of the portal's opaque parent-window identifier.
use gtk::prelude::*;

pub struct CaptureParent {
    surface: gdk4_wayland::WaylandToplevel,
    handle: String,
}
impl std::fmt::Debug for CaptureParent {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("CaptureParent(<exported Wayland window>)")
    }
}
impl CaptureParent {
    pub fn identifier(&self) -> String {
        format!("wayland:{}", self.handle)
    }
}
impl Drop for CaptureParent {
    fn drop(&mut self) {
        self.surface.drop_exported_handle(&self.handle);
    }
}

/// Exporting does not authorize capture or open a picker. The caller retains the
/// returned guard until portal/media shutdown has completed, before window close.
pub fn export(
    window: &impl IsA<gtk::Window>,
    send: impl Fn(Result<CaptureParent, String>) + 'static,
) {
    let Some(surface) = window.as_ref().surface() else {
        send(Err("Capture parent window is not realized".into()));
        return;
    };
    let Ok(surface) = surface.downcast::<gdk4_wayland::WaylandToplevel>() else {
        send(Err("Capture parent export requires a Wayland window".into()));
        return;
    };
    let send = std::rc::Rc::new(send);
    let callback = send.clone();
    let completed = std::rc::Rc::new(std::cell::Cell::new(false));
    let done = completed.clone();
    if !surface.export_handle(move |surface, handle| {
        done.set(true);
        callback(
            handle
                .map(|handle| CaptureParent {
                    surface: surface.clone(),
                    handle: handle.to_owned(),
                })
                .map_err(|error| error.to_string()),
        );
    }) {
        completed.set(true);
        send(Err(
            "The compositor does not support capture parent export".into()
        ));
    }
    // One bounded GTK timer per window: unsupported or stalled export cannot
    // indefinitely prevent explicit authorization with an optional parent.
    gtk::glib::timeout_add_local_once(std::time::Duration::from_secs(3), move || {
        if !completed.replace(true) {
            send(Err("Capture parent export timed out".into()));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "requires GNOME Wayland display; exports a window without opening a picker"]
    fn actual_wayland_export_is_owned_and_released_before_window_close() {
        gtk::init().unwrap();
        gtk::glib::MainContext::default().block_on(async {
            let window = gtk::Window::new();
            window.set_default_size(320, 200);
            window.present();
            gtk::glib::timeout_future(std::time::Duration::from_millis(100)).await;
            let result = std::rc::Rc::new(std::cell::RefCell::new(None));
            let weak = std::rc::Rc::downgrade(&result);
            export(&window, move |export| {
                if let Some(result) = weak.upgrade() {
                    *result.borrow_mut() = Some(export);
                }
            });
            for _ in 0..20 {
                if result.borrow().is_some() {
                    break;
                }
                gtk::glib::timeout_future(std::time::Duration::from_millis(50)).await;
            }
            let parent = result
                .borrow_mut()
                .take()
                .expect("bounded export callback")
                .unwrap();
            assert!(parent.identifier().starts_with("wayland:"));
            assert!(parent.identifier().len() > "wayland:".len());
            drop(parent);
            window.close();
            gtk::glib::timeout_future(std::time::Duration::from_millis(50)).await;
        });
    }
}
