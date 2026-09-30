---
name: gstreamer-rust
description: Build, debug, and validate Prismcast gstreamer-rs media graphs, GTK paintable preview, audio mixing, encoding, recording, and independently recoverable streaming outputs.
---

# GStreamer media engineering

GStreamer is the planned primary engine. Read the affected backend interface and task before changing a graph. Inspect crate/system versions and use gst-inspect-1.0 to confirm actual element properties, pad templates, caps and installed plugins. Hardware and protocol availability is a runtime capability, not guaranteed by a Rust dependency.

- Keep graph mutation in the media control owner. Streaming callbacks and bus sync handlers must not mutate GTK or block on the UI/core. Forward bounded typed data/events to the appropriate owner.
- Handle state transitions, bus Error/EOS messages, dynamic pads and negotiation failures explicitly. Preserve diagnostic context without exposing streaming credentials.
- Model each source, encoder and output with lifecycle and cleanup ownership. Release request pads, signal handlers and retained buffers when branches are removed.
- Place queues deliberately at thread/output boundaries. Bound their buffers, bytes or time and choose explicit drop/backpressure behavior. A slow output must not stall unrelated outputs; separate queues alone do not guarantee isolation under every failure.
- Share encoded streams only when codec, profile, rate control, resolution, frame rate, color and GOP requirements match. Isolate each destination's reconnect and error state.
- Preserve timestamps, segments, clock relationships and audio/video synchronization. Test discontinuities and reconnects rather than repairing timing with guessed offsets.
- For recording, send EOS and wait for muxer finalization with a deadline before tearing down. Verify that resulting files decode and contain the intended tracks.
- Start preview research with gtk4paintablesink and GTK Picture/GdkPaintable per PLAN.md. Verify version-dependent GL/DMABUF paths, caps and observed copies; do not claim zero-copy from element names alone.

Prototype with deterministic test sources. Use GST_DEBUG and graph dumps for negotiation or state problems, then translate the experiment into typed Rust ownership and errors. Measure latency, queue growth and copies under slow consumers, output failure and shutdown.

## Official references

- [Application manual and plugins](https://gstreamer.freedesktop.org/documentation/)
- [Threading](https://gstreamer.freedesktop.org/documentation/application-development/advanced/threads.html)
- [Rust bindings](https://gitlab.freedesktop.org/gstreamer/gstreamer-rs)
- [Rust API](https://docs.rs/gstreamer/)
- [GTK4 sink](https://gstreamer.freedesktop.org/documentation/gtk4/index.html)
