# RES-007 — obs-websocket 5.x Protocol Analysis

Research note, 2026-09-30. Feeds the native WebSocket protocol design (PLAN §22) and the future
obs-websocket compatibility adapter (PLAN §22/§51, ADR-0010).

## Scope and version baseline

Per PLAN §1, the OBS baseline is **OBS Studio 32.2.2** (released 2026-08-14). OBS 32.2.2 pins the
`obs-websocket` submodule to commit `1ef34bf4...`, whose `CMakeLists.txt` declares
**obs-websocket 5.7.4, `OBS_WEBSOCKET_RPC_VERSION = 1`**
([CMakeLists.txt @ 1ef34bf4](https://raw.githubusercontent.com/obsproject/obs-websocket/1ef34bf48110c2a18184e50e41cd0b1a855e2147/CMakeLists.txt);
submodule pin visible via the
[obs-studio 32.2.2 tree listing](https://api.github.com/repos/obsproject/obs-studio/contents/plugins/obs-websocket?ref=32.2.2)).

obs-websocket `master` currently declares the **same version (5.7.4, RPC 1)**
([CMakeLists.txt @ master](https://raw.githubusercontent.com/obsproject/obs-websocket/master/CMakeLists.txt)),
so the generated protocol reference on `master` —
[docs/generated/protocol.md](https://github.com/obsproject/obs-websocket/blob/master/docs/generated/protocol.md) —
describes *released* functionality as shipped in the 32.2.2 baseline. There is no 33.x-only divergence
in obs-websocket at the time of writing. Everything in this note is therefore released-baseline unless
marked otherwise. (Features tagged "Added in v5.7.0", e.g. the `Canvases` event category, are released
in the baseline since 5.7.4 > 5.7.0.)

Since OBS 28.0, obs-websocket is bundled into OBS Studio itself; standalone releases stopped at 5.0.1
([obs-websocket releases](https://github.com/obsproject/obs-websocket/releases)). Versioning now tracks
OBS releases, and the server is implemented in-process as a Qt plugin using **WebSocket++ 0.8 + standalone
Asio** for transport and **nlohmann_json** for both JSON and MessagePack encoding.

The default TCP port is **4455** (changed from 4444 during the 5.0.0 pre-release cycle to avoid clashes
with 4.x installs) ([5.0.0-alpha3 release notes](https://github.com/obsproject/obs-websocket/releases)).

## Design goals (stated by upstream)

From [protocol.md §Design Goals](https://github.com/obsproject/obs-websocket/blob/master/docs/generated/protocol.md):

- Dedicated message types for identification, events, requests, and batch requests.
- Naming conventions: `Get`, `Set`, `Get[x]List`, `Start[x]`, `Toggle[x]`; OBS-style field names
  (`sourceName`, `sourceKind`, `sceneName`, ...).
- Integer error codes with optional comment.
- Multiple encodings: JSON and MessagePack.
- PubSub: clients select which event categories they receive.
- RPC versioning: client and server negotiate a protocol version at connect time.

## Connection lifecycle

Protocol flow ([protocol.md §Connecting](https://github.com/obsproject/obs-websocket/blob/master/docs/generated/protocol.md),
implementation: [WebSocketServer.cpp](https://raw.githubusercontent.com/obsproject/obs-websocket/1ef34bf48110c2a18184e50e41cd0b1a855e2147/src/websocketserver/WebSocketServer.cpp)):

1. HTTP upgrade. The `Sec-WebSocket-Protocol` header selects the encoding:
   - `obswebsocket.json` — JSON over text frames (default if no subprotocol requested),
   - `obswebsocket.msgpack` — MessagePack over binary frames.
   The server picks the first requested subprotocol it recognizes in `onValidate`.
2. Server immediately sends **Hello (op 0)** with `obsStudioVersion`, `obsWebSocketVersion`,
   `rpcVersion`, and an optional `authentication` object (`challenge`, `salt`).
3. Client replies **Identify (op 1)** with `rpcVersion`, optional `authentication` string, and optional
   `eventSubscriptions` bitmask. Until **Identified (op 2)** is received, the client may send nothing but
   a single `Identify`; any other message closes the connection with `NotIdentified` (4007). Sending
   `Identify` twice closes with `AlreadyIdentified` (4008).
4. Server replies **Identified (op 2)** with `negotiatedRpcVersion`.
5. After identification, the client may send **Request (6)** / **RequestBatch (8)** / **Reidentify (3)**,
   and receives **Event (5)** / **RequestResponse (7)** / **RequestBatchResponse (9)**.
6. `Reidentify` may only change `eventSubscriptions`; other session parameters require reconnect.

Failure modes are signalled with WebSocket close codes, not error messages (see Close codes below).
Notable hard rule from the implementation: an unidentified client whose first message contains a
top-level `request-type` field is assumed to be a 4.x client and is closed with
`UnsupportedRpcVersion` (4010).

## Message envelope and opcodes

Every message is `{ "op": number, "d": object }`. Op 4 is deliberately unused (it belonged to 4.x-era
semantics; the gap also prevents accidental confusion).

| op | Name                | Direction       | Purpose |
|---:|---------------------|-----------------|---------|
| 0  | Hello               | server → client | Version info, RPC version, auth challenge |
| 1  | Identify            | client → server | Auth response, RPC version request, event subscriptions |
| 2  | Identified          | server → client | Session ready; `negotiatedRpcVersion` |
| 3  | Reidentify          | client → server | Update `eventSubscriptions` |
| 5  | Event               | server → client | `{eventType, eventIntent, eventData?}` |
| 6  | Request             | client → server | `{requestType, requestId, requestData?}` |
| 7  | RequestResponse     | server → client | `{requestType, requestId, requestStatus, responseData?}` |
| 8  | RequestBatch        | client → server | `{requestId, haltOnFailure?, executionType?, requests[]}` |
| 9  | RequestBatchResponse| server → client | `{requestId, results[]}` |

([protocol.md §Message Types](https://github.com/obsproject/obs-websocket/blob/master/docs/generated/protocol.md))

Key shapes:

- **Event** carries `eventIntent` — the subscription bitmask bit that gates this event — letting clients
  filter client-side as well.
- **Request/Response correlation** is by an opaque client-supplied `requestId` string (clients
  conventionally use UUIDs). `requestType` is mirrored back.
- `requestStatus` = `{ result: bool, code: number, comment?: string }`. `result` is true iff
  `code == 100 (Success)`; `comment` is required for some codes (`GenericError`, `InvalidRequestField`,
  `RequestProcessingFailed`).

## RPC versioning

- `rpcVersion` is an integer incremented **only on breaking protocol changes**. It has been **1** since
  5.0.0 and is still 1 in 5.7.4.
- Negotiation: server advertises its version in `Hello`; client requests its closest supported version in
  `Identify`; server answers with `negotiatedRpcVersion` in `Identified`. If the server cannot honor the
  requested version it closes with `UnsupportedRpcVersion` (4010).
- Non-breaking additions (new requests/events) do **not** bump `rpcVersion`; they are gated by the
  obs-websocket version string, which the docs call only a "soft feature level hint". The authoritative
  discovery mechanism is the `GetVersion` request, whose response includes
  `availableRequests: Array<String>` for the negotiated RPC version, plus
  `obsVersion`, `obsWebSocketVersion`, `rpcVersion`, `supportedImageFormats`, `platform`,
  `platformDescription`
  ([protocol.md §GetVersion](https://github.com/obsproject/obs-websocket/blob/master/docs/generated/protocol.md)).

Implication: capability discovery is *name-list based* (`availableRequests`), not schema based.

## Authentication

- Password-based, challenge-response over SHA-256
  ([protocol.md §Creating an authentication string](https://github.com/obsproject/obs-websocket/blob/master/docs/generated/protocol.md)):
  1. `base64_secret = base64(sha256(password + salt))`
  2. `authentication = base64(sha256(base64_secret + challenge))`
  3. Client sends the result as `authentication` in `Identify`; wrong/missing → close 4009
     `AuthenticationFailed`.
- The `salt` is generated once per server start; the `challenge` is per session
  ([WebSocketServer.cpp `Start()`/`onOpen()`](https://raw.githubusercontent.com/obsproject/obs-websocket/1ef34bf48110c2a18184e50e41cd0b1a855e2147/src/websocketserver/WebSocketServer.cpp)).
- Auth is enabled by default with an auto-generated password since 5.0.0 (minimum 6 characters)
  ([5.0.0 release notes](https://github.com/obsproject/obs-websocket/releases)).
- There is no TLS in obs-websocket itself and no per-client identity, roles, or per-request
  authorization: one password grants full control. `SessionInvalidated` (4011) exists so the UI "Kick"
  button can drop a session; clients must not auto-reconnect after it.

## Request status codes

`RequestStatus` codes group by hundreds
([protocol.md §RequestStatus](https://github.com/obsproject/obs-websocket/blob/master/docs/generated/protocol.md)):

| Range | Meaning | Examples |
|------:|---------|----------|
| 100   | Success | `Success` = 100 |
| 2xx   | Request-shape errors | `MissingRequestType` 203, `UnknownRequestType` 204, `GenericError` 205, `UnsupportedRequestBatchExecutionType` 206, `NotReady` 207 (added 5.3.0; e.g. during scene-collection change) |
| 3xx   | Missing data | `MissingRequestField` 300, `MissingRequestData` 301 |
| 4xx   | Invalid field values | `InvalidRequestField` 400, `InvalidRequestFieldType` 401, `RequestFieldOutOfRange` 402, `RequestFieldEmpty` 403, `TooManyRequestFields` 404 |
| 5xx   | State conflicts | `OutputRunning` 500, `OutputNotRunning` 501, `OutputPaused` 502, `OutputNotPaused` 503, `OutputDisabled` 504, `StudioModeActive` 505, `StudioModeNotActive` 506 |
| 6xx   | Resource problems | `ResourceNotFound` 600, `ResourceAlreadyExists` 601, `InvalidResourceType` 602, `NotEnoughResources` 603, `InvalidResourceState` 604, `InvalidInputKind` 605, `ResourceNotConfigurable` 606, `InvalidFilterKind` 607 |
| 7xx   | Action failures | `ResourceCreationFailed` 700, `ResourceActionFailed` 701, `RequestProcessingFailed` 702, `CannotAct` 703 |

## Request batches

`RequestBatch` ([protocol.md §RequestBatch](https://github.com/obsproject/obs-websocket/blob/master/docs/generated/protocol.md)):

- `haltOnFailure` (default false) stops a serial batch at the first failure; the response then contains
  only processed results.
- `executionType` (`RequestBatchExecutionType`):
  - `SerialRealtime` (0, default) — process serially, as fast as possible.
  - `SerialFrame` (1) — process serially, **in sync with the graphics thread**, for frame-accurate
    animations (the classic DVD-bounce demo).
  - `Parallel` (2) — process on the thread pool; documented as experimental, mainly useful for heavy
    requests like `GetSourceScreenshot`.
- The `Sleep` request (batch-only, `sleepMillis` ≤ 50000 or `sleepFrames` ≤ 10000) inserts delays inside
  serial batches.
- Historical note: 5.0.0-alpha3 had "variables" support (injecting one request's response field into a
  later request's fields within a batch) per its release notes, but this does **not** appear in the final
  5.x protocol documentation — it was dropped before release. Batches in released 5.x are *not*
  transactional: no rollback, and `Parallel` gives no ordering.
- Individual (non-batch) messages are dispatched to a worker thread pool in the server implementation
  ([WebSocketServer.cpp `onMessage`](https://raw.githubusercontent.com/obsproject/obs-websocket/1ef34bf48110c2a18184e50e41cd0b1a855e2147/src/websocketserver/WebSocketServer.cpp)),
  so strict client-visible ordering guarantees exist only within serial batches.

## Event subscription model

`eventSubscriptions` in `Identify`/`Reidentify` is a **bitmask**
([protocol.md §EventSubscription](https://github.com/obsproject/obs-websocket/blob/master/docs/generated/protocol.md)):

| Bit | Name | Notes |
|-----|------|-------|
| 0 | None | value 0: disable all events |
| 1<<0 | General | ExitStarted, VendorEvent, CustomEvent |
| 1<<1 | Config | scene collection / profile lifecycle |
| 1<<2 | Scenes | scene CRUD, program/preview switches |
| 1<<3 | Inputs | input CRUD, volume/mute/track changes |
| 1<<4 | Transitions | |
| 1<<5 | Filters | |
| 1<<6 | Outputs | stream/record/replay/virtualcam state |
| 1<<7 | SceneItems | item CRUD, enable/lock/select |
| 1<<8 | MediaInputs | playback started/ended/action |
| 1<<9 | Vendors | `VendorEvent` third-party channel |
| 1<<10 | Ui | StudioModeStateChanged, ScreenshotSaved |
| 1<<11 | Canvases | **added in obs-websocket 5.7.0** (released in the 32.2.2 baseline) |
| — | All | OR of all category bits above |
| 1<<16 | InputVolumeMeters | high-volume; levels of all active inputs **every 50 ms** |
| 1<<17 | InputActiveStateChanged | high-volume |
| 1<<18 | InputShowStateChanged | high-volume |
| 1<<19 | SceneItemTransformChanged | high-volume |

Semantics: default is `All` (all categories, **excluding** high-volume events); high-volume events must
be explicitly opted into. The gap between category bits (0–11) and high-volume bits (16–19) leaves room
for more categories. `eventIntent` on each emitted event echoes the gating bit.

## Close codes

`WebSocketCloseCode` uses the application-private range 4000+
([protocol.md §WebSocketCloseCode](https://github.com/obsproject/obs-websocket/blob/master/docs/generated/protocol.md)):
`DontClose` 0 (internal), `UnknownReason` 4000, `MessageDecodeError` 4002, `MissingDataField` 4003,
`InvalidDataFieldType` 4004, `InvalidDataFieldValue` 4005, `UnknownOpCode` 4006, `NotIdentified` 4007,
`AlreadyIdentified` 4008, `AuthenticationFailed` 4009, `UnsupportedRpcVersion` 4010,
`SessionInvalidated` 4011 (kick; do not auto-reconnect), `UnsupportedFeature` 4012.

Note 4001 is unused (it was `MessageDecodeError` in pre-release drafts; now 4002).

## Serialization

- JSON (default) over text frames, or MessagePack over binary frames, negotiated per connection via
  `Sec-WebSocket-Protocol`. Mixing frame kinds with the wrong encoding closes with `MessageDecodeError`.
- MessagePack support is implemented via nlohmann_json's `to_msgpack`/`from_msgpack`
  ([WebSocketServer.cpp](https://raw.githubusercontent.com/obsproject/obs-websocket/1ef34bf48110c2a18184e50e41cd0b1a855e2147/src/websocketserver/WebSocketServer.cpp))
  — i.e., the *same* data model, just a different codec. There is no schema or IDL; the protocol
  reference (`protocol.md` / `protocol.json`) is generated from source comments.
- Frame-level concerns (ping/pong keepalive, fragmentation, masking) are left to RFC 6455 / websocketpp
  defaults; the protocol defines no application-level heartbeat, no message size limit of its own, no
  rate limiting, and no per-client backpressure policy.

## Domain surface (what the adapter must map)

The full catalog is in [protocol.md §Events/§Requests](https://github.com/obsproject/obs-websocket/blob/master/docs/generated/protocol.md);
highlights relevant to Prismcast:

- **Addressing is by name (and, in newer versions, UUID) strings.** Scenes/inputs/filters are identified
  by `sceneName`/`inputName`/`sourceName` (plus optional `sceneUuid` etc. in later 5.x); scene items by a
  numeric `sceneItemId` that is only valid within a scene. There are no stable typed IDs — names are
  user-mutable, and rename events (`SceneNameChanged`, `InputNameChanged`, `SourceFilterNameChanged`)
  exist precisely because of this.
- **Single-stream/single-record assumption.** `GetStreamStatus`/`StartStream`/`StopStream`/`ToggleStream`
  and the `Record` group address *the* stream and *the* record output — no output selection parameter.
  `GetOutputList`/`GetOutputStatus`/`StartOutput` address outputs by `outputName` as a lower-level
  escape hatch. `StreamStateChanged`/`RecordStateChanged` events carry only `outputActive`/`outputState`
  (+ `outputPath` for record). This is the largest structural mismatch with Prismcast's OutputGraph
  (ADR-0007); ADR-0010 already decides the adapter maps "the stream" to a designated primary output and
  reports unsupported shapes as typed errors.
- **Studio mode is global** (`GetStudioModeEnabled`, `SetStudioModeEnabled`,
  `TriggerStudioModeTransition`, `SetTBarPosition`, `CurrentPreviewSceneChanged`).
- **Config realm**: scene collections and profiles (`CurrentSceneCollectionChanging` warns clients to
  pause requests — mid-change requests are documented as undefined behavior/crash-prone, and
  `RequestStatus::NotReady` (207) exists for the same window).
- **Persistence slots for clients**: `GetPersistentData`/`SetPersistentData` (per-realm JSON blobs),
  `BroadcastCustomEvent`, and the **vendor API** (`CallVendorRequest`, `VendorEvent`) let third-party
  plugins extend the protocol without changing it.
- **UI-reaching requests** (`OpenInputPropertiesDialog`, projectors, `GetMonitorList`) are client →
  server → OBS-UI calls; some are documented as 4.x-parity shims likely to change.
- **Stats**: `GetStats` exposes CPU/memory/FPS/render/output frame counters plus per-session websocket
  message counters.
- **Sleep** is only legal inside serial batches.

## Notable weaknesses (things the native protocol should not copy)

1. **Name-based addressing** — renames break automation; races between rename and use are unresolvable
   by the client.
2. **No schema/IDL** — request/response shapes documented in a generated markdown/JSON file parsed by
   client-library authors; typos in field names are runtime errors (`MissingRequestField`, code 300).
3. **Monolithic version for additive changes** — `availableRequests` name-listing is the only feature
   discovery; no per-request deprecation metadata on the wire.
4. **Coarse subscriptions** — category bitmask only; no per-entity filtering server-side (a client that
   wants one source's volume gets every active input every 50 ms).
5. **No backpressure/limits specified** — a slow client subscribed to `InputVolumeMeters` relies on
   websocketpp/TCP buffering; nothing in the protocol sheds load. (Cf. the local `rust-web` skill's
   overload guidance: bounded queues, drop-or-disconnect slow consumers, snapshot-on-lag.)
6. **Untyped errors over the wire** — integer code + free-text comment; `comment` has no structure
   (e.g. which field failed is stuffed into the comment string).
7. **Auth is all-or-nothing** — single password, no roles/read-only mode, no TLS in the server itself.
8. **Batches are not atomic** and `Parallel` undercuts ordering; `SerialFrame` couples the RPC layer to
   the graphics thread.
9. **Close-code-driven error signaling** during handshake means no retry semantics and no structured
   reason payload.
10. **Singleton outputs** — see Domain surface above.

## Conclusions for Prismcast

Implications for the native protocol (PLAN §22, `prismcast-protocol` + `prismcast-remote`) and the
future adapter (ADR-0010, Phase 9):

1. **Adopt the interaction shape, not the wire format** (already decided in ADR-0010): the
   Hello/Identify/Identified handshake, `{op, d}`-style envelope, request/response correlation by
   client-supplied ID, and a dedicated batch message are validated by years of obs-websocket use.
   Concrete borrowing list for ARCH-007 (remote protocol):
   - server-first greeting carrying versions + auth challenge;
   - explicit session establishment before any traffic, with strict close-on-violation rules;
   - response mirroring the request's type and ID;
   - event messages carrying their subscription category;
   - `Reidentify`-style subscription update without reconnect.
2. **Versioning**: copy the *negotiated integer RPC version for breaking changes* idea, but improve on
   additive-change discovery: instead of a bare `availableRequests` name list, serve a **machine-readable
   capability/schema document** (e.g. generated from `prismcast-protocol`'s serde types) so clients can
   introspect request/response shapes, deprecations, and per-request "since" versions. This is cheap in
   Rust (types are the single source of truth) and fixes obs-websocket weaknesses 2–3.
3. **Subscriptions**: use a typed subscription model — category flags **plus optional entity filters**
   (e.g. `meters: { source_ids: [...], interval_ms }`) — instead of a flat bitmask. Bitmasks are fine
   for a wire encoding but the native protocol should not need a "high-volume opt-in by magic bit 16"
   convention; make rate/interval explicit per subscription. Open question for ARCH-007: do we want
   subscription *renegotiation* to be a request (queryable, errorable) rather than a special op code?
4. **Serialization**: negotiate codec per connection via `Sec-WebSocket-Protocol` exactly like
   obs-websocket (`prismcast.json`, `prismcast.msgpack`?). In Rust, serde_json + rmp-serde give the same
   dual-codec shape from one data model. Decision needed in ARCH-007 whether MessagePack is v0.1 scope
   or deferred; note that the Unix IPC transport (ADR-0006) can share the same codec layer.
5. **Auth**: obs-websocket's salt+challenge SHA-256 is adequate for localhost-ish trust but weak for the
   "remote-first" goal (no roles, no TLS by itself). Native protocol should plan for: TLS termination
   (rustls per PLAN §23), token-based auth with scopes (read-only vs control), and per-session identity
   surfaced in logs (`tracing` with session context). The *challenge-response* pattern is still worth
   copying for the Unix-IPC/local path where TLS is absent.
6. **Errors**: keep a grouped integer code space (it is genuinely convenient for client switch
   statements) but add a **structured error payload** (`{ code, kind, message, details: { field?,
   expected? } }`) instead of comment-stuffing. Maps naturally onto our `thiserror` domain errors.
7. **Batches**: support serial batches with `haltOnFailure` from day one (trivial on an actor-style
   command dispatcher). Do **not** implement `SerialFrame` (graphics-thread coupling) in the native
   protocol; if frame-accurate sequencing is ever needed, it belongs in the compositor, not the RPC
   layer. Decide explicitly whether native batches are transactional (all-or-nothing via undo group —
   our Command/undo architecture actually makes this feasible, unlike OBS) or best-effort like OBS.
   Recommend: best-effort serial for v1, transactional as an explicit later feature. Note: obs-websocket
   dropped its batch "variables" feature before release — evidence that response-chaining inside batches
   is not essential.
8. **Backpressure** (weakness 5): the native server must have bounded per-session outbound queues and a
   written policy for lagging consumers (drop meter-class events → send resync snapshot → disconnect as
   last resort), consistent with AGENTS.md's "no unbounded channels" rule and the `rust-web` skill's
   overload budgets.
9. **Adapter mapping notes** (for the Phase 9 adapter, recording decisions for later):
   - Name↔ID bridge: maintain a name index in the adapter and translate OBS name-based requests to
     Prismcast typed-ID commands; subscribe to rename events to keep the index coherent. OBS clients
     assume names are stable identifiers; Prismcast must not weaken its typed-ID rule to match.
   - Stream/Record singletons: map onto a designated "primary stream output" / "primary record output"
     in the OutputGraph (per ADR-0010 §5), with `GetOutputList`-style requests mapped per-output-name.
     Multistream-specific state is simply not representable in 5.x — document, don't fake.
   - Studio mode, scene collections, profiles, hotkeys, vendor events, and persistent-data slots map
     cleanly onto planned Prismcast concepts (PLAN §22 lists equivalent benefits); UI-reaching requests
     (projectors, dialogs) should return `RequestStatus::GenericError`-style typed "unsupported" errors
     rather than no-op.
   - The adapter should speak rpcVersion 1 only; no 4.x support (4.x has been EOL since OBS 28 and the
     community has fully migrated).
10. **Testing hook**: because obs-websocket's protocol is documented in machine-readable
    `protocol.json` and has many independent client implementations (obs-websocket-js, simpleobsws,
    obws in Rust), the adapter can be validated against real third-party clients as PLAN §51 requires.
    `obws` (Rust obs-websocket client) is a candidate dev-dependency for adapter integration tests —
    verify its maintenance status when the adapter task starts (open question).

### Open questions handed to ARCH-007 / Phase 9

- Native codec set: JSON-only at first, or JSON+MessagePack from the start? (obs-websocket proves the
  dual-codec-via-subprotocol pattern is cheap.)
- Are native batches best-effort or optionally transactional via the undo system?
- Subscription model: bitmask-compatible shape (easier adapter) vs richer structured subscriptions
  (better native ergonomics)? The two can coexist if the wire format is structured but mappable.
- Do we need an equivalent of `SerialFrame`/`Sleep` for animation scripting, or is that a compositor
  timeline concern?
- Which Rust obs-websocket client (if any) is healthy enough to pin for adapter conformance tests?

## Sources

- [obs-websocket 5.x.x protocol reference (generated, master = 5.7.4)](https://github.com/obsproject/obs-websocket/blob/master/docs/generated/protocol.md)
- [obs-websocket CMakeLists.txt at the commit pinned by OBS 32.2.2](https://raw.githubusercontent.com/obsproject/obs-websocket/1ef34bf48110c2a18184e50e41cd0b1a855e2147/CMakeLists.txt) (version 5.7.4, RPC 1, deps: WebSocket++ 0.8, Asio 1.12.1, nlohmann_json 3.11)
- [obs-websocket CMakeLists.txt at master](https://raw.githubusercontent.com/obsproject/obs-websocket/master/CMakeLists.txt) (confirms master == 5.7.4, so generated docs describe released behavior)
- [OBS 32.2.2 submodule pin for plugins/obs-websocket](https://api.github.com/repos/obsproject/obs-studio/contents/plugins/obs-websocket?ref=32.2.2)
- [WebSocketServer.cpp @ pinned commit](https://raw.githubusercontent.com/obsproject/obs-websocket/1ef34bf48110c2a18184e50e41cd0b1a855e2147/src/websocketserver/WebSocketServer.cpp) (subprotocol selection, Hello construction, auth salt/challenge lifecycle, thread-pool dispatch, MsgPack via nlohmann_json)
- [obs-websocket releases](https://github.com/obsproject/obs-websocket/releases) (5.0.0 notes: default port 4455, auth on by default, request-batch execution types, InputVolumeMeters; bundling into OBS from 28.0; alpha3 batch "variables" later dropped)
- Local: `docs/adr/ADR-0010-obs-websocket-compat.md`, `docs/adr/ADR-0006-unix-socket-ipc.md`,
  `.agents/skills/rust-web/references/middleware-and-lifecycle.md` (overload/backpressure budgets).
