use gstreamer::{self as gst, prelude::*};
use prismcast_media_gst::{GstError, GstRuntime};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

// Test owner guarantees cleanup even when assertions fail.
struct Graph(gst::Pipeline);
impl Drop for Graph {
    fn drop(&mut self) {
        let _ = self.0.set_state(gst::State::Null);
    }
}

fn graph(fail: bool) -> (Graph, Arc<AtomicUsize>) {
    let runtime = GstRuntime::initialize().unwrap();
    for factory in ["videotestsrc", "identity", "fakesink"] {
        runtime.require_factory(factory).unwrap();
    }
    let pipeline = gst::Pipeline::new();
    let source = gst::ElementFactory::make("videotestsrc")
        .property("num-buffers", 8_i32)
        .build()
        .unwrap();
    let identity = gst::ElementFactory::make("identity")
        .property("error-after", if fail { 3_i32 } else { -1_i32 })
        .build()
        .unwrap();
    let sink = gst::ElementFactory::make("fakesink")
        .property("sync", false)
        .build()
        .unwrap();
    pipeline.add_many([&source, &identity, &sink]).unwrap();
    gst::Element::link_many([&source, &identity, &sink]).unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    let observed = count.clone();
    sink.static_pad("sink")
        .unwrap()
        .add_probe(gst::PadProbeType::BUFFER, move |_, _| {
            observed.fetch_add(1, Ordering::Relaxed);
            gst::PadProbeReturn::Ok
        });
    (Graph(pipeline), count)
}

#[test]
fn finite_pipeline_delivers_buffers_then_eos_and_stops() {
    let (graph, count) = graph(false);
    graph.0.set_state(gst::State::Playing).unwrap();
    let terminal = graph
        .0
        .bus()
        .unwrap()
        .timed_pop_filtered(
            gst::ClockTime::from_seconds(5),
            &[gst::MessageType::Eos, gst::MessageType::Error],
        )
        .expect("pipeline deadline");
    assert!(
        matches!(terminal.view(), gst::MessageView::Eos(_)),
        "{terminal:?}"
    );
    assert_eq!(count.load(Ordering::Relaxed), 8);
    graph.0.set_state(gst::State::Null).unwrap();
    assert_eq!(graph.0.current_state(), gst::State::Null);
}

#[test]
fn streaming_error_still_allows_null_teardown() {
    let (graph, count) = graph(true);
    graph.0.set_state(gst::State::Playing).unwrap();
    let terminal = graph
        .0
        .bus()
        .unwrap()
        .timed_pop_filtered(
            gst::ClockTime::from_seconds(5),
            &[gst::MessageType::Eos, gst::MessageType::Error],
        )
        .expect("pipeline deadline");
    assert!(matches!(terminal.view(), gst::MessageView::Error(_)));
    assert!(count.load(Ordering::Relaxed) > 0);
    graph.0.set_state(gst::State::Null).unwrap();
    assert_eq!(graph.0.current_state(), gst::State::Null);
}

#[test]
fn inventory_and_missing_optional_factory_are_explicit() {
    let runtime = GstRuntime::initialize().unwrap();
    assert!(runtime.capabilities().version >= (1, 26, 0, 0));
    assert!(runtime
        .capabilities()
        .plugins
        .iter()
        .any(|p| p.name == "coreelements"));
    assert!(runtime
        .capabilities()
        .element_factories
        .iter()
        .any(|f| f == "fakesink"));
    assert!(
        matches!(runtime.require_factory("prismcast_nonexistent_factory"),
        Err(GstError::MissingFactory { factory }) if factory == "prismcast_nonexistent_factory")
    );
    assert_eq!(
        runtime.capabilities(),
        GstRuntime::initialize().unwrap().capabilities()
    );
}
