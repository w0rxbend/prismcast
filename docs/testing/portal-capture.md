# Portal capture foundation (CAPTURE-001)

The foundation deliberately has no Core/UI/compositor wiring. `CaptureBroker`
requires an entered Tokio runtime; authorization explicitly opens a portal picker.
`PendingCapture.wait` yields an ephemeral `CaptureLease`; dropping either cancels
its independent worker. Status is latest-value bounded watch; `close().await`
returns a separate cleanup Result. Shutdown stops all workers. Revoked leases
retain their original FD until dropped and continue consuming capacity; at most
four default leases and one native probe per lease exist.

Headless checks:

```sh
cargo test -p prismcast-capture
just ci
just deny
```

Mocks exercise ordered calls, dropped caller futures, cancellation at create/select/
start/open-remote, permission failure, early/active closure, timeout, native stop
ordering, cleanup failure acknowledgement, retained FD limits and EOF cleanup.
These tests do not prove behavior of an actual portal backend. A native headless
videotestsrc probe checks negotiated RGBA 48x32 dimensions, timestamps, bytes,
checksum and NULL teardown. A separate pipewiresrc NULL-construction test verifies
installed property types and enum nick without contacting PipeWire or a portal.

Opt-in actual GNOME Wayland capture (manual picker selection required):

```sh
cargo test -p prismcast-capture actual_portal_capture_frames_and_session_teardown -- --ignored --nocapture --test-threads=1
PRISMCAST_CAPTURE_KIND=window cargo test -p prismcast-capture actual_portal_capture_frames_and_session_teardown -- --ignored --nocapture --test-threads=1
```

The test authorizes exactly once (120s deadline), obtains three native frames in a
15s probe on a blocking worker, then destroys the graph and closes the session.
Evidence prints actual negotiated pixel size, timestamp range, byte count and
checksum. Selection cancellation is reported as cancellation, never retried.
No monitor/window integration claim is made before those manual tests succeed.
The picker has no parent-window handle in this GTK-independent foundation; window
export belongs to CAPTURE-002. CPU probe supports negotiated dimensions <=8192,
SystemMemory RGBA conversion and two appsink buffers; this establishes no DMA-BUF
or end-to-end zero-copy result. Session revocation can happen externally before
native teardown; cooperative voluntary shutdown waits for the probe to stop,
with a bounded forced-revocation fallback if native calls wedge.

## Recorded evidence (2026-10-01)

`just ci` and `just deny` passed on agent/CAPTURE-001. Capture suite: 14 passed,
one actual-portal test ignored. Independent read-only ownership review found no
remaining blockers for the scoped foundation. Actual monitor/window portal frame
selection has not yet run; this headless result establishes no user-granted stream.
