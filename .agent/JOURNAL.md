# JOURNAL

Append-only development log. Newest entries at the bottom.

---

2026-09-30 BOOTSTRAP

Repository initialized. PLAN.md committed. AGENTS.md and .agent/ infrastructure created.
Decision: crate names use the `prismcast-*` scheme from the end of PLAN.md; the `studio-*`
sketch in §30 is treated as superseded.

---

2026-09-30 RES-003

Wrote docs/research/gstreamer-capabilities.md (gstreamer-rs 0.25.4 / GStreamer 1.28.7 stable,
1.30 due Q4 2026). Key findings: compositor/glvideomixer/vacompositor share the same
xpos/ypos/width/height/alpha/zorder pad vocabulary → one backend abstraction works.
gtk4paintablesink direct DMA-BUF needs GTK ≥ 4.14 + crate `dmabuf` feature. fmp4/mp4 Rust
plugins merged into `isobmff` in 1.28; gst-plugins-rs is NOT packaged by Ubuntu — we must
build it ourselves. whipsink deprecated in favour of whipclientsink; srtsink connection-key
is 1.30-only. pipewiresrc has known renegotiation/keepalive issues (PipeWire #3147/#3149).

2026-09-30 SKILL-001

Installed 30 project-local Rust, Relm4/GTK4, GStreamer, Linux capture, FFmpeg and packaging skills. Shared relative symlinks expose the set to Claude, Codex and Kimi. Preserved upstream revisions and licenses; six integration skills are locally authored. All skill metadata and local entrypoint links validated; just ci passed. See .agents/SKILLS.md.

2026-09-30 RES-007

Wrote docs/research/obs-websocket-protocol.md. Key findings: OBS 32.2.2 baseline ships obs-websocket
5.7.4 with rpcVersion 1 (verified via submodule pin + CMakeLists); master docs match released behavior,
no 33.x divergence. Protocol shape (Hello/Identify/Identified, {op,d} envelope, batch, bitmask event
subscriptions, JSON+MsgPack via Sec-WebSocket-Protocol, SHA-256 challenge auth) confirmed from source.
Noted weaknesses to avoid natively: name-based addressing, coarse subscriptions, no backpressure, no
schema discovery, non-transactional batches, singleton stream/record outputs (adapter maps these to
primary outputs per ADR-0010). Handed 5 open questions to ARCH-007/Phase 9.

## 2026-09-30 — RES-002 (OBS architecture research)

Wrote docs/research/obs-architecture.md against the 32.2.2 tag + docs.obsproject.com (master/33.x flagged separately).
Key findings: libobs = registry + 3 threads (graphics/video-io/audio-io) + 6 object vtables (source/output/encoder/service
+ scene/canvas); scene-is-source recursion, scene items carry transform/crop/bounds/blend; audio fixed at 6 mixes/8 channels
with 1024-frame ticks and Pulse monitoring; video path drops by duplicating frames at a 16-deep cache. Frontend (Qt) owns
scene collections/profiles/studio mode/undo — the exact split we reject; obs-websocket sits on that frontend API. OBS has no
native multistreaming (single streaming output hardcoded; multitrack video 30.2+ is one-destination quality ladders); canvas
API is self-declared unstable. Conclusions validate ADR-0004/0005/0007/0009; flagged audio bus model + canvas deferral as
open questions for ARCH-001/002.

## 2026-09-30 — RES-001 (OBS feature inventory)

Wrote docs/research/obs-feature-matrix.md (baseline OBS 32.2.2, latest stable as of 2026-08-14; 33.x = development).
Verified against release notes 30.2→32.2.2 plus the 32.2.2 source tree: full plugin/source/filter/transition/output
enumeration from per-plugin CMakeLists, obs-websocket 5.7.4 and CEF Chromium 127 pins confirmed via submodule SHAs.
Key findings: no multistream/per-app-audio/game-capture in OBS on Linux; Hybrid MP4/MOV default since 32.0; mixer
desync + audio dedup bug classes (fixed 32.0/32.2) validate our command/event core; Sept-2026 SCRT disclosure shows
unsandboxed Chromium 127 RCE in 32.2.2 (CEF 128+ fix targets 33.0) — do not commit to CEF, feed into RES-006 ADR.

2026-09-30 RES-004

Wrote docs/research/linux-capture.md (xdg-desktop-portal ScreenCast v1-v6, PipeWire, V4L2, X11 fallback).
Key findings: ScreenCast v6 (pipewire-serial stream property, node IDs deprecated for targeting)
landed in frontend 1.21.2 (2026-05) but only KDE master (Plasma 6.8) implements it; GNOME 51 still
v5, KDE stable 6.7.x v4. Decision: target streams via target-object with serial when portal
version >= 6, node ID otherwise; persist restore_token (single-use, rotate on every Start,
persist_mode=2 like OBS 32.2.2) plus stream id; never persist node IDs. Region crop belongs to the
scene graph (no portal region source type). Recommended stack: ashpd 0.13 + GStreamer pipewiresrc
(on-disconnect=error for recovery); ximagesrc only as legacy X11 fallback. Flagged ADR candidates
(portal-first capture, serial targeting, per-app PipeWire audio as an OBS-beating capability).

## 2026-09-30 — RES-005 (Encoder capability matrix)

Wrote docs/research/encoder-matrix.md. Verified against GStreamer 1.28.7/1.26 release notes, per-plugin docs
(nvcodec/va/qsv/svtav1), intel/media-driver feature table, OBS 32.2.2 plugins tree, gstreamer-rs 0.25.4 crates.
Key findings: `va` plugin is the only VA-API path (gstreamer-vaapi removed in 1.28); nvav1enc (1.26) takes
CUDA/GL/sysmem directly — zero-copy from GL compositor; va*enc sinks advertise VAMemory+sysmem only, so
vapostproc is the DMABuf import boundary; vavp9enc (1.26) and vaav1lpenc are driver-conditional and missing
from generated docs — runtime probing mandatory. AV1 HW encode: NVIDIA Ada+, Intel DG2/Arc+, AMD RDNA3/VCN4
(Mesa 23.1+); no HW VP9 encode except Intel. Recommended floors: GStreamer 1.26 (practical), 1.28 target.
Flagged ADR candidates: backend selection policy + shared-encoder multistream rule + minimum GStreamer version.

## 2026-09-30 — RES-006 (Browser-source research: WebKitGTK 6 / WPE / CEF)

Wrote docs/research/browser-source.md. Verified against webkitgtk.org 6.0 API docs, WPE architecture docs,
GStreamer 1.28/1.26 release notes, gst-plugins-bad 1.29.2 ext/wpe2 source (Debian), lib.rs webkit6 0.6.1,
OBS 33.0 dev release notes, and two WebKitGTK OBS-plugin prior arts. Key finding: WebKitGTK 6 has NO public
frame-export API (DMA-BUF renderer is internal; snapshot() is thumbnail-grade, GTK4 removed offscreen
surfaces) — browser frames must come from WPE, not the GTK widget. Decision: browser source = GStreamer
`wpevideosrc2` (wpe2 plugin, stable since GStreamer 1.28, WPEPlatform API, WPEBuffer→EGLImage→GLMemory,
GL RGBA / raw BGRA caps, no DMABuf caps, no audio yet, no Rust WPE bindings). Legacy wpesrc sits on
WPEBackend-FDO which WPE 2.54 declared legacy — avoid. CEF rejected (EOL Chromium 127 in released OBS
32.2.2, vendored-binary tax, no GstBuffer integration). Flagged ADR: browser-source engine + audio strategy.

## 2026-10-01 — ARCH-001/ARCH-002 (Domain model + Command/Event API in prismcast-core)

Implemented the full domain model and the pure Command/Event core in `crates/prismcast-core/`:
modules `source`, `scene` (with `Canvas` stub + new `CanvasId` newtype), `audio`, `output`,
`transition`, `project`, `command`, `event`, `state`. `AppState` is IndexMap-backed; `apply()`
validates preconditions, mutates, and returns strongly-typed `Event`s (Scene/Source/Audio/Output/
System sub-enums per PLAN §58). Everything serde-roundtrips; no GTK/GStreamer/Tokio deps.
100 unit tests, incl. a PLAN §67 milestone scenario and inverse-roundtrip tests.

Key decisions:

- **Audio bus matrix (RES-002 open question, resolved):** OBS's 6 fixed global mixes are NOT
  copied. Unbounded set of named `AudioBus`es; `AudioRoute { source_id, bus_id, tracks }` with
  `TrackMask` = bitset over u32 for per-bus output-track assignment; tracks are a muxer-level
  mapping, not a global mix mask. Documented in `audio.rs` module docs.
- **Delete policy: reject-by-default, cascade only where obvious.** Removing a source referenced
  by scene items or audio routes, a scene used by studio mode or a scene source, the active
  profile/collection, the last scene/bus, or a non-Stopped output is rejected with typed errors.
  Only cascades: `RemoveAudioBus` drops its routes (explicit `RouteRemoved` events) and
  `RemoveSource` drops its orphaned mixer entry (`MixerChanged` with defaults).
- **Undo approach:** `AppState::inverse(&cmd) -> Option<Command>` reconstructs inverses from the
  pre-application state (snapshot-free). Mutations invert exactly; creations/destructions/duplicates
  return `None` (irreversible — a later undo service may snapshot for those). `Command::Transaction`
  is the PLAN §59 transaction group: atomic apply via scratch-copy, inverse = reversed member
  inverses. Raise/lower invert to absolute z-index restores.
- **Output lifecycle:** `StartOutput` legal only from `Stopped`/`Failed` (→ `Starting`);
  `StopOutput` from `Starting`/`Running`/`Degraded`/`Reconnecting` (→ `Stopping`). Mid-lifecycle
  transitions (Running, Reconnecting, Failed, ...) will come from media-layer commands/events in
  later tasks; the domain store is their pure anchor.
- `TransitionToProgram` emits `SystemEvent::TransitionStarted` for non-Cut transitions; names are
  unique-ified OBS-style (`"cam (2)"`); visibility toggles are allowed on locked items (OBS
  behavior), transforms/crop/z-order are not.
- `SecretString` newtype: serializes for persistence, Displays/Debugs as `[REDACTED]`.

Validation: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
`cargo test -p prismcast-core` (100 passed) and `cargo test --workspace` all green. Not committed
(per orchestrator instructions).
