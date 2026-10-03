# Current state

CORE-007 is implemented and validated on main (8fc0d74): atomic conditional
scene placement edits per ADR-0026, closing the UI-002 snapshot-read to
dispatch race. Continue directly on main under the user's standing
preference. All 41 auxiliary worktree histories were already reconciled;
preserve their existing local edits. Assign disjoint files to parallel
agents; only the coordinator commits and pushes.

## Conditional placement edit contract (ADR-0026)

- `Command::SetSceneItemTransformIf { scene_id, item_id, transform, expect }`
  carries a flat `PlacementExpectation`: item transform/crop/bounds/locked,
  current_scene, active_profile + video config, and
  `source_dimensions: Option<SourceDimensions>`. Controllers obtain every
  value from the wire/local snapshot; no grants are exposed.
- The actor compares `source_dimensions` against its capture runtime before
  inverse preparation; domain `apply` enforces the state preimage
  (`check_placement_expectation`) in the same serialized turn before any
  mutation. Mismatch is `Error::Conflict("stale placement edit:
  expect.<field> mismatch")` and changes nothing: no state, events,
  revision, snapshot, history, meters, capture invalidation or persistence.
  Missing scene/item stays `NotFound`.
- Conditional commands are top-level only: rejected as transaction members
  by the domain and by wire mapping. History records the unconditional
  `SetSceneItemTransform` inverse, so Undo/Redo never replay a stale
  expectation (runtime dims can change without clearing redo — covered by
  test).
- Unrelated commits do not invalidate a pending conditional edit — the
  deliberate advantage of entity preimage over global revision.
- Native protocol v1 additive: `set_scene_item_transform_if` is the 66th
  advertised request; Conflict -> state_conflict 500 with `field`; obs-ws
  unchanged (Conflict -> 604). GTK gesture finish() and numeric/action
  controls submit the conditional command; rejections use the existing
  command-error toast. Unconditional commands remain for deliberate control.

## Verification

See docs/testing/conditional-placement-edits.md. Final just ci passed 716
tests, zero failed, 20 environment-dependent ignored; fmt, workspace
all-target Clippy and just deny clean. No dependencies changed. Deterministic
owner race tests (app), preimage matrix (core), real Unix + loopback
WebSocket conflict tests (remote, tests/conditional_placement.rs) and two
headless actor-backed GTK race tests all pass. The ignored real-display GTK
placement regression passed separately:

```sh
GDK_BACKEND=wayland GSK_RENDERER=cairo G_DEBUG=fatal-criticals cargo test -p prismcast-ui production_preview_gesture_signals_commit_once_and_cancel_stale_edits -- --ignored --nocapture --test-threads=1
```

## Next work candidates (no task file is marked ready)

Pick from the open questions in STATE.yaml and PLAN.md; create/scoped task
files as needed. Notable follow-ups: conditional crop/bounds variants can
reuse PlacementExpectation; destructive Add/Remove undo and persisted
history; wire history availability query and native group APIs; z-order
boundary overflow and missing neighbor events; scene-nesting cycle check;
the CAPTURE-002 live window preview coordinated-selection retry (below).

## Canonical history contract (ADR-0025, unchanged)

Command::Undo/Redo remain parameterless canonical commands in ordinary
permission-aware dispatch; pure AppState owns no history and rejects them,
including nested in Transactions. History is one bounded global timeline
with controller group ownership; an open group blocks Undo/Redo for
everyone. Successful replay emits ordinary events, publishes the snapshot,
clears stale meters and notifies persistence. AppSnapshot.history() is
advisory presentation metadata. Capture authorization never enters history;
replayed settings/enabled changes invalidate transient capture without
restoring grants. Native undo/redo return ResponseData::Empty; no history
payloads cross the wire. CLI undo/redo exit codes: 0 success, 1 transport,
2 request/usage. GTK header buttons and win.undo/win.redo send Core
Commands; bubble Ctrl+Z/Ctrl+Shift+Z guards Editable/TextView ancestors.

## Capture/audio context and remaining platform evidence

CAPTURE-004 remains complete under ADR-0024; see docs/testing/pipewire-audio.md.
PipeWire settings persist only versioned advisory name/mode. AudioOwner effects
freeze source/generation/settings; AudioSession owns explicit grants. Native
resolver checks exact class/name, cookie/serial and a pinned absolute endpoint;
fresh owned sockets predate inventory verification and stay retained through NULL.
Never fallback, reconnect or rebind a grant to a replacement daemon. Meters require
active generation and the exact reconciled revision. Source settings/enable/remove
invalidate runtime. Discovery and streams retain their established bounds.

Private synthetic input/sink-monitor/app sentinel and reused-serial daemon restart
fixtures passed previously, along with real AudioSession and native meter socket
checks. Physical microphones, desktop policies and Flatpak permissions remain
unverified. Application capture selects one playback stream. Three-second no-data
watchdog can require Retry after buffers stop; one fault revokes all physical grants.
Upstream pipewiresrc synchronous startup may block about 30 seconds if the daemon
dies after final EOF preflight; cancellation waits native return. No replacement
capture occurs. State settlement limits do not bound every plugin call.

Monitoring/playback, balance, sync delay, filters, encoded tracks, OBS pre-fader
peaks/output statistics and desktop remote bootstrap remain follow-ups; buses end
in nonplaying fakesinks. The overall broadcasting application is not complete.

CAPTURE-002 integrated live window preview remains unverified: earlier grants
selected the wrong windows. Run the raw actual_window_capture_consumer_frames_show_fixture_pixels
and integrated actual_window_capture_preview_pixels_placement_and_shutdown display
filters separately with a coordinated selection of the small flashing window
"Prismcast capture test target – select this window". Monitor/KDE/X11 capture,
camera unplug UX and live UI camera preview remain unverified. Camera paths can
renumber; stable identity and wire-exposed discovery remain follow-ups.
