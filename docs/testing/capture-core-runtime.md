# Capture core/API runtime contract verification

CAPTURE-002 adds AuthorizeSourceCapture to the single command surface. It accepts
only enabled PipeWireDisplay/PipeWireWindow sources and ControlScenes permission.
The same command explicitly retries terminal failures; it is rejected in atomic
Transactions and has no undo inverse. Source creation/restoration/enabling and
scene or canvas changes never request a chooser.

AppHandle::authorize_source_capture passes an optional exported parent through a
local-only envelope. Identifiers are limited to 2048 bytes with wayland:/x11:
validation; Debug redacts them. The wire command carries only SourceId. An absent,
closed or full singleton owner request receiver rejects before runtime/events are
published. The actor reserves queue capacity, publishes Authorizing, then delivers
the effect, so a receiver cannot mistake the current request for a stale snapshot.

The request channel and transient snapshot map each have capacity 8. Runtime
observations retain terminal diagnostics; disabling/removing/reconfiguring a source
frees its map slot. Reports share the bounded application actor queue. The opaque
owner capability and monotonic CaptureGeneration are both checked before publishing;
terminal generations cannot become Active again. Retry always allocates a fresh
generation. Report diagnostics normalize controls and truncate safely to 512 UTF-8
bytes before enqueue. Active requires actual negotiated pixels, 1..8192 per axis;
terminal states must clear dimensions. Tests cover the real-display-derived
6144x3456 case without pretending portal logical coordinates are pixel caps.

AppSnapshot's runtime map is independent of serialized AppState and settings.
Runtime reports never invalidate redo, append to undo groups or trigger persistence.
Settings changes, disable and removal invalidate runtime and produce RuntimeChanged
with None. Profile/collection selection currently changes an active ID only, so it
preserves capture generations. Future actual collection replacement must invalidate
replaced sources. Fresh actor restoration always starts with no runtime/grants.

The owner watches latest snapshots to cancel stale/disabled/removed sources. Owner
Drop or closing its request receiver is observed independently of actor queue space;
the actor revokes its capability and publishes recoverable Failed states for pending
or active captures. The media owner owns native graph teardown and voluntary portal
close order, as ADR-0018 records. These core tests do not open dialogs or prove pixels.

Protocol v1 adds an advertised authorize_source_capture request, two Source Events
and an optional source_runtime snapshot field. Older snapshots lacking the field
still decode. Wire types remain distinct from domain types; no parent identifier,
FD, node or grant enters persisted source settings or the wire observation.

Validation on 2026-10-01:

```sh
cargo test -p prismcast-core -p prismcast-app -p prismcast-protocol -p prismcast-remote
just ci
just deny
```

Eight application integration tests cover explicit effect delivery/publication,
permissions/parent admission, native caps, stale retries/terminal updates, full
queues, singleton owner disconnect/replacement, generation invalidation and full-
queue cancellation, runtime-map limits, transactional exclusion, undo isolation,
profile/placement preservation and restore isolation. Remote mapping tests cover
request conversion, entity events, transient snapshot round trips and old snapshot
compatibility; command coverage pins all 50 core variants and 63 advertised requests.
Actual portal and production GTK/media evidence is recorded separately.
