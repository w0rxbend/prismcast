//! GTK-local preview attachment. Pipeline ownership lives on a media thread.

use gstreamer::{self as gst, prelude::*};

/// Failures at the GTK/native media integration boundary.
#[derive(Debug, thiserror::Error)]
pub enum PreviewError {
    #[error("preview initialization must run on the GTK main thread")]
    WrongThread,
    #[error("preview owner thread failed: {0}")]
    Thread(#[source] std::io::Error),
    #[error("preview media backend failed: {0}")]
    Backend(#[source] prismcast_core::Error),
    #[error("preview owner panicked")]
    WorkerPanic,
    #[error("preview owner completion channel closed")]
    WorkerGone,
    #[error("GStreamer initialization failed: {0}")]
    Initialize(#[source] gst::glib::Error),
    #[error("GTK sink registration/construction failed: {0}")]
    Sink(#[source] gst::glib::BoolError),
    #[error("GTK sink did not provide a paintable")]
    MissingPaintable,
}

/// Construct the terminal sink and its local presentation object before the
/// owner thread starts the pipeline. Only the element may cross threads.
fn native_sink() -> Result<(gst::Element, gtk::gdk::Paintable), PreviewError> {
    if !gtk::is_initialized_main_thread() {
        return Err(PreviewError::WrongThread);
    }
    gst::init().map_err(PreviewError::Initialize)?;
    // Registry owns the static plugin; repeated windows reuse it without a
    // project-level mutable singleton. Dynamic distro plugins are replaced.
    let statically_registered = gst::Registry::get()
        .find_plugin("gtk4")
        .is_some_and(|plugin| plugin.filename().is_none());
    if !statically_registered {
        gstgtk4::plugin_register_static().map_err(PreviewError::Sink)?;
    }
    let sink = gst::ElementFactory::make("gtk4paintablesink")
        .build()
        .map_err(PreviewError::Sink)?;
    let paintable = sink
        .property::<Option<gtk::gdk::Paintable>>("paintable")
        .ok_or(PreviewError::MissingPaintable)?;
    Ok((sink, paintable))
}

use prismcast_app::{AppHandle, AppSnapshot};
use prismcast_core::{CanvasId, VideoConfig};
use prismcast_media::{BackendComponent, BackendEvent, ComponentState, CompositorBackend};
use prismcast_media_gst::GstCompositor;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{oneshot, watch};

/// Latest-only observed preview health; never part of persisted domain state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreviewStatus {
    Starting,
    Running,
    Degraded(String),
    Failed(String),
    Stopped,
}

/// GTK-local lifetime guard for the paintable and its dedicated media owner.
/// Dropping requests cancellation; orderly window close must await shutdown.
pub struct PreviewSession {
    paintable: gtk::gdk::Paintable,
    // Keep the terminal native sink alive until GTK-local teardown.
    sink: gst::Element,
    status: watch::Receiver<PreviewStatus>,
    cancel: Option<oneshot::Sender<()>>,
    completion: Option<oneshot::Receiver<Result<(), PreviewError>>>,
}

impl PreviewSession {
    /// Attach native presentation locally and start asynchronous owner setup.
    /// Graph construction and mutation run exclusively on the owner thread.
    pub fn start(handle: AppHandle) -> Result<Self, PreviewError> {
        let (sink, paintable) = native_sink()?;
        let snapshots = handle.subscribe_snapshots();
        let (cancel, cancellation) = oneshot::channel();
        let (status_tx, status) = watch::channel(PreviewStatus::Starting);
        let (completed, completion) = oneshot::channel();
        let owner_sink = sink.clone();
        let owner_status = status_tx.clone();
        let owner = std::thread::Builder::new()
            .name("prismcast-media-preview".into())
            .spawn(move || media_owner(owner_sink, snapshots, cancellation, owner_status))
            .map_err(PreviewError::Thread)?;
        // Reaping away from GTK means completion proves the owner has exited,
        // without a blocking join in a GLib future.
        std::thread::Builder::new()
            .name("prismcast-preview-reaper".into())
            .spawn(move || {
                let result = owner.join().unwrap_or(Err(PreviewError::WorkerPanic));
                match &result {
                    Ok(()) => {
                        status_tx.send_replace(PreviewStatus::Stopped);
                    }
                    Err(error) => {
                        status_tx.send_replace(PreviewStatus::Failed(error.to_string()));
                    }
                }
                let _ = completed.send(result);
            })
            .map_err(PreviewError::Thread)?;
        Ok(Self {
            paintable,
            sink,
            status,
            cancel: Some(cancel),
            completion: Some(completion),
        })
    }

    pub fn paintable(&self) -> &gtk::gdk::Paintable {
        &self.paintable
    }

    /// A watch cell retaining at most one latest health status.
    pub fn subscribe_status(&self) -> watch::Receiver<PreviewStatus> {
        self.status.clone()
    }

    /// Cancel first, then await NULL teardown and owner-thread join while the
    /// GTK main context continues servicing sink callbacks.
    pub async fn shutdown(mut self) -> Result<(), PreviewError> {
        if let Some(cancel) = self.cancel.take() {
            let _ = cancel.send(());
        }
        let completion = self.completion.take().ok_or(PreviewError::WorkerGone)?;
        completion.await.map_err(|_| PreviewError::WorkerGone)?
    }
}

impl Drop for PreviewSession {
    fn drop(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            let _ = cancel.send(());
        }
        // Touch no pipeline state here: final native references are GTK-local.
        tracing::debug!(sink = %self.sink.name(), "preview attachment released");
    }
}

enum Wake {
    Snapshot,
    Health,
    Stop,
}

fn media_owner(
    sink: gst::Element,
    mut snapshots: watch::Receiver<Arc<AppSnapshot>>,
    mut cancellation: oneshot::Receiver<()>,
    status: watch::Sender<PreviewStatus>,
) -> Result<(), PreviewError> {
    let wait_runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .map_err(PreviewError::Thread)?;
    let mut compositor = GstCompositor::new(sink).map_err(PreviewError::Backend)?;
    let canvas = CanvasId::new();
    let initial = snapshots.borrow_and_update().clone();
    reconcile(&mut compositor, &initial, canvas, &status);
    loop {
        // Only waiting futures execute on Tokio. Every native graph operation
        // occurs after block_on returns, on this dedicated OS thread.
        let wake = wait_runtime.block_on(async {
            tokio::select! {
                biased;
                _ = &mut cancellation => Wake::Stop,
                changed = snapshots.changed() => if changed.is_ok() { Wake::Snapshot } else { Wake::Stop },
                _ = tokio::time::sleep(Duration::from_millis(100)) => Wake::Health,
            }
        });
        match wake {
            Wake::Stop => break,
            Wake::Snapshot => {
                drain_health(&mut compositor, &status);
                let snapshot = snapshots.borrow_and_update().clone();
                reconcile(&mut compositor, &snapshot, canvas, &status);
            }
            Wake::Health => drain_health(&mut compositor, &status),
        }
    }
    drain_health(&mut compositor, &status);
    compositor.stop().map_err(PreviewError::Backend)
}

fn reconcile(
    compositor: &mut GstCompositor,
    snapshot: &AppSnapshot,
    canvas: CanvasId,
    status: &watch::Sender<PreviewStatus>,
) {
    let state = snapshot.state();
    let video = state
        .active_profile
        .and_then(|id| state.profiles.get(&id))
        .map(|profile| profile.video)
        .unwrap_or(VideoConfig {
            width: 1280,
            height: 720,
            fps_num: 30,
            fps_den: 1,
        });
    let result = (|| {
        compositor.configure_canvas(canvas, video)?;
        if let Some(scene) = snapshot
            .current_scene()
            .and_then(|id| state.scenes.get(&id))
        {
            let sources = state.sources.values().cloned().collect::<Vec<_>>();
            compositor.sync_snapshot(&sources, scene)?;
        } else {
            compositor.clear_scene()?;
        }
        if matches!(
            compositor.state(),
            ComponentState::Stopped | ComponentState::Failed
        ) {
            compositor.start()?;
        }
        Ok::<(), prismcast_core::Error>(())
    })();
    match result {
        Ok(()) => {
            status.send_replace(PreviewStatus::Running);
        }
        Err(error) => {
            tracing::warn!(revision = snapshot.revision(), %error, "preview snapshot could not be rendered");
            // Avoid displaying stale content as though it were the new scene.
            if let Err(stop_error) = compositor.stop() {
                tracing::error!(%stop_error, "preview cleanup after failure failed");
            }
            status.send_replace(PreviewStatus::Failed(error.to_string()));
        }
    }
}

fn drain_health(compositor: &mut GstCompositor, status: &watch::Sender<PreviewStatus>) {
    for event in compositor.drain_events() {
        match event {
            BackendEvent::Warning { message } => {
                status.send_replace(PreviewStatus::Degraded(message));
            }
            BackendEvent::Error { message } | BackendEvent::DeviceLost { reason: message } => {
                status.send_replace(PreviewStatus::Failed(message));
            }
            BackendEvent::EndOfStream => {
                status.send_replace(PreviewStatus::Failed(
                    "Preview reached end of stream".into(),
                ));
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod display_tests {
    use super::*;
    use gtk::prelude::*;
    use prismcast_core::{Command, Event, SourceEvent, SourceKind};
    use std::{cell::Cell, rc::Rc, time::Instant};

    struct RendererGuard(gtk::gsk::Renderer);
    impl std::ops::Deref for RendererGuard {
        type Target = gtk::gsk::Renderer;
        fn deref(&self) -> &Self::Target {
            &self.0
        }
    }
    impl Drop for RendererGuard {
        fn drop(&mut self) {
            if self.0.is_realized() {
                self.0.unrealize();
            }
        }
    }

    fn pixel(paintable: &gtk::gdk::Paintable, renderer: &gtk::gsk::Renderer) -> Option<[u8; 4]> {
        if paintable.intrinsic_width() <= 0 || paintable.intrinsic_height() <= 0 {
            return None;
        }
        let snapshot = gtk::Snapshot::new();
        paintable.snapshot(&snapshot, 320.0, 180.0);
        let node = snapshot.to_node()?;
        let texture = renderer.render_texture(
            &node,
            Some(&gtk::graphene::Rect::new(0.0, 0.0, 320.0, 180.0)),
        );
        let stride = texture.width() as usize * 4;
        let mut pixels = vec![0; stride * texture.height() as usize];
        texture.download(&mut pixels, stride);
        let offset = texture.height() as usize / 2 * stride + texture.width() as usize / 2 * 4;
        Some(pixels[offset..offset + 4].try_into().unwrap())
    }

    async fn wait_pixel(
        paintable: &gtk::gdk::Paintable,
        renderer: &gtk::gsk::Renderer,
        expected: [u8; 3],
    ) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if pixel(paintable, renderer).is_some_and(|pixel| {
                pixel[..3]
                    .iter()
                    .zip(expected)
                    .all(|(actual, expected)| actual.abs_diff(expected) < 12)
            }) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "displayed pixel did not become {expected:?}; observed {:?}",
                pixel(paintable, renderer)
            );
            gtk::glib::timeout_future(Duration::from_millis(30)).await;
        }
    }

    #[test]
    #[ignore = "requires a real GTK display; run explicitly with --ignored --test-threads=1"]
    fn native_preview_commands_change_displayed_pixels_and_stop_owner() {
        gtk::init().unwrap();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let handle = {
            let _enter = runtime.enter();
            AppHandle::spawn(prismcast_app::CoreConfig::default())
        };
        gtk::glib::MainContext::default().block_on(async {
            let profile = prismcast_core::Profile::new("Preview test", VideoConfig { width:1280, height:720, fps_num:30, fps_den:1 });
            let profile_id = profile.id;
            handle.dispatch(Command::AddProfile { profile }).await.unwrap();
            handle.dispatch(Command::SelectProfile { profile_id }).await.unwrap();
            let session = PreviewSession::start(handle.clone()).unwrap();
            let status = session.subscribe_status();
            let plugin = gst::Registry::get().find_plugin("gtk4").unwrap();
            assert!(plugin.filename().is_none(), "preview must use statically registered matching sink");
            assert!(plugin.version().starts_with("0.15"));
            let paintable = session.paintable().clone();
            let invalidations = Rc::new(Cell::new(0usize));
            paintable.connect_invalidate_contents({ let count = invalidations.clone(); move |_| count.set(count.get() + 1) });
            let picture = gtk::Picture::for_paintable(&paintable);
            let window = gtk::Window::new();
            window.set_default_size(640, 360);
            window.set_child(Some(&picture));
            window.present();
            gtk::glib::timeout_future(Duration::from_millis(100)).await;
            let renderer = RendererGuard(gtk::gsk::Renderer::for_surface(&window.surface().unwrap()).unwrap());
            wait_pixel(&paintable, &renderer, [0, 0, 0]).await;
            handle.dispatch(Command::AddScene { name: "Red".into() }).await.unwrap();
            let scene_id = handle.snapshot().current_scene().unwrap();
            let created = handle.dispatch(Command::AddSource { kind: SourceKind::TestPattern, name: "Color pattern".into() }).await.unwrap();
            let source_id = created.events.iter().find_map(|event| match event { Event::Source(SourceEvent::Added { source }) => Some(source.id), _ => None }).unwrap();
            handle.dispatch(Command::SetSourceSettings { source_id, settings: serde_json::json!({"width":1280,"height":720,"fps":30,"pattern":"red"}) }).await.unwrap();
            handle.dispatch(Command::AddSceneItem { scene_id, source_id }).await.unwrap();
            wait_pixel(&paintable, &renderer, [0, 0, 255]).await;
            let item_id = handle.snapshot().state().scenes[&scene_id].items[0].id;
            handle.dispatch(Command::SetSceneItemVisible { scene_id, item_id, visible: false }).await.unwrap();
            wait_pixel(&paintable, &renderer, [0, 0, 0]).await;
            handle.dispatch(Command::SetSceneItemVisible { scene_id, item_id, visible: true }).await.unwrap();
            handle.dispatch(Command::SetSourceSettings { source_id, settings: serde_json::json!({"width":1280,"height":720,"fps":30,"pattern":"blue"}) }).await.unwrap();
            wait_pixel(&paintable, &renderer, [255, 0, 0]).await;
            handle.dispatch(Command::AddScene { name: "Empty".into() }).await.unwrap();
            let empty = handle.snapshot().scenes().find(|scene| scene.id != scene_id).unwrap().id;
            handle.dispatch(Command::SetCurrentScene { scene_id: empty }).await.unwrap();
            wait_pixel(&paintable, &renderer, [0, 0, 0]).await;
            assert!(invalidations.get() >= 4);
            assert_eq!(paintable.intrinsic_width(), 1280);
            assert_eq!(paintable.intrinsic_height(), 720);
            let created = handle.dispatch(Command::AddSource { kind: SourceKind::Color, name: "Unsupported prototype source".into() }).await.unwrap();
            let unsupported_id = created.events.iter().find_map(|event| match event { Event::Source(SourceEvent::Added { source }) => Some(source.id), _ => None }).unwrap();
            handle.dispatch(Command::AddSceneItem { scene_id: empty, source_id: unsupported_id }).await.unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            while !matches!(*status.borrow(), PreviewStatus::Failed(_)) {
                assert!(Instant::now() < deadline, "unsupported source was not reported");
                gtk::glib::timeout_future(Duration::from_millis(30)).await;
            }
            let unsupported_item = handle.snapshot().state().scenes[&empty].items[0].id;
            handle.dispatch(Command::RemoveSceneItem { scene_id: empty, item_id: unsupported_item }).await.unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            while *status.borrow() != PreviewStatus::Running {
                assert!(Instant::now() < deadline, "preview did not recover after unsupported source removal");
                gtk::glib::timeout_future(Duration::from_millis(30)).await;
            }
            wait_pixel(&paintable, &renderer, [0,0,0]).await;
            gtk::glib::future_with_timeout(Duration::from_secs(5), session.shutdown()).await.unwrap().unwrap();
            assert_eq!(*status.borrow(), PreviewStatus::Stopped);
            drop(renderer);
            window.close();
            handle.shutdown().await;
        });
    }
}
