# ADR-0010: obs-websocket 5.x compatibility as an adapter — the internal protocol stays native

## Context

obs-websocket 5.x is the de-facto remote-control standard in the OBS ecosystem: Stream
Deck tools, mobile clients, automation scripts, bots, and home automation integrate
against it. It validates a proven interaction model — Hello/Identify/Identified,
Request/RequestResponse, RequestBatch, Event subscriptions, RPC versioning, JSON and
MessagePack encoding (PLAN §22). Prismcast's remote-first goal (PLAN §1) makes an
ecosystem migration path highly valuable.

But adopting obs-websocket as the *internal* protocol would import OBS's domain concepts,
naming, and historic compromises into the core — including single-stream assumptions that
PLAN §79 deliberately rejects, and shapes that cannot express Prismcast's OutputGraph
(ADR-0007). PLAN §22 is explicit: "Do NOT make the internal domain protocol identical to
obs-websocket."

## Decision

1. **The internal domain protocol is native** to Prismcast: the Command/Query/Event
   contract of ADR-0005, serialized through `prismcast-protocol` over the native
   WebSocket API and Unix IPC (ADR-0006). It is designed around Prismcast's domain
   (OutputGraph, multi-track audio, typed IDs) without OBS-compatibility constraints.
2. The native WebSocket protocol **intentionally resembles** obs-websocket's proven
   interaction shape — Hello/Identify/Identified, Request/RequestResponse, RequestBatch,
   Event subscriptions (PLAN §22) — because that model is validated, not because of
   wire compatibility.
3. **obs-websocket 5.x compatibility is delivered as a separate adapter** (PLAN §22),
   translating obs-websocket requests/events onto the Core Command API:

   ```
   Core Command API
        ↑
        ├── Native WS adapter
        ├── OBS websocket adapter
        ├── Unix IPC adapter
        └── CLI adapter
   ```

4. The adapter is built after the native WebSocket protocol, event subscriptions, auth,
   and batch operations exist (Phase 9, PLAN §51), and is validated against real
   obs-websocket clients ("Compatibility testing should use existing OBS WebSocket
   clients", PLAN §51). Research task RES-007 (obs-websocket analysis, PLAN §66) feeds
   its design.
5. The adapter targets obs-websocket **5.x** semantics. Where OBS concepts have no
   Prismcast equivalent (e.g. single-stream assumptions vs. OutputGraph), the adapter
   defines explicit mappings (e.g. "current stream" → a designated primary output) and
   documents unsupported requests as typed errors — never silent no-ops.

## Alternatives

- **Make obs-websocket the internal protocol.** Rejected: couples the domain to OBS's
  concepts and history, cannot natively express multistreaming, and contradicts PLAN §22.
- **No compatibility layer at all.** Rejected: forfeits the existing ecosystem migration
  path that PLAN §22 lists as a key benefit (Stream Deck tools, mobile clients,
  automation scripts, bots, home automation).
- **Reimplement OBS concepts in the domain so the adapter is trivial.** Rejected: same
  coupling problem, worse — the domain itself becomes OBS-shaped.
- **Fork/passthrough proxy to a real OBS instance.** Absurd for this product; listed only
  for completeness.

## Consequences

- The native protocol evolves freely (batch, subscriptions, RPC versioning) without
  obs-websocket wire-compat constraints.
- The adapter is one more thin adapter over the same core contract — proving ADR-0005's
  interchangeability claim with an external, adversarial protocol.
- Compatibility is necessarily partial where Prismcast is richer (multistream);
  documentation of mapping and gaps is part of the adapter deliverable.
- Research baseline discipline applies: OBS 32.2.2 released behavior vs. 33.x development
  docs must be distinguished (PLAN §1) when implementing the adapter.

## Evidence

- PLAN.md §22 (WebSocket API: obs-websocket-resembling concepts; "Strong recommendation:
  implement an additional obs-websocket compatibility adapter"; "Do NOT make the internal
  domain protocol identical to obs-websocket"; adapter diagram).
- PLAN.md §51 (Phase 9: native WS first, then the compatibility adapter; test with real
  OBS WebSocket clients), §66 (RES-007 obs-websocket analysis), §78 (compat adapter in
  v0.2 scope), §1 (research baseline caveat).

## Status

Accepted (2026-09-30)
