//! CPU prototype: reconstruct topology behind a NULL barrier on scene changes.
//! Media owner thread only. A GTK adapter may provide a Send sink element, but
//! this module neither imports GTK nor accesses its paintable.
use crate::{build_test_pattern_bin, GstRuntime, TestPatternSettings};
use gstreamer::{self as gst, prelude::*};
use prismcast_core::{
    Anchor, BlendMode, BoundsKind, CanvasId, Crop, Error, Result, Scene, SceneId, SceneItem,
    SceneItemId, Source, SourceId, SourceKind, Transition, TransitionKind, VideoConfig,
};
use prismcast_media::{BackendComponent, BackendEvent, ComponentState, CompositorBackend};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::mpsc::{self, Receiver},
};

fn media(error: impl std::fmt::Display) -> Error {
    Error::Media(error.to_string())
}
fn invalid(message: impl Into<String>) -> Error {
    Error::InvalidInput(message.into())
}
fn settings(source: &Source) -> Result<TestPatternSettings> {
    if source.kind != SourceKind::TestPattern {
        return Err(invalid(
            "CPU prototype only renders TestPattern sources; capture/nesting are later tasks",
        ));
    }
    TestPatternSettings::from_json(if source.settings.is_null() {
        serde_json::json!({})
    } else {
        source.settings.clone()
    })
}
fn validate_video(video: &VideoConfig) -> Result<()> {
    if !(1..=8192).contains(&video.width)
        || !(1..=8192).contains(&video.height)
        || video.fps_num == 0
        || video.fps_den == 0
        || video.fps_num > i32::MAX as u32
        || video.fps_den > i32::MAX as u32
        || video.fps_num as f64 / video.fps_den as f64 > 240.0
    {
        return Err(invalid(
            "canvas requires dimensions 1..8192 and positive signed-32-bit rational fps <=240",
        ));
    }
    Ok(())
}
fn validate_scene(scene: &Scene, sources: &HashMap<SourceId, Source>) -> Result<()> {
    if scene.items.len() > 256 {
        return Err(invalid("CPU prototype supports at most 256 scene items"));
    }
    let mut ids = HashSet::new();
    for item in &scene.items {
        if !ids.insert(item.id) {
            return Err(invalid("duplicate scene item id"));
        }
        let source = sources
            .get(&item.source_id)
            .ok_or_else(|| Error::NotFound(format!("source {}", item.source_id)))?;
        if !source.filters.is_empty() {
            return Err(invalid(
                "source filter rendering awaits filter backend tasks",
            ));
        }
        let source = settings(source)?;
        let t = item.transform;
        if t.rotation != 0.0
            || t.anchor != Anchor::TopLeft
            || item.crop != Crop::default()
            || item.bounds.kind != BoundsKind::None
            || item.blend_mode != BlendMode::Normal
        {
            return Err(invalid("CPU prototype supports top-left positive scale/position and normal alpha; crop, rotation, anchors, bounds and other blends await MEDIA-005"));
        }
        if !t.position.x.is_finite()
            || !t.position.y.is_finite()
            || t.position.x.abs() > 1_000_000.0
            || t.position.y.abs() > 1_000_000.0
            || !t.scale.x.is_finite()
            || !t.scale.y.is_finite()
            || t.scale.x <= 0.0
            || t.scale.y <= 0.0
            || t.scale.x > 64.0
            || t.scale.y > 64.0
            || source.width as f32 * t.scale.x > 8192.0
            || source.height as f32 * t.scale.y > 8192.0
            || !item.opacity.is_finite()
            || !(0.0..=1.0).contains(&item.opacity)
        {
            return Err(invalid("invalid or excessive scene item geometry/opacity"));
        }
    }
    Ok(())
}
fn render_matches(
    old: &Scene,
    new: &Scene,
    old_sources: &HashMap<SourceId, Source>,
    new_sources: &HashMap<SourceId, Source>,
) -> bool {
    let mut old_items = old.items.clone();
    let mut new_items = new.items.clone();
    old_items.sort_by_key(|item| item.z_index);
    new_items.sort_by_key(|item| item.z_index);
    for item in old_items.iter_mut().chain(new_items.iter_mut()) {
        item.locked = false;
    }
    if old_items != new_items {
        return false;
    }
    new_items.iter().all(|item| {
        match (
            old_sources.get(&item.source_id),
            new_sources.get(&item.source_id),
        ) {
            (Some(a), Some(b)) => {
                a.kind == b.kind
                    && a.enabled == b.enabled
                    && a.filters == b.filters
                    && settings(a).ok() == settings(b).ok()
            }
            _ => false,
        }
    })
}
struct SharedSource {
    bin: gst::Bin,
    tee: gst::Element,
}
struct Branch {
    queue: gst::Element,
    tee: gst::Element,
    tee_pad: gst::Pad,
    mixer_pad: gst::Pad,
}

/// One CPU compositor and one current program scene per media owner.
pub struct GstCompositor {
    runtime: GstRuntime,
    pipeline: gst::Pipeline,
    mixer: gst::Element,
    capsfilter: gst::Element,
    sink: gst::Element,
    terminal: Receiver<BackendEvent>,
    sources: HashMap<SourceId, Source>,
    scenes: HashMap<SceneId, Scene>,
    current: Option<SceneId>,
    video: VideoConfig,
    canvas: Option<CanvasId>,
    shared: HashMap<SourceId, SharedSource>,
    branches: Vec<Branch>,
    state: ComponentState,
    events: VecDeque<BackendEvent>,
}
impl GstCompositor {
    /// Sink must be unattached. GTK-created sinks cross the thread boundary as
    /// native GstElements; all GTK object access stays with the UI adapter.
    pub fn new(sink: gst::Element) -> Result<Self> {
        let runtime = GstRuntime::initialize().map_err(media)?;
        let mixer = runtime
            .require_factory("compositor")
            .map_err(media)?
            .create()
            .property("force-live", true)
            .property("ignore-inactive-pads", true)
            .property_from_str("background", "black")
            .build()
            .map_err(media)?;
        let capsfilter = runtime
            .require_factory("capsfilter")
            .map_err(media)?
            .create()
            .build()
            .map_err(media)?;
        let pipeline = gst::Pipeline::new();
        pipeline
            .add_many([&mixer, &capsfilter, &sink])
            .map_err(media)?;
        gst::Element::link_many([&mixer, &capsfilter, &sink]).map_err(media)?;
        let (tx, terminal) = mpsc::sync_channel(8);
        pipeline
            .bus()
            .ok_or_else(|| media("compositor bus unavailable"))?
            .set_sync_handler(move |_, message| {
                let event = match message.view() {
                    gst::MessageView::Error(error) => Some(BackendEvent::Error {
                        message: error.error().to_string(),
                    }),
                    gst::MessageView::Eos(_) => Some(BackendEvent::EndOfStream),
                    _ => None,
                };
                if let Some(event) = event {
                    let _ = tx.try_send(event);
                }
                gst::BusSyncReply::Drop
            });
        let result = Self {
            runtime,
            pipeline,
            mixer,
            capsfilter,
            sink,
            terminal,
            sources: HashMap::new(),
            scenes: HashMap::new(),
            current: None,
            video: VideoConfig {
                width: 1280,
                height: 720,
                fps_num: 30,
                fps_den: 1,
            },
            canvas: None,
            shared: HashMap::new(),
            branches: Vec::new(),
            state: ComponentState::Stopped,
            events: VecDeque::new(),
        };
        result.apply_caps();
        Ok(result)
    }
    /// Atomically validate authoritative sources and the selected scene before
    /// changing the graph. Native construction failure leaves it stopped/Failed.
    pub fn sync_snapshot(&mut self, sources: &[Source], scene: &Scene) -> Result<()> {
        let sources = Self::source_map(sources)?;
        validate_scene(scene, &sources)?;
        let same = self.current == Some(scene.id)
            && self
                .scenes
                .get(&scene.id)
                .is_some_and(|old| render_matches(old, scene, &self.sources, &sources));
        if !self.scenes.contains_key(&scene.id) && self.scenes.len() >= 256 {
            return Err(invalid("CPU prototype caches at most 256 scenes"));
        }
        self.sources = sources;
        self.scenes.insert(scene.id, scene.clone());
        self.current = Some(scene.id);
        if same {
            Ok(())
        } else {
            self.rebuild()
        }
    }
    /// Replace registry definitions; every current placement must still resolve.
    pub fn sync_sources(&mut self, sources: &[Source]) -> Result<()> {
        let sources = Self::source_map(sources)?;
        if let Some(scene) = self.current.and_then(|id| self.scenes.get(&id)) {
            validate_scene(scene, &sources)?;
        }
        let same = self
            .current
            .and_then(|id| self.scenes.get(&id))
            .is_none_or(|scene| render_matches(scene, scene, &self.sources, &sources));
        self.sources = sources;
        if same {
            Ok(())
        } else {
            self.rebuild()
        }
    }
    /// Clear the current scene when the authoritative snapshot has no program scene.
    pub fn clear_scene(&mut self) -> Result<()> {
        if self.current.is_none() {
            return Ok(());
        }
        self.current = None;
        self.rebuild()
    }
    fn source_map(sources: &[Source]) -> Result<HashMap<SourceId, Source>> {
        if sources.len() > 4096 {
            return Err(invalid(
                "CPU prototype supports at most 4096 source definitions",
            ));
        }
        let mut map = HashMap::new();
        for source in sources {
            if map.insert(source.id, source.clone()).is_some() {
                return Err(invalid("duplicate source id"));
            }
        }
        Ok(map)
    }
    pub fn video(&self) -> &VideoConfig {
        &self.video
    }
    pub fn source_count(&self) -> usize {
        self.shared.len()
    }
    pub fn branch_count(&self) -> usize {
        self.branches.len()
    }
    pub fn start(&mut self) -> Result<()> {
        if self.state == ComponentState::Running {
            return Ok(());
        }
        self.rebuild()?;
        if let Err(error) = self.pipeline.set_state(gst::State::Playing) {
            return self.fail(media(error));
        }
        self.set_state(ComponentState::Running);
        Ok(())
    }
    /// Stops streaming, unlinks every branch and releases both request pads.
    pub fn stop(&mut self) -> Result<()> {
        if let Err(error) = self.clear_graph() {
            return self.fail(error);
        }
        self.set_state(ComponentState::Stopped);
        Ok(())
    }
    fn fail(&mut self, error: Error) -> Result<()> {
        self.push(BackendEvent::Error {
            message: error.to_string(),
        });
        self.set_state(ComponentState::Failed);
        if let Err(cleanup) = self.clear_graph() {
            tracing::error!(%cleanup,"compositor cleanup failed");
        }
        Err(error)
    }
    fn push(&mut self, event: BackendEvent) {
        if self.events.len() == 32 {
            self.events.pop_front();
        }
        self.events.push_back(event);
    }
    fn set_state(&mut self, state: ComponentState) {
        if self.state != state {
            self.state = state;
            self.push(BackendEvent::StateChanged { state });
        }
    }
    fn apply_caps(&self) {
        self.capsfilter.set_property(
            "caps",
            gst::Caps::builder("video/x-raw")
                .field("format", "RGBA")
                .field("width", self.video.width as i32)
                .field("height", self.video.height as i32)
                .field(
                    "framerate",
                    gst::Fraction::new(self.video.fps_num as i32, self.video.fps_den as i32),
                )
                .field("pixel-aspect-ratio", gst::Fraction::new(1, 1))
                .build(),
        );
    }
    fn clear_graph(&mut self) -> Result<()> {
        self.pipeline.set_state(gst::State::Null).map_err(media)?;
        // NULL joins streaming threads: no pad blocking callback or live unlink race.
        for branch in self.branches.drain(..) {
            if let Some(sink) = branch.queue.static_pad("sink") {
                let _ = branch.tee_pad.unlink(&sink);
            }
            if let Some(src) = branch.queue.static_pad("src") {
                let _ = src.unlink(&branch.mixer_pad);
            }
            branch.tee.release_request_pad(&branch.tee_pad);
            self.mixer.release_request_pad(&branch.mixer_pad);
            self.pipeline.remove(&branch.queue).map_err(media)?;
        }
        for (_, source) in self.shared.drain() {
            source.bin.unlink(&source.tee);
            self.pipeline
                .remove_many([source.bin.upcast_ref::<gst::Element>(), &source.tee])
                .map_err(media)?;
        }
        // Recover partial native additions even if add_many failed halfway through.
        for child in self.pipeline.children() {
            if child != self.mixer && child != self.capsfilter && child != self.sink {
                self.pipeline.remove(&child).map_err(media)?;
            }
        }
        while self.terminal.try_recv().is_ok() {}
        Ok(())
    }
    fn rebuild(&mut self) -> Result<()> {
        let running = self.state == ComponentState::Running;
        if let Err(error) = self.clear_graph() {
            return self.fail(error);
        }
        self.apply_caps();
        if let Err(error) = self.build_current() {
            return self.fail(error);
        }
        if running {
            if let Err(error) = self.pipeline.set_state(gst::State::Playing) {
                return self.fail(media(error));
            }
        }
        Ok(())
    }
    fn build_current(&mut self) -> Result<()> {
        let Some(scene) = self.current.and_then(|id| self.scenes.get(&id)).cloned() else {
            return Ok(());
        };
        let mut items = scene.items.clone();
        items.sort_by_key(|item| item.z_index);
        for (rank, item) in items.iter().enumerate() {
            let source = self
                .sources
                .get(&item.source_id)
                .ok_or_else(|| Error::NotFound(format!("source {}", item.source_id)))?;
            if !source.enabled {
                continue;
            }
            let source_settings = settings(source)?;
            if !self.shared.contains_key(&item.source_id) {
                let bin = build_test_pattern_bin(&self.runtime, item.source_id, &source_settings)?;
                let tee = self
                    .runtime
                    .require_factory("tee")
                    .map_err(media)?
                    .create()
                    .property("allow-not-linked", true)
                    .build()
                    .map_err(media)?;
                self.pipeline
                    .add_many([bin.upcast_ref::<gst::Element>(), &tee])
                    .map_err(media)?;
                self.shared.insert(
                    item.source_id,
                    SharedSource {
                        bin: bin.clone(),
                        tee: tee.clone(),
                    },
                );
                bin.link(&tee).map_err(media)?;
            }
            let tee = self
                .shared
                .get(&item.source_id)
                .ok_or_else(|| media("shared source registry missing entry"))?
                .tee
                .clone();
            self.add_branch(tee, item, &source_settings, rank as u32)?;
        }
        Ok(())
    }
    fn add_branch(
        &mut self,
        tee: gst::Element,
        item: &SceneItem,
        settings: &TestPatternSettings,
        rank: u32,
    ) -> Result<()> {
        let queue = self
            .runtime
            .require_factory("queue")
            .map_err(media)?
            .create()
            .property("max-size-buffers", 2_u32)
            .property("max-size-bytes", 0_u32)
            .property("max-size-time", 0_u64)
            .property_from_str("leaky", "downstream")
            .build()
            .map_err(media)?;
        self.pipeline.add(&queue).map_err(media)?;
        let Some(tee_pad) = tee.request_pad_simple("src_%u") else {
            self.pipeline.remove(&queue).map_err(media)?;
            return Err(media("tee request pad unavailable"));
        };
        let Some(mixer_pad) = self.mixer.request_pad_simple("sink_%u") else {
            tee.release_request_pad(&tee_pad);
            self.pipeline.remove(&queue).map_err(media)?;
            return Err(media("compositor request pad unavailable"));
        };
        // Register ownership before fallible links so failure cleanup releases pads.
        self.branches.push(Branch {
            queue: queue.clone(),
            tee,
            tee_pad: tee_pad.clone(),
            mixer_pad: mixer_pad.clone(),
        });
        let sink = queue
            .static_pad("sink")
            .ok_or_else(|| media("queue sink missing"))?;
        let src = queue
            .static_pad("src")
            .ok_or_else(|| media("queue src missing"))?;
        tee_pad.link(&sink).map_err(media)?;
        src.link(&mixer_pad).map_err(media)?;
        mixer_pad.set_property("xpos", item.transform.position.x.round() as i32);
        mixer_pad.set_property("ypos", item.transform.position.y.round() as i32);
        mixer_pad.set_property(
            "width",
            (settings.width as f32 * item.transform.scale.x)
                .round()
                .max(1.0) as i32,
        );
        mixer_pad.set_property(
            "height",
            (settings.height as f32 * item.transform.scale.y)
                .round()
                .max(1.0) as i32,
        );
        mixer_pad.set_property(
            "alpha",
            if item.visible {
                item.opacity as f64
            } else {
                0.0
            },
        );
        mixer_pad.set_property("zorder", rank);
        tracing::debug!(source_id=%item.source_id,item_id=%item.id,rank,"CPU compositor item linked");
        Ok(())
    }
}
impl Drop for GstCompositor {
    fn drop(&mut self) {
        if let Err(error) = self.clear_graph() {
            tracing::error!(%error,"compositor drop cleanup failed");
        }
        if let Some(bus) = self.pipeline.bus() {
            bus.unset_sync_handler();
        }
    }
}
impl BackendComponent for GstCompositor {
    fn state(&self) -> ComponentState {
        self.state
    }
    fn drain_events(&mut self) -> Vec<BackendEvent> {
        let terminal: Vec<_> = self.terminal.try_iter().take(8).collect();
        if !terminal.is_empty() {
            let failed = self.state == ComponentState::Failed
                || terminal
                    .iter()
                    .any(|event| matches!(event, BackendEvent::Error { .. }));
            for event in terminal {
                self.push(event);
            }
            if let Err(error) = self.clear_graph() {
                self.push(BackendEvent::Error {
                    message: error.to_string(),
                });
                self.set_state(ComponentState::Failed);
            } else {
                self.set_state(if failed {
                    ComponentState::Failed
                } else {
                    ComponentState::Stopped
                });
            }
        }
        self.events.drain(..).collect()
    }
}
impl CompositorBackend for GstCompositor {
    fn configure_canvas(&mut self, canvas_id: CanvasId, video: VideoConfig) -> Result<()> {
        validate_video(&video)?;
        self.canvas = Some(canvas_id);
        if self.video == video {
            return Ok(());
        }
        self.video = video;
        self.rebuild()?;
        self.push(BackendEvent::RenegotiationRequired);
        Ok(())
    }
    fn sync_scene(&mut self, scene: &Scene) -> Result<()> {
        validate_scene(scene, &self.sources)?;
        let same = self
            .scenes
            .get(&scene.id)
            .is_some_and(|old| render_matches(old, scene, &self.sources, &self.sources));
        if !self.scenes.contains_key(&scene.id) && self.scenes.len() >= 256 {
            return Err(invalid("CPU prototype caches at most 256 scenes"));
        }
        self.scenes.insert(scene.id, scene.clone());
        if self.current == Some(scene.id) && !same {
            self.rebuild()?;
        }
        Ok(())
    }
    fn upsert_item(&mut self, scene_id: SceneId, item: &SceneItem) -> Result<()> {
        let mut scene = self
            .scenes
            .get(&scene_id)
            .cloned()
            .ok_or_else(|| Error::NotFound(format!("scene {scene_id}")))?;
        if let Some(existing) = scene.item_mut(item.id) {
            *existing = item.clone();
        } else {
            scene.add_item(item.clone());
        }
        self.sync_scene(&scene)
    }
    fn remove_item(&mut self, scene_id: SceneId, item_id: SceneItemId) -> Result<()> {
        let mut scene = self
            .scenes
            .get(&scene_id)
            .cloned()
            .ok_or_else(|| Error::NotFound(format!("scene {scene_id}")))?;
        scene.remove_item(item_id);
        self.sync_scene(&scene)
    }
    fn set_program_scene(&mut self, scene_id: SceneId) -> Result<()> {
        let scene = self
            .scenes
            .get(&scene_id)
            .ok_or_else(|| Error::NotFound(format!("scene {scene_id}")))?;
        validate_scene(scene, &self.sources)?;
        if self.current == Some(scene_id) {
            return Ok(());
        }
        self.current = Some(scene_id);
        self.rebuild()
    }
    fn start_transition(&mut self, transition: &Transition) -> Result<()> {
        if transition.kind != TransitionKind::Cut {
            self.push(BackendEvent::Warning {message:"CPU prototype transition falls back to cut; program scene selected by set_program_scene".into()});
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::mpsc::{self, Receiver},
        time::{Duration, Instant},
    };
    type Pixels = (usize, [u8; 4], [u8; 4]);
    fn setup() -> (GstCompositor, Receiver<Pixels>) {
        let runtime = GstRuntime::initialize().unwrap();
        let sink = runtime
            .require_factory("fakesink")
            .unwrap()
            .create()
            .property("sync", false)
            .build()
            .unwrap();
        let (tx, rx) = mpsc::sync_channel(8);
        sink.static_pad("sink")
            .unwrap()
            .add_probe(gst::PadProbeType::BUFFER, move |pad, info| {
                if let (Some(buffer), Some(caps)) = (info.buffer(), pad.current_caps()) {
                    let structure = caps.structure(0).unwrap();
                    let width = structure.get::<i32>("width").unwrap() as usize;
                    if let Ok(mapped) = buffer.map_readable() {
                        let bytes = mapped.as_slice();
                        let left = (4 * width + 4) * 4;
                        let right = (4 * width + 20) * 4;
                        if bytes.len() >= right + 4 {
                            let _ = tx.try_send((
                                bytes.len(),
                                bytes[left..left + 4].try_into().unwrap(),
                                bytes[right..right + 4].try_into().unwrap(),
                            ));
                        }
                    }
                }
                gst::PadProbeReturn::Ok
            });
        let mut compositor = GstCompositor::new(sink).unwrap();
        compositor
            .configure_canvas(
                CanvasId::new(),
                VideoConfig {
                    width: 32,
                    height: 16,
                    fps_num: 30,
                    fps_den: 1,
                },
            )
            .unwrap();
        (compositor, rx)
    }
    fn solid(pattern: &str) -> Source {
        let mut source = Source::new(SourceKind::TestPattern, pattern);
        source.settings = serde_json::json!({"width":32,"height":16,"pattern":pattern});
        source
    }
    fn wait_pixels(
        compositor: &mut GstCompositor,
        rx: &Receiver<Pixels>,
        left: [u8; 4],
        right: [u8; 4],
    ) {
        while rx.try_recv().is_ok() {}
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut matches = 0;
        loop {
            let events = compositor.drain_events();
            assert!(
                !events
                    .iter()
                    .any(|event| matches!(event, BackendEvent::Error { .. })),
                "{events:?}"
            );
            if let Ok((size, a, b)) = rx.recv_timeout(Duration::from_millis(50)) {
                if a == left && b == right {
                    assert_eq!(size, 32 * 16 * 4);
                    matches += 1;
                    if matches >= 2 {
                        return;
                    }
                } else {
                    matches = 0;
                }
            }
            assert!(
                Instant::now() < deadline,
                "did not observe expected pixels {left:?} {right:?}"
            );
        }
    }
    const RED: [u8; 4] = [255, 0, 0, 255];
    const BLUE: [u8; 4] = [0, 0, 255, 255];
    const BLACK: [u8; 4] = [0, 0, 0, 255];
    #[test]
    fn two_sources_visibility_zorder_removal_and_scene_switch_change_pixels() {
        let (mut compositor, rx) = setup();
        let red = solid("red");
        let blue = solid("blue");
        let mut scene = Scene::new("two");
        let a = SceneItem::new(red.id, -3);
        let mut b = SceneItem::new(blue.id, -2);
        b.transform.position.x = 16.0;
        b.transform.scale.x = 0.5;
        scene.add_item(a.clone());
        scene.add_item(b.clone());
        compositor
            .sync_snapshot(&[red.clone(), blue.clone()], &scene)
            .unwrap();
        compositor.start().unwrap();
        wait_pixels(&mut compositor, &rx, RED, BLUE);
        assert_eq!(compositor.source_count(), 2);
        assert_eq!(compositor.branch_count(), 2);
        b.visible = false;
        compositor.upsert_item(scene.id, &b).unwrap();
        wait_pixels(&mut compositor, &rx, RED, RED);
        b.visible = true;
        b.transform.position.x = 0.0;
        b.transform.scale.x = 1.0;
        compositor.upsert_item(scene.id, &b).unwrap();
        wait_pixels(&mut compositor, &rx, BLUE, BLUE);
        let mut top = a.clone();
        top.z_index = 5;
        compositor.upsert_item(scene.id, &top).unwrap();
        wait_pixels(&mut compositor, &rx, RED, RED);
        compositor.remove_item(scene.id, a.id).unwrap();
        wait_pixels(&mut compositor, &rx, BLUE, BLUE);
        assert_eq!(compositor.source_count(), 1);
        let mut other = Scene::new("other");
        other.add_item(a);
        compositor.sync_scene(&other).unwrap();
        compositor.set_program_scene(other.id).unwrap();
        wait_pixels(&mut compositor, &rx, RED, RED);
        for _ in 0..5 {
            compositor.stop().unwrap();
            assert_eq!(compositor.branch_count(), 0);
            assert_eq!(compositor.source_count(), 0);
            assert!(compositor.mixer.sink_pads().is_empty());
            compositor.start().unwrap();
            wait_pixels(&mut compositor, &rx, RED, RED);
        }
    }
    #[test]
    fn repeated_placements_share_source_and_empty_scene_produces_black() {
        let (mut compositor, rx) = setup();
        let red = solid("red");
        let mut scene = Scene::new("shared");
        scene.add_item(SceneItem::new(red.id, 0));
        scene.add_item(SceneItem::new(red.id, 0));
        compositor.sync_snapshot(&[red], &scene).unwrap();
        compositor.start().unwrap();
        wait_pixels(&mut compositor, &rx, RED, RED);
        assert_eq!(compositor.source_count(), 1);
        assert_eq!(compositor.branch_count(), 2);
        let tee = &compositor.shared.values().next().unwrap().tee;
        assert_eq!(tee.src_pads().len(), 2);
        let empty = Scene::new("empty");
        compositor.sync_snapshot(&[], &empty).unwrap();
        wait_pixels(&mut compositor, &rx, BLACK, BLACK);
        assert_eq!(compositor.source_count(), 0);
        assert!(compositor.mixer.sink_pads().is_empty());
    }
    #[test]
    fn bad_geometry_unknown_source_and_bad_canvas_leave_old_graph_intact() {
        let (mut compositor, rx) = setup();
        let red = solid("red");
        let mut scene = Scene::new("valid");
        scene.add_item(SceneItem::new(red.id, 0));
        compositor.sync_snapshot(&[red], &scene).unwrap();
        compositor.start().unwrap();
        wait_pixels(&mut compositor, &rx, RED, RED);
        let mut bad = scene.items[0].clone();
        bad.transform.rotation = 45.0;
        assert!(matches!(
            compositor.upsert_item(scene.id, &bad),
            Err(Error::InvalidInput(_))
        ));
        bad = scene.items[0].clone();
        bad.source_id = SourceId::new();
        assert!(matches!(
            compositor.upsert_item(scene.id, &bad),
            Err(Error::NotFound(_))
        ));
        let mut unsupported = compositor
            .sources
            .get(&scene.items[0].source_id)
            .unwrap()
            .clone();
        unsupported.kind = SourceKind::Color;
        assert!(matches!(
            compositor.sync_snapshot(&[unsupported], &scene),
            Err(Error::InvalidInput(_))
        ));
        assert!(compositor
            .configure_canvas(
                CanvasId::new(),
                VideoConfig {
                    width: 32,
                    height: 16,
                    fps_num: 30,
                    fps_den: 0
                }
            )
            .is_err());
        wait_pixels(&mut compositor, &rx, RED, RED);
    }
    #[test]
    fn disabled_sources_clear_scene_and_metadata_noops_are_observed() {
        let (mut compositor, rx) = setup();
        let mut red = solid("red");
        let mut scene = Scene::new("on");
        scene.add_item(SceneItem::new(red.id, 0));
        compositor
            .sync_snapshot(std::slice::from_ref(&red), &scene)
            .unwrap();
        compositor.start().unwrap();
        wait_pixels(&mut compositor, &rx, RED, RED);
        let tee = compositor.shared.get(&red.id).unwrap().tee.clone();
        let bin = compositor.shared.get(&red.id).unwrap().bin.clone();
        red.name = "renamed".into();
        scene.name = "renamed scene".into();
        scene.items[0].locked = true;
        compositor
            .sync_snapshot(std::slice::from_ref(&red), &scene)
            .unwrap();
        assert_eq!(compositor.shared.get(&red.id).unwrap().bin, bin);
        assert_eq!(compositor.shared.get(&red.id).unwrap().tee, tee);
        red.enabled = false;
        compositor
            .sync_snapshot(std::slice::from_ref(&red), &scene)
            .unwrap();
        wait_pixels(&mut compositor, &rx, BLACK, BLACK);
        assert_eq!(compositor.source_count(), 0);
        assert!(tee.src_pads().is_empty());
        red.enabled = true;
        compositor.sync_snapshot(&[red], &scene).unwrap();
        wait_pixels(&mut compositor, &rx, RED, RED);
        compositor.clear_scene().unwrap();
        wait_pixels(&mut compositor, &rx, BLACK, BLACK);
        assert!(compositor.current.is_none());
        compositor.stop().unwrap();
        assert_eq!(compositor.pipeline.children().len(), 3);
    }
    #[test]
    fn error_dominates_eos_in_both_terminal_orders() {
        for error_first in [true, false] {
            let (mut compositor, _) = setup();
            let error =
                gst::message::Error::builder(gst::LibraryError::Failed, "native fault").build();
            let eos = gst::message::Eos::new();
            let messages = if error_first {
                [error, eos]
            } else {
                [eos, error]
            };
            for message in messages {
                compositor.pipeline.post_message(message).unwrap();
            }
            let events = compositor.drain_events();
            assert!(events
                .iter()
                .any(|event| matches!(event, BackendEvent::Error { .. })));
            assert!(events.contains(&BackendEvent::EndOfStream));
            assert_eq!(compositor.state(), ComponentState::Failed);
            assert_eq!(compositor.pipeline.current_state(), gst::State::Null);
        }
    }
    #[test]
    fn canvas_change_renegotiates_actual_output_and_stop_releases_tee_pads() {
        let (mut compositor, rx) = setup();
        let red = solid("red");
        let mut scene = Scene::new("canvas");
        scene.add_item(SceneItem::new(red.id, 0));
        compositor
            .sync_snapshot(std::slice::from_ref(&red), &scene)
            .unwrap();
        compositor.start().unwrap();
        wait_pixels(&mut compositor, &rx, RED, RED);
        let tee = compositor.shared.get(&red.id).unwrap().tee.clone();
        compositor
            .configure_canvas(
                CanvasId::new(),
                VideoConfig {
                    width: 64,
                    height: 32,
                    fps_num: 30000,
                    fps_den: 1001,
                },
            )
            .unwrap();
        assert!(tee.src_pads().is_empty());
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok((size, _, _)) = rx.recv_timeout(Duration::from_millis(50)) {
                if size == 64 * 32 * 4 {
                    break;
                }
            }
            assert!(Instant::now() < deadline);
        }
        let caps = compositor
            .capsfilter
            .static_pad("src")
            .unwrap()
            .current_caps()
            .unwrap();
        assert_eq!(
            caps.structure(0)
                .unwrap()
                .get::<gst::Fraction>("framerate")
                .unwrap(),
            gst::Fraction::new(30000, 1001)
        );
        let tee = compositor.shared.get(&red.id).unwrap().tee.clone();
        compositor.stop().unwrap();
        compositor.stop().unwrap();
        assert!(tee.src_pads().is_empty());
        assert!(compositor.mixer.sink_pads().is_empty());
        assert_eq!(compositor.pipeline.children().len(), 3);
    }
}
