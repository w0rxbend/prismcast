# ADR-0007: OutputGraph — N independent outputs with shared/dedicated encoders and failure isolation

## Context

OBS models streaming as essentially a single optional streaming output
(`Option<StreamingOutput>` shape); multistreaming is an extension. PLAN §10 calls the
output graph a "crucial difference from OBS": Prismcast treats native simultaneous
multistreaming as core (PLAN §1, §11) and deliberately does not copy OBS's single-stream
assumptions (PLAN §79). Milestone 4 — YouTube + Twitch simultaneously with independent
status, output recovery, and shared-encoder optimization — is where the project stops
being an OBS clone (PLAN §70).

Multistreaming raises two design problems: (1) encoding the same program multiple times
wastes GPU/CPU, so identical encoder settings should share one encoder; (2) outputs fail
independently (network drops, auth rejection, disk full), and one broken output must
never stop another (PLAN §11).

## Decision

1. **The output model is an `OutputGraph`** holding N independent outputs — recording,
   Twitch, YouTube, custom RTMP, SRT, WHIP, virtual camera (PLAN §10). Streaming is never
   modeled as a singleton/optional.

   ```rust
   Output {
       id: OutputId,
       video_encoder,
       audio_encoders,
       service,
       reconnect_policy,
       state,
       statistics,
   }
   ```

2. **Shared encoders where legal, dedicated encoders otherwise** (PLAN §11). If two
   destinations require exactly the same resolution, FPS, codec, profile, bitrate, GOP,
   and color format, they share one encoder feeding an encoded-packet tee into per-output
   muxers/outputs. Otherwise each output gets its own encoder branch.
3. **Per-output independence**: each destination owns its state machine, network queue,
   reconnect/backoff policy, statistics, error handling, credentials, latency, and
   rate-control policy (PLAN §11). Output failures follow the
   Running/Degraded/Recovering/Failed/Stopped model (PLAN §61).
4. **Failure isolation is an architectural requirement**: one broken output must never
   stop another. Critical tests: a dead Twitch sink leaves YouTube uninterrupted, and
   recording continues while every remote stream reconnects (PLAN §50).
5. The replay buffer consumes the encoded stream as a circular packet buffer from an
   encoder, not raw frames (PLAN §14), so it composes with the shared-encoder tee.
6. Outputs are created/started/stopped via Commands and report via Events (ADR-0005);
   the UI and remote API see per-output state and statistics uniformly.

## Alternatives

- **OBS-style single streaming output + plugins for multistream.** Rejected: multistream
  becomes a bolt-on with shared fate; contradicts PLAN §10–11 and the milestone ladder.
- **Always-dedicated encoders.** Rejected: duplicates encode cost for the common case
  (same settings to two RTMP services); the tee optimization is explicitly specified in
  PLAN §11.
- **Always-shared encoders with per-output transrate.** Rejected: forces identical
  settings across services whose constraints differ (bitrate caps, keyframe cadence).

## Consequences

- The encoder graph is a first-class structure: mapping outputs → encoders (shared or
  dedicated) is computed from settings equality and re-planned when settings change.
- Output isolation requires per-output queues and no shared mutable output state;
  backpressure in one output cannot stall the compositor or sibling outputs (bounded
  queues, PLAN §75).
- Statistics, reconnect, and credentials are per-output in the domain model, protocol,
  and UI — no "the stream" singular concepts anywhere.
- Encoder count is bounded by hardware; the graph planner must surface resource
  exhaustion as a typed error at command time rather than a runtime collapse.

## Evidence

- PLAN.md §10 (Output graph — crucial difference from OBS; `Output` struct).
- PLAN.md §11 (Native multistream; shared-encoder tee optimization; per-output
  independence list; "One broken output must never stop another output").
- PLAN.md §50 (Phase 8 critical failure-isolation tests), §70 (milestone 4
  differentiators), §14 (replay buffer as encoded packet ring buffer), §79 (single-stream
  assumptions not copied).

## Status

Accepted (2026-09-30)
