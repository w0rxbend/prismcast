//! Headless `GstSourceBackend` tests: real buffers, deterministic caps,
//! settings validation, lifecycle idempotency, and error-path teardown.
//! Requires only videotestsrc/capsfilter/identity/fakesink — no display,
//! audio device, encoder, or network.

use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use gstreamer::{self as gst, prelude::*};
use prismcast_core::{Error, SourceId, SourceKind};
use prismcast_media::{BackendComponent, BackendEvent, ComponentState, SourceBackend};
use prismcast_media_gst::{GstRuntime, GstSourceBackend, TestPatternKind, TestPatternSettings};

const DEADLINE: Duration = Duration::from_secs(10);

fn backend(settings: serde_json::Value) -> GstSourceBackend {
    GstRuntime::initialize().unwrap();
    GstSourceBackend::new_test_pattern(SourceId::new(), settings).unwrap()
}

/// Counts buffers reaching the backend's output pad; the probe runs on
/// GStreamer streaming threads, never on the test thread.
struct Probe {
    buffers: Arc<AtomicUsize>,
    caps: Arc<Mutex<Option<gst::Caps>>>,
    _probe_id: gst::PadProbeId,
}

fn attach_probe(pad: &gst::Pad) -> Probe {
    let buffers = Arc::new(AtomicUsize::new(0));
    let caps: Arc<Mutex<Option<gst::Caps>>> = Arc::new(Mutex::new(None));
    let probe_id = pad.add_probe(gst::PadProbeType::BUFFER, {
        let buffers = buffers.clone();
        let caps = caps.clone();
        move |pad, info| {
            buffers.fetch_add(1, Ordering::Relaxed);
            let mut observed = caps.lock().unwrap_or_else(|p| p.into_inner());
            if observed.is_none() {
                *observed = pad.current_caps();
            }
            let _ = info;
            gst::PadProbeReturn::Ok
        }
    })
    .expect("probe attaches");
    Probe {
        buffers,
        caps,
        _probe_id: probe_id,
    }
}

/// Polls `drain_events` until `pred` matches an event or the deadline passes;
/// returns all drained events.
fn wait_for_event(
    backend: &mut GstSourceBackend,
    pred: impl Fn(&BackendEvent) -> bool,
) -> Vec<BackendEvent> {
    let deadline = Instant::now() + DEADLINE;
    let mut seen = Vec::new();
    while Instant::now() < deadline {
        seen.extend(backend.drain_events());
        if seen.iter().any(&pred) {
            return seen;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("deadline waiting for backend event; seen: {seen:?}");
}

#[test]
fn finite_stream_delivers_exact_buffers_with_deterministic_caps_then_eos() {
    let mut source = backend(serde_json::json!({
        "pattern": "ball",
        "width": 320,
        "height": 240,
        "fps": 30,
        "num_buffers": 16,
    }));
    source.start().unwrap();
    let probe = attach_probe(&source.output_pad().expect("running output pad"));
    let events = wait_for_event(&mut source, |e| matches!(e, BackendEvent::EndOfStream));
    assert!(
        events
            .iter()
            .any(|e| matches!(e, BackendEvent::StateChanged { state: ComponentState::Running })),
        "{events:?}"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, BackendEvent::StateChanged { state: ComponentState::Stopped })),
        "{events:?}"
    );
    assert_eq!(source.state(), ComponentState::Stopped);
    assert_eq!(probe.buffers.load(Ordering::Relaxed), 16);
    let caps = probe
        .caps
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone()
        .expect("probe observed caps");
    assert_eq!(caps, source.output_caps());
    assert_eq!(
        caps.to_string(),
        "video/x-raw, format=(string)I420, width=(int)320, height=(int)240, framerate=(fraction)30/1"
    );
    // EOS teardown leaves stop() a no-op and start() able to relaunch.
    source.stop().unwrap();
    assert_eq!(source.state(), ComponentState::Stopped);
    source.start().unwrap();
    assert_eq!(source.state(), ComponentState::Running);
    source.stop().unwrap();
}

#[test]
fn start_and_stop_are_idempotent() {
    let mut source = backend(serde_json::json!({"num_buffers": 0}));
    assert_eq!(source.state(), ComponentState::Stopped);
    source.start().unwrap();
    source.start().unwrap();
    assert_eq!(source.state(), ComponentState::Running);
    source.stop().unwrap();
    source.stop().unwrap();
    assert_eq!(source.state(), ComponentState::Stopped);
    let transitions: Vec<ComponentState> = source
        .drain_events()
        .into_iter()
        .filter_map(|event| match event {
            BackendEvent::StateChanged { state } => Some(state),
            _ => None,
        })
        .collect();
    // Exactly one Running and one Stopped transition despite duplicate calls.
    assert_eq!(
        transitions,
        [ComponentState::Running, ComponentState::Stopped]
    );
}

#[test]
fn identity_is_stable_and_kind_is_test_pattern() {
    let id = SourceId::new();
    GstRuntime::initialize().unwrap();
    let source = GstSourceBackend::new_test_pattern(id, serde_json::Value::Null).unwrap();
    assert_eq!(source.source_id(), id);
    assert_eq!(source.kind(), SourceKind::TestPattern);
    assert!(source.audio_levels().is_none());
    assert_eq!(source.state(), ComponentState::Stopped);
    assert!(source.output_pad().is_none(), "no pad before start");
}

#[test]
fn invalid_settings_matrix_is_rejected_without_partial_state() {
    let mut source = backend(serde_json::json!({
        "pattern": "smpte",
        "width": 640,
        "height": 360,
        "num_buffers": 8,
    }));
    let before = source.settings().clone();
    let bad = [
        serde_json::json!({"width": 0}),
        serde_json::json!({"width": 641}), // odd, I420 needs even geometry
        serde_json::json!({"width": 99999}),
        serde_json::json!({"height": -4}),
        serde_json::json!({"fps": 0}),
        serde_json::json!({"fps": 1000}),
        serde_json::json!({"pattern": "plaid"}),
        serde_json::json!({"width": "640"}),
        serde_json::json!({"surprise": 1}),
        serde_json::json!(17),
        serde_json::json!(["smpte"]),
    ];
    for payload in bad {
        assert!(
            matches!(source.update_settings(payload.clone()), Err(Error::InvalidInput(_))),
            "payload accepted: {payload}"
        );
        assert_eq!(source.settings(), &before, "state mutated by {payload}");
        assert_eq!(source.state(), ComponentState::Stopped);
    }
    // The backend is still fully usable afterwards.
    source.start().unwrap();
    let probe = attach_probe(&source.output_pad().unwrap());
    wait_for_event(&mut source, |e| matches!(e, BackendEvent::EndOfStream));
    assert_eq!(probe.buffers.load(Ordering::Relaxed), 8);
    source.stop().unwrap();
}

#[test]
fn invalid_update_while_running_keeps_streaming() {
    let mut source = backend(serde_json::json!({"num_buffers": 0}));
    source.start().unwrap();
    let probe = attach_probe(&source.output_pad().unwrap());
    while probe.buffers.load(Ordering::Relaxed) < 5 {
        std::thread::sleep(Duration::from_millis(5));
    }
    let before = probe.buffers.load(Ordering::Relaxed);
    assert!(matches!(
        source.update_settings(serde_json::json!({"fps": 0})),
        Err(Error::InvalidInput(_))
    ));
    assert_eq!(source.state(), ComponentState::Running);
    let deadline = Instant::now() + DEADLINE;
    while probe.buffers.load(Ordering::Relaxed) <= before && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        probe.buffers.load(Ordering::Relaxed) > before,
        "stream stalled after rejected update"
    );
    source.stop().unwrap();
}

#[test]
fn update_settings_while_running_applies_new_caps() {
    let mut source = backend(serde_json::json!({
        "width": 320,
        "height": 240,
        "num_buffers": 0,
    }));
    source.start().unwrap();
    let probe = attach_probe(&source.output_pad().unwrap());
    let deadline = Instant::now() + DEADLINE;
    while probe.caps.lock().unwrap_or_else(|p| p.into_inner()).is_none() && Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        probe
            .caps
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
            .unwrap()
            .to_string(),
        "video/x-raw, format=(string)I420, width=(int)320, height=(int)240, framerate=(fraction)30/1"
    );
    source
        .update_settings(serde_json::json!({
            "pattern": "zone-plate",
            "width": 640,
            "height": 360,
            "fps": 60,
            "num_buffers": 4,
        }))
        .unwrap();
    assert_eq!(source.state(), ComponentState::Running);
    assert_eq!(source.settings().pattern, TestPatternKind::ZonePlate);
    let probe = attach_probe(&source.output_pad().expect("relaunched output pad"));
    wait_for_event(&mut source, |e| matches!(e, BackendEvent::EndOfStream));
    assert_eq!(probe.buffers.load(Ordering::Relaxed), 4);
    assert_eq!(
        probe
            .caps
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
            .unwrap()
            .to_string(),
        "video/x-raw, format=(string)I420, width=(int)640, height=(int)360, framerate=(fraction)60/1"
    );
    source.stop().unwrap();
}

#[test]
fn mid_stream_error_surfaces_and_teardown_is_clean() {
    let mut source = backend(serde_json::json!({
        "num_buffers": 0,
        "error_after": 3,
    }));
    source.start().unwrap();
    let probe = attach_probe(&source.output_pad().unwrap());
    let events = wait_for_event(&mut source, |e| matches!(e, BackendEvent::Error { .. }));
    assert!(
        events
            .iter()
            .any(|e| matches!(e, BackendEvent::StateChanged { state: ComponentState::Failed })),
        "{events:?}"
    );
    assert_eq!(source.state(), ComponentState::Failed);
    assert!(probe.buffers.load(Ordering::Relaxed) > 0);
    // Error teardown is complete: stop() and drop() are clean, and a fresh
    // start() (the required recovery verb) relaunches the stream.
    source.stop().unwrap();
    assert_eq!(source.state(), ComponentState::Stopped);
    source
        .update_settings(serde_json::json!({"num_buffers": 6}))
        .unwrap();
    source.start().unwrap();
    let probe = attach_probe(&source.output_pad().unwrap());
    wait_for_event(&mut source, |e| matches!(e, BackendEvent::EndOfStream));
    assert_eq!(probe.buffers.load(Ordering::Relaxed), 6);
    source.stop().unwrap();
}

#[test]
fn settings_roundtrip_through_domain_payload() {
    // The same JSON shape lives in `Source::settings`; serialize the typed
    // settings and confirm the backend accepts its own output.
    let settings = TestPatternSettings {
        pattern: TestPatternKind::Checkers8,
        width: 1920,
        height: 1080,
        fps: 60,
        num_buffers: 0,
        error_after: None,
    };
    let payload = serde_json::to_value(&settings).unwrap();
    let source = backend(payload);
    assert_eq!(source.settings(), &settings);
}
