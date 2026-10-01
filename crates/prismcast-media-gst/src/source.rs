//! [`SourceBackend`] implementations backed by GStreamer pipelines.
//!
//! [`GstSourceBackend`] implements `SourceKind::TestPattern` via
//! `videotestsrc ! capsfilter ! fakesink`, one pipeline per source instance,
//! with deterministic output caps derived from validated settings.
//!
//! # Threading contract (PLAN.md §57, crate-level docs)
//!
//! All methods run on the media control actor's thread. GStreamer's streaming
//! threads are owned by the pipeline itself; the only extra thread is one bus
//! watch thread per started instance, which translates bus messages into
//! [`BackendEvent`]s and is always joined on stop, rebuild, and drop. No
//! blocking graph work happens on GTK or Tokio threads.

use std::{
    sync::{Arc, Mutex},
    thread::JoinHandle,
};

use gstreamer::{self as gst, prelude::*};
use serde::{Deserialize, Serialize};

use prismcast_core::{Error, Result, SourceId, SourceKind};
use prismcast_media::{AudioLevels, BackendComponent, BackendEvent, ComponentState, SourceBackend};

/// Inclusive bounds for validated test-pattern geometry (ADR-0011 diagnostics).
const MIN_DIMENSION: u32 = 16;
/// Maximum frame dimension accepted by the test-pattern backend.
const MAX_DIMENSION: u32 = 7680;
/// Maximum frame rate accepted by the test-pattern backend.
const MAX_FPS: u32 = 240;

/// Test pattern selection, mirroring `videotestsrc`'s `pattern` enum.
///
/// The kebab-case serde representation is exactly the GStreamer enum nick, so
/// persisted settings and element properties use one vocabulary.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TestPatternKind {
    /// SMPTE 100% color bars.
    #[default]
    Smpte,
    /// Random (television snow).
    Snow,
    /// 100% black.
    Black,
    /// 100% white.
    White,
    /// Solid red.
    Red,
    /// Solid green.
    Green,
    /// Solid blue.
    Blue,
    /// 1px checkers.
    #[serde(rename = "checkers-1")]
    Checkers1,
    /// 2px checkers.
    #[serde(rename = "checkers-2")]
    Checkers2,
    /// 4px checkers.
    #[serde(rename = "checkers-4")]
    Checkers4,
    /// 8px checkers.
    #[serde(rename = "checkers-8")]
    Checkers8,
    /// Circular pattern.
    Circular,
    /// Blinking pattern.
    Blink,
    /// SMPTE 75% color bars.
    Smpte75,
    /// Zone plate.
    ZonePlate,
    /// Gamut checkers.
    Gamut,
    /// Chroma zone plate.
    ChromaZonePlate,
    /// Solid color.
    SolidColor,
    /// Moving ball.
    Ball,
    /// SMPTE 100% color bars (alternate generator).
    Smpte100,
    /// Bar.
    Bar,
    /// Pinwheel.
    Pinwheel,
    /// Spokes.
    Spokes,
    /// Gradient.
    Gradient,
    /// Colors.
    Colors,
    /// SMPTE RP 219 test pattern.
    #[serde(rename = "smpte-rp-219")]
    SmpteRp219,
}

impl TestPatternKind {
    /// The `videotestsrc` `pattern` enum nick for this kind.
    pub fn nick(self) -> &'static str {
        match self {
            Self::Smpte => "smpte",
            Self::Snow => "snow",
            Self::Black => "black",
            Self::White => "white",
            Self::Red => "red",
            Self::Green => "green",
            Self::Blue => "blue",
            Self::Checkers1 => "checkers-1",
            Self::Checkers2 => "checkers-2",
            Self::Checkers4 => "checkers-4",
            Self::Checkers8 => "checkers-8",
            Self::Circular => "circular",
            Self::Blink => "blink",
            Self::Smpte75 => "smpte75",
            Self::ZonePlate => "zone-plate",
            Self::Gamut => "gamut",
            Self::ChromaZonePlate => "chroma-zone-plate",
            Self::SolidColor => "solid-color",
            Self::Ball => "ball",
            Self::Smpte100 => "smpte100",
            Self::Bar => "bar",
            Self::Pinwheel => "pinwheel",
            Self::Spokes => "spokes",
            Self::Gradient => "gradient",
            Self::Colors => "colors",
            Self::SmpteRp219 => "smpte-rp-219",
        }
    }

    /// All kinds, for schema generation and tests.
    pub const ALL: &'static [Self] = &[
        Self::Smpte,
        Self::Snow,
        Self::Black,
        Self::White,
        Self::Red,
        Self::Green,
        Self::Blue,
        Self::Checkers1,
        Self::Checkers2,
        Self::Checkers4,
        Self::Checkers8,
        Self::Circular,
        Self::Blink,
        Self::Smpte75,
        Self::ZonePlate,
        Self::Gamut,
        Self::ChromaZonePlate,
        Self::SolidColor,
        Self::Ball,
        Self::Smpte100,
        Self::Bar,
        Self::Pinwheel,
        Self::Spokes,
        Self::Gradient,
        Self::Colors,
        Self::SmpteRp219,
    ];
}

/// Validated settings for a `SourceKind::TestPattern` source.
///
/// Deserializing is strict (`deny_unknown_fields`): unknown keys are rejected
/// rather than silently discarded. Validation happens in full before any of it
/// is applied, so a bad payload never leaves a partially configured backend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TestPatternSettings {
    /// Which pattern `videotestsrc` renders.
    pub pattern: TestPatternKind,
    /// Frame width in pixels (even, 16..=7680; I420 requires even geometry).
    pub width: u32,
    /// Frame height in pixels (even, 16..=7680).
    pub height: u32,
    /// Frames per second (1..=240).
    pub fps: u32,
    /// Diagnostic: stop after this many buffers (0 = unlimited). Maps to
    /// `videotestsrc`'s `num-buffers`; used by headless tests to get finite
    /// streams and EOS.
    pub num_buffers: u32,
    /// Diagnostic: inject a streaming error after this many buffers (via an
    /// internal `identity error-after` element). Used by headless tests to
    /// exercise the error path deterministically.
    pub error_after: Option<u32>,
}

impl Default for TestPatternSettings {
    fn default() -> Self {
        Self {
            pattern: TestPatternKind::default(),
            width: 1280,
            height: 720,
            fps: 30,
            num_buffers: 0,
            error_after: None,
        }
    }
}

impl TestPatternSettings {
    /// Parses and fully validates a settings payload. `null` means defaults
    /// (matching a freshly created domain `Source`). Returns
    /// [`Error::InvalidInput`] on any mismatch without touching anything.
    pub fn from_json(value: &serde_json::Value) -> Result<Self> {
        if value.is_null() {
            return Ok(Self::default());
        }
        // serde would otherwise accept a sequence as positional struct fields.
        if !value.is_object() {
            return Err(Error::InvalidInput(format!(
                "test-pattern settings must be a JSON object, got {value}"
            )));
        }
        let settings: Self = serde_json::from_value(value.clone()).map_err(|err| {
            Error::InvalidInput(format!("invalid test-pattern settings: {err}"))
        })?;
        settings.validate()?;
        Ok(settings)
    }

    /// Checks geometry/rate bounds after structural deserialization.
    pub fn validate(&self) -> Result<()> {
        for (field, value) in [("width", self.width), ("height", self.height)] {
            if !(MIN_DIMENSION..=MAX_DIMENSION).contains(&value) {
                return Err(Error::InvalidInput(format!(
                    "test-pattern {field} must be within {MIN_DIMENSION}..={MAX_DIMENSION}, got {value}"
                )));
            }
            if value % 2 != 0 {
                return Err(Error::InvalidInput(format!(
                    "test-pattern {field} must be even (I420 subsampling), got {value}"
                )));
            }
        }
        if !(1..=MAX_FPS).contains(&self.fps) {
            return Err(Error::InvalidInput(format!(
                "test-pattern fps must be within 1..={MAX_FPS}, got {}",
                self.fps
            )));
        }
        if self.num_buffers > i32::MAX as u32 {
            return Err(Error::InvalidInput(format!(
                "test-pattern num_buffers exceeds {}: {}",
                i32::MAX,
                self.num_buffers
            )));
        }
        if let Some(error_after) = self.error_after {
            if error_after == 0 || error_after > i32::MAX as u32 {
                return Err(Error::InvalidInput(format!(
                    "test-pattern error_after must be within 1..={}, got {error_after}",
                    i32::MAX
                )));
            }
        }
        Ok(())
    }
}

/// Interior state shared with the bus watch thread. One mutex keeps the
/// state/events ordering consistent (a transition always lands in the queue
/// together with its `StateChanged` event).
#[derive(Debug, Default)]
struct Shared {
    inner: Mutex<SharedInner>,
}

#[derive(Debug, Default)]
struct SharedInner {
    state: ComponentState,
    events: Vec<BackendEvent>,
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, SharedInner> {
        self.inner.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn state(&self) -> ComponentState {
        self.lock().state
    }

    fn push(&self, event: BackendEvent) {
        self.lock().events.push(event);
    }

    fn transition(&self, state: ComponentState) {
        let mut inner = self.lock();
        if inner.state != state {
            inner.state = state;
            inner.events.push(BackendEvent::StateChanged { state });
        }
    }
}

/// Watches a pipeline bus and translates terminal messages into backend events
/// and state transitions. Exits on terminal message or when `shutdown` is set
/// (checked at `POLL` cadence so `join` is bounded).
fn watch_bus(pipeline: gst::Pipeline, shared: Arc<Shared>, shutdown: Arc<std::sync::atomic::AtomicBool>) {
    const POLL: gst::ClockTime = gst::ClockTime::from_mseconds(100);
    let Some(bus) = pipeline.bus() else {
        // A GstPipeline always owns a bus; treat absence as impossible
        // invariant and report rather than panic.
        shared.push(BackendEvent::Error {
            message: "pipeline has no bus".to_string(),
        });
        shared.transition(ComponentState::Failed);
        return;
    };
    loop {
        if shutdown.load(std::sync::atomic::Ordering::Relaxed) {
            return;
        }
        let Some(message) = bus.timed_pop(POLL) else {
            continue;
        };
        match message.view() {
            gst::MessageView::Error(err) => {
                let detail = err
                    .debug()
                    .map(|debug| format!(" ({debug})"))
                    .unwrap_or_default();
                shared.transition(ComponentState::Failed);
                shared.push(BackendEvent::Error {
                    message: format!("pipeline error: {}{detail}", err.error()),
                });
                // Terminal: take the graph down from the watch thread (never a
                // streaming thread, so this cannot self-join deadlock).
                let _ = pipeline.set_state(gst::State::Null);
                return;
            }
            gst::MessageView::Eos(..) => {
                shared.push(BackendEvent::EndOfStream);
                shared.transition(ComponentState::Stopped);
                let _ = pipeline.set_state(gst::State::Null);
                return;
            }
            gst::MessageView::Warning(warning) => {
                shared.push(BackendEvent::Warning {
                    message: format!("pipeline warning: {}", warning.error()),
                });
            }
            _ => {}
        }
    }
}

/// [`SourceBackend`] implementation for `SourceKind::TestPattern`.
///
/// Owns one GStreamer pipeline per running instance:
/// `videotestsrc ! capsfilter ! [identity error-after] ! fakesink`. The
/// capsfilter pins deterministic `video/x-raw,I420` caps derived from the
/// validated settings. The final sink is internal scaffolding until the
/// compositor bridge (MEDIA-003) exists; tests observe the stream through
/// [`GstSourceBackend::output_pad`].
#[derive(Debug)]
pub struct GstSourceBackend {
    id: SourceId,
    settings: TestPatternSettings,
    shared: Arc<Shared>,
    shutdown: Arc<std::sync::atomic::AtomicBool>,
    pipeline: Option<gst::Pipeline>,
    bus_watch: Option<JoinHandle<()>>,
    /// Src pad of the last element before the sink, for probes/caps checks.
    output_pad: Option<gst::Pad>,
}

impl GstSourceBackend {
    /// Creates a stopped test-pattern backend. The settings payload is parsed
    /// and fully validated up front; invalid payloads fail with
    /// [`Error::InvalidInput`] and no backend is created.
    pub fn new_test_pattern(id: SourceId, settings: serde_json::Value) -> Result<Self> {
        let settings = TestPatternSettings::from_json(&settings)?;
        Ok(Self {
            id,
            settings,
            shared: Arc::new(Shared::default()),
            shutdown: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            pipeline: None,
            bus_watch: None,
            output_pad: None,
        })
    }

    /// The validated settings currently in effect.
    pub fn settings(&self) -> &TestPatternSettings {
        &self.settings
    }

    /// Deterministic output caps for the current settings:
    /// `video/x-raw, format=I420, width, height, framerate=fps/1`.
    pub fn output_caps(&self) -> gst::Caps {
        gst::Caps::builder("video/x-raw")
            .field("format", "I420")
            .field("width", self.settings.width as i32)
            .field("height", self.settings.height as i32)
            .field("framerate", gst::Fraction::new(self.settings.fps as i32, 1))
            .build()
    }

    /// Src pad feeding the internal sink, while running. Diagnostic/test hook
    /// for buffer probes and caps verification; later the compositor bridge
    /// consumes the same graph position.
    pub fn output_pad(&self) -> Option<gst::Pad> {
        self.output_pad.clone()
    }

    /// Builds the pipeline and returns it plus the pad feeding the sink.
    fn build_pipeline(&self) -> Result<(gst::Pipeline, gst::Pad)> {
        let make = |factory: &str| -> Result<gst::Element> {
            gst::ElementFactory::make(factory)
                .build()
                .map_err(|err| Error::Media(format!("failed to create {factory}: {err}")))
        };
        let source = make("videotestsrc")?;
        source.set_property_from_str("pattern", self.settings.pattern.nick());
        source.set_property("is-live", true);
        source.set_property("num-buffers", self.settings.num_buffers as i32);
        let capsfilter = make("capsfilter")?;
        capsfilter.set_property("caps", &self.output_caps());
        let sink = make("fakesink")?;
        sink.set_property("sync", false);
        sink.set_property("async", false);

        let pipeline = gst::Pipeline::new();
        let tail = if let Some(error_after) = self.settings.error_after {
            let identity = make("identity")?;
            identity.set_property("error-after", error_after as i32);
            pipeline
                .add_many([&source, &capsfilter, &identity, &sink])
                .map_err(|err| Error::Media(format!("failed to assemble pipeline: {err}")))?;
            gst::Element::link_many([&source, &capsfilter, &identity, &sink])
                .map_err(|err| Error::Media(format!("failed to link pipeline: {err}")))?;
            identity
        } else {
            pipeline
                .add_many([&source, &capsfilter, &sink])
                .map_err(|err| Error::Media(format!("failed to assemble pipeline: {err}")))?;
            gst::Element::link_many([&source, &capsfilter, &sink])
                .map_err(|err| Error::Media(format!("failed to link pipeline: {err}")))?;
            capsfilter
        };
        let pad = tail
            .static_pad("src")
            .ok_or_else(|| Error::Media("pipeline tail has no src pad".to_string()))?;
        Ok((pipeline, pad))
    }

    /// Tears down any pipeline and joins the bus watch thread. Bounded: the
    /// watch loop polls at 100 ms cadence and honors the shutdown flag.
    fn teardown(&mut self) {
        self.shutdown
            .store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(pipeline) = self.pipeline.take() {
            let _ = pipeline.set_state(gst::State::Null);
        }
        if let Some(watch) = self.bus_watch.take() {
            if watch.join().is_err() {
                tracing::warn!(source_id = %self.id, "bus watch thread panicked");
            }
        }
        self.output_pad = None;
    }

    /// Builds and starts the pipeline, then transitions to `Running`.
    fn launch(&mut self) -> Result<()> {
        let (pipeline, pad) = self.build_pipeline()?;
        self.shutdown
            .store(false, std::sync::atomic::Ordering::Relaxed);
        let watch = std::thread::Builder::new()
            .name(format!("gst-bus-{}", self.id))
            .spawn({
                let pipeline = pipeline.clone();
                let shared = self.shared.clone();
                let shutdown = self.shutdown.clone();
                move || watch_bus(pipeline, shared, shutdown)
            })
            .map_err(|err| Error::Media(format!("failed to spawn bus watch thread: {err}")))?;
        if let Err(err) = pipeline.set_state(gst::State::Playing) {
            self.shutdown
                .store(true, std::sync::atomic::Ordering::Relaxed);
            let _ = pipeline.set_state(gst::State::Null);
            let _ = watch.join();
            return Err(Error::Media(format!("failed to start pipeline: {err}")));
        }
        self.pipeline = Some(pipeline);
        self.bus_watch = Some(watch);
        self.output_pad = Some(pad);
        self.shared.transition(ComponentState::Running);
        Ok(())
    }
}

impl BackendComponent for GstSourceBackend {
    fn state(&self) -> ComponentState {
        self.shared.state()
    }

    fn drain_events(&mut self) -> Vec<BackendEvent> {
        std::mem::take(&mut self.shared.lock().events)
    }
}

impl SourceBackend for GstSourceBackend {
    fn source_id(&self) -> SourceId {
        self.id
    }

    fn kind(&self) -> SourceKind {
        SourceKind::TestPattern
    }

    fn settings_schema(&self) -> serde_json::Value {
        let patterns: Vec<&str> = TestPatternKind::ALL.iter().map(|kind| kind.nick()).collect();
        serde_json::json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "title": "Test pattern source settings",
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "pattern": {
                    "type": "string",
                    "enum": patterns,
                    "default": "smpte",
                },
                "width": {
                    "type": "integer",
                    "minimum": MIN_DIMENSION,
                    "maximum": MAX_DIMENSION,
                    "multipleOf": 2,
                    "default": 1280,
                },
                "height": {
                    "type": "integer",
                    "minimum": MIN_DIMENSION,
                    "maximum": MAX_DIMENSION,
                    "multipleOf": 2,
                    "default": 720,
                },
                "fps": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": MAX_FPS,
                    "default": 30,
                },
                "num_buffers": {
                    "type": "integer",
                    "minimum": 0,
                    "default": 0,
                    "description": "Diagnostic: stop after N buffers (0 = unlimited).",
                },
                "error_after": {
                    "type": ["integer", "null"],
                    "minimum": 1,
                    "default": null,
                    "description": "Diagnostic: inject a streaming error after N buffers.",
                },
            }
        })
    }

    fn start(&mut self) -> Result<()> {
        if self.shared.state() == ComponentState::Running {
            return Ok(());
        }
        self.teardown();
        self.launch()
    }

    fn stop(&mut self) -> Result<()> {
        self.teardown();
        self.shared.transition(ComponentState::Stopped);
        Ok(())
    }

    fn update_settings(&mut self, settings: serde_json::Value) -> Result<()> {
        // Validate first: a bad payload leaves the running stream untouched.
        let new_settings = TestPatternSettings::from_json(&settings)?;
        if self.shared.state() == ComponentState::Running {
            self.teardown();
            self.settings = new_settings;
            if let Err(err) = self.launch() {
                self.shared.transition(ComponentState::Failed);
                return Err(err);
            }
        } else {
            self.settings = new_settings;
        }
        Ok(())
    }

    fn audio_levels(&self) -> Option<AudioLevels> {
        None
    }
}

impl Drop for GstSourceBackend {
    fn drop(&mut self) {
        self.teardown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_apply_to_null_and_empty_payloads() {
        let from_null = TestPatternSettings::from_json(&serde_json::Value::Null).unwrap();
        let from_empty = TestPatternSettings::from_json(&serde_json::json!({})).unwrap();
        assert_eq!(from_null, TestPatternSettings::default());
        assert_eq!(from_empty, TestPatternSettings::default());
    }

    #[test]
    fn pattern_nicks_match_serde_and_are_distinct() {
        let mut nicks: Vec<&str> = TestPatternKind::ALL.iter().map(|kind| kind.nick()).collect();
        let total = nicks.len();
        nicks.sort_unstable();
        nicks.dedup();
        assert_eq!(nicks.len(), total, "duplicate nicks");
        for kind in TestPatternKind::ALL {
            let json = serde_json::to_string(kind).unwrap();
            assert_eq!(json, format!("\"{}\"", kind.nick()));
            let back: TestPatternKind = serde_json::from_str(&json).unwrap();
            assert_eq!(*kind, back);
        }
    }

    #[test]
    fn invalid_settings_matrix_is_rejected() {
        let bad = [
            serde_json::json!({"width": 0}),
            serde_json::json!({"width": 15}),
            serde_json::json!({"width": 7682}),
            serde_json::json!({"width": 321}),   // odd
            serde_json::json!({"height": 0}),
            serde_json::json!({"height": 721}),  // odd
            serde_json::json!({"fps": 0}),
            serde_json::json!({"fps": 241}),
            serde_json::json!({"fps": -1}),
            serde_json::json!({"pattern": "plaid"}),
            serde_json::json!({"pattern": 3}),
            serde_json::json!({"width": "640"}),
            serde_json::json!({"unknown_key": true}),
            serde_json::json!({"num_buffers": i32::MAX as u64 + 1}),
            serde_json::json!({"error_after": 0}),
            serde_json::json!(42),
            serde_json::json!(["smpte"]),
        ];
        for payload in bad {
            assert!(
                matches!(
                    TestPatternSettings::from_json(&payload),
                    Err(Error::InvalidInput(_))
                ),
                "payload accepted unexpectedly: {payload}"
            );
        }
    }

    #[test]
    fn boundary_settings_are_accepted() {
        for payload in [
            serde_json::json!({"width": 16, "height": 16, "fps": 1}),
            serde_json::json!({"width": 7680, "height": 7680, "fps": 240}),
        ] {
            assert!(TestPatternSettings::from_json(&payload).is_ok(), "{payload}");
        }
    }

    #[test]
    fn output_caps_are_deterministic() {
        crate::GstRuntime::initialize().unwrap();
        let settings = TestPatternSettings {
            width: 640,
            height: 360,
            fps: 60,
            ..TestPatternSettings::default()
        };
        let backend = GstSourceBackend::new_test_pattern(
            SourceId::new(),
            serde_json::to_value(&settings).unwrap(),
        )
        .unwrap();
        let caps = backend.output_caps();
        assert_eq!(
            caps.to_string(),
            "video/x-raw, format=(string)I420, width=(int)640, height=(int)360, framerate=(fraction)60/1"
        );
        assert_eq!(caps, backend.output_caps());
    }

    #[test]
    fn schema_covers_every_pattern_and_bound() {
        let backend =
            GstSourceBackend::new_test_pattern(SourceId::new(), serde_json::Value::Null).unwrap();
        let schema = backend.settings_schema();
        let patterns = schema["properties"]["pattern"]["enum"].as_array().unwrap();
        assert_eq!(patterns.len(), TestPatternKind::ALL.len());
        assert_eq!(schema["properties"]["width"]["minimum"], MIN_DIMENSION);
        assert_eq!(schema["properties"]["width"]["maximum"], MAX_DIMENSION);
        assert_eq!(schema["properties"]["fps"]["maximum"], MAX_FPS);
        assert_eq!(schema["additionalProperties"], false);
    }

    #[test]
    fn constructor_rejects_invalid_settings_without_a_backend() {
        let result = GstSourceBackend::new_test_pattern(
            SourceId::new(),
            serde_json::json!({"width": 0}),
        );
        assert!(matches!(result, Err(Error::InvalidInput(_))));
    }
}
