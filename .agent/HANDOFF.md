# Current state

OBSWS-002 (obs-websocket adapter hardening) and WS-003 (native WebSocket
TLS) are implemented, integrated, and shipped to main via parallel
worktree swarms under kimi-code orchestration, from a single scoping pass
that produced a zero-overlap file partition between the two tasks.
CAPTURE-002's integrated live window preview remains UNVERIFIED — the
longest-standing follow-up, needing one user-coordinated picker selection
(see below).

## Implemented (OBSWS-002, ADR-0021)

- MessagePack subprotocol (obswebsocket.msgpack): negotiation (JSON wins
  when both offered, unknown-only refuses HTTP 400), binary frames via
  rmp-serde to_vec_named (already a dependency — zero new crates), codec
  at the session framing boundary (obs_ws/codec.rs), 7 byte-pinned golden
  fixtures, cross-codec frames close 4002, hostile payloads never panic.
- RequestBatch executionType 1 (SerialFrame): serial + haltOnFailure,
  Sleep.sleepFrames from the active profile's fps (60 fps default,
  fps_num==0 guarded, 50 s cap). executionType 2 (Parallel): bounded
  JoinSet (cap 8) over dispatch_with_permissions, request-order results,
  haltOnFailure ignored (upstream semantics), rate-limiter token consumed
  per member before spawn. The whole-batch 206 fallback is removed.
- OBS-shaped version advertisement: obsVersion "30.2.0" compat constant
  (obws >= 30.2 gate) — obws 0.15 conformance connects with NO
  DangerousConnectConfig skip flags.
- BroadcastCustomEvent → CustomEvent: bounded (64) server-wide broadcast
  bus in SharedServices, General-bit gating, originator included, works
  inside all batch modes, drift guard unedited (required eventData → 300).

## Implemented (WS-003, ADR-0022)

- Server TLS (wss://): rustls 0.23 (ring, no aws-lc-rs) + tokio-rustls;
  WsServerConfig.tls with provided PEM paths (leaf-first chain, first key
  found via rustls-pki-types PemObject — rustls-pemfile avoided,
  RUSTSEC-unmaintained); acceptor built once at bind, per-connection TLS
  handshake in the spawned task under a 10 s bound; ws.rs stream types
  generalized over S: AsyncRead+AsyncWrite. Policy: non-loopback bind
  without TLS → typed WsError::TlsRequired; AllowLocal rejection unchanged.
- Client wss: WsClient::connect_url (ws:// and wss://, other schemes typed
  error before I/O), ClientTlsConfig { extra_ca_path,
  danger_accept_invalid_certs } (native roots via rustls-native-certs,
  partial-store failures warn+continue, danger mode warn-logged); token
  and password auth matrix re-verified over wss.
- CLI: --url ws(s):// (conflicts --socket, IPC stays default), --tls-ca /
  --insecure scoped to wss (clap + transport_plan double enforcement),
  CliClient enum dispatch, --insecure prints a stderr warning, secrets
  never in errors, subprocess e2e over a real TLS server.
- Integration fix: tokio-tungstenite rustls-tls-native-roots moved to
  prismcast-remote's MAIN dependency (wss arm previously only compiled
  under dev-feature unification — caught via cargo check -p standalone).
- obs-websocket adapter stays plaintext (upstream has no wss; reverse
  proxy is the ecosystem pattern). remote.toml untouched.

## Evidence

just ci green on both integration branches and on the combined main tree
(fmt, clippy -D warnings, workspace tests, deny). 188+ prismcast-remote
tests (msgpack fixtures/session/hostile-payload, batch mode matrices,
custom-event bus, wss session/auth/client-trust matrices), 37
prismcast-cli tests (flag matrix + real wss subprocess e2e). Native
protocol golden tripwires untouched. Stats/screenshots/meter events remain
deferred — no producer exists (verified: OutputGraph stats not wired to
the app snapshot, no compositor frame access, no meter producer);
AUDIO-001 is the unblock.

## Next and remaining limits

Next tasks: AUDIO-001 (audio mixer/meter foundation — unblocks obs meter
events and GetStats) or CAPTURE-004 (PipeWire audio input/output/
application capture, completes PLAN Phase 3). CAPTURE-002 picker retry
still pending user coordination: run the raw consumer probe
`actual_window_capture_consumer_frames_show_fixture_pixels` then the
integrated `actual_window_capture_preview_pixels_placement_and_shutdown`
(GDK_BACKEND=wayland GSK_RENDERER=cairo G_DEBUG=fatal-criticals, --ignored
--nocapture --test-threads=1, separate processes), selecting the window
titled "Prismcast capture test target – select this window" (small
flashing red/blue), NOT "Prismcast capture preview" or a maximized window.
Monitor/KDE/X11 capture, camera unplug UX, and UI live camera preview
remain unverified. Worktrees .worktrees/obsws-002-*, ws-003-* remain
reviewable. Never persist grants/FDs/secrets; test certs are rcgen-
generated per test into tempdirs, nothing committed.
