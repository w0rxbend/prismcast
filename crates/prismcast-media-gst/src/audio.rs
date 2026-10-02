//! Dedicated-owner audio graph. Native callbacks retain bounded observations
//! only; no GTK/core calls or graph mutation occur on streaming threads.
//! Diagnostic audio is an independent live generator: visual num_buffers/EOS
//! does not determine its duration. Source disable/removal/session stop does.
use gstreamer::{self as gst, prelude::*};
use prismcast_audio::{finite_dbfs, BusMeter, MixerPlan, SourceMeter};
use prismcast_core::{AppState, AudioBusId, Error, Result, SourceId, SourceKind};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
        Arc, Mutex,
    },
};

fn media(error: impl std::fmt::Display) -> Error {
    Error::Media(error.to_string())
}
fn element(factory: &str) -> Result<gst::Element> {
    gst::ElementFactory::make(factory).build().map_err(media)
}
fn queue() -> Result<gst::Element> {
    gst::ElementFactory::make("queue")
        .property("max-size-buffers", 8_u32)
        .property("max-size-bytes", 0_u32)
        .property("max-size-time", 100_000_000_u64)
        .property_from_str("leaky", "downstream")
        .build()
        .map_err(media)
}
fn sink() -> Result<gst::Element> {
    gst::ElementFactory::make("fakesink")
        .property("sync", false)
        .property("async", false)
        .build()
        .map_err(media)
}
fn level() -> Result<gst::Element> {
    gst::ElementFactory::make("level")
        .property("post-messages", true)
        .property("interval", 50_000_000_u64)
        .build()
        .map_err(media)
}
fn caps() -> gst::Caps {
    gst::Caps::builder("audio/x-raw")
        .field("format", "F32LE")
        .field("rate", 48_000_i32)
        .field("channels", 2_i32)
        .field("layout", "interleaved")
        .build()
}

#[derive(Clone)]
struct Measurement {
    peak: Vec<f32>,
    rms: Vec<f32>,
}
fn measurement(message: &gst::Message) -> Option<Measurement> {
    let structure = message.structure()?;
    if structure.name() != "level" {
        return None;
    }
    let values = |key| -> Option<Vec<f32>> {
        let array = structure.get::<gst::glib::ValueArray>(key).ok()?;
        if array.len() != 2 {
            return None;
        }
        array
            .iter()
            .map(|v| finite_dbfs(v.get::<f64>().ok()?))
            .collect()
    };
    Some(Measurement {
        peak: values("peak")?,
        rms: values("rms")?,
    })
}

/// Native graph and its callback/request-pad cleanup are one ownership unit.
struct Graph {
    pipeline: gst::Pipeline,
    requests: Vec<(gst::Element, gst::Pad)>,
    sources: Vec<SourceId>,
    buses: Vec<AudioBusId>,
    observations: Arc<Mutex<Vec<Option<Measurement>>>>,
    terminal: Receiver<String>,
    failed: Arc<AtomicBool>,
}
impl Graph {
    fn build(plan: &MixerPlan) -> Result<Self> {
        let pipeline = gst::Pipeline::new();
        let (terminal_tx, terminal) = mpsc::sync_channel(1);
        let observations = Arc::new(Mutex::new(Vec::new()));
        let failed = Arc::new(AtomicBool::new(false));
        let mut graph = Self {
            pipeline,
            requests: Vec::new(),
            sources: Vec::new(),
            buses: Vec::new(),
            observations,
            terminal,
            failed,
        };
        let mut mixer_elements = HashMap::new();
        let mut levels = Vec::new();
        for bus_id in &plan.buses {
            if !plan
                .sources
                .iter()
                .any(|source| source.routes.iter().any(|route| route.bus_id == *bus_id))
            {
                continue;
            }
            let mix = gst::ElementFactory::make("audiomixer")
                .property("force-live", true)
                .property("ignore-inactive-pads", true)
                .build()
                .map_err(media)?;
            let filter = gst::ElementFactory::make("capsfilter")
                .property("caps", caps())
                .build()
                .map_err(media)?;
            let meter = level()?;
            let output = sink()?;
            graph
                .pipeline
                .add_many([&mix, &filter, &meter, &output])
                .map_err(media)?;
            gst::Element::link_many([&mix, &filter, &meter, &output]).map_err(media)?;
            graph.buses.push(*bus_id);
            levels.push(meter);
            mixer_elements.insert(*bus_id, mix);
        }
        let mut source_levels = Vec::new();
        for source in &plan.sources {
            tracing::debug!(source_id=%source.source_id, "building diagnostic audio branch");
            let tone = gst::ElementFactory::make("audiotestsrc")
                .property("is-live", true)
                .property("freq", 440.0_f64)
                .property("volume", 0.5_f64)
                .property_from_str("wave", "sine")
                .build()
                .map_err(media)?;
            let convert = element("audioconvert")?;
            let resample = element("audioresample")?;
            let filter = gst::ElementFactory::make("capsfilter")
                .property("caps", caps())
                .build()
                .map_err(media)?;
            let gain = gst::ElementFactory::make("volume")
                .property("volume-full-range", source.gain)
                .property("mute", source.muted)
                .build()
                .map_err(media)?;
            let meter = level()?;
            let tee = element("tee")?;
            let monitor_queue = queue()?;
            let monitor_sink = sink()?;
            graph
                .pipeline
                .add_many([
                    &tone,
                    &convert,
                    &resample,
                    &filter,
                    &gain,
                    &meter,
                    &tee,
                    &monitor_queue,
                    &monitor_sink,
                ])
                .map_err(media)?;
            gst::Element::link_many([&tone, &convert, &resample, &filter, &gain, &meter, &tee])
                .map_err(media)?;
            monitor_queue.link(&monitor_sink).map_err(media)?;
            let pad = graph.request(&tee, "src_%u")?;
            let input = monitor_queue
                .static_pad("sink")
                .ok_or_else(|| media("audio queue has no sink pad"))?;
            pad.link(&input).map_err(media)?;
            for route in &source.routes {
                let mixer = mixer_elements
                    .get(&route.bus_id)
                    .ok_or_else(|| media("planned audio bus absent"))?;
                let branch = queue()?;
                graph.pipeline.add(&branch).map_err(media)?;
                let output = graph.request(&tee, "src_%u")?;
                let input = branch
                    .static_pad("sink")
                    .ok_or_else(|| media("audio queue has no sink pad"))?;
                output.link(&input).map_err(media)?;
                let mix_input = graph.request(mixer, "sink_%u")?;
                mix_input.set_property("mute", route.solo_muted);
                branch
                    .static_pad("src")
                    .ok_or_else(|| media("audio queue has no src pad"))?
                    .link(&mix_input)
                    .map_err(media)?;
            }
            graph.sources.push(source.source_id);
            source_levels.push(meter);
        }
        source_levels.extend(levels);
        // Finite slots correspond to source IDs first, bus IDs second.
        let observations = Arc::new(Mutex::new(vec![None; source_levels.len()]));
        graph.observations = observations.clone();
        let failed = graph.failed.clone();
        let bus = graph
            .pipeline
            .bus()
            .ok_or_else(|| media("audio pipeline has no bus"))?;
        bus.set_sync_handler(move |_, message| {
            match message.view() {
                gst::MessageView::Error(error) => {
                    let text: String = error
                        .error()
                        .to_string()
                        .chars()
                        .scan(0, |bytes, c| {
                            *bytes += c.len_utf8();
                            (*bytes <= 512).then_some(c)
                        })
                        .collect();
                    let _ = terminal_tx.try_send(text);
                    failed.store(true, Ordering::Release);
                }
                gst::MessageView::Eos(_) => {
                    let _ = terminal_tx.try_send("live audio graph ended unexpectedly".into());
                    failed.store(true, Ordering::Release);
                }
                gst::MessageView::Element(_) => {
                    if let Some(index) = source_levels
                        .iter()
                        .position(|level| message.src() == Some(level.upcast_ref()))
                    {
                        if let Some(value) = measurement(message) {
                            // Never wait for the owner or another streaming callback.
                            if let Ok(mut latest) = observations.try_lock() {
                                latest[index] = Some(value);
                            }
                        } else if message
                            .structure()
                            .is_some_and(|structure| structure.name() == "level")
                        {
                            let _ = terminal_tx
                                .try_send("invalid native audio level observation".into());
                            failed.store(true, Ordering::Release);
                        }
                    }
                }
                _ => {}
            }
            // Dropping EVERY message prevents an unbounded native bus backlog.
            gst::BusSyncReply::Drop
        });
        graph
            .pipeline
            .set_state(gst::State::Playing)
            .map_err(media)?;
        Ok(graph)
    }
    fn request(&mut self, element: &gst::Element, template: &str) -> Result<gst::Pad> {
        let pad = element
            .request_pad_simple(template)
            .ok_or_else(|| media("audio request pad unavailable"))?;
        self.requests.push((element.clone(), pad.clone()));
        Ok(pad)
    }
    fn stop(&mut self) -> Result<()> {
        let transition = self.pipeline.set_state(gst::State::Null).map_err(media);
        let (settled, state, _) = self.pipeline.state(gst::ClockTime::from_seconds(2));
        if let Some(bus) = self.pipeline.bus() {
            bus.unset_sync_handler();
        }
        for (element, pad) in self.requests.drain(..) {
            if let Some(peer) = pad.peer() {
                if pad.direction() == gst::PadDirection::Src {
                    let _ = pad.unlink(&peer);
                } else {
                    let _ = peer.unlink(&pad);
                }
            }
            element.release_request_pad(&pad);
        }
        transition?;
        settled.map_err(media)?;
        if state != gst::State::Null {
            return Err(media("audio teardown did not reach NULL before deadline"));
        }
        Ok(())
    }
    fn take(&mut self) -> Result<(Vec<SourceMeter>, Vec<BusMeter>)> {
        if self.failed.load(Ordering::Acquire) {
            return Err(media(
                self.terminal
                    .try_recv()
                    .unwrap_or_else(|_| "audio graph failed".into()),
            ));
        }
        let mut values = self
            .observations
            .lock()
            .map_err(|_| media("audio observations poisoned"))?;
        let mut sources = Vec::new();
        let mut buses = Vec::new();
        for (index, slot) in values.iter_mut().enumerate() {
            if let Some(value) = slot.take() {
                if index < self.sources.len() {
                    sources.push(SourceMeter {
                        source_id: self.sources[index],
                        peak_dbfs: value.peak,
                        rms_dbfs: value.rms,
                    });
                } else {
                    buses.push(BusMeter {
                        bus_id: self.buses[index - self.sources.len()],
                        peak_dbfs: value.peak,
                        rms_dbfs: value.rms,
                    });
                }
            }
        }
        Ok((sources, buses))
    }
}
impl Drop for Graph {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

/// Synchronous backend; construct, reconcile, poll and stop on its owner thread.
/// Outputs terminate in fakesinks: no capture or playback device is opened.
pub struct GstAudioMixer {
    plan: Option<MixerPlan>,
    graph: Option<Graph>,
    bus_meters: Vec<BusMeter>,
}
impl GstAudioMixer {
    pub fn new() -> Result<Self> {
        crate::GstRuntime::initialize().map_err(media)?;
        Ok(Self {
            plan: None,
            graph: None,
            bus_meters: Vec::new(),
        })
    }
    /// Rebuild only for an effective signal/routing change. Video/UI changes
    /// leave the running audio clock and graph untouched.
    pub fn reconcile(&mut self, state: &AppState) -> Result<()> {
        let mut active = Vec::new();
        for source in state.sources.values() {
            if source.enabled && source.kind == SourceKind::TestPattern {
                if let Some(flag) = source.settings.get("audio_test") {
                    if !flag.is_boolean() {
                        return Err(Error::InvalidInput("audio_test must be a boolean".into()));
                    }
                    if flag == true {
                        crate::TestPatternSettings::from_json(source.settings.clone())?;
                        if active.len() == prismcast_audio::MAX_AUDIO_SOURCES {
                            return Err(Error::InvalidInput(
                                "audio graph supports at most 32 sources".into(),
                            ));
                        }
                        active.push(source.id);
                    }
                }
            }
        }
        let plan = MixerPlan::from_state(state, &active)?;
        if self.plan.as_ref() == Some(&plan) {
            return Ok(());
        }
        self.stop()?;
        if !plan.sources.is_empty() {
            self.graph = Some(Graph::build(&plan)?);
        }
        self.plan = Some(plan);
        Ok(())
    }
    pub fn poll(&mut self) -> Result<Vec<SourceMeter>> {
        let Some(graph) = self.graph.as_mut() else {
            return Ok(Vec::new());
        };
        let (sources, buses) = graph.take()?;
        // Retain at most one observation per configured bus, including slow consumers.
        for value in buses {
            if let Some(slot) = self
                .bus_meters
                .iter_mut()
                .find(|slot| slot.bus_id == value.bus_id)
            {
                *slot = value;
            } else {
                self.bus_meters.push(value);
            }
        }
        Ok(sources)
    }
    /// Drain latest mixed-bus observations, for diagnostics and signal tests.
    pub fn take_bus_meters(&mut self) -> Vec<BusMeter> {
        std::mem::take(&mut self.bus_meters)
    }
    pub fn stop(&mut self) -> Result<()> {
        self.plan = None;
        self.bus_meters.clear();
        match self.graph.take() {
            Some(mut graph) => graph.stop(),
            None => Ok(()),
        }
    }
}
impl Drop for GstAudioMixer {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use prismcast_core::{
        audio::{AudioRoute, TrackMask},
        Source,
    };
    fn state() -> AppState {
        let mut state = AppState::new();
        let mut source = Source::new(SourceKind::TestPattern, "tone");
        source.settings = serde_json::json!({"audio_test":true});
        state.audio.routes.push(AudioRoute {
            source_id: source.id,
            bus_id: state.audio.buses[0].id,
            tracks: TrackMask::ALL,
        });
        state.sources.insert(source.id, source);
        state
    }
    #[test]
    fn unrelated_reconciliation_keeps_graph_and_stop_releases_pads_and_callback() {
        let mut state = state();
        let mut mixer = GstAudioMixer::new().unwrap();
        mixer.reconcile(&state).unwrap();
        let pipeline = mixer.graph.as_ref().unwrap().pipeline.clone();
        let requested = mixer.graph.as_ref().unwrap().requests.clone();
        assert_eq!(requested.len(), 3); // source drain tee, bus tee, mixer pad
        state.sources.values_mut().next().unwrap().name = "renamed".into();
        mixer.reconcile(&state).unwrap();
        assert_eq!(pipeline, mixer.graph.as_ref().unwrap().pipeline);
        mixer.stop().unwrap();
        assert_eq!(pipeline.current_state(), gst::State::Null);
        for (element, pad) in requested {
            assert!(pad.peer().is_none());
            assert!(!element.pads().contains(&pad));
        }
        let bus = pipeline.bus().unwrap();
        bus.set_flushing(false);
        bus.post(
            gst::message::Application::builder(gst::Structure::new_empty("after-cleanup")).build(),
        )
        .unwrap();
        assert!(
            bus.pop().is_some(),
            "callback still dropping messages after shutdown"
        );
    }
    #[test]
    fn terminal_failure_has_priority_over_pending_meter_and_native_bus_is_empty() {
        let mut mixer = GstAudioMixer::new().unwrap();
        mixer.reconcile(&state()).unwrap();
        let graph = mixer.graph.as_mut().unwrap();
        graph.observations.lock().unwrap()[0] = Some(Measurement {
            peak: vec![-6.0; 2],
            rms: vec![-9.0; 2],
        });
        let bus = graph.pipeline.bus().unwrap();
        for _ in 0..100 {
            bus.post(
                gst::message::Error::builder(gst::CoreError::Failed, "injected terminal failure")
                    .src(&graph.pipeline)
                    .build(),
            )
            .unwrap();
        }
        assert!(bus.pop().is_none());
        assert!(
            matches!(mixer.poll(), Err(Error::Media(message)) if message.contains("injected terminal failure"))
        );
        mixer.stop().unwrap();
        assert!(mixer.poll().unwrap().is_empty());
    }
    #[test]
    fn invalid_native_levels_fail_instead_of_publishing_stale_measurements() {
        let mut mixer = GstAudioMixer::new().unwrap();
        mixer.reconcile(&state()).unwrap();
        let pipeline = mixer.graph.as_ref().unwrap().pipeline.clone();
        let level = pipeline
            .children()
            .into_iter()
            .find(|element| {
                element
                    .factory()
                    .is_some_and(|factory| factory.name() == "level")
            })
            .unwrap();
        let invalid = gst::Structure::builder("level")
            // A malformed GstArray is deliberately distinct from the native
            // GValueArray; keep the hostile fixture safe to send as a message.
            .field("peak", gst::Array::new([f64::NAN, -6.0]))
            .field("rms", gst::Array::new([-9.0_f64; 2]))
            .build();
        pipeline
            .bus()
            .unwrap()
            .post(gst::message::Element::builder(invalid).src(&level).build())
            .unwrap();
        assert!(
            matches!(mixer.poll(), Err(Error::Media(message)) if message.contains("invalid native audio level"))
        );
    }
}
