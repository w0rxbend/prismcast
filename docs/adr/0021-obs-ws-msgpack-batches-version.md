# ADR-0021: obs-websocket adapter: MessagePack codec, batch execution modes, version advertisement

Status: accepted for OBSWS-002.

## Context

OBSWS-001 shipped the obs-websocket 5.x adapter speaking JSON text frames
only, with `obswebsocket.msgpack` refused at the upgrade, `SerialFrame` and
`Parallel` batch execution answered with a whole-batch 206, and `GetVersion`
reporting Prismcast's own version as `obsVersion` (which trips clients that
enforce a minimum OBS Studio version). OBSWS-002 closes those gaps. Three
decisions must be fixed before code: how MessagePack is encoded and
negotiated, how the remaining batch execution modes behave, and what the
adapter advertises as its OBS compatibility version.

## Decision

**(a) MessagePack via `rmp-serde`, zero new dependencies.** `rmp-serde` is
already a `prismcast-remote` dependency (the IPC codec, `src/codec.rs`), so
the MessagePack subprotocol adds no dependency and no `Cargo.toml` churn.
Encoding is **struct-as-map**: `rmp_serde::to_vec_named` produces
string-keyed maps, so the MessagePack payload is the same self-describing
`{op, d}` envelope shape as the JSON text — one JSON-shaped data model
serves both codecs, exactly like the IPC codec. MessagePack frames travel as
WebSocket **binary** frames; JSON stays on text frames.

**(b) Negotiation: JSON stays the default; unknown subprotocols still refuse
the upgrade.** The `Sec-WebSocket-Protocol` offer is parsed in priority
order: if `obswebsocket.json` is among the offered tokens the session is
JSON (echoed) — **JSON wins when both are offered**; else if
`obswebsocket.msgpack` is offered the session is MessagePack (echoed); no
header means JSON (the obs default); anything else refuses the HTTP upgrade
with 400, extending the OBSWS-001 hardening rather than relaxing it.
Tungstenite's accept callback cannot return a value, so the negotiated codec
is captured in a shared cell set inside the callback and read after the
upgrade await.

**(c) The codec lives at the session framing boundary; everything above it
stays codec-agnostic.** A `pub(crate)` `ObsCodec` (`Json` | `MsgPack`)
encodes/decodes `serde_json::Value` envelopes at exactly two points: the
outbound writer and the inbound frame reader. The session engine already
works on `serde_json::Value` (`ObsOutbound::Message`, `read_value`), so
translation, request dispatch, and event gating never learn which codec is
on the wire. A frame of the wrong kind for the negotiated codec (text in a
MessagePack session, binary in a JSON session), an undecodable payload, or
hostile MessagePack (ext types, oversized payloads) maps to a 4002
(`MessageDecodeError`) close, never a panic; the existing 1 MiB raw-payload
limit applies to both codecs.

**(d) Batch execution modes.** `Parallel` batches run on a bounded
`JoinSet` with a concurrency cap of **8** per batch (request translation is
async and mostly snapshot reads / command dispatch; 8 bounds core-actor
pressure per client without serializing independent requests). Results are
returned in request order, matching the upstream contract.
`SerialFrame` — upstream's "in sync with the graphics thread" — is executed
serially like `SerialRealtime`, with `Sleep` in **frame-timed** mode
(`sleepFrames`) converted to a wall-clock delay at the emulated frame rate;
there is no graphics thread to couple to, and a documented serial
approximation keeps the batch semantics (ordering, `haltOnFailure`) intact.
Both are implemented by later OBSWS-002 slices; this ADR fixes the semantics
so the slices do not re-litigate them.

**(e) Version advertisement: `obsVersion` "30.2.0" compat constant.**
`GetVersion.obsVersion` reports a fixed OBS Studio compatibility version —
**"30.2.0"**, the floor enforced by mainstream clients (e.g. `obws`'s
default minimum-studio-version check) — instead of Prismcast's own crate
version, so stock clients connect without relaxing their checks. The
constant is centralized in `proto` and pinned by a test. `Hello` still omits
`obsStudioVersion` (ADR-0020 §d): greeting-shape honesty, response-shape
compatibility. Implemented by the version-advertisement slice.

## Consequences

- The adapter still adds no dependency; `prismcast-protocol` and its golden
  tripwire tests stay untouched.
- Golden fixtures pin JSON string ↔ MessagePack bytes for every envelope
  type, so a serde-shape drift fails in tests, not at a client.
- A client that offers only unknown subprotocols is still refused with HTTP
  400; a client offering both known subprotocols deterministically gets
  JSON.
- Stats, screenshots, and meter/high-volume event producers stay **deferred**
  — there are no domain producers behind them, and fabricating values would
  be worse than the typed 204 / inert-bit answers the adapter gives today.

## Alternatives considered

**A separate msgpack serde type tree** (obs-shaped rmp structs beside the
JSON ones) was rejected: it duplicates every wire type for zero wire benefit
— `to_vec_named` on the existing `serde_json::Value` envelopes already
produces the string-keyed maps obs clients decode — and invites drift
between the two trees.

**Accepting `obswebsocket.msgpack` silently as JSON** (upstream accepts any
subprotocol offer and defaults to JSON) was rejected: a client that
negotiated MessagePack and receives JSON text frames fails later and
weirder than an honest 400 at the upgrade.

**Unbounded `Parallel` fan-out** was rejected: an unbounded `JoinSet` lets
one batch of N requests put N concurrent loads on the core actor, violating
the no-unbounded-resource rule (PLAN.md §75); the cap of 8 matches the
scale of the core's command budget.

**Reporting Prismcast's version as `obsVersion`** was rejected for
`GetVersion`: it is technically honest but practically hostile — clients
gate on the OBS Studio version, and "the adapter speaks 5.7.4" is already
truthfully advertised in `obsWebSocketVersion`.
