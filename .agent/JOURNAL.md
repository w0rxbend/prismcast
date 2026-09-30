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

## 2026-10-01 — ARCH-006 (Persistence model design)

Wrote `docs/architecture/persistence-model.md` per PLAN §19 / ADR-0008.

Key decisions:

- **Envelope structs in the persistence layer, not core:** `CollectionFileV1` / profile
  equivalents wrap domain aggregates with `schemaVersion` + a `#[serde(flatten)]` unknown-key
  map (JSON) or a retained `toml` document tree (TOML, so hand edits and comments survive).
  Domain structs stay pure; no persisted struct is reused as a domain struct.
- **Outputs/encoders/services persist into `profile.toml`** per PLAN §19, although `AppState`
  currently holds outputs top-level — flagged for CORE-004 to nest/index by `ProfileId`.
  `Output.state` is runtime-only and never persisted; `SessionState` (current scene, studio
  mode) lives inside `collection.json` OBS-style.
- **Atomic write = buffer → temp (same dir, 0600 profiles) → fsync → rename → dir fsync**;
  `.bak` is refreshed only from verified-good loads, so the §61 corrupted-config fallback is
  trustworthy. Newer-schema files error typed, never guessed.
- **Persistence actor owns the whole config tree**; saves are triggered by core classifying
  applied commands (dirty marking + debounced coalescing over bounded channel, snapshots not
  references), per PLAN §57 — UI never touches files.

Validation: design doc cross-checked against ADR-0008 decisions/consequences, PLAN §19/§27/
§48/§57/§61/§63/§67, and every prismcast-core aggregate (aggregate→file table in §3).
Doc-only task; no code to compile. Not committed (orchestrator integrates).

## 2026-10-01 — ARCH-003 — Scene graph design doc

Wrote `docs/architecture/scene-graph.md`: the contract mapping `prismcast-core`'s
`Scene`/`SceneItem` model onto GStreamer compositor pad properties.

Key decisions:

- **Shared pad vocabulary as the contract:** everything is expressed in the
  `xpos/ypos/width/height/alpha/zorder/operator` vocabulary that `compositor`,
  `glvideomixer` and `vacompositor` all expose (RES-003 §2), so the backend can swap
  elements without touching the domain. `compositor` stays the correctness-first default.
- **Crop = per-item `videocrop` branch; scale/position = pad props; anchor and bounds are
  pure math** producing derived integers (fixed rounding rules). Every `SceneItem` field
  has a defined compositor behavior, including `locked` (explicitly none).
- **Rotation is the hard limit:** no stock element rotates pads; v1 supports cardinal
  angles via `videoflip` (and flips as negative scale), quantizes arbitrary angles with a
  warning, and defers true rotation — plus `Multiply`/`Screen` blends — to a custom
  GL/Vulkan element. Capability bits on `CompositorBackend` let UI render honest
  affordances.
- **z-order maps via dense ranks** (0..n over the sorted item list), never raw `z_index`;
  visibility hides via pad `alpha=0` to avoid live relinks.
- **Groups flagged as a domain gap** (PLAN §8 requires Group/Ungroup, ARCH-001 has no
  group type); nesting via `SourceKind::Scene` maps to recursive sub-compositor bins with
  a max depth of 8 and a flagged follow-up for a domain-side cycle check.

Validation: pad property types verified locally with `gst-inspect-1.0 compositor`
(GStreamer 1.28.2); every design claim cross-checked against RES-003 §2/§7; ASCII
diagrams and Markdown tables machine-checked for alignment. Doc-only; no code. Not
committed (orchestrator integrates).

## 2026-10-01 — ARCH-005 (Output graph design + types in prismcast-output)

Implemented the domain-level `OutputGraph` runtime model in `crates/prismcast-output`
(error, spec, plan, runtime, graph modules) plus `docs/architecture/output-graph.md`.

Key decisions:

- **`EncoderSpec` as the sole share-identity** (codec, bitrate, GOP, JSON settings,
  resolution/FPS/color format from `VideoConfig`); encoder IDs play no role, so
  value-identical settings share one instance. Groups pick the smallest member
  `EncoderId` as the deterministic tee instance; plans are order-independent (tested).
- **Replan-from-scratch** on every output/encoder/video-config change — no incremental
  bookkeeping; output sets are tens of entries.
- **Failure isolation is structural**: per-output `OutputRuntime` (state machine +
  policy + stats), no shared mutable state; a dead output doesn't even re-plan.
- **Two state-machine extensions** beyond the minimal PLAN §61 table (documented):
  `Running → Failed` for hard failures, `Reconnecting → Reconnecting` for retries.
- **Backoff**: saturating exponential doubling capped at `max_backoff_ms`, jitter-free
  for testability (jitter deferred to the media layer's timer); `None` = give up.

Validation: `cargo test -p prismcast-output` 30/30 pass, clippy `-D warnings` clean,
`cargo fmt` clean. Not committed (orchestrator integrates). Follow-ups noted in the
doc: encoder unregistration, NVENC session-budget check, jitter, stats gauges.


## 2026-10-01 — ARCH-004 (Media abstraction traits in prismcast-media)

Defined the PLAN §4 backend trait surface in `prismcast-media`: `SourceBackend`,
`VideoFilterBackend`/`AudioFilterBackend` (over a shared `FilterBackend`), `CompositorBackend`,
`EncoderBackend` + `EncoderRegistry`, `OutputBackend`, `StreamingServiceBackend`, plus mocks and
tests. Key decisions:

- **Async-agnostic = synchronous, actor-called**: traits are blocking-capable sync fns invoked only
  on the media control actor thread (never Tokio/GTK); async happenings surface via a poll-based
  `BackendComponent::drain_events()` instead of callbacks/channels, keeping backends runtime-free.
- **Control seam only, no media frames cross traits**: PLAN §16's `Filter.process` is deliberately
  absent — processing stays inside the engine graph, so no `gst::Buffer`-like types leak (ADR-0004).
- **Observed vs persisted state split**: media-level `ComponentState` (PLAN §61: Stopped/Running/
  Degraded/Recovering/Failed) is separate from persisted `OutputState`; the actor maps between them.
- **Runtime probing**: `EncoderRegistry::probe()` returns `EncoderCapability` (hardware kind, rate
  controls, force-keyframe support) per RES-003 §5 — availability is never assumed.
- Errors reuse the workspace-wide `prismcast_core::Error`; all wire-adjacent support types are serde.

Validation: `cargo test -p prismcast-media` 19/19 pass (9 unit + 10 trait/mock integration),
`cargo clippy -p prismcast-media --all-targets -- -D warnings` clean, `cargo fmt --check` clean,
`cargo check --workspace` clean. Not committed (orchestrator integrates). Follow-up: the media
control actor itself is a later task; mocks are `pub` so other crates can reuse them in tests.

---

2026-10-01 ARCH-007

Designed docs/protocols/native-protocol.md and implemented prismcast-protocol (10 modules,
47 tests). Key decisions: string-tagged envelopes (`{type, data}`) instead of obs-websocket
numeric ops; UUID addressing; wire types in `data.rs` mirror domain types but stay distinct
(per-class typed IDs are a domain concern — wire uses plain Uuid, boundary converts via
From<Uuid>). Structured errors keep obs's grouped integer code space + typed kind/field/details.
Subscription model: typed SubscriptionSet with per-entity filters and explicit throttle_ms
(meter default 50 ms), no Reidentify — `update_subscriptions` is a request. Batches serial
best-effort + `halt_on_failure`; atomicity via the `transaction` request. Events carry per-session
`seq` for drop detection (resync via get_snapshot). JSON-only for v1; `prismcast.msgpack` reserved.
Command↔request mirror enforced by tests/command_coverage.rs (49 variants, dev-dep on core).
Validation: fmt/clippy(-D warnings)/test all green. Not committed (orchestrator integrates).

## 2026-10-01 — CORE-001/002/003 (Application core services in prismcast-app)

Created `crates/prismcast-app` (new workspace member; tokio allowed here, unlike the pure
domain crate): the core actor owning `AppState`, the command dispatcher's authorization
hook, the event broadcaster, immutable snapshots, and the undo service. Modules:
`actor.rs` (CoreActor + cloneable `AppHandle`, the only way in), `dispatch.rs`
(`Permission`/`Permissions`, `required_permission`, read-only `Query`), `broadcaster.rs`
(multi-subscriber fan-out), `snapshot.rs` (`AppSnapshot`), `undo.rs` (undo/redo stacks +
transaction grouping).

Key decisions:

- **Snapshot strategy: clone-on-publish.** The actor clones the (small, domain-only)
  `AppState` into `Arc<AppSnapshot>` after every applied command and publishes it over a
  `watch` channel. Reads (`AppHandle::snapshot()`, `query()`, subscriptions) never touch
  the command queue — an Arc swap is the only synchronization (PLAN §57). Revision counter
  on each snapshot; `subscribe_snapshots()` for reactive UIs. No persistent-data-structure
  dependency; internals can switch later without API change.
- **Slow-consumer policy: drop-oldest + pinned coalesced `Lagged` notice.** Each
  subscriber has a bounded queue (default 256, clamped ≥2). On overflow the oldest events
  are dropped and a front-pinned `StreamEvent::Lagged { dropped }` counts the loss (drops
  coalesce into one notice); the publisher never blocks and other subscribers are
  unaffected. Matches the protocol's seq-gap → re-sync-from-snapshot contract.
  `EventFilter` supports per-category + per-entity filtering with the same semantics as
  `prismcast_protocol::Subscription`, but the types live in prismcast-app (`EventCategory`
  minus `Meter`) — no dependency on prismcast-protocol (direction is app <- remote).
- **Permission mapping (PLAN §24):** scenes/items/sources/studio/transitions →
  `ControlScenes`; mixer/routes/buses → `ControlAudio`; outputs → `ControlOutputs`;
  profiles/collections → `ModifyConfiguration`; `Admin` supersedes. Checked via
  `Permissions::check` in the actor *before* any state access; rejected with
  `Error::Unauthorized` (no mutation, no snapshot publish, no events — tested).
  Transactions are checked per member (union of scopes required), not by a folded scope.
  Queries require `Read`.
- **Undo:** actor computes `state.inverse()` pre-apply and records `(label, inverse)`;
  undo/redo are actor messages whose inverse commands flow through the same apply →
  events → snapshot pipeline (redo = inverse-of-inverse recomputed at undo time).
  `begin_transaction(label)`/`end_transaction()` group a drag gesture into one undo entry
  (distinct from `Command::Transaction`'s atomicity). Failed undo/redo restores the entry.
  Known limitation (ARCH-002, unchanged): `Add*`/`Remove*` inverses return `None` → not
  undoable; undoing across them can fail at apply time. Interim undo authz: any
  non-read-only caller (`Permissions::can_control()`); fine-grained undo authz is CORE-005
  follow-up.
- **Graceful shutdown** is a FIFO queue message: queued commands complete, then streams
  close (subscribers drain, then see `None`). Dropping all handles also stops the actor.

Validation: 39 prismcast-app tests pass (30 unit + 8 integration + 1 doctest) including
PLAN §67 steps 2–8 domain-side with three `AppHandle` clones ("GTK"/"CLI"/"WS") all seeing
identical ordered event streams; concurrency test (4 readers × 200 + 2 writers × 50,
multi-thread runtime) proves snapshot reads never block the queue. `cargo fmt --check`,
`cargo clippy -p prismcast-app --all-targets -- -D warnings`, `cargo test -p prismcast-app`,
and `cargo test --workspace` all green. Not committed (orchestrator integrates).
Follow-ups: meter coalescing/throttle (PLAN §56) once core emits meter events;
snapshot-restore undo for destructive cascades; per-domain undo authz (CORE-005).

---

2026-10-01 IPC-001/IPC-002

First real controller of the core, proving the layered architecture end-to-end: a
`CoreActor` behind a Unix-socket `IpcServer`, driven by the shared `IpcClient` and the
new `prismcast-cli` binary — no GTK/GStreamer anywhere in the client path.

Layout:
- `crates/prismcast-remote/src/{lib,auth,paths,codec,map,session,server,client}.rs`,
  integration proof in `crates/prismcast-remote/tests/ipc.rs`.
- `crates/prismcast-cli/src/{lib,main}.rs` (clap derive; thin `main` → `run`),
  subprocess tests in `crates/prismcast-cli/tests/cli.rs` (`CARGO_BIN_EXE`).
- Root `Cargo.toml` workspace deps: added `rmp-serde = "1"`, `clap = "4"` (derive),
  `prismcast-app` path entry; tokio features extended with `net`, `io-util`, `time`.

Key decisions:
- **Codec**: 4-byte BE length prefix + MessagePack in rmp-serde's *human-readable* mode
  with `with_struct_map()` (the `to_vec_named` shape). Human-readable matters: `Uuid`'s
  serde switches to a 16-byte blob otherwise, which `serde_json::Value` cannot represent.
  Decode goes through a `serde_json::Value` intermediate so the `type` tag can be
  classified before typed decode — that powers the protocol's unknown-tag semantics (202
  `unknown_request_type` error with recovered `request_id`, `UnknownMessageType` close)
  and the IPC-only terminal `{"type":"closing","data":{code,reason,message}}` frame (the
  Unix-socket substitute for WS close codes, protocol doc §8). Max frame 4 MiB,
  oversize rejected before allocation.
- **Auth**: filesystem permissions are primary (dir 0700, socket 0600; stale-socket
  probe: connect → `AlreadyInUse` error, else remove). Default policy `allow-local`
  grants full `Admin` (documented in `auth.rs`). Optional bearer token from
  `$XDG_CONFIG_HOME/prismcast/remote.toml` (`token`, `permissions` keys) or injected
  `AuthConfig::token`; parsed with a small built-in subset parser (no TOML crate allowed
  in scope) — interim until the workspace adopts one. Challenge-response is not offered
  by this server yet (no `AuthChallenge` in `Hello`).
- **Sessions**: one task per connection; `Hello` → `Identify` (version negotiation via
  `protocol::version::negotiate`, auth) → `Identified` → steady-state `select!` over
  inbound frames / session event receiver / throttle-flush timer / server shutdown.
  Writes go through a bounded outbound mpsc queue + writer task. Invalid *initial*
  subscription sets (no request id to answer) refuse the session with a closing notice
  instead of silently clamping; invalid `update_subscriptions` gets a 901
  `invalid_subscription` error naming the problem in `details`.
- **Event fan-out**: one upstream `EventStream` per *server*, re-broadcast over a bounded
  `tokio::sync::broadcast` channel; sessions subscribe to that. Rationale: the app
  broadcaster has no unsubscribe API, so per-session upstream subscriptions would leak a
  registration on every disconnect/resubscribe; the broadcast channel's bounded Lagged
  semantics also match the drop-oldest policy. Category/entity filtering happens
  in-session, which also enables per-category entity filters `EventFilter` can't express.
- **Throttle/coalescing**: per `(category, entity)` key; first event in a window delivers
  immediately, later ones replace a single pending slot flushed at window end (fixed
  cadence: flush restarts the window). Safe because events carry full snapshots.
- **Backpressure**: per-session `seq` starts at 0 after `identified`. Broadcaster lag and
  outbound-queue overflow drops both *consume* sequence numbers, so clients always see
  loss as a gap and re-sync via `get_snapshot` (protocol §7). Responses that can't be
  enqueued within `send_timeout` (1 s) or 8 consecutive event overflows shed the session
  with `SlowConsumer` (4013). Fixed-window inbound rate limiter (200 req/s burst) answers
  excess with 900 `rate_limited`; sustained-abuse close is a follow-up.
- **Error mapping** follows the protocol doc table exactly: `NotFound`→600,
  `InvalidInput`→400, `Unauthorized`→800, `Protocol`→200, `Media`/`Io`/`Persistence`→700;
  actor shutdown → 204 `not_ready`; unknown request tag → 202; missing entity on
  `get_scene`/`get_source`/`get_output` → 600 with `field` set.
- **`AVAILABLE_REQUESTS`** (62 tags) lives in `prismcast-remote::map` for `get_version`
  capability discovery and unknown-tag classification; a drift test validates every entry
  against `RequestKind`'s serde tags without constructing instances.
- **Wire `Permission` → `dispatch::Permissions`** mapped 1:1 at the boundary; sessions
  dispatch via `AppHandle::dispatch_with_permissions` (authz checked by the actor before
  state access), queries via `AppHandle::query` — read paths never touch the command
  queue. `add_output`/`add_profile`/`add_scene_collection` ignore client-supplied
  ids/state (server-assigned, per protocol §5).
- **CLI** (`prismcast-cli`): `ping`, `status`, `scene list`, `scene switch
  <uuid-or-name>` (UUID parse first, else exact-name lookup via `list_scenes`), `--json`
  global flag, `--socket` override. Exit codes: 0 ok, 1 transport, 2 rejected (unknown
  scene name = 2/600). Talks only through the socket via `IpcClient`, current-thread
  runtime, subscriptions disabled (`SubscriptionSet::none`).

Workarounds (no core/app/protocol internals modified, per scope):
- Wire `StateSnapshot` has no `revision` field → `status` cannot report one; adding it is
  a protocol schema change (needs its own task per PLAN §74).
- Core `TrackMask` has no `from_bits` constructor → rebuilt via `with(track)` loop in
  `map.rs`.
- `prismcast-app` broadcaster lacks unsubscribe → server-wide fan-out (above).

Tests: 27 remote unit (auth parser, codec framing/closing-notice, map coverage incl.
error table, throttle windows, rate limiter, stale-socket/mode handling) + 9 remote
integration (ping/snapshot, scene command roundtrip, two-client event delivery,
subscription filtering incl. entity filter + 901 rejection, throttle coalescing with
latest-wins assertion, token auth: wrong/missing token → 4009 close, read-only token →
800 on mutation, unknown request → 202, oversized frame → 4002 close, pre-identify
request → 4007 close) + 4 CLI subprocess tests (ping text+JSON, status, scene
list/switch by name and UUID with server-state verification, missing server → exit 1).
Validation: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
`cargo test --workspace` (all green; 273 tests workspace-wide), `cargo deny check` clean.
Not committed (orchestrator integrates).
Follow-ups: broadcaster unsubscribe API (app-level); WS transport (same session
machinery); client-side batches; sustained-abuse rate-limit close; TOML crate for
remote.toml; `revision` on wire snapshot; meter events once the domain emits them.
