# Current state

WS-002 (challenge-response auth + protocol consistency) is implemented and
integrated on agent/WS-002 via parallel worktree agents under kimi-code
orchestration. CAPTURE-003 (V4L2 cameras) shipped before it on main
(379b6e9). CAPTURE-002's integrated live window preview remains UNVERIFIED
and is the longest-standing follow-up; a raw consumer-frame diagnostic is
armed for it.

## Implemented (WS-002)

- AuthConfig::Password { password, salt, permissions }: per-server salt,
  per-session challenge advertised in Hello.authentication; obs-websocket
  SHA-256 construction (base64(sha256(base64(sha256(password+salt)) +
  challenge))) in one shared helper (auth.rs challenge_response). Wrong/
  missing responses close 4009 via existing machinery. remote.toml `password`
  key; password+token together is a parse error. AuthConfig Debug redacts
  secrets. Non-constant-time comparison documented (localhost scope).
- ClientAuth enum { None, Token(String), Password(String) } in
  prismcast-remote (client.rs, re-exported at root); IpcClientConfig and
  WsClientConfig gained `auth` while the legacy `token` field remains with
  auth-wins-unless-None resolution, so 21 pre-existing integration tests are
  literally unchanged. Clients answer the Hello challenge; absent challenge
  with Password config is a client-side decode error.
- prismcast-cli: --token/--password flags with PRISMCAST_TOKEN/
  PRISMCAST_PASSWORD env fallbacks (hide_env_values), mutual exclusion is a
  usage error (exit 2), secrets never printed.
- native-protocol.md documents implemented challenge-response, per-transport
  frame limits (IPC 4 MiB trusted local 0600 socket; WS 1 MiB untrusted
  network), and the pre-identify invalid-subscription close (no request ID
  exists to answer). No protocol schema change; golden tripwire intact.

## Evidence

just ci green on the integrated branch (fmt, clippy -D warnings, workspace
tests, deny — sha2/base64/rand admitted; rand duplicate-version warn allowed).
New: 6 auth unit tests (known vector, salt stability, challenge freshness,
failure matrix, toml parse, redaction), 6 IPC + 7 WS auth integration tests,
3 CLI e2e tests over real sockets (password/token success, wrong/missing auth
4009, no secret leakage). No live hardware or external validation needed.

## Next and remaining limits

Immediate: user-coordinated picker retry for the CAPTURE-002 integrated
window preview (diagnostic armed; select the small flashing red/blue window
titled "Prismcast capture test target – select this window"). Next tasks:
OBSWS-001 obs-websocket adapter (ADR-0010; unblocked now that auth exists) or
WS-003 TLS wss:// + non-loopback bind. Deferred: meter event producer
(media/audio layer; throttling machinery unexercised), broadcaster
unsubscribe API (retires server-wide fanout workaround), msgpack subprotocol,
general-category event producers, admin kick/session listing. Monitor/KDE/X11
capture validation and camera unplug UX remain unverified. UI live camera
preview pixels with real hardware are unverified end-to-end (headless and
probe evidence only). Wire-exposed camera device listing needs a protocol
schema change (follow-up). Prior overlapping edits remain archived on
archive/paused-agent-phase2 (2ce3390); do not reapply wholesale. Worktrees
.worktrees/ws-002-* and .worktrees/capture-003-* remain reviewable. Never
persist grants/FDs/device sessions.

## Implemented (CAPTURE-003)

- AuthorizeSourceCapture reused unchanged as an explicit open-device effect for
  SourceKind::V4l2Camera (ADR-0019): user-initiated, generation-guarded via
  transient SourceRuntime, never replayed by restore/undo/transactions, no
  portal session/lease/FD/parent window. Persisted identity is only the
  validated device path setting (absolute, /dev/, <=255 bytes, no control
  chars). Core admission, actor report gate and preview owner admit the kind.
- prismcast-capture devices.rs: one-shot GstDeviceMonitor enumeration
  (show-all-devices(true) is MANDATORY — PipeWire hides the v4l2 provider on
  target desktops) plus VideoDeviceMonitor publishing bounded watch snapshots
  via timed_pop polling on a dedicated thread; extraction is a pure function.
  camera.rs: validate_device_path, build_v4l2_source with probe.rs-style
  property checks, CameraSession with pre-open ENOENT/EACCES/EBUSY mapping and
  a dedicated OS thread FrameProducer; blocking close with native-first order.
- media-gst compositor capture placement is feed-keyed (was portal-kind-gated;
  audit disproved the plan's kind-agnostic assumption) with a regression test
  verified by negative control.
- Preview capture owner drives portal leases and camera sessions behind
  OpenedCapture/NativeCapture arms sharing pending/active/report bounds,
  prune/retire/shutdown ordering and the 10s no-frame timeout. Camera open is
  synchronous on the media owner thread; only completion delivery rides Tokio.
- UI camera creation with async discovered-device picker (worker thread +
  GLib oneshot delivery; GTK never blocks), kind-aware labels ("Start
  Camera"/"Opening camera…"), authorize/retry reuse; cameras never export a
  parent window. One-shot list + manual refresh; no live hotplug wiring.

## Evidence and live limitations

Combined just ci passed on the integrated branch (fmt, clippy -D warnings,
workspace tests, deny). Live real-camera probe passed on the integrated
branch: PRISMCAST_CAMERA_DEVICE=/dev/video2, Anker PowerConf C200, 640x480, 4
frames, clean teardown. Live discovery enumerated the camera. Camera USB link
flapped during validation and nodes renumbered (video1/2 -> video2/3): typed
busy/unplug mapping exists, live unplug UX untested, sysfs/serial-stable
identity is a follow-up.

CAPTURE-002 integrated window preview: three picker grants captured windows
that were NOT the fixture (white, dark-brown and pure-black composites at a
constant 6144x3456 geometry, frames flowing at ~60fps); a fourth attempt saw
no selection and timed out cleanly. Diagnosis instrumentation is in place on
main: composite PNG dump + frame count on pixel-check failure, and an opt-in
raw consumer probe `actual_window_capture_consumer_frames_show_fixture_pixels`
that dumps the actual captured frame to target/tmp/capture-raw-frame.png.
Run both opt-in tests with GDK_BACKEND=wayland GSK_RENDERER=cairo
G_DEBUG=fatal-criticals, -- --ignored --nocapture --test-threads=1, in
separate processes from other GTK tests. The user must select the window
titled "Prismcast capture test target – select this window" (small flashing
red/blue 480x270), NOT "Prismcast capture preview" and not a maximized window.
Do not open another dialog without user confirmation.

