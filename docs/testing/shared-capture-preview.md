# CAPTURE-002 media integration checks

`prismcast-capture::producer` retains an independent native producer and ephemeral
lease across consumer graph rebuilds. CAPTURE-002 consumes the exclusive application
capture effect receiver on the preview media OS thread. It opens a picker only for
an admitted Core Command; restored sources have no grant and render no capture.

Headless evidence checks genuine videotestsrc/appsink/appsrc frames across two
consumer pipeline epochs, preserved producer PLAYING state, rebased timestamp
differences, missing/backwards PTS resets, exact negotiated caps, bounded queues,
actual frame byte limits and retired endpoint identity. Mixed compositor pixels
show pending capture as black beside a blue TestPattern, active capture as red,
visibility and duplicate placements sharing the producer, and capture removal
without hiding the generator. Owner tests exercise restored-state silence, retry
cancellation, delayed generation errors, removal/disable and bounded diagnostics.
All frame bridges keep one latest sample/one appsrc queue; byte budget is128 MiB,
SystemMemory RGBA with preserved buffer video metadata, without per-placement copies.

The media owner's Tokio runtime enables I/O for ashpd. All graph changes run after
block_on returns; callbacks never touch GTK/Core. Snapshot cancellation precedes
request admission. Native bus events drain before graph mutation can retire them.
Producer/consumer NULL cleanup precedes voluntary portal Close; terminal reports
that time out are retained in a bounded8-entry queue until acknowledged or stale.
A failed capture consumer graph clears stale frames and exposes PreviewStatusFailed.

Opt-in integrated GNOME window test (separate GTK process):

```sh
cargo test -p prismcast-preview actual_window_capture_preview_pixels_placement_and_shutdown -- --ignored --nocapture --test-threads=1
```

Select **Prismcast capture test target – select this window** in the one portal
picker. The target alternates red/blue. The test checks Core Active with negotiated
pixel dimensions, native sink frames, actual paintable red/blue pixels, hidden black,
visible recovery after rebuild with the same grant generation/caps, source removal
and orderly preview/native/lease shutdown. Test cleanup runs even if selection or
pixel assertions fail. Parent context is None in this service-level test; the UI
suite separately verifies exported GTK context passed through an ephemeral command
envelope. No automated picker retry or other-app capture is performed.

Live selection evidence must be recorded separately; the earlier CAPTURE-001 actual
window probe establishes6144x3456 frames, not this new preview integration.

## Recorded headless evidence (2026-10-01)

Task-local `just ci` and `just deny` passed after the service/producer/compositor
changes. Three new bridge tests, mixed compositor pixel evidence, two owner
interleaving tests and the shared pure dimension test passed. Independent read-only
media lifecycle review found no remaining blockers after the common pre-mutation
bus drain fix. The integrated Window fixture test compiled and remains opt-in;
no portal dialog was opened during this agent's checks.

Timestamp follow-up additionally tests nonzero TIME start/base/time conversion,
rejection of non-unit applied/playback rates, byte segments, default TIME segments and out-of-range
PTS. Persistent native producer tests still exercise actual appsink TIME segments.
Consumer construction rechecks active state under the endpoint installation lock,
preventing a concurrently retired public feed from attaching a fresh endpoint.
