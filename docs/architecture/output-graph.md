# Output Graph Architecture

Implements PLAN.md §10–11 and ADR-0007. Code: `crates/prismcast-output`
(domain-level runtime model; **no GStreamer types** — the media engine in
`prismcast-media` will execute what this graph plans).

## Why not OBS's model

OBS effectively models streaming as `Option<StreamingOutput>`; multistreaming
is a plugin with shared fate. Prismcast makes multistreaming native: the output
model is an `OutputGraph` of N independent outputs (recording, N×RTMP, SRT,
WHIP, virtual camera). Streaming is never a singleton.

## Structure

```text
OutputGraph (prismcast-output)
 ├── encoder registry: EncoderId → EncoderSpec
 ├── bindings: OutputId → [EncoderId]        (video + audio, declared)
 ├── runtimes: OutputId → OutputRuntime      (fully independent)
 └── plan: EncoderPlan                       (recomputed on any change)
```

Domain entities (`Output`, `EncoderSettings`, `Service`, `ReconnectPolicy`,
`OutputState`) live in `prismcast-core::output`; this crate adds the runtime
layer on top without reusing protocol or persistence structs for runtime state.

## Encoder sharing plan

Sharing criterion (PLAN §11): two outputs share one encoder instance iff their
`EncoderSpec` values are equal. `EncoderSpec` covers every listed dimension:

```rust
EncoderSpec {
    codec,               // "h264", "av1", "aac", ...
    bitrate_kbps,
    keyframe_interval,   // GOP
    settings,            // profile / preset / rate-control, JSON value equality
    video: Option<VideoSpec { width, height, fps_num, fps_den, color_format }>,
}
```

- Derived from `EncoderSettings` + program `VideoConfig` + an optional color
  format tag (`EncoderSpec::from_video` / `from_audio`). Resolution and FPS
  come from the video config, so a canvas change re-plans the graph.
- Video and audio encoders never share (`video: None` distinguishes them);
  audio encoders with identical settings share by the same rule.
- Encoder **IDs play no role** in sharing — two outputs configured with
  independently-created but value-identical settings still share.

`EncoderPlan::compute(&[PlanEntry])` folds entries into `EncoderGroup`s:

```rust
EncoderGroup {
    spec,                 // shared identity
    instance: EncoderId,  // smallest member ID — deterministic representative
    members: Vec<EncoderId>,   // declared encoders folded into the instance
    consumers: Vec<OutputId>,  // outputs fed through the packet tee
}
```

Determinism: groups, members, and consumers are sorted by ID; equal input sets
produce identical plans in any insertion order (unit-tested). The media graph
builds one physical encoder per `instance` and a tee with one branch per
consumer:

```text
Renderer → Encoder (instance) → encoded packet tee ──┬─ Twitch mux/output
                                                     └─ YouTube mux/output
```

Outputs whose specs differ in any dimension get dedicated branches instead.

### Re-planning

The plan is recomputed from scratch (`OutputGraph::replan`) whenever:

- an output is added/removed (`add_output` / `remove_output`),
- encoder settings are (re-)registered (`register_video_encoder` /
  `register_audio_encoder` — re-registering an ID with new settings is how
  settings changes enter the graph),
- the program video configuration changes (`set_video_config`).

Re-planning from scratch avoids incremental-bookkeeping bugs; output sets are
tens of entries, so the cost is irrelevant. ADR-0007 consequence: resource
exhaustion (encoder count beyond hardware budget) is surfaced at command time
by the application layer, which can read `plan().instance_count()` before
committing — the graph itself stays hardware-agnostic. Per RES-005
(`docs/research/encoder-matrix.md` §6): consumer NVENC is limited to a small
number of concurrent sessions, so sharing is the default, not an optimization.

## Per-output independence and failure isolation

Each output owns an `OutputRuntime`: state machine, reconnect policy, and
`OutputStats` (bytes/packets sent, dropped frames, reconnect attempts, last
error). Runtimes share **no** mutable state — isolation is structural, not
conventional. `OutputGraph::transition` / `connection_lost` take an `OutputId`
and touch exactly one runtime; the plan is a pure function of the output set,
so a failed output does not even re-plan (unit-tested: a dead Twitch sink
leaves YouTube `Running` and the recording untouched, per PLAN §50).

State machine (PLAN §61 model from `prismcast-core::output`):

```text
Stopped ─┐
         ├─→ Starting ─→ Running ─→ Degraded ─→ Running
Failed ──┘        │         │   ↕        ├─→ Failed
                  ↓         ↓            └─→ Stopping ─→ Stopped
               Failed    Reconnecting ──→ Running
```

Two edges extend the documented minimal table (documented on
`OutputRuntime::is_legal_transition`):

- `Running → Failed` — hard failures (encoder crash, sink error with no retry
  budget) must not detour through `Degraded`.
- `Reconnecting → Reconnecting` — subsequent retry attempts.

Illegal transitions return `OutputGraphError::IllegalTransition` and leave
state unchanged.

## Reconnect backoff

`backoff_ms(policy, attempt) -> Option<u64>`: exponential growth from
`initial_backoff_ms`, doubling per 1-based attempt, capped at
`max_backoff_ms`, all arithmetic saturating. `None` means "do not retry"
(`max_retries == 0`, `attempt == 0`, or `attempt > max_retries`) — the caller
goes `Failed`. Default policy (from `prismcast-core`): 1s initial, 30s cap,
10 retries → 1s, 2s, 4s, 8s, 16s, then 30s flat.

The value is jitter-free on purpose so scheduling is reproducible and
unit-testable; the media layer may add jitter when arming the actual timer.

`OutputRuntime::connection_lost(reason)` combines the pieces: from
`Running`/`Degraded`/`Reconnecting` it either enters
`Reconnecting { attempt }` and returns `ReconnectStep::Retry { backoff }`, or
exhausts the budget, goes `Failed`, and returns `ReconnectStep::Exhausted`.

## Command/Event integration

Outputs are created/started/stopped via `prismcast-core` Commands
(`AddOutput`, `RemoveOutput`, `StartOutput`, `StopOutput`,
`SetOutputReconnectPolicy`) and report via `OutputEvent`s (ADR-0005). The
application core drives this graph in lockstep: a `StartOutput` command maps
to `graph.transition(id, Starting)`, media-engine outcomes drive the
remaining transitions, and each transition emits
`OutputEvent::StateChanged`. Removal mirrors the domain rule: only `Stopped`
or `Failed` outputs leave the graph (`OutputGraphError::OutputNotStopped`).

## Replay buffer composition

Per ADR-0007 §5 and PLAN §14, the replay buffer is a circular buffer of
*encoded packets* tapped from an encoder instance — so it composes with the
shared-encoder tee for free: attach the ring buffer as one more consumer of
the instance's packet stream. No raw-frame duplication.

## Edge cases (unit-tested)

- Identical settings with different encoder IDs → one shared instance.
- Any single differing dimension (bitrate, resolution, color format, codec)
  → dedicated encoders.
- JSON settings key order does not affect sharing (`serde_json::Value`
  equality is order-insensitive).
- Plan determinism under shuffled input; empty graph → empty plan.
- Backoff: cap clamping, saturation with `u32::MAX` initial backoff,
  `max_retries = 0` → immediate `Failed`.
- Failure isolation: exhausted Twitch leaves YouTube/recording `Running`;
  stats are per-output.
- Removal of a shared consumer demotes the survivor to a dedicated branch;
  running outputs cannot be removed; unregistered or kind-mismatched
  (audio-in-video-slot) encoders are rejected at `add_output`.

## Follow-ups (not in ARCH-005 scope)

- Encoder unregistration (registry is append/replace-only today).
- Hardware session-budget check at command time (needs RES-005 probing in
  `prismcast-media`).
- Jittered reconnect scheduling in the media layer.
- `OutputStats` extension with bitrate/fps gauges once the media engine
  reports them (RES-005: `nvencoder` 1.28 `emit-frame-stats`).
