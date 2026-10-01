# Native test-pattern source

`cargo test -p prismcast-media-gst` exercises real GStreamer buffers without a
window or audio device. Requires GStreamer >=1.26 core development libraries
and runtime coreelements + videotestsrc (plugins-base).

`GstTestPatternSource::new(SourceId, settings)` implements `SourceBackend`.
An empty settings object uses 1920×1080 RGBA, 30 fps, square pixels, SMPTE bars,
and indefinite live production. Supported patterns are smpte, black, white,
red, green, blue, ball. Width/height range is 1..8192; fps 1..240; num_buffers
is null or a positive signed-32-bit count. Unknown fields are rejected.
`settings_schema()` exposes these defaults and limits. Invalid changes leave
both the old settings and graph intact. Valid changes build a replacement
before stopping the original; a running original is restarted if replacement
startup fails. Failed rollback changes health to Failed and emits an error.

The standalone backend drains into fakesink and is useful for source lifecycle
checks. Compositors instead use `build_test_pattern_bin(&runtime, source_id,
&settings)`, a stopped bin with a single `src` ghost pad. Build it once per
shared SourceId, then fan out via tees and bounded branch queues. The builder
has no queue of its own; its parent graph controls scheduling and lifetime.

Only the media owner thread calls graph methods. Bus sync callbacks inspect
ERROR/EOS, forward through an eight-slot nonblocking terminal channel, and drop
native bus messages so its queue cannot grow. The control event queue retains
the latest 32 events. Draining terminal events stops the pipeline; state() is
the last observed state, so owners must call drain_events regularly. Start and
stop are idempotent, and finite sources restart after EOS. Dropping a graph
sets NULL and removes its bus handler.

Tests count eight negotiated 64×48 RGBA buffers with timestamps, observe EOS,
restart for another eight frames, reject malformed settings while running,
replace settings, inject a native bus error, and bound repeated lifecycle
notifications. Lifecycle tests also inject an actual identity streaming error.
GUI/compositor coverage belongs to subsequent tasks.
