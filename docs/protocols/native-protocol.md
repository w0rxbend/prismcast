# Prismcast Native Protocol

Version: protocol **v1** (2026-10-01). Implemented by `crates/prismcast-protocol`; served by
`prismcast-remote` over WebSocket (PLAN §22) and Unix-socket IPC (PLAN §21, ADR-0006).

The interaction shape intentionally resembles obs-websocket 5.x (ADR-0010 §2) because that model is
validated: server-first handshake, request/response correlation, batches, event subscriptions.
The wire format is **not** obs-websocket-compatible; the compatibility adapter (Phase 9) is a
separate translation layer. This document is the native protocol reference; deviations from
obs-websocket are justified per section with pointers to `docs/research/obs-websocket-protocol.md`
("RES-007").

## 1. Transports and framing

| Transport | Framing | Codec | Auth context |
|---|---|---|---|
| WebSocket (`ws://` loopback, `wss://` anywhere, PLAN §22) | One message per WS frame | JSON text frames (v1) | token or challenge-response; TLS per the bind policy below |
| Unix socket (`$XDG_RUNTIME_DIR/prismcast/control.sock`, ADR-0006) | Length-prefixed frames (`u32` BE length + payload) | MessagePack | filesystem permissions + optional challenge-response |

One serde data model serves both codecs; the protocol crate defines no codec logic. MessagePack
over WebSocket (an `obswebsocket.msgpack`-style subprotocol) is **deferred** — RES-007 open
question 1 answered: JSON-only for v1, the subprotocol name `prismcast.msgpack` is reserved.
On WebSocket the encoding is selected via `Sec-WebSocket-Protocol: prismcast.json` (the default
when no subprotocol is requested).

**TLS (`wss://`)** is implemented by the native WebSocket server (ADR-0022): `prismcast-remote`
terminates rustls (ring provider) at the accept loop, before the WebSocket upgrade — the same
framing and handshake then run over the encrypted stream unchanged. The server takes
operator-provided PEM files (`WsServerConfig::tls`: a leaf-first certificate chain and a
PKCS#8/PKCS#1 private key); there is no built-in self-signed generation. Bind hardening is
enforced at bind time: **a non-loopback bind without TLS fails** (`WsError::TlsRequired`), so
exposing the control plane on a network interface requires `wss://`; plaintext `ws://` remains
valid on loopback only. The TLS handshake on each accepted connection is bounded (10 s), so a
plaintext client pointed at a `wss://` port fails fast without stalling other clients. Client
trust is the native system roots plus an optional extra CA bundle, with an explicit
warn-logged danger-insecure escape hatch (library surface in `prismcast-remote::tls`; CLI flags
follow). The obs-websocket adapter (Phase 9) is **not** covered: it stays plaintext, matching
upstream obs-websocket, where a reverse proxy is the ecosystem pattern.

Limits (enforced by `prismcast-remote`, not expressible in the type layer): max frame/message
size is **per-transport** — **4 MiB** on Unix IPC (`codec::DEFAULT_MAX_FRAME_SIZE`) and **1 MiB**
on WebSocket (`ws::DEFAULT_MAX_MESSAGE_SIZE`); larger → close `MessageDecodeError`. The IPC
socket is a trusted local path already gated by `0600` filesystem permissions, and large
snapshots (`get_snapshot`) need the headroom; the WebSocket path is the untrusted network
surface, so it gets the tighter bound. One in-flight `Identify`; inbound request rate
limit per session (default 100 req/s, burst 200) — excess → `RateLimited` error responses, then
close `RateLimited` on sustained abuse.

## 2. Message envelopes

Every frame is a self-describing tagged object — `{"type": <string>, "data": {...}}` — instead of
obs-websocket's numeric `{op, d}` (RES-007 weaknesses 2–3: names are log-readable, stable under
renumbering, and generate into a schema cleanly).

Client → server: `identify`, `request`, `request_batch`.
Server → client: `hello`, `identified`, `event`, `request_response`, `request_batch_response`.

Rust types: `ClientMessage` / `ServerMessage` in `prismcast_protocol::message`. Unknown `type`
tags fail decoding; before identification that closes the session, afterwards it yields an
`invalid_request` error response when a `request_id` is recoverable, else close
`UnknownMessageType`.

**Unknown-field tolerance:** receivers ignore fields they do not know. This makes additive
evolution (new optional fields, new request/event variants) possible without a version bump.
Senders must never rely on a peer ignoring a field for correctness.

## 3. Connection lifecycle

```
client                                  server
  |  (connect)                            |
  |<------------- hello ------------------|  versions + auth challenge
  |--------------- identify ------------->|  requested version, auth, subscriptions
  |<------------- identified -------------|  negotiated version, session_id, permissions
  |<------ event / request/response ----->|  steady state
```

1. The server sends `hello` immediately: `prismcast_version`, `protocol_version` (server max),
   `min_protocol_version`, and optionally `authentication: {salt, challenge}`.
2. The client replies with exactly one `identify`: requested `protocol_version`, optional
   `authentication`, optional `subscriptions`, optional `client` info. Until `identified`
   arrives, **any other message closes the session** (`NotIdentified`); a second `identify`
   closes with `AlreadyIdentified` (obs-websocket's strict rules, RES-007 §Connection lifecycle).
3. The server answers `identified`: `negotiated_protocol_version`, `session_id`, `permissions`.
4. Steady state: `request`/`request_batch` from the client; `event`, `request_response`,
   `request_batch_response` from the server.

There is **no `reidentify` message**: subscription changes are the `update_subscriptions`
*request* — correlatable, errorable, and logged like any other operation (RES-007 open question
3, answered: request).

### Version negotiation

`protocol_version` is an integer bumped **only on breaking changes** (removed/renamed requests or
events, changed field semantics, changed handshake flow). Negotiation
(`prismcast_protocol::version::negotiate`): the client requests its preferred version; the server
answers `min(requested, server_max)` if that is ≥ `server_min`, else closes with
`UnsupportedProtocolVersion`. Additive changes never bump the version; they are discovered through
`get_version`'s `available_requests` name list (a full machine-readable schema document is a
planned follow-up, RES-007 conclusion 2).

## 4. Authentication and permissions

Two methods (`AuthResponse`, tagged `method`):

- `challenge` — SHA-256 challenge-response (**implemented**), same construction as
  obs-websocket: `base64(sha256(base64(sha256(password + salt)) + challenge))`. When password
  auth is configured (the `password` key in `remote.toml`), the server advertises an
  `AuthChallenge` in `hello.authentication` for every session: the salt is stable per server
  start, the challenge is freshly generated per session. The client answers in
  `identify.authentication`; a wrong or missing response closes with `AuthenticationFailed`
  (4009). Suitable for the Unix-IPC and plain-`ws://` loopback paths where TLS is absent; the
  IPC server optionally offers the same challenge-response (parity with WebSocket) on top of
  its filesystem-permission gate.
- `token` — configured bearer token (PLAN §24), unchanged: the client presents the configured
  token verbatim in `identify.authentication`. Valid on both WebSocket schemes; off loopback
  it always travels over `wss://`, because the server refuses non-loopback plaintext binds
  (§1, ADR-0022).

The `password` and `token` keys are mutually exclusive in `remote.toml` — configure exactly one
server-side credential.

Wrong or missing auth → close `AuthenticationFailed`. Admin "kick" → close `SessionInvalidated`
(clients must not auto-reconnect).

`identified` carries the session's `permissions` (PLAN §24): `read`, `control_scenes`,
`control_audio`, `control_outputs`, `modify_configuration`, `admin`. Every request maps to a
required scope; failures are `forbidden` errors (code 800), not disconnects.

## 5. Requests and responses

`request`: flat object — `{"request_id": <opaque client string>, "request": <tag>, ...fields}`.
`request_response` echoes `request_id` and `request_type` and carries `status` and optional typed
`data`:

```json
{"type":"request_response","data":{"request_id":"req-9","request_type":"stop_output",
  "status":{"ok":false,"error":{"code":500,"kind":"state_conflict",
  "message":"output is not running","field":"output_id","details":{"state":"stopped"}}}}}
```

**Request kinds.** All 49 `prismcast_core::Command` variants are representable, with identical
snake_case tags (`add_scene` … `transaction`) and identical field semantics with wire types
(UUIDs instead of typed-ID newtypes, `prismcast_protocol::data` structs instead of domain
structs). The mirror is test-enforced (`tests/command_coverage.rs`); adding a core command
requires adding the wire variant and extending that test in the same commit.

Command responses carry server-assigned IDs (`scene_created`, `source_created`, …) or `empty`.
Queries are read-only request kinds: `get_version`, `get_snapshot`, `list_scenes`, `get_scene`,
`list_sources`, `get_source`, `list_outputs`, `get_output`, `get_audio_state`, `list_profiles`,
`list_scene_collections`. Session requests: `update_subscriptions`, `get_subscriptions`.

`authorize_source_capture` requires `control_scenes` (or `admin`) for every
capture kind, including audio. CAPTURE-004 keeps the existing request/event
shape: PipeWire audio targets are advisory versioned source settings; only
an explicit authorization command opens capture. Audio `runtime_changed`
observations have `dimensions: null` even when `status: active`; video Active
observations contain negotiated pixel dimensions. Target serials and native
grants are never exposed or persisted. See [PipeWire audio verification](../testing/pipewire-audio.md).

`transaction` members must be command kinds (queries are rejected); nesting `transaction` inside
`transaction` is rejected. It maps to `Command::Transaction` — atomic, all-or-nothing
(PLAN §59).

### Structured errors

`WireError`: `{code, kind, message, field?, details?}`. Keeps obs-websocket's grouped integer
code space (client `switch` convenience, adapter-friendly) but replaces comment-stuffing with
typed structure (RES-007 weakness 6):

| Range | Meaning | Codes |
|---|---|---|
| 2xx | request-shape | `generic_error` 200, `missing_request_type` 201, `unknown_request_type` 202, `invalid_batch` 203, `not_ready` 204 |
| 3xx | missing data | `missing_field` 300 |
| 4xx | invalid values | `invalid_field` 400, `invalid_field_type` 401, `field_out_of_range` 402 |
| 5xx | state conflicts | `state_conflict` 500 (illegal output lifecycle transition, delete-policy rejection, studio-mode precondition) |
| 6xx | resources | `not_found` 600, `already_exists` 601 |
| 7xx | action failures | `processing_failed` 700 |
| 8xx | authorization | `forbidden` 800 |
| 9xx | overload | `rate_limited` 900, `invalid_subscription` 901 |

Mapping from core errors (`prismcast_core::Error`) at the boundary: `NotFound` → 600,
`InvalidInput` → 400, `Unauthorized` → 800, `Protocol` → 200, `Media`/`Io`/`Persistence` → 700.

## 6. Batches

`request_batch`: `{request_id, halt_on_failure = false, requests: [{request_id?, ...request}]}`
→ `request_batch_response: {request_id, results: [...]}` with per-member status/data in execution
order.

Semantics (RES-007 conclusion 7):

- **Serial, in order, as fast as possible** — the only execution mode in v1.
- **Best-effort, not atomic**: a failed member does not roll back earlier ones; with
  `halt_on_failure` the batch stops and `results` is shorter than `requests`. Clients needing
  atomicity use a single `transaction` request.
- No `SerialFrame` (graphics-thread coupling belongs in the compositor), no `Parallel`
  (undercuts ordering), no `Sleep`, no response-chaining "variables" (obs-websocket dropped it
  before release).
- Batches cannot nest (`invalid_batch`).

## 7. Events and subscriptions

`event`: `{seq, category, domain, event, ...fields}` — e.g.:

```json
{"type":"event","data":{"seq":0,"category":"scene","domain":"scene","event":"added",
  "scene_id":"11111111-1111-4111-8111-111111111111","name":"Main"}}
```

`WireEvent` mirrors `prismcast_core::Event` (domains `scene`, `source`,
`audio`, `output`, `system`, `meter`). Meter levels are transient observations
with per-channel `peak_dbfs`/`rms_dbfs`, outside persisted state and undo history.
AUDIO-001 produces post-gain/mute levels for explicit diagnostic audio sources;
CAPTURE-004 adds explicitly authorized PipeWire audio sources. Native ingress
checks the active capture generation and reconciled snapshot revision.
See [audio mixer verification](../testing/audio-mixer.md). Meter delivery
requires opt-in and uses the existing source filtering and throttling rules.

### Subscriptions

Typed set instead of a bitmask (RES-007 weakness 4, conclusion 3):

```json
{"subscriptions":[{"category":"scene"},
  {"category":"meter","entity_ids":["<source-uuid>"],"throttle_ms":100}]}
```

- **Categories**: `general`, `scene`, `source`, `audio`, `output`, `system` (standard) and
  `meter` (high-volume, opt-in only). Default when `identify.subscriptions` is absent: all
  standard categories (obs-websocket's `All`-minus-high-volume semantics). Explicit empty set =
  no events.
- **Per-entity filters** (`entity_ids`, max 256): delivery restricted to events whose primary
  entity is listed. Events without a filterable primary entity (`scene_reordered`,
  `studio_mode_changed`, `transition_changed`, `transition_started`) pass only unfiltered
  category subscriptions. `WireEvent::primary_entity()` defines the mapping.
- **Throttle** (`throttle_ms`, 10–600_000): minimum interval between deliveries *per entity*;
  within a window the server coalesces to the latest state (safe because events carry full
  snapshots — `item_updated`, `mixer_changed`). For `meter` the interval defaults to 50 ms
  (obs-websocket's cadence) when unset. Invalid sets are rejected with `invalid_subscription`
  naming the offending entries in `details` — never silently clamped. Exception: an invalid
  *initial* set carried by `identify` cannot be answered with a request error (no request ID
  exists pre-identify), so the session is simply closed instead — close `UnknownReason` (4000)
  with an explanatory reason, delivered as the closing notice on Unix IPC.
- Replacement semantics: `update_subscriptions` atomically swaps the whole set and returns the
  applied set; no incremental add/remove in v1.

### Sequence numbers and backpressure

`seq` is a per-session monotonically increasing counter starting at 0 after `identified`
(obs-websocket has no drop detection — RES-007 weakness 5). The server keeps a **bounded**
per-session outbound queue (AGENTS.md: no unbounded channels). Overload policy, in order:

1. drop queued `meter` events for lagging sessions (latest-wins coalescing),
2. drop coalesceable state events within a throttle window,
3. disconnect with close code `SlowConsumer` (4013).

On a `seq` gap or after `SlowConsumer` reconnect, the client must re-sync with `get_snapshot`
(PLAN §23: initial snapshot + incremental events; never periodic full-state polling).

## 8. Close codes

Application-private 4000+ range, numerically aligned with obs-websocket where the concept exists
(easing the Phase 9 adapter): `UnknownReason` 4000, `MessageDecodeError` 4002,
`UnknownMessageType` 4006, `NotIdentified` 4007, `AlreadyIdentified` 4008, `AuthenticationFailed`
4009, `UnsupportedProtocolVersion` 4010, `SessionInvalidated` 4011, `UnsupportedFeature` 4012;
native additions: `SlowConsumer` 4013, `RateLimited` 4014, `ServerShutdown` 4015. 4001 and
4003–4005 are deliberately unused (obs-websocket compatibility). On Unix IPC, where close codes
do not exist, the server sends a final error payload carrying the same numeric code before
closing.

## 9. Deliberate deviations from obs-websocket (summary)

| obs-websocket 5.x | Native protocol | Rationale |
|---|---|---|
| name-based addressing | UUID addressing | RES-007 weakness 1: renames break automation |
| numeric `op` codes | string `type` tags | weaknesses 2–3: readability, schema generation |
| integer + `comment` errors | `{code, kind, field?, details?}` | weakness 6 |
| category bitmask + magic high-volume bits | typed `SubscriptionSet` with filters/throttles | weakness 4 |
| `Reidentify` op | `update_subscriptions` request | errorable, correlatable |
| `availableRequests` discovery | same now + planned schema document | conclusion 2 |
| no drop detection | per-session `seq` + resync rule | weakness 5 |
| best-effort batches only | best-effort batches + atomic `transaction` | conclusion 7 |
| single password | tokens + scopes + challenge-response | weakness 7, PLAN §24 |
| singleton stream/record | full OutputGraph (`start_output {output_id}` …) | weakness 10, ADR-0007 |

## 10. Evolution policy

- Breaking change → bump `PROTOCOL_VERSION`; server may keep serving older versions down to
  `MIN_PROTOCOL_VERSION`.
- Additive change (new request/event/optional field) → no bump; announced via
  `get_version.available_requests`; receivers ignore unknown fields.
- Every wire-visible change updates the golden tests (`tests/golden.rs`) in the same commit —
  a golden failure is the tripwire that the schema moved (PLAN §63).
- Only one agent may change this schema at a time (PLAN §74, ADR-0006 §Consequences).

## References

- PLAN.md §21 (IPC), §22 (WebSocket API), §23 (remote web UI: snapshot + events), §24 (auth),
  §59 (transaction groups), §63 (golden tests), §75 (protocol ≠ domain structs).
- ADR-0006 (Unix IPC framing/versioning), ADR-0007 (OutputGraph), ADR-0010 (native vs adapter).
- RES-007: `docs/research/obs-websocket-protocol.md` (interaction shape, weaknesses, conclusions).
