# Current state

The Phase 2 CPU media prototype and interactive preview milestone are complete.
This wave used separate MEDIA-005/UI-002 implementation worktrees and independent
review, integrated on agent/TRANSFORM-INTEGRATION before advancing main.

## Completed in this wave

- MEDIA-005: shared pure prismcast-compositor geometry (5fb0831), then native
  crop, all anchors, bounds fitting, cardinal rotation, source-axis signed flips
  and canvas-axis sizing (f1f525c). Geometry handles huge crop edges, zero/subpixel
  scale and finite/resource validation. Eighteen native backend tests plus six
  pure geometry tests cover actual asymmetric pixel orientation and cleanup.
- UI-002: selection outlines and a placement picker, letterbox-aware hit-testing,
  local move/resize drafts, one Command per gesture, numeric Apply, Rotate90 and
  Flip X/Y (b59f585). Locked/unavailable placements cannot be edited. Fresh core
  snapshots, scene/profile/item/source changes, Escape/cancel and allocation
  changes invalidate stale drafts. Dedicated preview acknowledgements gate
  pending edits independently of other command responses.
- Actual GTK main-loop regression caught a picker feedback loop; stable choice
  models, an explicit placeholder and unchanged-ID notification guards resolve it.
  The native test waits after gesture begin to prove it survives queued GTK work.
- ADR-0014 records shared geometry, local drafts and the remaining non-atomic
  snapshot-read/dispatch boundary. No new core Command, schema or protocol.

## Validation

Final combined just ci passed: 382 tests, with five display tests ignored by the
headless suite. just deny passed. Separate real-display tests cover native
paintable pixels/failure recovery/shutdown, actual RelmApp preview and repeated
window close, production preview gesture/action signals, scene dialogs and rapid
source toggles. Commands and evidence are in docs/testing/preview-editor.md,
cpu-transforms.md, native-preview.md and the scene/source signal notes.

## Next work and limits

Author a scoped CORE-005 task for undo/redo refinements; core already has inverses
and grouped-undo scaffolding, but open groups can grow without bounds and group
commands from different controllers are not isolated. UI-002 deliberately sends
one final gesture command and does not use global BeginUndoGroup. Z-order actions
need i32-boundary hardening before exposing more editor controls.

Prepare the Phase 3 Linux capture wave from PLAN §45 and existing capture research:
portal/PipeWire monitor/window capture, then V4L2 and audio/device discovery. The
prototype currently renders TestPattern sources; other kinds show a backend error.
Future preview support needs negotiated source dimensions beyond TestPattern.

Free-angle rotation quantizes to cardinal steps with bounded diagnostics. Only
Normal blending is supported. Bounds-driven placements can move but pointer
resize/numeric scale edits are disabled. CPU graph changes retain the NULL barrier
and can briefly interrupt frames. GPU/capture/audio/output and physical pointer
recognition are not established by these tests. A future expected-version core
command is needed for atomic remote-edit protection after client preflight.

Run: cargo run -p prismcast-ui --bin prismcast. Prior overlapping agent edits remain
preserved on archive/paused-agent-phase2 (2ce3390) and the named stash; do not
reapply its alternate backend API wholesale. Worktrees remain for review.
