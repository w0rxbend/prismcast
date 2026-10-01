# ADR-0020: obs-websocket adapter placement and mapping decisions

Status: accepted for OBSWS-001.

## Context

ADR-0010 decided that obs-websocket 5.x compatibility arrives as an adapter
translating onto the native Core Command API; OBSWS-001 builds it. Four
placement/mapping questions need to be fixed before code: where the adapter
lives, how its wire types relate to `prismcast-protocol`, how OBS's
name/number addressing maps onto typed IDs and the OutputGraph, and what the
adapter advertises.

## Decision

**(a) Placement: a module `obs_ws` inside `prismcast-remote`, not a new
crate.** The machinery worth reusing — `AuthConfig::authenticate`,
`auth::challenge_response` (byte-identical to obs's SHA-256 construction),
`server::EventFanout`, and the `session_kit` extracted from the native
session engine — is `pub(crate)` or private. A separate crate would force
widening that visibility for exactly one consumer. If the module outgrows
the crate (MessagePack, the full request catalog), extraction is a mechanical
follow-up; the module boundary already isolates it.

**(b) Wire types: the adapter defines its own obs-shaped serde types
(`obs_ws::proto`) and never extends `prismcast-protocol`.** The native wire
schema stays untouched (the golden tripwire tests stay green); obs field
names (`requestType`, `eventSubscriptions`, ...) live only in the adapter.
Request translation pivots obs `requestType` + `requestData` through native
`RequestKind` and then calls the existing `map::command_from_wire`, so the
adapter inherits the native drift guards instead of growing a parallel
command-mapping table.

**(c) Addressing: stateless name→ID resolution, stateful scene-item IDs.**
`sceneName`/`inputName` resolve by scanning the latest `AppSnapshot`; on
duplicate names the first match wins and a warning is logged — a documented
divergence from OBS, where duplicate names are not creatable through the
protocol. Numeric `sceneItemId` needs a stateful per-scene `ItemIdMap`
(sequential integers per OBS semantics) with eviction on every removal path
(scene item removed, scene removed, snapshot replace). Name renames need no
index maintenance precisely because resolution is stateless.

**(d) Advertisement: `obsWebSocketVersion` "5.7.4", `rpcVersion` 1, and an
accurate `availableRequests`.** 5.7.4 is the released baseline pinned by OBS
32.2.2 (RES-007); rpcVersion has been 1 since 5.0.0. `availableRequests`
lists exactly the request types the adapter implements, guarded by a drift
test (every listed type must dispatch). `obsStudioVersion` is omitted from
`Hello`: there is no OBS version to report and clients treat it as optional.
The version is lowered only if real clients misbehave.

**(e) Singleton outputs: designated primaries, typed errors when absent.**
obs's single-stream model (`StartStream`, `GetRecordStatus`, ...) maps onto
the OutputGraph (ADR-0007) via designated primaries: the stream singleton is
the first `Rtmp` output, falling back to `Srt`/`Whip`; the record singleton
is the first `Recording` output. Absent primaries answer with typed request
statuses — 600 `ResourceNotFound` when no such output exists, 500/501
`OutputRunning`/`OutputNotRunning` on state conflicts — never silent no-ops.

## Consequences

- The adapter adds no dependency and no `prismcast-protocol` churn; the
  public surface of `prismcast-remote` grows by `obs_ws` only.
- The foundation slice (handshake, wire types, session engine, batch
  scaffolding, subscription bitmask) lands before any request translation;
  every request is answered with a typed 204 `UnknownRequestType` until the
  translation slice replaces the stub.
- Duplicate-name first-match resolution and the omitted `obsStudioVersion`
  are documented divergences from OBS behavior, acceptable for the MVP.
- `SerialFrame`/`Parallel` batch execution, MessagePack, and high-volume
  event producers stay deferred (OBSWS-002+); requests for them get typed
  errors (206 whole-batch for execution types 1/2).

## Alternatives considered

A separate `prismcast-obs-compat` crate was rejected for now: it would force
premature `pub` widening of auth, fan-out, and session machinery for one
consumer, and the adapter is a thin translation layer by design (ADR-0010 §3),
not a service. Mapping obs requests directly to Core Commands without
pivoting through `RequestKind` was rejected: it duplicates the
wire→command table and silently drifts from the native protocol's
permissions and validation. Making stream/record singletons configurable
user settings was deferred: first-of-kind primaries match OBS mental models
for the MVP and can become explicit settings later without a wire change.
