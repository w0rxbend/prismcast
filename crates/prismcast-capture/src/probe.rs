//! Bounded CPU frame probe. Run on a dedicated OS thread, never a Tokio worker.
use crate::{CaptureError, CaptureLease, CaptureStatus, Result};
use gstreamer::{self as gst, prelude::*};
use gstreamer_app::AppSink;
use std::{
    os::fd::AsRawFd,
    time::{Duration, Instant},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameEvidence {
    pub width: u32,
    pub height: u32,
    pub buffers: u32,
    pub bytes: usize,
    pub first_pts_ns: Option<u64>,
    pub last_pts_ns: Option<u64>,
    pub checksum: u64,
}
struct Graph(gst::Pipeline);
impl Drop for Graph {
    fn drop(&mut self) {
        let _ = self.0.set_state(gst::State::Null);
    }
}
fn native(error: impl std::fmt::Display) -> CaptureError {
    CaptureError::Native(error.to_string())
}

/// Lease borrowing prevents explicit close during the probe. Broker shutdown can
/// revoke it; the probe observes status between bounded sample waits and stops.
pub fn capture_frames(
    lease: &CaptureLease,
    frames: u32,
    timeout: Duration,
) -> Result<FrameEvidence> {
    if !matches!(lease.status(), CaptureStatus::Ready) {
        return Err(CaptureError::Closed);
    }
    gst::init().map_err(native)?;
    let _native = lease.grant().use_native()?;
    // Recheck after registering usage: shutdown may have raced initial status.
    if !matches!(lease.status(), CaptureStatus::Ready) {
        return Err(CaptureError::Closed);
    }
    let source = build_source(lease.grant())?;
    run_source(source, frames, timeout, || {
        matches!(lease.status(), CaptureStatus::Ready)
    })
}
pub(crate) fn build_source(grant: &crate::CaptureGrant) -> Result<gst::Element> {
    let factory = gst::ElementFactory::find("pipewiresrc")
        .ok_or_else(|| native("pipewiresrc plugin unavailable"))?;
    let source = factory.create().build().map_err(native)?;
    for (name, expected) in [
        ("fd", i32::static_type()),
        ("path", String::static_type()),
        ("max-buffers", i32::static_type()),
    ] {
        let property = source
            .find_property(name)
            .ok_or_else(|| native(format!("pipewiresrc missing {name}")))?;
        if property.value_type() != expected
            || !property.flags().contains(gst::glib::ParamFlags::WRITABLE)
        {
            return Err(native(format!("pipewiresrc incompatible {name} property")));
        }
    }
    let property = source
        .find_property("on-disconnect")
        .ok_or_else(|| native("pipewiresrc missing on-disconnect"))?;
    let class = gst::glib::EnumClass::with_type(property.value_type())
        .ok_or_else(|| native("on-disconnect not enum"))?;
    if class.value_by_nick("error").is_none()
        || !property.flags().contains(gst::glib::ParamFlags::WRITABLE)
    {
        return Err(native("pipewiresrc lacks disconnect error mode"));
    }
    source.set_property("fd", grant.remote().as_raw_fd());
    source.set_property("path", grant.node_id().to_string());
    source.set_property("max-buffers", 3i32);
    source.set_property_from_str("on-disconnect", "error");
    Ok(source)
}
fn run_source(
    source: gst::Element,
    frames: u32,
    timeout: Duration,
    alive: impl Fn() -> bool,
) -> Result<FrameEvidence> {
    if !(1..=300).contains(&frames) || timeout.is_zero() || timeout > Duration::from_secs(60) {
        return Err(CaptureError::Unsupported(
            "probe bounds: 1..300 frames and <=60s".into(),
        ));
    }
    let graph = Graph(gst::Pipeline::new());
    let convert = gst::ElementFactory::make("videoconvert")
        .build()
        .map_err(native)?;
    let sink = AppSink::builder()
        .caps(
            &gst::Caps::builder("video/x-raw")
                .field("format", "RGBA")
                .build(),
        )
        .max_buffers(2)
        .drop(true)
        .sync(false)
        .build();
    graph
        .0
        .add_many([&source, &convert, sink.upcast_ref()])
        .map_err(native)?;
    gst::Element::link_many([&source, &convert, sink.upcast_ref()]).map_err(native)?;
    let bus = graph
        .0
        .bus()
        .ok_or_else(|| native("pipeline missing bus"))?;
    graph.0.set_state(gst::State::Playing).map_err(native)?;
    let deadline = Instant::now() + timeout;
    let mut result = FrameEvidence {
        width: 0,
        height: 0,
        buffers: 0,
        bytes: 0,
        first_pts_ns: None,
        last_pts_ns: None,
        checksum: 0,
    };
    while result.buffers < frames {
        if !alive() {
            return Err(CaptureError::Closed);
        }
        while let Some(message) = bus.pop() {
            match message.view() {
                gst::MessageView::Error(error) => return Err(native(error.error())),
                gst::MessageView::Eos(..) if result.buffers < frames => {
                    return Err(native("EOS before requested frames"))
                }
                _ => {}
            }
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(CaptureError::Timeout);
        }
        let wait = remaining.min(Duration::from_millis(100));
        let Some(sample) =
            sink.try_pull_sample(gst::ClockTime::from_nseconds(wait.as_nanos() as u64))
        else {
            continue;
        };
        let caps = sample
            .caps()
            .and_then(|caps| caps.structure(0))
            .ok_or_else(|| native("missing negotiated caps"))?;
        let width = caps.get::<i32>("width").map_err(native)?;
        let height = caps.get::<i32>("height").map_err(native)?;
        if !(1..=8192).contains(&width) || !(1..=8192).contains(&height) {
            return Err(native("negotiated dimensions outside CPU probe limits"));
        }
        if result.buffers > 0 && (result.width != width as u32 || result.height != height as u32) {
            return Err(native("size changed during bounded probe"));
        }
        result.width = width as u32;
        result.height = height as u32;
        let buffer = sample
            .buffer()
            .ok_or_else(|| native("missing frame buffer"))?;
        let map = buffer.map_readable().map_err(native)?;
        if map.is_empty() {
            return Err(native("empty frame"));
        }
        result.bytes = result
            .bytes
            .checked_add(map.len())
            .ok_or_else(|| native("byte count overflow"))?;
        result.checksum = map.as_slice().iter().fold(result.checksum, |sum, byte| {
            sum.wrapping_mul(31).wrapping_add(u64::from(*byte))
        });
        let pts = buffer.pts().map(|pts| pts.nseconds());
        if result.buffers == 0 {
            result.first_pts_ns = pts;
        }
        result.last_pts_ns = pts;
        result.buffers += 1;
    }
    graph.0.set_state(gst::State::Null).map_err(native)?;
    let (state_result, state, _) = graph.0.state(gst::ClockTime::from_seconds(3));
    state_result.map_err(native)?;
    if state != gst::State::Null {
        return Err(native("capture graph did not reach NULL"));
    }
    Ok(result)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn installed_pipewire_properties_build_without_starting_capture() {
        use std::{os::unix::net::UnixStream, sync::Arc};
        gst::init().unwrap();
        let (remote, _peer) = UnixStream::pair().unwrap();
        let grant = crate::CaptureGrant {
            source_id: prismcast_core::SourceId::new(),
            kind: crate::CaptureKind::Monitor,
            node_id: 71,
            remote: remote.into(),
            native_use: Arc::default(),
            _slot: Arc::new(
                Arc::new(tokio::sync::Semaphore::new(1))
                    .try_acquire_owned()
                    .unwrap(),
            ),
        };
        let source = build_source(&grant).unwrap();
        assert_eq!(source.property::<i32>("fd"), grant.remote().as_raw_fd());
        assert_eq!(source.property::<String>("path"), "71");
        assert_eq!(source.current_state(), gst::State::Null);
        drop(source);
    }
    #[test]
    fn actual_native_caps_buffers_and_teardown() {
        gst::init().unwrap();
        let source = gst::ElementFactory::make("videotestsrc")
            .property("is-live", true)
            .build()
            .unwrap();
        let bin = gst::Bin::new();
        let caps = gst::ElementFactory::make("capsfilter")
            .property(
                "caps",
                gst::Caps::builder("video/x-raw")
                    .field("width", 48i32)
                    .field("height", 32i32)
                    .field("framerate", gst::Fraction::new(30, 1))
                    .build(),
            )
            .build()
            .unwrap();
        bin.add_many([&source, &caps]).unwrap();
        source.link(&caps).unwrap();
        let pad = gst::GhostPad::with_target(&caps.static_pad("src").unwrap()).unwrap();
        bin.add_pad(&pad).unwrap();
        let evidence = run_source(bin.upcast(), 3, Duration::from_secs(3), || true).unwrap();
        assert_eq!(
            (evidence.width, evidence.height, evidence.buffers),
            (48, 32, 3)
        );
        assert!(evidence.bytes >= 48 * 32 * 4 * 3);
        assert_ne!(evidence.checksum, 0);
        assert!(evidence.last_pts_ns.unwrap() > evidence.first_pts_ns.unwrap());
        assert_eq!(source.current_state(), gst::State::Null);
    }
}
