//! Persistent producer pipeline with a bounded, rebuildable raw-video consumer.
//! All lifecycle methods run on the native media owner OS thread.
use crate::{CaptureError, CaptureLease, CaptureStatus, NativeGuard, Result};
use gstreamer::{self as gst, prelude::*};
use gstreamer_app::{AppLeakyType, AppSink, AppSinkCallbacks, AppSrc};
use std::sync::{Arc, Mutex};
const MAX_FRAME_BYTES: usize = 128 * 1024 * 1024;
fn native(error: impl std::fmt::Display) -> CaptureError {
    CaptureError::Native(error.to_string())
}

#[derive(Default)]
struct TimestampEpoch {
    origin: Option<gst::ClockTime>,
    base: gst::ClockTime,
    last: Option<gst::ClockTime>,
}
impl TimestampEpoch {
    fn map(&mut self, pts: Option<gst::ClockTime>, now: gst::ClockTime) -> (gst::ClockTime, bool) {
        let reset = self.origin.is_none()
            || pts.is_none()
            || self.last.zip(pts).is_some_and(|(last, pts)| pts < last);
        if reset {
            self.origin = pts;
            self.base = now;
        }
        self.last = pts;
        let mapped = pts
            .zip(self.origin)
            .map(|(pts, origin)| self.base.saturating_add(pts.saturating_sub(origin)))
            .unwrap_or(now);
        (mapped, reset)
    }
}
struct Endpoint {
    source: AppSrc,
    epoch: TimestampEpoch,
}
#[derive(Default)]
struct Bridge {
    active: bool,
    latest: Option<gst::Sample>,
    endpoint: Option<Endpoint>,
    dimensions: Option<(u32, u32)>,
    error: Option<String>,
    received: u64,
}
/// Clones share metadata and GstBuffer references, never clone raw pixel bytes.
#[derive(Clone, Default)]
pub struct CaptureFeed(Arc<Mutex<Bridge>>);
impl CaptureFeed {
    pub fn same_producer(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
    pub fn dimensions(&self) -> Option<(u32, u32)> {
        self.0.lock().ok().and_then(|state| state.dimensions)
    }
    pub fn error(&self) -> Option<String> {
        self.0
            .lock()
            .map(|state| state.error.clone())
            .unwrap_or(Some("capture bridge poisoned".into()))
    }
    pub fn received(&self) -> u64 {
        self.0.lock().map(|state| state.received).unwrap_or(0)
    }
    /// One appsrc per shared source; placements fan out only after its tee.
    pub fn consumer(&self) -> Result<CaptureConsumer> {
        let (dimensions, caps) = {
            let state = self.0.lock().map_err(native)?;
            if !state.active {
                return Err(native("capture feed stopped"));
            }
            (
                state.dimensions,
                state
                    .latest
                    .as_ref()
                    .and_then(|sample| sample.caps().map(ToOwned::to_owned)),
            )
        };
        if dimensions.is_none() {
            return Err(native("capture has not negotiated a frame"));
        }
        let source = AppSrc::builder()
            .is_live(true)
            .format(gst::Format::Time)
            .block(false)
            .max_buffers(1)
            .max_bytes(MAX_FRAME_BYTES as u64)
            .leaky_type(AppLeakyType::Downstream)
            .build();
        source.set_property("emit-signals", false);
        source.set_caps(caps.as_ref());
        let bin = gst::Bin::new();
        bin.add(&source).map_err(native)?;
        let pad = source
            .static_pad("src")
            .ok_or_else(|| native("appsrc has no src pad"))?;
        bin.add_pad(&gst::GhostPad::with_target(&pad).map_err(native)?)
            .map_err(native)?;
        self.0.lock().map_err(native)?.endpoint = Some(Endpoint {
            source: source.clone(),
            epoch: TimestampEpoch::default(),
        });
        Ok(CaptureConsumer {
            bin,
            source,
            feed: self.clone(),
        })
    }
    fn publish(
        &self,
        sample: gst::Sample,
    ) -> std::result::Result<gst::FlowSuccess, gst::FlowError> {
        let result = (|| -> Result<Option<(AppSrc, gst::Buffer, gst::Caps)>> {
            let caps = sample
                .caps()
                .ok_or_else(|| native("capture sample lacks caps"))?;
            if caps.size() != 1
                || caps.features(0).is_none_or(|features| {
                    features.is_any()
                        || (!features.is_empty() && !features.contains("memory:SystemMemory"))
                })
            {
                return Err(native("capture requires SystemMemory caps"));
            }
            let structure = caps
                .structure(0)
                .ok_or_else(|| native("capture caps empty"))?;
            if structure.name() != "video/x-raw" {
                return Err(native("capture requires raw video"));
            }
            if structure.get::<String>("format").map_err(native)? != "RGBA" {
                return Err(native("capture bridge requires RGBA"));
            }
            let width = structure.get::<i32>("width").map_err(native)?;
            let height = structure.get::<i32>("height").map_err(native)?;
            if !(1..=8192).contains(&width) || !(1..=8192).contains(&height) {
                return Err(native("capture dimensions outside 1..8192"));
            }
            let buffer = sample
                .buffer()
                .ok_or_else(|| native("capture sample lacks buffer"))?;
            let minimum = (width as usize)
                .checked_mul(height as usize)
                .and_then(|pixels| pixels.checked_mul(4))
                .ok_or_else(|| native("capture pixel size overflow"))?;
            if buffer.size() < minimum || buffer.size() > MAX_FRAME_BYTES {
                return Err(native("capture frame outside 128 MiB budget"));
            }
            let mut state = self.0.lock().map_err(native)?;
            if !state.active {
                return Ok(None);
            }
            state.dimensions = Some((width as u32, height as u32));
            state.received = state.received.saturating_add(1);
            state.latest = Some(sample.clone());
            let Some(endpoint) = state.endpoint.as_mut() else {
                return Ok(None);
            };
            if endpoint.source.current_state() != gst::State::Playing {
                return Ok(None);
            };
            let Some(now) = endpoint.source.current_running_time() else {
                return Ok(None);
            };
            let (pts, discont) = endpoint.epoch.map(buffer.pts(), now);
            let mut buffer = buffer.to_owned();
            let writable = buffer.make_mut();
            writable.set_pts(pts);
            writable.set_dts(None);
            if discont {
                writable.set_flags(gst::BufferFlags::DISCONT);
            }
            Ok(Some((endpoint.source.clone(), buffer, caps.to_owned())))
        })();
        match result {
            Ok(Some((source, buffer, caps))) => {
                if source.caps().as_ref() != Some(&caps) {
                    source.set_caps(Some(&caps));
                }
                // A retired NULL consumer can return Flushing; its failure cannot
                // stop the independent producer or revive a discarded scene.
                let _ = source.push_buffer(buffer);
                Ok(gst::FlowSuccess::Ok)
            }
            Ok(None) => Ok(gst::FlowSuccess::Ok),
            Err(error) => {
                if let Ok(mut state) = self.0.lock() {
                    state.error = Some(error.to_string());
                }
                Err(gst::FlowError::Error)
            }
        }
    }
    fn clear(&self) {
        if let Ok(mut state) = self.0.lock() {
            state.active = false;
            state.endpoint = None;
            state.latest = None;
            state.dimensions = None;
        }
    }
}
pub struct CaptureConsumer {
    pub bin: gst::Bin,
    source: AppSrc,
    feed: CaptureFeed,
}
impl Drop for CaptureConsumer {
    fn drop(&mut self) {
        if let Ok(mut state) = self.feed.0.lock() {
            if state
                .endpoint
                .as_ref()
                .is_some_and(|endpoint| endpoint.source == self.source)
            {
                state.endpoint = None;
            }
        }
    }
}
struct ProducerGraph(gst::Pipeline);
impl Drop for ProducerGraph {
    fn drop(&mut self) {
        let _ = self.0.set_state(gst::State::Null);
    }
}
/// Reusable CPU bridge owner; useful for deterministic generators and future
/// capture sources. Native lifecycle is confined to the media OS owner thread.
pub struct FrameProducer {
    graph: Option<ProducerGraph>,
    feed: CaptureFeed,
}
impl FrameProducer {
    pub fn start(source: gst::Element) -> Result<Self> {
        gst::init().map_err(native)?;
        let (graph, feed) = build_producer(source)?;
        Ok(Self {
            graph: Some(graph),
            feed,
        })
    }
    pub fn feed(&self) -> CaptureFeed {
        self.feed.clone()
    }
    pub fn shutdown_native(&mut self) -> Result<()> {
        self.feed.clear();
        let Some(graph) = self.graph.take() else {
            return Ok(());
        };
        let result = graph
            .0
            .set_state(gst::State::Null)
            .map_err(native)
            .and_then(|_| {
                let (result, state, _) = graph.0.state(gst::ClockTime::from_seconds(3));
                result.map_err(native)?;
                if state != gst::State::Null {
                    return Err(native("producer failed to reach NULL"));
                }
                Ok(())
            });
        if result.is_err() {
            self.graph = Some(graph);
        } else {
            drop(graph);
        }
        result
    }
}
impl Drop for FrameProducer {
    fn drop(&mut self) {
        let _ = self.shutdown_native();
    }
}
pub struct CaptureProducer {
    producer: FrameProducer,
    guard: Option<NativeGuard>,
    lease: Option<CaptureLease>,
}
impl CaptureProducer {
    pub fn start(lease: CaptureLease) -> Result<Self> {
        if lease.status() != CaptureStatus::Ready {
            return Err(CaptureError::Closed);
        }
        gst::init().map_err(native)?;
        let guard = lease.grant().use_native()?;
        if lease.status() != CaptureStatus::Ready {
            return Err(CaptureError::Closed);
        }
        let source = crate::probe::build_source(lease.grant())?;
        let producer = FrameProducer::start(source)?;
        Ok(Self {
            producer,
            guard: Some(guard),
            lease: Some(lease),
        })
    }
    pub fn feed(&self) -> CaptureFeed {
        self.producer.feed()
    }
    pub fn status(&self) -> CaptureStatus {
        self.lease
            .as_ref()
            .map(CaptureLease::status)
            .unwrap_or(CaptureStatus::Closed)
    }
    pub fn error(&self) -> Option<String> {
        self.producer.feed.error()
    }
    /// Disconnect consumers first; caller then awaits returned lease.close.
    /// NULL failure retains graph and native guard until final native object drop.
    pub fn shutdown_native(&mut self) -> Result<()> {
        let result = self.producer.shutdown_native();
        if self.producer.graph.is_none() {
            self.guard.take();
        }
        result
    }
    pub fn take_stopped_lease(&mut self) -> Result<CaptureLease> {
        if self.producer.graph.is_some() {
            return Err(native("stop native producer before taking lease"));
        }
        self.lease.take().ok_or(CaptureError::Closed)
    }
}
impl Drop for CaptureProducer {
    fn drop(&mut self) {
        let _ = self.shutdown_native();
    }
}
fn build_producer(source: gst::Element) -> Result<(ProducerGraph, CaptureFeed)> {
    let graph = ProducerGraph(gst::Pipeline::new());
    let feed = CaptureFeed::default();
    feed.0.lock().map_err(native)?.active = true;
    let errors = feed.clone();
    graph
        .0
        .bus()
        .ok_or_else(|| native("producer bus missing"))?
        .set_sync_handler(move |_, message| {
            let error = match message.view() {
                gst::MessageView::Error(error) => Some(error.error().to_string()),
                gst::MessageView::Eos(_) => Some("capture producer EOS".into()),
                _ => None,
            };
            if let Some(error) = error {
                if let Ok(mut state) = errors.0.lock() {
                    if state.error.is_none() {
                        state.error = Some(error)
                    }
                }
            }
            gst::BusSyncReply::Drop
        });
    let convert = gst::ElementFactory::make("videoconvert")
        .build()
        .map_err(native)?;
    let sink = AppSink::builder()
        .caps(
            &gst::Caps::builder("video/x-raw")
                .field("format", "RGBA")
                .build(),
        )
        .max_buffers(1)
        .drop(true)
        .sync(false)
        .build();
    let callback = feed.clone();
    sink.set_callbacks(
        AppSinkCallbacks::builder()
            .new_sample(move |sink| {
                let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                callback.publish(sample)
            })
            .build(),
    );
    graph
        .0
        .add_many([&source, &convert, sink.upcast_ref()])
        .map_err(native)?;
    gst::Element::link_many([&source, &convert, sink.upcast_ref()]).map_err(native)?;
    graph.0.set_state(gst::State::Playing).map_err(native)?;
    Ok((graph, feed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};
    #[test]
    fn timestamp_epoch_preserves_deltas_and_marks_resets() {
        let mut epoch = TimestampEpoch::default();
        let ns = gst::ClockTime::from_nseconds;
        assert_eq!(epoch.map(Some(ns(1000)), ns(50)), (ns(50), true));
        assert_eq!(epoch.map(Some(ns(1100)), ns(800)), (ns(150), false));
        assert_eq!(epoch.map(Some(ns(3)), ns(900)), (ns(900), true));
        assert_eq!(epoch.map(None, ns(950)), (ns(950), true));
        assert_eq!(epoch.map(Some(ns(9)), ns(1000)), (ns(1000), true));
        assert_eq!(epoch.map(Some(ns(19)), ns(1001)), (ns(1010), false));
    }
    fn sample(width: i32, height: i32, format: &str, bytes: usize) -> gst::Sample {
        gst::Sample::builder()
            .caps(
                &gst::Caps::builder("video/x-raw")
                    .field("format", format)
                    .field("width", width)
                    .field("height", height)
                    .build(),
            )
            .buffer(&gst::Buffer::with_size(bytes).unwrap())
            .build()
    }
    #[test]
    fn bridge_checks_actual_bytes_format_and_retired_consumer_identity() {
        gst::init().unwrap();
        let feed = CaptureFeed::default();
        feed.0.lock().unwrap().active = true;
        assert!(feed.publish(sample(32, 16, "RGBA", 32 * 16 * 4)).is_ok());
        let old = feed.consumer().unwrap();
        let fresh = feed.consumer().unwrap();
        drop(old);
        assert_eq!(
            feed.0.lock().unwrap().endpoint.as_ref().unwrap().source,
            fresh.source
        );
        assert_eq!(fresh.source.property::<u64>("max-buffers"), 1);
        assert_eq!(
            fresh.source.property::<u64>("max-bytes"),
            MAX_FRAME_BYTES as u64
        );
        drop(fresh);
        assert!(feed.0.lock().unwrap().endpoint.is_none());
        assert!(feed.publish(sample(32, 16, "BGRA", 32 * 16 * 4)).is_err());
        assert!(feed
            .publish(sample(32, 16, "RGBA", MAX_FRAME_BYTES + 1))
            .is_err());
        feed.clear();
        assert!(feed.consumer().is_err());
        assert!(feed.dimensions().is_none());
    }
    #[test]
    fn independent_producer_survives_two_consumer_pipeline_epochs() {
        gst::init().unwrap();
        let source = gst::ElementFactory::make("videotestsrc")
            .property("is-live", true)
            .build()
            .unwrap();
        let mut producer = FrameProducer::start(source.clone()).unwrap();
        let feed = producer.feed();
        let deadline = Instant::now() + Duration::from_secs(2);
        while feed.dimensions().is_none() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        let mut previous_received = 0;
        for _ in 0..2 {
            let consumer = feed.consumer().unwrap();
            let sink = AppSink::builder()
                .max_buffers(1)
                .drop(true)
                .sync(false)
                .build();
            let pipeline = gst::Pipeline::new();
            pipeline
                .add_many([consumer.bin.upcast_ref::<gst::Element>(), sink.upcast_ref()])
                .unwrap();
            consumer.bin.link(&sink).unwrap();
            pipeline.set_state(gst::State::Playing).unwrap();
            let first = sink
                .try_pull_sample(gst::ClockTime::from_seconds(2))
                .unwrap();
            let second = sink
                .try_pull_sample(gst::ClockTime::from_seconds(2))
                .unwrap();
            let a = first.buffer().unwrap();
            let b = second.buffer().unwrap();
            assert!(a.pts().unwrap() < gst::ClockTime::from_seconds(1));
            assert!(b.pts().unwrap() > a.pts().unwrap());
            assert!(a.flags().contains(gst::BufferFlags::DISCONT));
            assert!(feed.received() > previous_received);
            previous_received = feed.received();
            pipeline.set_state(gst::State::Null).unwrap();
            drop(pipeline);
            drop(consumer);
            assert_eq!(source.current_state(), gst::State::Playing);
        }
        producer.shutdown_native().unwrap();
        assert_eq!(source.current_state(), gst::State::Null);
        assert!(feed.dimensions().is_none());
    }
}
