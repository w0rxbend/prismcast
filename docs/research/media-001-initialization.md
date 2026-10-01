# MEDIA-001 initialization research

Inspected upstream Rust [GStreamer API](https://docs.rs/gstreamer/0.25.2/gstreamer/)
and the [initialization manual](https://gstreamer.freedesktop.org/documentation/application-development/basics/init.html).
`gst::init()` is fallible and repeatable. `Registry::get().plugins()` and
`features(ElementFactory::static_type())` enumerate registered capabilities.
`ElementFactory::find` resolves requested factories. Graph lifetime is separate:
PLAYING starts streaming threads, the bus reports ERROR/EOS, NULL stops the graph.
Finite tests use pad probes to count actual buffers, a bounded bus wait, and
explicit NULL plus drop cleanup. Tests require only videotestsrc, identity,
fakesink; they need no window, audio device, encoder, or network.

Observed pkg-config GStreamer core/video/audio 1.28.2, rustc 1.98.1.
Dependency manifests set gstreamer/glib/GTK MSRV 1.92 and Relm4 MSRV 1.93.
The declared 1.93 minimum is dependency-derived; actual checks currently run on
installed stable 1.98.1, not a claimed 1.93 toolchain validation.

Cargo resolved gstreamer 0.25.4; its downloaded source and manifest confirm
the same API family and Rust 1.92 requirement.
