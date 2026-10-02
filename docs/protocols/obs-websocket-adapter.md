# obs-websocket 5.x Compatibility Adapter

Version: adapter implements **obs-websocket 5.7.4** semantics with `rpcVersion` **1** (OBSWS-001;
ADR-0010, ADR-0020). Implemented by `crates/prismcast-remote`'s `obs_ws` module
(`ObsWsServer`). This document is the adapter protocol reference: what an obs-websocket client
(Stream Deck tools, `obs-websocket-js`, `obws`, mobile remotes) can expect when it connects to
Prismcast instead of OBS.

The adapter is a **thin translation layer**: every obs request pivots through the native
`RequestKind` → `map::command_from_wire` → Core Command path, and domain events are translated to
obs events. The native protocol (`native-protocol.md`) and its wire schema are untouched; the
adapter defines its own obs-shaped wire types (`obs_ws::proto`, golden-pinned against 5.7.4).

## 1. Transport and framing

| Property | Value |
|---|---|
| Transport | WebSocket over TCP (`ws://`); no TLS yet (WS-003) |
| Default bind | `127.0.0.1:4455` (the obs-websocket default port, loopback only) |
| Enabled | **off by default**; requires an explicit credential (password or token) |
| Codec | JSON text frames or MessagePack binary frames, negotiated by `Sec-WebSocket-Protocol`: `obswebsocket.json` is echoed when offered (JSON wins when both are offered), `obswebsocket.msgpack` selects MessagePack, and no subprotocol means JSON (the obs default) |
| Message size limit | 1 MiB inbound raw payload (both codecs); larger → close 4002 |

Both codecs encode the **same** `{op, d}` envelope shape: MessagePack uses struct-as-map encoding
(string-keyed maps), so every wire type is identical in either codec and the choice is invisible
above the framing layer. In a MessagePack session **every** protocol frame — Hello, Identified,
responses, events — is a binary frame; a text frame closes the session with **4002**
(`MessageDecodeError`), as does a binary frame in a JSON session, an undecodable payload, or
hostile MessagePack (ext types, garbage bytes).

Offering only unsupported subprotocols **refuses the HTTP upgrade with 400**. Upstream silently
defaults to JSON; refusing unknown codecs is deliberate hardening (see §10).

The local-trust auth policy (`AuthConfig::AllowLocal`) is rejected at `ObsWsServer::bind` with
`AuthRequired`, same as the native `WsServer`: a network transport always requires a credential.

## 2. Handshake and authentication

```
client                                  server
  |  (connect)                            |
  |<------------- Hello (op 0) -----------|  obsWebSocketVersion 5.7.4, rpcVersion 1, authentication?
  |--------------- Identify (op 1) ------>|  rpcVersion, authentication?, eventSubscriptions?
  |<------------- Identified (op 2) ------|  negotiatedRpcVersion 1
  |<------ Event / Request/Response ----->|  steady state
```

- `Hello` carries `obsWebSocketVersion: "5.7.4"`, `rpcVersion: 1`, and — for password-configured
  servers — `authentication: {challenge, salt}`. The salt is stable per server start; the
  challenge is fresh per session. `obsStudioVersion` is **omitted** (ADR-0020 §d: there is no OBS
  build to report and clients treat the field as optional).
- **Password auth** is obs's SHA-256 challenge-response, byte-identical to the native protocol's:
  `base64(sha256(base64(sha256(password + salt)) + challenge))`, answered in
  `Identify.authentication`. Both are `prismcast_remote::auth::challenge_response`.
- **Token auth** is a Prismcast extension: on token-configured servers the
  `Identify.authentication` string carries the bearer token verbatim (the `Hello` offers no
  challenge).
- Wrong or missing authentication closes with **4009**. Anything other than one `Identify` before
  identification closes with **4007**; a second `Identify` closes with **4008**; an unknown/missing
  `op` closes with **4006**. A message with a top-level `request-type` field marks an obs-websocket
  4.x client and closes with **4010** (the 4.x protocol is **not implemented**).

### rpcVersion policy

`rpcVersion` has been 1 since obs-websocket 5.0.0, and this adapter serves **exactly 1**:
`Identify.rpcVersion != 1` closes with **4010** (`UnsupportedRpcVersion`), and `Identified` always
reports `negotiatedRpcVersion: 1`.

### Reidentify

`Reidentify` (op 3) updates `eventSubscriptions` only and is answered with a fresh `Identified`
(op 2) — upstream behavior. Auth cannot change on a live session.

### Close codes

obs application-private codes (4000+): 4000 `UnknownReason` (also used for slow-consumer shedding —
obs defines no backpressure code), 4002 `MessageDecodeError` (undecodable or malformed `d`,
oversized messages), 4005 `InvalidDataFieldValue` (out-of-range batch `executionType`), 4006
`UnknownOpcode`, 4007 `NotIdentified`, 4008 `AlreadyIdentified`, 4009 `AuthenticationFailed`, 4010
`UnsupportedRpcVersion`. Server shutdown closes with RFC 6455 **1001** (`going away`), like
upstream's "Server stopping." 4001, 4003, 4004 are not emitted (see §10 for the 4002-instead
divergence).

## 3. Requests

Client → server: `Request` (op 6) `{requestType, requestId, requestData?}`; server answers
`RequestResponse` (op 7) mirroring `requestType`/`requestId` with
`requestStatus: {result, code, comment?}` and optional `responseData`. Unsupported request types
are answered with a typed **204** (`UnknownRequestType`), never silently dropped.

### Request mapping (OBSWS-001 implemented set)

| obs `requestType` | core command / query | Notes |
|---|---|---|
| `GetVersion` | — (no core call) | obs-shaped fields; `availableRequests` is drift-guarded (every advertised type dispatches) |
| `Sleep` | — (local delay) | `sleepMillis` ≤ 50 000; standalone use accepted (upstream: batches only) |
| `BroadcastCustomEvent` | — (no core call; server-wide broadcast bus) | `eventData` required, must be an object (300/401); every `General`-subscribed session — originator included — receives `CustomEvent` with the payload verbatim |
| `GetSceneList` | snapshot query | `currentProgramScene*` are `null` when no scene is current; preview fields only in studio mode |
| `GetCurrentProgramScene` | snapshot query | 600 when no current scene |
| `SetCurrentProgramScene` | `SetCurrentScene` | by `sceneName` |
| `GetCurrentPreviewScene` | snapshot query | 506 when studio mode is inactive |
| `SetCurrentPreviewScene` | `SetPreviewScene` | 506 when studio mode is inactive |
| `CreateScene` | `AddScene` | 601 on duplicate name; `responseData.sceneUuid` |
| `RemoveScene` | `RemoveScene` | evicts the scene's `ItemIdMap` registry |
| `SetSceneName` | `RenameScene` | 601 on duplicate name |
| `GetSceneItemList` | snapshot query | mints numeric `sceneItemId`s bottom-to-top |
| `GetSceneItemId` | snapshot query | mints on demand; `searchOffset` honored |
| `CreateSceneItem` | `AddSceneItem` (+ `SetSceneItemVisible` when `sceneItemEnabled: false`) | `responseData.sceneItemId` |
| `RemoveSceneItem` | `RemoveSceneItem` | evicts the minted number |
| `SetSceneItemEnabled` | `SetSceneItemVisible` | numeric `sceneItemId` |
| `GetSceneItemTransform` | snapshot query | `sourceWidth`/`sourceHeight` are 0 until capture reports dimensions |
| `SetSceneItemTransform` | `Transaction` of `SetSceneItemTransform` / `SetSceneItemCrop` / `SetSceneItemBounds` | only touched groups are dispatched; an empty patch is a successful no-op |
| `GetStudioModeEnabled` | snapshot query | |
| `SetStudioModeEnabled` | `SetStudioModeEnabled` | |
| `TriggerStudioModeTransition` | `TransitionToProgram` | 506 when studio mode is inactive |
| `GetInputList` | snapshot query | scenes-as-sources excluded (upstream parity); `inputKind` filter honored |
| `GetInputMute` | snapshot query | mixer state |
| `SetInputMute` | `SetSourceMuted` | |
| `ToggleInputMute` | `SetSourceMuted` (inverted) | `responseData.inputMuted` |
| `GetInputVolume` | snapshot query | both `inputVolumeMul` and `inputVolumeDb` |
| `SetInputVolume` | `SetSourceVolume` | `inputVolumeMul` preferred when both are given (upstream parity); `mul` 0 → −100 dB |
| `SetInputName` | `RenameSource` | 601 on duplicate name |
| `GetCurrentSceneTransition` | snapshot query | |
| `SetCurrentSceneTransition` | `SetTransition` | resolves display name or kind id; only the kind switches |
| `GetOutputList` | snapshot query | |
| `GetOutputStatus` | snapshot query | runtime metrics read as zero (stats are OBSWS-002+) |
| `StartOutput` / `StopOutput` / `ToggleOutput` | `StartOutput` / `StopOutput` | by `outputName`; state pre-checks give 500/501 |
| `GetStreamStatus` / `StartStream` / `StopStream` / `ToggleStream` | same, on the designated **primary stream** output | §5 |
| `GetRecordStatus` / `StartRecord` / `StopRecord` / `ToggleRecord` | same, on the designated **primary record** output | §5; `GetRecordStatus` adds `outputPaused: false` |

### Request status codes

| condition | code |
|---|---|
| success | 100 |
| unknown/unsupported `requestType` | 204 |
| core actor shut down | 207 |
| missing `requestData` field | 300 |
| invalid field value (bad enum string, generic `InvalidInput`) | 400 |
| field of the wrong JSON type | 401 |
| numeric field out of range | 402 |
| output already running / core "cannot start" | 500 |
| output not running / core "cannot stop" / absent primary on status+stop | 501 |
| studio mode not active (preview/transition requests) | 506 |
| name/number/uuid resolution failure, unknown transition | 600 |
| duplicate target name | 601 |
| permission denied | 703 |
| media/IO/persistence failures | 701 |

### Batches

`RequestBatch` (op 8) → `RequestBatchResponse` (op 9). All three execution types are implemented
(ADR-0021 §d):

- **`SerialRealtime`** (executionType 0, the default): requests run serially, in order, as fast as
  possible; `haltOnFailure` ends the batch at the first failure with a shortened `results`; `Sleep`
  delays are honored up to the 50 s cap (`sleepMillis`; `sleepFrames` is a typed 400 here — there
  is no frame clock outside `SerialFrame`).
- **`SerialFrame`** (executionType 1): the same serial loop (in order, `haltOnFailure` honored),
  except `Sleep.sleepFrames` resolves against the active profile's frame rate
  (`frames × fps_den / fps_num` seconds; 60 fps when no profile is active or the profile's rate is
  degenerate), with the same 50 s total-sleep cap. There is no graphics thread to couple to, so
  frame timing is a wall-clock approximation of upstream's graphics-thread sync (§10).
- **`Parallel`** (executionType 2): every member runs in its own task with at most **8** in flight
  per batch (spawning member *n* awaits a finished one), so a `Sleep` member never serializes the
  batch. Upstream defines no ordering between members; the core actor serializes the underlying
  commands itself. Results are returned in **request order**, one per member; `haltOnFailure` is
  **ignored** (upstream semantics). Minted `sceneItemId` numbers under Parallel are opaque — no
  assignment order is guaranteed between members.

An out-of-range `executionType` closes the session with 4005, like upstream. Batches cannot nest
through this adapter (there is no `RequestBatch` request type).

## 4. Events and subscriptions

Server → client: `Event` (op 5) `{eventType, eventIntent, eventData?}`. Every event carries the
obs-exact `eventIntent` bit; delivery gating uses native event categories (§4.2), which are
coarser.

### Event mapping

| domain event | obs `eventType` | `eventIntent` | `eventData` specifics |
|---|---|---|---|
| `SceneEvent::Added` | `SceneCreated` | Scenes | `sceneName`, `sceneUuid`, `isGroup: false` |
| `SceneEvent::Removed` | `SceneRemoved` | Scenes | name resolved from the per-session memo (the snapshot no longer has the scene) |
| `SceneEvent::Renamed` | `SceneNameChanged` | Scenes | `oldSceneName`, `sceneName`, `sceneUuid` |
| `SceneEvent::CurrentChanged` | `CurrentProgramSceneChanged` | Scenes | `sceneName`, `sceneUuid` |
| `SceneEvent::ItemAdded` | `SceneItemCreated` | SceneItems | `sceneItemId` is the stable UUID-derived placeholder (§6) |
| `SceneEvent::ItemRemoved` | `SceneItemRemoved` | SceneItems | names from the memo |
| `SceneEvent::ItemUpdated` (visibility flip only) | `SceneItemEnableStateChanged` | SceneItems | other field changes emit nothing in MVP scope |
| `SourceEvent::Added` | `InputCreated` | Inputs | `inputKind` from the adapter's kind table; `defaultInputSettings: {}` |
| `SourceEvent::Removed` | `InputRemoved` | Inputs | name from the memo |
| `SourceEvent::Renamed` | `InputNameChanged` | Inputs | `oldInputName`, `inputName`, `inputUuid` |
| `AudioEvent::MixerChanged` (mute flip) | `InputMuteStateChanged` | Inputs | detected by diffing against the session memo |
| `AudioEvent::MixerChanged` (volume change) | `InputVolumeChanged` | Inputs | both `inputVolumeMul` and `inputVolumeDb` |
| `OutputEvent::StateChanged` | `OutputStateChanged`¹ | Outputs | `outputName`, `outputUuid`, `outputState` (upstream `OBS_WEBSOCKET_OUTPUT_*` vocabulary) |
| … output is the stream primary | + `StreamStateChanged` | Outputs | `outputActive`, `outputState` |
| … output is the record primary | + `RecordStateChanged` | Outputs | + `outputPath: null` (the domain does not model the path yet) |
| `SystemEvent::StudioModeChanged` | `StudioModeStateChanged` | Ui | `studioModeEnabled` |
| `SystemEvent::PreviewSceneChanged` | `CurrentPreviewSceneChanged` | Scenes | gated by native `System`, see §4.2 note |
| — (server-generated: a client's `BroadcastCustomEvent`) | `CustomEvent` | General | `eventData` verbatim from the request; relayed to every `General`-admitting session, originator included |

¹ `OutputStateChanged` is a **Prismcast extension**: upstream has no per-output state event (its
outputs are singletons). The singleton events are emitted only when the changing output is the
designated primary (§5).

Everything else — scene reorder, source settings/enabled/runtime, audio bus/route changes, output
add/remove/policy, transition, profile, and collection events — has no obs-websocket 5.x
counterpart in MVP scope and is never emitted.

### Subscription bitmask → native categories

`Identify`/`Reidentify` `eventSubscriptions` is the obs bitmask; it becomes a native
`SubscriptionSet` at the session boundary:

| obs bit (value) | native category |
|---|---|
| `General` (1) | `General` |
| `Config` (2) | `System` |
| `Scenes` (4) | `Scene` |
| `Inputs` (8) | `Source` + `Audio` |
| `Transitions` (16) | `System` |
| `Filters` (32) | `Source` |
| `Outputs` (64) | `Output` |
| `SceneItems` (128) | `Scene` |
| `MediaInputs` (256) | `Source` |
| `Vendors` (512) | `General` |
| `Ui` (1024) | `System` |
| `Canvases` (2048) | — (no native equivalent; accepted, inert) |
| `InputVolumeMeters` (1<<16) | `Meter` (translation deferred; native post-fader meters available) |
| `InputActiveStateChanged`, `InputShowStateChanged`, `SceneItemTransformChanged` (1<<17..19) | — (high-volume; accepted, inert without producers) |

Absent `eventSubscriptions` in `Identify` means `All` (category bits 0–11, no high-volume). Inert
bits are accepted silently — subscribing is not an error; they admit no events until producers
exist (OBSWS-002+).

**Gating granularity note:** gating happens by native category *before* translation, so the bit
that admits an event may be wider than its `eventIntent`. Example: `CurrentPreviewSceneChanged` is
native `System`, so it is admitted by the `Config`/`Transitions`/`Ui` bits, not by `Scenes` alone
— while its reported `eventIntent` is `Scenes`.

## 5. Stream/record singletons → designated primaries

obs models one stream and one record; Prismcast has an OutputGraph. The singleton requests and
events address **designated primaries** (ADR-0020 §e):

- **stream** = the first `Rtmp` output, falling back to the first `Srt`, then `Whip` (insertion
  order).
- **record** = the first `Recording` output.

Absent primary → typed errors, never silent no-ops: **600** on start/toggle (not found), **501**
on status/stop (not running); state conflicts use the standard 500/501. `StreamStateChanged` /
`RecordStateChanged` fire only for the primary's state changes; every output's changes are visible
through the `OutputStateChanged` extension event.

## 6. Addressing: names and numeric `sceneItemId`s

- Scenes, inputs, and outputs are addressed by **name**, resolved statelessly against the latest
  snapshot. On duplicate names the **first match in list order wins** and a warning is logged (OBS
  cannot create duplicates through the protocol; Prismcast's core auto-uniquifies names, so
  duplicates only arise from state restore or injection).
- Scene items use obs's numeric, per-scene `sceneItemId`, backed by a server-wide stateful
  **ItemIdMap**: sequential positive integers minted lazily on enumeration (`GetSceneItemList`,
  `GetSceneItemId`) or creation, shared by all sessions so clients agree on numbers. Numbers are
  never reused within a registry's lifetime; eviction happens on every removal path (item removed,
  scene removed, scene-collection switch; a lagged event stream clears the whole map).
- Until the request slice's map is shared with the event path at integration, event payloads carry
  a **stable UUID-derived placeholder** `sceneItemId`; request payloads carry the real registry
  numbers.

## 7. Version advertisement and `GetVersion`

`Hello` advertises `obsWebSocketVersion: "5.7.4"` (the OBS 32.2.2 baseline, RES-007) and
`rpcVersion: 1`. `GetVersion` returns `availableRequests` equal to the implemented set (§3),
`supportedImageFormats: []` (screenshots are OBSWS-002+), `platform: "linux"`,
`platformDescription: "Linux (Prismcast obs-websocket adapter)"`, and `obsVersion: "30.2.0"` —
a compatibility constant, not Prismcast's own version (ADR-0021). "30.2.0" is the minimum
`obsStudioVersion` the `obws` client's default gate accepts; advertising the 32.x baseline
would imply features this adapter answers 204 for. Clients therefore pass both of obws's
default version checks (studio ≥ 30.2, websocket ^5.5) unskipped; the conformance test
(`tests/obs_ws_obws.rs`) connects with no `DangerousConnectConfig` overrides.

## 8. Conformance

`tests/obs_ws_obws.rs` drives the adapter with `obws` 0.15 (the reference Rust client) over a real
socket: password handshake, `GetVersion`, scene list/create/program-switch, and input mute toggle,
with results asserted against the core `AppHandle` state. `tests/obs_ws.rs`,
`tests/obs_ws_events.rs`, and `tests/obs_ws_requests.rs` pin the wire behavior with scripted
tungstenite clients (handshake matrix, auth, subprotocols, batches, status codes, event gating).

## 9. Not implemented (OBSWS-002+)

OBS meter translation and other
high-volume events (`InputVolumeMeters`, `InputActiveStateChanged`,
`InputShowStateChanged`, `SceneItemTransformChanged`), filters, screenshots, stats
(`GetStats`/`GetOutputStats`; output runtime metrics read as zero), vendor and persistent data,
media-input control, virtualcam and replay-buffer requests, UI-reaching requests, source settings
(`GetInputSettings`/`SetInputSettings`), the obs-websocket 4.x protocol, and TLS (WS-003).
Unsupported request types get the typed 204, never a silent no-op.

AUDIO-001 supplies native post-fader source peak/RMS telemetry. OBS
`InputVolumeMeters` remains deferred: its channel triples also include a
distinct input-peak measurement, which the current graph does not produce.
No inferred or duplicated input peak is advertised.

## 10. Documented divergences from upstream obs-websocket

- **Subprotocol hardening:** unknown/unsupported subprotocol offers refuse the HTTP upgrade with
  400 instead of silently defaulting to JSON; when both known tags are offered, **JSON wins** (a
  fixed priority, deterministic across clients).
- **Malformed `d` payloads** close with 4002 (`MessageDecodeError`) where upstream sometimes uses
  the more specific 4003/4004/4005; invalid batch `executionType` values do close with 4005 like
  upstream.
- **Token auth** is a Prismcast extension: on token-configured servers `Identify.authentication`
  carries the bearer token.
- **`OutputStateChanged`** (per-output, `outputName`/`outputUuid`-addressed) is a Prismcast
  extension event; upstream's singleton `StreamStateChanged`/`RecordStateChanged` are emitted only
  for the designated primaries.
- **Duplicate-name resolution** is first-match-in-list-order with a warning (OBS cannot create
  duplicates through the protocol).
- **`Failed` output state** maps to `OBS_WEBSOCKET_OUTPUT_STOPPED`; `Degraded` has no obs state and
  emits nothing.
- **`RecordStateChanged.outputPath` is `null`** — the domain does not model the record path yet.
- **`SetInputVolume` with `inputVolumeMul: 0` maps to −100 dB**, not −∞: the core requires finite
  gains.
- **`Sleep` is accepted standalone** (upstream registers it for batches only), keeping
  `availableRequests` truthful; `sleepFrames` resolves only inside a `SerialFrame` batch (typed
  400 elsewhere, since no other context has a frame clock).
- **`SerialFrame` is not graphics-thread coupled** (there is none): the serial batch loop is
  identical to `SerialRealtime`, and frame timing is a wall-clock approximation driven by the
  active profile's frame rate (§3).
- **`Parallel` concurrency is bounded at 8 in-flight members per batch** (upstream's thread pool
  is unbounded by contract); results still return in request order and `haltOnFailure` is ignored,
  like upstream.
- **Adapter-specific `inputKind`/`outputKind` strings** (`color_source`, `v4l2_input`,
  `pipewire_display_capture`, `rtmp_output`, `recording_output`, …; `prismcast_*` for kinds with no
  OBS counterpart in events): there is no OBS plugin registry behind them; `unversionedInputKind`
  mirrors `inputKind`.
- **Gating granularity:** event delivery is gated by native categories, which are coarser than obs
  bits (see §4.2, e.g. `CurrentPreviewSceneChanged`).
- **Backpressure** (absent upstream): a bounded outbound queue per session; persistent overflow
  sheds the session with 4000 (`UnknownReason`), since obs defines no slow-consumer code.
- **The `CustomEvent` bus is bounded** (capacity 64, server-wide): a session that falls behind
  drops custom events with a log line (obs has no resync contract), exactly like domain-event lag
  — the session is never killed over a lagged broadcast.
- **`obsStudioVersion` omitted** from `Hello` (ADR-0020 §d); `obsVersion` in `GetVersion` is the
  compatibility constant `"30.2.0"`, not the real OBS or Prismcast version (§7, ADR-0021).
- Event `sceneItemId` values are UUID-derived placeholders until the `ItemIdMap` is shared with
  the event path at integration (§6).

## References

- PLAN.md §22 (WebSocket API), §51 (obs-websocket compatibility), §76 (central invariant).
- ADR-0010 (native protocol vs adapter), ADR-0020 (adapter placement and mapping decisions).
- RES-007: `docs/research/obs-websocket-protocol.md`.
- `docs/protocols/native-protocol.md` (the protocol this adapter translates onto).
- Upstream: obs-websocket 5.7.4 protocol reference (obsproject/obs-websocket).
