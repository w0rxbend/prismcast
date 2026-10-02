# Current state

OBSWS-001 (obs-websocket 5.x compatibility adapter MVP) is implemented and
integrated on agent/OBSWS-001 via four worktree waves under kimi-code
orchestration, preceded by ADR-0020. WS-002 (challenge-response auth) and
CAPTURE-003 (V4L2 cameras) shipped before it on main. CAPTURE-002's
integrated live window preview remains UNVERIFIED — the longest-standing
follow-up, needing one user-coordinated picker selection (see below).

## Implemented (OBSWS-001)

- obs_ws module inside prismcast-remote (ADR-0020): second WS server,
  default 127.0.0.1:4455, disabled by default, obswebsocket.json subprotocol
  only (others refused with HTTP 400; msgpack deferred), obs close codes,
  rpcVersion 1 only (4010 otherwise), Reidentify updates subscriptions.
- Auth reuses WS-002 verbatim: obs Identify.authentication string verifies
  through AuthConfig::authenticate/challenge_response; AllowLocal rejected at
  bind; token accepted as bearer (adapter extension).
- 43 request types (GetVersion with drift-guarded availableRequests, scenes,
  scene items incl. transform merge via Transactions, studio mode, inputs
  with volumeMul↔dB conversion, transitions, outputs by name, stream/record
  singletons → primary outputs: first Rtmp else Srt/Whip, first Recording,
  absent → 600/501) pivot through native RequestKind → map::command_from_wire
  → dispatch_with_permissions; queries read snapshots. Name→ID resolution is
  stateless snapshot scan (duplicates: first match + warn); numeric
  sceneItemId comes from the server-wide eviction-tracked ItemIdMap shared by
  requests AND events.
- Domain events translate through a per-session EventTranslator (memoized
  names/numbers, seeded at identify/reidentify) into obs Events with correct
  eventIntent, gated by the subscription bitmask. Removal events resolve IDs
  memo-first because request-driven removals evict eagerly.
- Batch: serial execution, haltOnFailure, bounded Sleep (standalone accepted,
  documented divergence), SerialFrame/Parallel → whole-batch 206.
- docs/protocols/obs-websocket-adapter.md documents the full mapping tables
  and divergences (OutputStateChanged extension event, Failed→STOPPED,
  outputPath null, mul 0 → −100 dB, adapter-specific kind strings, gating
  granularity).

## Evidence

just ci green on the integrated branch (fmt, clippy -D warnings, workspace
tests, deny). 158+ prismcast-remote tests: golden serde fixtures, real-socket
handshake/close-code matrix, request families, event gating, request/event
sceneItemId cross-consistency, and 4 conformance tests with the real obws
0.15 client (handshake+auth, 4009 rejection, scene list/program switch, mute
toggle — all asserted against AppHandle state). Conformance caught and fixed
an upstream-incompatible response field (inputMuted). obws needs
DangerousConnectConfig::skip_studio_version_check because obsVersion reports
the crate version — OBS-shaped advertisement is an OBSWS-002 consideration.
Native protocol schema untouched; golden tripwires green.

## Next and remaining limits

Next tasks: OBSWS-002 (MessagePack, SerialFrame/Parallel, meter events after
a producer exists, filters/stats/screenshots, OBS-shaped version
advertisement) or WS-003 (TLS wss:// + non-loopback bind). CAPTURE-002 picker
retry still pending user coordination: run the raw consumer probe
`actual_window_capture_consumer_frames_show_fixture_pixels` then the
integrated `actual_window_capture_preview_pixels_placement_and_shutdown`
(GDK_BACKEND=wayland GSK_RENDERER=cairo G_DEBUG=fatal-criticals, --ignored
--nocapture --test-threads=1, separate processes), selecting the window
titled "Prismcast capture test target – select this window" (small flashing
red/blue), NOT "Prismcast capture preview" or a maximized window. Monitor/
KDE/X11 capture, camera unplug UX, and UI live camera preview remain
unverified. Prior overlapping edits stay archived on
archive/paused-agent-phase2 (2ce3390). Worktrees .worktrees/obsws-001-*,
ws-002-*, capture-003-* remain reviewable. Never persist grants/FDs/secrets.
