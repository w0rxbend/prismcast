# Conditional scene placement edits

This page records CORE-007 validation of the atomic conditional placement
edit contract in [ADR-0026](../adr/0026-conditional-scene-placement-edits.md).
It closes the UI-002 snapshot-read to dispatch race: GTK gesture and numeric
placement edits now submit `Command::SetSceneItemTransformIf` carrying the
typed `PlacementExpectation` basis they were computed from, and the native
protocol advertises the additive `set_scene_item_transform_if` request.

The application actor owns both the domain state and the capture runtime and
serializes every mutation, so the check-and-apply is atomic. The actor
compares `expect.source_dimensions` against its capture runtime before
inverse preparation; the domain `apply` enforces the state preimage (current
scene, active profile, video config, item transform/crop/bounds/locked) in
the same dispatch turn before any mutation. A mismatch is `Error::Conflict`
and changes nothing: no state, events, revision bump, snapshot publication,
history, meter clear, capture invalidation or persistence notification. A
missing scene or item keeps ordinary `not_found` semantics. History records
the unconditional `SetSceneItemTransform` inverse, so Undo/Redo replay never
re-checks a stale expectation. Conditional commands are top-level only and
are rejected as transaction members on both the domain and wire paths.

Deterministic race tests (the actor serializes dispatches, so sequenced
dispatches from one task are a deterministic race):

- `prismcast-app`: `conditional_edit_rejects_stale_transform_without_side_effects`,
  `conditional_edit_rejects_stale_basis_changes` (crop, lock, removal,
  current-scene, profile video), `unrelated_commits_do_not_invalidate_conditional_edit`,
  `conditional_edit_source_dimensions_guard` (real capture-runtime renegotiation),
  `undo_redo_after_conditional_edit_replays_without_preconditions` (runtime
  dims change between undo and redo; redo succeeds because the recorded
  forward op is unconditional), `conditional_edit_requires_control_scenes_permission`,
  `conditional_edit_noop_records_no_history`.
- `prismcast-core`: preimage match/mismatch matrix, transaction containment
  (top-level and nested), unconditional inverse, no-op, serde roundtrips.
- `prismcast-ui`: `concurrent_transform_conflicts_and_newer_value_survives`
  and `unrelated_commits_do_not_stale_the_captured_basis` drive a real
  CoreActor: a gesture draft begun before a concurrent commit finishes into a
  conditional command that the core rejects, and the newer value survives.
- `prismcast-remote` (`tests/conditional_placement.rs`): over real Unix
  sockets AND loopback WebSockets — success built entirely from the wire
  snapshot (proving controllers obtain the context without runtime grants),
  stale conflict returns 500 `state_conflict` with `field: expect.transform`
  and the newer value survives, read-only sessions get 800, transaction
  membership gets `invalid_request`.

Existing unconditional placement commands remain for deliberate unconditional
control; the obs-websocket adapter is unchanged.

Validation on 2026-10-03:

```sh
just ci
just deny
GDK_BACKEND=wayland GSK_RENDERER=cairo G_DEBUG=fatal-criticals cargo test -p prismcast-ui production_preview_gesture_signals_commit_once_and_cancel_stale_edits -- --ignored --nocapture --test-threads=1
```

The ignored real-display test exercises production gesture/button signals
against a real core and asserts the committed commands are the conditional
variant. It does not inject physical compositor input. See
[preview editor testing](preview-editor.md) for the full editor contract and
[native protocol](../protocols/native-protocol.md) for the wire mapping.
