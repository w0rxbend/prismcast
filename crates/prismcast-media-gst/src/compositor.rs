//! CPU prototype: reconstruct topology behind a NULL barrier on scene changes.
//! Media owner thread only. A GTK adapter may provide a Send sink element, but
//! this module neither imports GTK nor accesses its paintable.
use crate::{build_test_pattern_bin, GstRuntime, TestPatternSettings};
use gstreamer::{self as gst, prelude::*};
use prismcast_compositor::{layout_item, CardinalRotation, SourceSize};
use prismcast_core::{
    BlendMode, CanvasId, Error, Result, Scene, SceneId, SceneItem, SceneItemId, Source, SourceId,
    SourceKind, Transition, TransitionKind, VideoConfig,
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
        if item.blend_mode != BlendMode::Normal {
            return Err(invalid(
                "CPU compositor supports normal alpha blending only",
            ));
        }
        layout_item(
            item,
            SourceSize {
                width: source.width,
                height: source.height,
            },
        )?;
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
    elements: Vec<gst::Element>,
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
    warned_rotation: HashSet<SceneItemId>,
    warned_scale: HashSet<SceneItemId>,
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
            warned_rotation: HashSet::new(),
            warned_scale: HashSet::new(),
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
            if let Some(sink) = branch
                .elements
                .first()
                .and_then(|element| element.static_pad("sink"))
            {
                let _ = branch.tee_pad.unlink(&sink);
            }
            if let Some(src) = branch
                .elements
                .last()
                .and_then(|element| element.static_pad("src"))
            {
                let _ = src.unlink(&branch.mixer_pad);
            }
            branch.tee.release_request_pad(&branch.tee_pad);
            self.mixer.release_request_pad(&branch.mixer_pad);
            self.pipeline
                .remove_many(branch.elements.iter())
                .map_err(media)?;
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
            self.warned_rotation.clear();
            self.warned_scale.clear();
            return Ok(());
        };
        self.warned_rotation.retain(|id| scene.item(*id).is_some());
        self.warned_scale.retain(|id| scene.item(*id).is_some());
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
        let layout = layout_item(
            item,
            SourceSize {
                width: settings.width,
                height: settings.height,
            },
        )?;
        let crop = self
            .runtime
            .require_factory("videocrop")
            .map_err(media)?
            .create()
            .property("left", layout.crop.left as i32)
            .property("right", layout.crop.right as i32)
            .property("top", layout.crop.top as i32)
            .property("bottom", layout.crop.bottom as i32)
            .build()
            .map_err(media)?;
        let mut elements = vec![queue.clone(), crop];
        let flip = match (layout.flip_x, layout.flip_y) {
            (true, true) => Some("180"),
            (true, false) => Some("horiz"),
            (false, true) => Some("vert"),
            _ => None,
        };
        let rotation = match layout.rotation {
            CardinalRotation::None => None,
            CardinalRotation::Clockwise => Some("90r"),
            CardinalRotation::HalfTurn => Some("180"),
            CardinalRotation::Counterclockwise => Some("90l"),
        };
        // Signed flips are in original source axes and precede rotation.
        for direction in [flip, rotation].into_iter().flatten() {
            let element = self
                .runtime
                .require_factory("videoflip")
                .map_err(media)?
                .create()
                .property_from_str("video-direction", direction)
                .build()
                .map_err(media)?;
            elements.push(element);
        }
        self.pipeline.add_many(elements.iter()).map_err(media)?;
        let Some(tee_pad) = tee.request_pad_simple("src_%u") else {
            self.pipeline.remove_many(elements.iter()).map_err(media)?;
            return Err(media("tee request pad unavailable"));
        };
        let Some(mixer_pad) = self.mixer.request_pad_simple("sink_%u") else {
            tee.release_request_pad(&tee_pad);
            self.pipeline.remove_many(elements.iter()).map_err(media)?;
            return Err(media("compositor request pad unavailable"));
        };
        // Register ownership before fallible links so failure cleanup releases pads.
        self.branches.push(Branch {
            elements: elements.clone(),
            tee,
            tee_pad: tee_pad.clone(),
            mixer_pad: mixer_pad.clone(),
        });
        gst::Element::link_many(elements.iter()).map_err(media)?;
        let sink = queue
            .static_pad("sink")
            .ok_or_else(|| media("queue sink missing"))?;
        let src = elements
            .last()
            .and_then(|element| element.static_pad("src"))
            .ok_or_else(|| media("placement src missing"))?;
        tee_pad.link(&sink).map_err(media)?;
        src.link(&mixer_pad).map_err(media)?;
        mixer_pad.set_property("xpos", layout.rect.x);
        mixer_pad.set_property("ypos", layout.rect.y);
        mixer_pad.set_property("width", layout.rect.width as i32);
        mixer_pad.set_property("height", layout.rect.height as i32);
        if layout.rotation_quantized && self.warned_rotation.insert(item.id) {
            tracing::warn!(item_id=%item.id,rotation=item.transform.rotation,"rotation quantized to nearest cardinal angle");
            self.push(BackendEvent::Warning {message:format!("Item {} rotation is quantized to the nearest90°; free rotation is unsupported.",item.id)});
        }
        if layout.scale_clamped && self.warned_scale.insert(item.id) {
            tracing::warn!(item_id=%item.id,"scale magnitudes clamped to64");
            self.push(BackendEvent::Warning {
                message: format!("Item {} scale magnitude is limited to64.", item.id),
            });
        }
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
        bad.transform.rotation = f32::NAN;
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

#[cfg(test)]
mod transform_pixel_tests {
    use super::*;
    use prismcast_core::{Anchor, Bounds, BoundsKind, Crop, Vec2};
    use std::{
        sync::mpsc::{self, Receiver},
        time::{Duration, Instant},
    };
    struct Frame {
        width: usize,
        height: usize,
        bytes: Vec<u8>,
    }
    impl Frame {
        fn pixel(&self, x: usize, y: usize) -> [u8; 4] {
            assert!(x < self.width && y < self.height);
            self.bytes[(y * self.width + x) * 4..(y * self.width + x) * 4 + 4]
                .try_into()
                .unwrap()
        }
    }
    fn setup() -> (GstCompositor, Receiver<Frame>, Source, SceneItem, Scene) {
        let runtime = GstRuntime::initialize().unwrap();
        let sink = runtime
            .require_factory("fakesink")
            .unwrap()
            .create()
            .property("sync", false)
            .build()
            .unwrap();
        let (tx, rx) = mpsc::sync_channel(2);
        sink.static_pad("sink")
            .unwrap()
            .add_probe(gst::PadProbeType::BUFFER, move |pad, info| {
                if let (Some(buffer), Some(caps)) = (info.buffer(), pad.current_caps()) {
                    let s = caps.structure(0).unwrap();
                    let width = s.get::<i32>("width").unwrap() as usize;
                    let height = s.get::<i32>("height").unwrap() as usize;
                    if let Ok(map) = buffer.map_readable() {
                        let _ = tx.try_send(Frame {
                            width,
                            height,
                            bytes: map.as_slice().to_vec(),
                        });
                    }
                }
                gst::PadProbeReturn::Ok
            });
        let mut compositor = GstCompositor::new(sink).unwrap();
        compositor
            .configure_canvas(
                CanvasId::new(),
                VideoConfig {
                    width: 96,
                    height: 96,
                    fps_num: 30,
                    fps_den: 1,
                },
            )
            .unwrap();
        let mut source = Source::new(SourceKind::TestPattern, "asymmetric SMPTE");
        source.settings = serde_json::json!({"width":64,"height":48,"pattern":"smpte"});
        let item = SceneItem::new(source.id, 0);
        let mut scene = Scene::new("transform");
        scene.add_item(item.clone());
        compositor
            .sync_snapshot(std::slice::from_ref(&source), &scene)
            .unwrap();
        compositor.start().unwrap();
        (compositor, rx, source, item, scene)
    }
    fn wait_frame(
        compositor: &mut GstCompositor,
        rx: &Receiver<Frame>,
        matches: impl Fn(&Frame) -> bool,
    ) -> Frame {
        while rx.try_recv().is_ok() {}
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut consecutive = 0;
        loop {
            let events = compositor.drain_events();
            assert!(
                !events
                    .iter()
                    .any(|e| matches!(e, BackendEvent::Error { .. })),
                "{events:?}"
            );
            if let Ok(frame) = rx.recv_timeout(Duration::from_millis(100)) {
                if matches(&frame) {
                    consecutive += 1;
                    if consecutive >= 2 {
                        return frame;
                    }
                } else {
                    consecutive = 0;
                }
            }
            assert!(
                Instant::now() < deadline,
                "transformed pixels did not arrive"
            );
        }
    }
    #[test]
    fn asymmetric_crop_all_cardinal_rotations_and_signed_flips_match_source_pixels() {
        let (mut compositor, rx, _, mut item, scene) = setup();
        let reference = wait_frame(&mut compositor, &rx, |frame| {
            frame.pixel(4, 8) != frame.pixel(56, 8)
        });
        // Upper SMPTE region is static but horizontally asymmetric. Vertical
        // reflection moves these static samples into a different image region.
        let points = [(14_usize, 20_usize), (24, 12), (40, 8), (56, 24)];
        for rotation in [0.0, 90.0, 180.0, 270.0] {
            for (flip_x, flip_y) in [(false, false), (true, false), (false, true), (true, true)] {
                item.crop = Crop {
                    left: 10,
                    right: 6,
                    top: 4,
                    bottom: 8,
                };
                item.transform.rotation = rotation;
                item.transform.scale = Vec2::new(
                    if flip_x { -1.0 } else { 1.0 },
                    if flip_y { -1.0 } else { 1.0 },
                );
                item.transform.position = Vec2::new(48.0, 48.0);
                item.transform.anchor = Anchor::Center;
                compositor.upsert_item(scene.id, &item).unwrap();
                let rect = layout_item(
                    &item,
                    SourceSize {
                        width: 64,
                        height: 48,
                    },
                )
                .unwrap()
                .rect;
                let samples: Vec<_> = points
                    .iter()
                    .map(|&(x, y)| {
                        let mut x = x - 10;
                        let mut y = y - 4;
                        if flip_x {
                            x = 47 - x;
                        }
                        if flip_y {
                            y = 35 - y;
                        }
                        let (x, y) = match rotation as u32 {
                            0 => (x, y),
                            90 => (35 - y, x),
                            180 => (47 - x, 35 - y),
                            _ => (y, 47 - x),
                        };
                        (rect.x as usize + x, rect.y as usize + y)
                    })
                    .collect();
                // Compute expectations independently from orientation elements:
                // only the original source coordinates select colors.
                let samples: Vec<_> = samples
                    .into_iter()
                    .zip(points)
                    .map(|((x, y), (sx, sy))| (x, y, reference.pixel(sx, sy)))
                    .collect();
                wait_frame(&mut compositor, &rx, |frame| {
                    samples
                        .iter()
                        .all(|&(x, y, color)| frame.pixel(x, y) == color)
                });
                assert_eq!(compositor.source_count(), 1);
            }
        }
        // Combined90° + horizontal source flip + unequal canvas scales must
        // agree with documented orientation-before-sizing semantics.
        item.transform.rotation = 90.0;
        item.transform.scale = Vec2::new(-0.5, 1.5);
        compositor.upsert_item(scene.id, &item).unwrap();
        let rect = layout_item(
            &item,
            SourceSize {
                width: 64,
                height: 48,
            },
        )
        .unwrap()
        .rect;
        assert_eq!((rect.x, rect.y, rect.width, rect.height), (39, 12, 18, 72));
        let samples: Vec<_> = points
            .into_iter()
            .map(|(sx, sy)| {
                let rotated_x = 35 - (sy - 4);
                let rotated_y = 47 - (sx - 10);
                let x = rect.x as usize + ((rotated_x as f32 + 0.5) * 0.5).floor() as usize;
                let y = rect.y as usize + ((rotated_y as f32 + 0.5) * 1.5).floor() as usize;
                (x, y, reference.pixel(sx, sy))
            })
            .collect();
        wait_frame(&mut compositor, &rx, |frame| {
            samples
                .iter()
                .all(|&(x, y, color)| frame.pixel(x, y) == color)
        });
        compositor.stop().unwrap();
        assert_eq!(compositor.pipeline.children().len(), 3);
        assert!(compositor.mixer.sink_pads().is_empty());
    }

    #[test]
    fn anchors_bounds_and_clamped_crop_have_actual_native_extents() {
        let (mut compositor, rx, mut source, mut item, scene) = setup();
        source.settings = serde_json::json!({"width":64,"height":48,"pattern":"red"});
        let mut updated = scene.clone();
        updated.items[0] = item.clone();
        compositor.sync_snapshot(&[source], &updated).unwrap();
        item.transform.position = Vec2::new(48.0, 48.0);
        item.transform.anchor = Anchor::BottomRight;
        item.bounds = Bounds {
            kind: BoundsKind::FitInner,
            size: Vec2::new(32.0, 32.0),
            alignment: Anchor::Center,
        };
        compositor.upsert_item(scene.id, &item).unwrap();
        // Bounds top-left(16,16), 4:3 image32x24 centered vertically at y20.
        let red = [255, 0, 0, 255];
        let black = [0, 0, 0, 255];
        wait_frame(&mut compositor, &rx, |f| {
            f.pixel(16, 20) == red
                && f.pixel(47, 43) == red
                && f.pixel(16, 19) == black
                && f.pixel(48, 20) == black
        });
        item.bounds.kind = BoundsKind::FitOuter;
        compositor.upsert_item(scene.id, &item).unwrap();
        // 4:3 outer fit draws43x32 at roundedx11 with horizontal overflow.
        wait_frame(&mut compositor, &rx, |f| {
            f.pixel(11, 16) == red
                && f.pixel(53, 47) == red
                && f.pixel(10, 16) == black
                && f.pixel(54, 16) == black
        });
        item.bounds.kind = BoundsKind::Stretch;
        compositor.upsert_item(scene.id, &item).unwrap();
        wait_frame(&mut compositor, &rx, |f| {
            f.pixel(16, 16) == red && f.pixel(47, 47) == red && f.pixel(15, 16) == black
        });
        item.bounds.kind = BoundsKind::None;
        item.transform.anchor = Anchor::Center;
        item.crop = Crop {
            left: u32::MAX,
            right: u32::MAX,
            top: u32::MAX,
            bottom: u32::MAX,
        };
        compositor.upsert_item(scene.id, &item).unwrap();
        wait_frame(&mut compositor, &rx, |f| {
            f.pixel(48, 48) == red && f.pixel(47, 48) == black && f.pixel(49, 48) == black
        });
        item.crop = Crop::default();
        item.transform.scale = Vec2::new(0.0, 0.0);
        compositor.upsert_item(scene.id, &item).unwrap();
        wait_frame(&mut compositor, &rx, |f| {
            f.pixel(48, 48) == red && f.pixel(47, 48) == black && f.pixel(49, 48) == black
        });
    }
    #[test]
    fn quantized_rotation_warns_once_and_nonfinite_update_preserves_graph() {
        let (mut compositor, rx, _, mut item, scene) = setup();
        item.transform.rotation = 44.0;
        compositor.upsert_item(scene.id, &item).unwrap();
        assert_eq!(
            compositor
                .drain_events()
                .iter()
                .filter(|e| matches!(e, BackendEvent::Warning { .. }))
                .count(),
            1
        );
        item.transform.position.x = 2.0;
        compositor.upsert_item(scene.id, &item).unwrap();
        assert!(!compositor
            .drain_events()
            .iter()
            .any(|e| matches!(e, BackendEvent::Warning { .. })));
        let bin = compositor.shared.get(&item.source_id).unwrap().bin.clone();
        item.transform.rotation = f32::INFINITY;
        assert!(compositor.upsert_item(scene.id, &item).is_err());
        assert_eq!(compositor.shared.get(&item.source_id).unwrap().bin, bin);
        wait_frame(&mut compositor, &rx, |f| f.pixel(4, 4) != [0, 0, 0, 255]);
    }
}
