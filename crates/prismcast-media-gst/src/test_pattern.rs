//! Test-pattern graph construction and standalone lifecycle.
//! All graph methods run on the media owner thread. The reusable bin has one
//! `src` ghost pad; a compositor can construct it once per SourceId and fan out
//! through its shared-source tee registry instead of starting standalone graphs.
use crate::GstRuntime;
use gstreamer::{self as gst, prelude::*};
use prismcast_core::{Error, Result, SourceId, SourceKind};
use prismcast_media::{AudioLevels, BackendComponent, BackendEvent, ComponentState, SourceBackend};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::VecDeque,
    sync::mpsc::{self, Receiver},
};

const MAX_EVENTS: usize = 32;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Pattern {
    #[default]
    Smpte,
    Black,
    White,
    Red,
    Green,
    Blue,
    Ball,
}
impl Pattern {
    fn nick(self) -> &'static str {
        match self {
            Self::Smpte => "smpte",
            Self::Black => "black",
            Self::White => "white",
            Self::Red => "red",
            Self::Green => "green",
            Self::Blue => "blue",
            Self::Ball => "ball",
        }
    }
}

/// JSON settings accept `{}` with deterministic defaults; unknown fields fail.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TestPatternSettings {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub pattern: Pattern,
    /// None streams indefinitely; finite counts support diagnostics/tests.
    pub num_buffers: Option<u32>,
}
impl Default for TestPatternSettings {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            fps: 30,
            pattern: Pattern::Smpte,
            num_buffers: None,
        }
    }
}
impl TestPatternSettings {
    pub fn from_json(value: Value) -> Result<Self> {
        let settings: Self = serde_json::from_value(value)
            .map_err(|error| Error::InvalidInput(format!("test pattern settings: {error}")))?;
        settings.validate()?;
        Ok(settings)
    }
    pub fn validate(&self) -> Result<()> {
        if !(1..=8192).contains(&self.width)
            || !(1..=8192).contains(&self.height)
            || !(1..=240).contains(&self.fps)
            || self
                .num_buffers
                .is_some_and(|count| count == 0 || count > i32::MAX as u32)
        {
            return Err(Error::InvalidInput("test pattern needs dimensions 1..8192, fps 1..240, and positive signed-32-bit buffer count".into()));
        }
        Ok(())
    }
    pub fn schema() -> Value {
        json!({"$schema":"https://json-schema.org/draft/2020-12/schema", "type":"object",
        "additionalProperties":false, "properties": {
            "width":{"type":"integer","minimum":1,"maximum":8192,"default":1920},
            "height":{"type":"integer","minimum":1,"maximum":8192,"default":1080},
            "fps":{"type":"integer","minimum":1,"maximum":240,"default":30},
            "pattern":{"type":"string","enum":["smpte","black","white","red","green","blue","ball"],"default":"smpte"},
            "num_buffers":{"type":["integer","null"],"minimum":1,"maximum":2147483647,"default":null}
        }})
    }
}
fn media(error: impl std::fmt::Display) -> Error {
    Error::Media(error.to_string())
}

/// Build a stopped, unattached live RGBA source bin for the native media graph.
/// Settings are validated before any native object is created.
pub fn build_test_pattern_bin(
    runtime: &GstRuntime,
    id: SourceId,
    settings: &TestPatternSettings,
) -> Result<gst::Bin> {
    settings.validate()?;
    let source = runtime
        .require_factory("videotestsrc")
        .map_err(media)?
        .create()
        .property("is-live", true)
        .property_from_str("pattern", settings.pattern.nick())
        .property("num-buffers", settings.num_buffers.map_or(-1, |n| n as i32))
        .build()
        .map_err(media)?;
    let caps = gst::Caps::builder("video/x-raw")
        .field("format", "RGBA")
        .field("width", settings.width as i32)
        .field("height", settings.height as i32)
        .field("framerate", gst::Fraction::new(settings.fps as i32, 1))
        .field("pixel-aspect-ratio", gst::Fraction::new(1, 1))
        .build();
    let filter = runtime
        .require_factory("capsfilter")
        .map_err(media)?
        .create()
        .property("caps", caps)
        .build()
        .map_err(media)?;
    let bin = gst::Bin::builder().name(format!("source-{id}")).build();
    bin.add_many([&source, &filter]).map_err(media)?;
    source.link(&filter).map_err(media)?;
    let pad = filter
        .static_pad("src")
        .ok_or_else(|| media("capsfilter has no src pad"))?;
    let ghost = gst::GhostPad::builder_with_target(&pad)
        .map_err(media)?
        .name("src")
        .build();
    bin.add_pad(&ghost).map_err(media)?;
    Ok(bin)
}

struct Graph {
    pipeline: gst::Pipeline,
    terminal: Receiver<BackendEvent>,
}
impl Graph {
    fn new(runtime: &GstRuntime, id: SourceId, settings: &TestPatternSettings) -> Result<Self> {
        let bin = build_test_pattern_bin(runtime, id, settings)?;
        let sink = runtime
            .require_factory("fakesink")
            .map_err(media)?
            .create()
            .property("sync", false)
            .build()
            .map_err(media)?;
        let pipeline = gst::Pipeline::new();
        pipeline
            .add_many([bin.upcast_ref::<gst::Element>(), &sink])
            .map_err(media)?;
        bin.link(&sink).map_err(media)?;
        let bus = pipeline
            .bus()
            .ok_or_else(|| media("pipeline bus unavailable"))?;
        let (sender, terminal) = mpsc::sync_channel(8);
        // Drop every bus message after inspection: no unbounded native bus queue.
        // Only terminal messages cross this bounded channel; no graph/UI mutation here.
        bus.set_sync_handler(move |_, message| {
            let event = match message.view() {
                gst::MessageView::Eos(_) => Some(BackendEvent::EndOfStream),
                gst::MessageView::Error(error) => Some(BackendEvent::Error {
                    message: error.error().to_string(),
                }),
                _ => None,
            };
            if let Some(event) = event {
                let _ = sender.try_send(event);
            }
            gst::BusSyncReply::Drop
        });
        Ok(Self { pipeline, terminal })
    }
    fn stop(&self) -> Result<()> {
        self.pipeline.set_state(gst::State::Null).map_err(media)?;
        Ok(())
    }
}
impl Drop for Graph {
    fn drop(&mut self) {
        if let Err(error) = self.stop() {
            tracing::error!(%error, "test-pattern graph cleanup failed");
        }
        if let Some(bus) = self.pipeline.bus() {
            bus.unset_sync_handler();
        }
    }
}

/// Standalone source implementation with a headless drain sink. The compositor
/// uses `build_test_pattern_bin` for sources participating in its shared graph.
pub struct GstTestPatternSource {
    id: SourceId,
    runtime: GstRuntime,
    settings: TestPatternSettings,
    graph: Graph,
    state: ComponentState,
    events: VecDeque<BackendEvent>,
}
impl GstTestPatternSource {
    pub fn new(id: SourceId, settings: Value) -> Result<Self> {
        let settings = TestPatternSettings::from_json(settings)?;
        let runtime = GstRuntime::initialize().map_err(media)?;
        let graph = Graph::new(&runtime, id, &settings)?;
        Ok(Self {
            id,
            runtime,
            settings,
            graph,
            state: ComponentState::Stopped,
            events: VecDeque::new(),
        })
    }
    pub fn settings(&self) -> &TestPatternSettings {
        &self.settings
    }
    fn push(&mut self, event: BackendEvent) {
        if self.events.len() == MAX_EVENTS {
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
}
impl BackendComponent for GstTestPatternSource {
    fn state(&self) -> ComponentState {
        self.state
    }
    fn drain_events(&mut self) -> Vec<BackendEvent> {
        while let Ok(event) = self.graph.terminal.try_recv() {
            let next = match &event {
                BackendEvent::Error { .. } => ComponentState::Failed,
                _ if self.state == ComponentState::Failed => ComponentState::Failed,
                _ => ComponentState::Stopped,
            };
            if let Err(error) = self.graph.stop() {
                self.push(BackendEvent::Error {
                    message: error.to_string(),
                });
                self.set_state(ComponentState::Failed);
            } else {
                self.set_state(next);
            }
            self.push(event);
        }
        self.events.drain(..).collect()
    }
}
impl SourceBackend for GstTestPatternSource {
    fn source_id(&self) -> SourceId {
        self.id
    }
    fn kind(&self) -> SourceKind {
        SourceKind::TestPattern
    }
    fn settings_schema(&self) -> Value {
        TestPatternSettings::schema()
    }
    fn start(&mut self) -> Result<()> {
        if self.state == ComponentState::Running {
            return Ok(());
        }
        // Retire terminal messages from the previous run before restarting.
        while self.graph.terminal.try_recv().is_ok() {}
        if let Err(error) = self.graph.pipeline.set_state(gst::State::Playing) {
            self.push(BackendEvent::Error {
                message: error.to_string(),
            });
            self.set_state(ComponentState::Failed);
            self.graph.stop()?;
            return Err(media(error));
        }
        tracing::debug!(source_id = %self.id, "test pattern started");
        self.set_state(ComponentState::Running);
        Ok(())
    }
    fn stop(&mut self) -> Result<()> {
        if let Err(error) = self.graph.stop() {
            self.push(BackendEvent::Error {
                message: error.to_string(),
            });
            self.set_state(ComponentState::Failed);
            return Err(error);
        }
        self.set_state(ComponentState::Stopped);
        Ok(())
    }
    fn update_settings(&mut self, value: Value) -> Result<()> {
        let settings = TestPatternSettings::from_json(value)?;
        if settings == self.settings {
            return Ok(());
        }
        let candidate = Graph::new(&self.runtime, self.id, &settings)?;
        let running = self.state == ComponentState::Running;
        if let Err(error) = self.graph.stop() {
            self.push(BackendEvent::Error {
                message: error.to_string(),
            });
            self.set_state(ComponentState::Failed);
            return Err(error);
        }
        if running {
            if let Err(error) = candidate.pipeline.set_state(gst::State::Playing) {
                // Original settings/graph remain owned until replacement succeeds.
                if let Err(rollback) = self.graph.pipeline.set_state(gst::State::Playing) {
                    let failure = media(format!(
                        "replacement failed: {error}; original graph restart failed: {rollback}"
                    ));
                    self.push(BackendEvent::Error {
                        message: failure.to_string(),
                    });
                    self.set_state(ComponentState::Failed);
                    if let Err(cleanup) = self.graph.stop() {
                        tracing::error!(source_id = %self.id, %cleanup, "rollback cleanup failed");
                    }
                    return Err(failure);
                }
                return Err(media(error));
            }
        }
        self.graph = candidate;
        self.settings = settings;
        self.push(BackendEvent::RenegotiationRequired);
        Ok(())
    }
    fn audio_levels(&self) -> Option<AudioLevels> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
        time::{Duration, Instant},
    };

    fn finite_source() -> GstTestPatternSource {
        GstTestPatternSource::new(
            SourceId::new(),
            json!({"width":64,"height":48,"fps":240,"num_buffers":8}),
        )
        .unwrap()
    }
    fn wait_terminal(source: &mut GstTestPatternSource) -> Vec<BackendEvent> {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut observed = Vec::new();
        loop {
            let events = source.drain_events();
            let terminal = events.iter().any(|event| {
                matches!(
                    event,
                    BackendEvent::EndOfStream | BackendEvent::Error { .. }
                )
            });
            observed.extend(events);
            if terminal {
                return observed;
            }
            assert!(Instant::now() < deadline, "source deadline");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    #[test]
    fn source_streams_negotiated_buffers_and_emits_eos() {
        let mut source = finite_source();
        let bin = source
            .graph
            .pipeline
            .by_name(&format!("source-{}", source.id))
            .unwrap();
        let count = Arc::new(AtomicUsize::new(0));
        let observed = count.clone();
        let pad = bin.static_pad("src").unwrap();
        pad.add_probe(gst::PadProbeType::BUFFER, move |_, info| {
            let buffer = info.buffer().unwrap();
            assert_eq!(buffer.size(), 64 * 48 * 4);
            assert!(buffer.pts().is_some());
            observed.fetch_add(1, Ordering::Relaxed);
            gst::PadProbeReturn::Ok
        });
        source.start().unwrap();
        source.start().unwrap();
        let events = wait_terminal(&mut source);
        assert_eq!(count.load(Ordering::Relaxed), 8);
        assert!(events.contains(&BackendEvent::EndOfStream));
        assert_eq!(source.state(), ComponentState::Stopped);
        assert_eq!(source.graph.pipeline.current_state(), gst::State::Null);
        source.stop().unwrap();
        source.stop().unwrap();
        assert!(source.drain_events().is_empty());
        // Finite source can restart after EOS and generates another full run.
        source.start().unwrap();
        assert!(wait_terminal(&mut source).contains(&BackendEvent::EndOfStream));
        assert_eq!(count.load(Ordering::Relaxed), 16);
    }
    #[test]
    fn malformed_settings_never_modify_running_source() {
        let mut source =
            GstTestPatternSource::new(SourceId::new(), json!({"width":64,"height":48})).unwrap();
        source.start().unwrap();
        let before = source.settings().clone();
        for invalid in [
            json!({"width":0}),
            json!({"height":8193}),
            json!({"fps":0}),
            json!({"pattern":"nonsense"}),
            json!({"num_buffers":0}),
            json!({"num_buffers":2147483648_u64}),
            json!({"unknown":true}),
            Value::Null,
        ] {
            assert!(matches!(
                source.update_settings(invalid),
                Err(Error::InvalidInput(_))
            ));
            assert_eq!(source.settings(), &before);
            assert_eq!(source.state(), ComponentState::Running);
        }
        source.stop().unwrap();
    }
    #[test]
    fn settings_replace_graph_and_preserve_identity() {
        let mut source = finite_source();
        let id = source.source_id();
        source.start().unwrap();
        source
            .update_settings(
                json!({"width":32,"height":24,"fps":240,"pattern":"red","num_buffers":2}),
            )
            .unwrap();
        assert_eq!(source.source_id(), id);
        assert_eq!(source.settings().width, 32);
        let events = wait_terminal(&mut source);
        assert!(events.contains(&BackendEvent::RenegotiationRequired));
        assert!(events.contains(&BackendEvent::EndOfStream));
    }
    #[test]
    fn bus_error_is_observed_and_stops_graph() {
        let mut source = finite_source();
        source.start().unwrap();
        source
            .graph
            .pipeline
            .post_message(
                gst::message::Error::builder(gst::LibraryError::Failed, "injected native error")
                    .build(),
            )
            .unwrap();
        source
            .graph
            .pipeline
            .post_message(gst::message::Eos::new())
            .unwrap();
        let events = wait_terminal(&mut source);
        assert!(events.iter().any(|event| matches!(event, BackendEvent::Error {message} if message.contains("injected native error"))));
        assert_eq!(source.state(), ComponentState::Failed);
        assert_eq!(source.graph.pipeline.current_state(), gst::State::Null);
    }
    #[test]
    fn control_event_accumulation_is_bounded() {
        let mut source = finite_source();
        for _ in 0..100 {
            source.start().unwrap();
            source.stop().unwrap();
        }
        assert!(source.events.len() <= MAX_EVENTS);
        assert!(source.drain_events().len() <= MAX_EVENTS);
    }
    #[test]
    fn defaults_roundtrip_and_schema_matches_limits() {
        assert_eq!(
            TestPatternSettings::from_json(json!({})).unwrap(),
            TestPatternSettings::default()
        );
        let settings = TestPatternSettings::default();
        assert_eq!(
            TestPatternSettings::from_json(serde_json::to_value(&settings).unwrap()).unwrap(),
            settings
        );
        assert_eq!(
            TestPatternSettings::schema()["properties"]["fps"]["maximum"],
            240
        );
    }
}
