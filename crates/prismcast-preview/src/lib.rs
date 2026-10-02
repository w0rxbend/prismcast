//! GTK-local preview attachment. Pipeline ownership lives on a media thread.

use gstreamer::{self as gst, prelude::*};
mod capture_owner;

/// Failures at the GTK/native media integration boundary.
#[derive(Debug, thiserror::Error)]
pub enum PreviewError {
    #[error("preview initialization must run on the GTK main thread")]
    WrongThread,
    #[error("preview owner thread failed: {0}")]
    Thread(#[source] std::io::Error),
    #[error("preview media backend failed: {0}")]
    Backend(#[source] prismcast_core::Error),
    #[error("capture service failed: {0}")]
    Capture(String),
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
        let owner_handle = handle.clone();
        let owner = std::thread::Builder::new()
            .name("prismcast-media-preview".into())
            .spawn(move || {
                media_owner(
                    owner_sink,
                    snapshots,
                    cancellation,
                    owner_status,
                    owner_handle,
                )
            })
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
    CaptureRequest(prismcast_app::CaptureAuthorizationRequest),
    CaptureComplete(capture_owner::Completion),
}

fn media_owner(
    sink: gst::Element,
    mut snapshots: watch::Receiver<Arc<AppSnapshot>>,
    mut cancellation: oneshot::Receiver<()>,
    status: watch::Sender<PreviewStatus>,
    handle: AppHandle,
) -> Result<(), PreviewError> {
    let wait_runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(PreviewError::Thread)?;
    let owner = wait_runtime
        .block_on(handle.attach_capture_owner())
        .map_err(|error| PreviewError::Capture(error.to_string()))?;
    let mut captures = capture_owner::Captures::new(owner)
        .map_err(|error| PreviewError::Capture(error.to_string()))?;
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
                completion = captures.completed.recv() => if let Some(completion)=completion {Wake::CaptureComplete(completion)} else {Wake::Stop},
                request = captures.owner.requests.recv() => if let Some(request)=request {Wake::CaptureRequest(request)} else {Wake::Stop},
                _ = tokio::time::sleep(Duration::from_millis(100)) => Wake::Health,
            }
        });
        drain_health(&mut compositor, &status);
        match wake {
            Wake::Stop => break,
            Wake::Snapshot => {
                let snapshot = snapshots.borrow_and_update().clone();
                captures.health(&snapshot, &wait_runtime, &mut compositor);
                reconcile(&mut compositor, &snapshot, canvas, &status);
            }
            Wake::Health => {
                let snapshot = snapshots.borrow().clone();
                captures.health(&snapshot, &wait_runtime, &mut compositor);
                drain_health(&mut compositor, &status);
            }
            Wake::CaptureRequest(request) => {
                let snapshot = snapshots.borrow().clone();
                captures.authorize(request, &snapshot, &wait_runtime, &mut compositor);
            }
            Wake::CaptureComplete(completion) => {
                let snapshot = snapshots.borrow().clone();
                captures.complete(completion, &snapshot, &wait_runtime, &mut compositor);
            }
        }
        if let Some(error) = captures.graph_error() {
            status.send_replace(PreviewStatus::Failed(error.to_owned()));
        }
    }
    drain_health(&mut compositor, &status);
    let result = compositor.stop().map_err(PreviewError::Backend);
    captures.shutdown(&wait_runtime, &mut compositor);
    result
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

    fn render(
        paintable: &gtk::gdk::Paintable,
        renderer: &gtk::gsk::Renderer,
    ) -> Option<gtk::gdk::Texture> {
        if paintable.intrinsic_width() <= 0 || paintable.intrinsic_height() <= 0 {
            return None;
        }
        let snapshot = gtk::Snapshot::new();
        paintable.snapshot(&snapshot, 320.0, 180.0);
        let node = snapshot.to_node()?;
        Some(renderer.render_texture(
            &node,
            Some(&gtk::graphene::Rect::new(0.0, 0.0, 320.0, 180.0)),
        ))
    }

    fn pixel(paintable: &gtk::gdk::Paintable, renderer: &gtk::gsk::Renderer) -> Option<[u8; 4]> {
        let texture = render(paintable, renderer)?;
        let stride = texture.width() as usize * 4;
        let mut pixels = vec![0; stride * texture.height() as usize];
        texture.download(&mut pixels, stride);
        let offset = texture.height() as usize / 2 * stride + texture.width() as usize / 2 * 4;
        Some(pixels[offset..offset + 4].try_into().unwrap())
    }

    fn dump_png(
        paintable: &gtk::gdk::Paintable,
        renderer: &gtk::gsk::Renderer,
        name: &str,
    ) -> String {
        let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/tmp");
        let _ = std::fs::create_dir_all(&directory);
        let path = directory.join(name);
        match render(paintable, renderer) {
            Some(texture) => {
                texture.save_to_png(&path).unwrap();
                path.display().to_string()
            }
            None => "no texture".into(),
        }
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
    #[test]
    #[ignore = "opens one real Window portal picker; select Prismcast capture test target"]
    fn actual_window_capture_preview_pixels_placement_and_shutdown() {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };
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
            let session=PreviewSession::start(handle.clone()).unwrap();let status=session.subscribe_status();let paintable=session.paintable().clone();
            let frames=Arc::new(AtomicUsize::new(0));let count=frames.clone();
            session.sink.static_pad("sink").unwrap().add_probe(gst::PadProbeType::BUFFER,move|_,_|{count.fetch_add(1,Ordering::SeqCst);gst::PadProbeReturn::Ok});
            let window=gtk::Window::new();window.set_title(Some("Prismcast capture preview"));window.set_default_size(640,360);window.set_child(Some(&gtk::Picture::for_paintable(&paintable)));window.present();
            let target=gtk::Window::new();target.set_title(Some("Prismcast capture test target – select this window"));target.set_default_size(480,270);
            let area=gtk::DrawingArea::new();let red=Rc::new(Cell::new(true));
            area.set_draw_func({let red=red.clone();move|_,context,_,_|{if red.get(){context.set_source_rgb(1.0,0.0,0.0)}else{context.set_source_rgb(0.0,0.0,1.0)}let _=context.paint();}});
            target.set_child(Some(&area));target.present();
            let timer=gtk::glib::timeout_add_local(Duration::from_millis(250),{let area=area.downgrade();move||{if let Some(area)=area.upgrade(){red.set(!red.get());area.queue_draw();gtk::glib::ControlFlow::Continue}else{gtk::glib::ControlFlow::Break}}});
            gtk::glib::timeout_future(Duration::from_millis(150)).await;
            let renderer=RendererGuard(gtk::gsk::Renderer::for_surface(&window.surface().unwrap()).unwrap());
            let result:Result<(),String>=async {
                handle.dispatch(Command::AddScene{name:"Capture scene".into()}).await.map_err(|error|error.to_string())?;
                let scene_id=handle.snapshot().current_scene().ok_or("no scene")?;
                let created=handle.dispatch(Command::AddSource{kind:SourceKind::PipeWireWindow,name:"Window under test".into()}).await.map_err(|error|error.to_string())?;
                let source_id=created.events.iter().find_map(|event|match event{Event::Source(SourceEvent::Added{source})=>Some(source.id),_=>None}).ok_or("no source")?;
                handle.dispatch(Command::AddSceneItem{scene_id,source_id}).await.map_err(|error|error.to_string())?;
                let item_id=handle.snapshot().state().scenes[&scene_id].items[0].id;
                let deadline=Instant::now()+Duration::from_secs(5);
                while *status.borrow()!=PreviewStatus::Running{if Instant::now()>deadline{return Err(format!("preview service not ready: {:?}",status.borrow()))}gtk::glib::timeout_future(Duration::from_millis(30)).await;}
                eprintln!("Portal will open once: choose 'Prismcast capture test target – select this window'.");
                handle.authorize_source_capture(source_id,None).await.map_err(|error|error.to_string())?;
                let deadline=Instant::now()+Duration::from_secs(130);
                let observed=loop{
                    let snapshot=handle.snapshot();
                    if let Some(observed)=snapshot.source_runtime(source_id){
                        if observed.status==prismcast_core::CaptureStatus::Active{break observed.clone()}
                        if observed.status!=prismcast_core::CaptureStatus::Authorizing{return Err(format!("capture terminal: {observed:?}"))}
                    }
                    if Instant::now()>deadline{return Err("capture authorization deadline".into())}
                    gtk::glib::timeout_future(Duration::from_millis(30)).await;
                };
                let dimensions=observed.dimensions.ok_or("active capture missing dimensions")?;
                let transform=prismcast_core::Transform{scale:prismcast_core::Vec2::new(1280.0/dimensions.width as f32,720.0/dimensions.height as f32),..Default::default()};
                handle.dispatch(Command::SetSceneItemTransform{scene_id,item_id,transform}).await.map_err(|error|error.to_string())?;
                let first_count=frames.load(Ordering::SeqCst);
                let deadline=Instant::now()+Duration::from_secs(10);let mut red_seen=false;let mut blue_seen=false;
                while !(red_seen&&blue_seen){
                    if let Some(pixel)=pixel(&paintable,&renderer){red_seen|=pixel[2]>200&&pixel[0]<30&&pixel[1]<30;blue_seen|=pixel[0]>200&&pixel[2]<30&&pixel[1]<30;}
                    if Instant::now()>deadline{let dump=dump_png(&paintable,&renderer,"capture-preview-failure.png");return Err(format!("selected window did not show fixture red/blue pixels: {:?}, native frames {}, dimensions {dimensions:?}, dump {dump}",pixel(&paintable,&renderer),frames.load(Ordering::SeqCst)-first_count))}
                    gtk::glib::timeout_future(Duration::from_millis(30)).await;
                }
                if frames.load(Ordering::SeqCst)<=first_count+2{return Err("no native frames after capture activation".into())}
                handle.dispatch(Command::SetSceneItemVisible{scene_id,item_id,visible:false}).await.map_err(|error|error.to_string())?;
                let deadline=Instant::now()+Duration::from_secs(5);
                while !pixel(&paintable,&renderer).is_some_and(|pixel|pixel[..3].iter().all(|byte|*byte<12)){
                    if Instant::now()>deadline{return Err("hidden placement did not become black".into())}gtk::glib::timeout_future(Duration::from_millis(30)).await;
                }
                handle.dispatch(Command::SetSceneItemVisible{scene_id,item_id,visible:true}).await.map_err(|error|error.to_string())?;
                let deadline=Instant::now()+Duration::from_secs(5);
                while !pixel(&paintable,&renderer).is_some_and(|pixel|pixel[0]>200||pixel[2]>200){if Instant::now()>deadline{return Err("capture did not survive placement rebuild".into())}gtk::glib::timeout_future(Duration::from_millis(30)).await;}
                let snapshot=handle.snapshot();let retained=snapshot.source_runtime(source_id).ok_or("capture runtime lost")?;
                if retained.generation!=observed.generation||retained.dimensions!=Some(dimensions){return Err("placement changed capture grant/caps".into())}
                eprintln!("Integrated window capture: {dimensions:?}, generation {:?}, native frames {}",observed.generation,frames.load(Ordering::SeqCst)-first_count);
                handle.dispatch(Command::RemoveSource{source_id}).await.map_err(|error|error.to_string())?;
                gtk::glib::timeout_future(Duration::from_millis(300)).await;
                if handle.snapshot().source_runtime(source_id).is_some(){return Err("removed source revived".into())}
                Ok(())
            }.await;
            // Cleanup completes even if manual selection, pixels, or caps checks fail.
            let shutdown=gtk::glib::future_with_timeout(Duration::from_secs(15),session.shutdown()).await;
            timer.remove();drop(renderer);target.close();window.close();handle.shutdown().await;
            shutdown.unwrap().unwrap();result.unwrap();
        });
    }
    #[test]
    #[ignore = "opens one real Window portal picker; select Prismcast capture test target"]
    fn actual_window_capture_consumer_frames_show_fixture_pixels() {
        gtk::init().unwrap();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        gtk::glib::MainContext::default().block_on(async {
            let target=gtk::Window::new();target.set_title(Some("Prismcast capture test target – select this window"));target.set_default_size(480,270);
            let area=gtk::DrawingArea::new();let red=Rc::new(Cell::new(true));
            area.set_draw_func({let red=red.clone();move|_,context,_,_|{if red.get(){context.set_source_rgb(1.0,0.0,0.0)}else{context.set_source_rgb(0.0,0.0,1.0)}let _=context.paint();}});
            target.set_child(Some(&area));target.present();
            let timer=gtk::glib::timeout_add_local(Duration::from_millis(250),{let area=area.downgrade();move||{if let Some(area)=area.upgrade(){red.set(!red.get());area.queue_draw();gtk::glib::ControlFlow::Continue}else{gtk::glib::ControlFlow::Break}}});
            gtk::glib::timeout_future(Duration::from_millis(150)).await;
            let result:Result<(),String>=async {
                let _entered=runtime.enter();
                eprintln!("Portal will open once: choose 'Prismcast capture test target – select this window'.");
                let broker=prismcast_capture::CaptureBroker::new(prismcast_capture::CaptureConfig::default()).map_err(|error|error.to_string())?;
                let lease=broker.authorize(prismcast_core::SourceId::new(),prismcast_capture::CaptureKind::Window).map_err(|error|error.to_string())?.wait().await.map_err(|error|error.to_string())?;
                let mut producer=prismcast_capture::producer::CaptureProducer::start(lease).map_err(|error|error.to_string())?;
                let feed=producer.feed();
                let deadline=Instant::now()+Duration::from_secs(15);
                while feed.dimensions().is_none(){
                    if let Some(error)=feed.error(){return Err(format!("feed error before dimensions: {error}"))}
                    if Instant::now()>deadline{return Err("capture negotiated no frame within 15s".into())}
                    gtk::glib::timeout_future(Duration::from_millis(30)).await;
                }
                let dimensions=feed.dimensions().unwrap();
                let consumer=feed.consumer().map_err(|error|error.to_string())?;
                let latest=std::sync::Arc::new(std::sync::Mutex::new(None::<(gst::Buffer,gst::Caps)>));
                let pad=consumer.bin.static_pad("src").ok_or("consumer missing src pad")?;
                pad.add_probe(gst::PadProbeType::BUFFER,{let latest=latest.clone();move|pad,info|{
                    if let Some(gst::PadProbeData::Buffer(ref buffer))=info.data{
                        if let Some(caps)=pad.current_caps(){*latest.lock().unwrap()=Some((buffer.clone(),caps));}
                    }
                    gst::PadProbeReturn::Ok
                }});
                let sink=gst::ElementFactory::make("fakesink").build().map_err(|error|error.to_string())?;
                let pipeline=gst::Pipeline::new();
                pipeline.add_many([consumer.bin.upcast_ref::<gst::Element>(),&sink]).map_err(|error|error.to_string())?;
                gst::Element::link_many([consumer.bin.upcast_ref::<gst::Element>(),&sink]).map_err(|error|error.to_string())?;
                pipeline.set_state(gst::State::Playing).map_err(|error|error.to_string())?;
                let deadline=Instant::now()+Duration::from_secs(10);let mut red_seen=false;let mut blue_seen=false;let mut dumped=false;let mut last_center=None;
                while !(red_seen&&blue_seen){
                    let frame=latest.lock().unwrap().clone();
                    if let Some((buffer,caps))=frame{
                        let structure=caps.structure(0).ok_or("consumer frame missing caps structure")?;
                        let width=structure.get::<i32>("width").map_err(|error|error.to_string())? as usize;
                        let height=structure.get::<i32>("height").map_err(|error|error.to_string())? as usize;
                        let map=buffer.map_readable().map_err(|error|error.to_string())?;
                        let offset=(height/2*width+width/2)*4;
                        let center=[map[offset],map[offset+1],map[offset+2],map[offset+3]];
                        last_center=Some(center);
                        red_seen|=center[0]>200&&center[1]<30&&center[2]<30;
                        blue_seen|=center[2]>200&&center[0]<30&&center[1]<30;
                        if !dumped{
                            dumped=true;
                            let bytes=gtk::glib::Bytes::from_owned(map.as_slice().to_vec());
                            let texture=gtk::gdk::MemoryTexture::new(width as i32,height as i32,gtk::gdk::MemoryFormat::R8g8b8a8,&bytes,width*4);
                            let path=std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/tmp/capture-raw-frame.png");
                            texture.save_to_png(&path).map_err(|error|error.to_string())?;
                            eprintln!("raw consumer frame dump: {}",path.display());
                        }
                    }
                    if Instant::now()>deadline{return Err(format!("consumer frames did not show fixture red/blue: center {last_center:?}, dimensions {dimensions:?}, received {}",feed.received()))}
                    gtk::glib::timeout_future(Duration::from_millis(30)).await;
                }
                eprintln!("consumer frames verified: dimensions {dimensions:?}, received {}",feed.received());
                pipeline.set_state(gst::State::Null).map_err(|error|error.to_string())?;
                drop(consumer);drop(pipeline);
                producer.shutdown_native().map_err(|error|error.to_string())?;
                let lease=producer.take_stopped_lease().map_err(|error|error.to_string())?;
                lease.close().await.map_err(|error|error.to_string())?;
                broker.shutdown().await.map_err(|error|error.to_string())?;
                Ok(())
            }.await;
            timer.remove();target.close();
            result.unwrap();
        });
    }
}

pub mod audio_owner;
pub use audio_owner::{AudioSession, AudioSessionError, AudioStatus};
