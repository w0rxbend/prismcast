# Current state

CORE-005 undo hardening and CAPTURE-001 portal/PipeWire foundations are complete.
Separate task worktrees and independent reviews were integrated on
agent/CAPTURE-INTEGRATION. No remote push was requested or performed.

## Completed

- CORE-005 (f8b4137, 2477c4b): typed controller identities, isolated remote
  controllers, bounded undo payloads/groups/labels/nesting, chronological foreign
  mutation boundaries, actor-side inverse authorization and failure-preserving
  history. Own/foreign no-op commands preserve group and redo history, including
  when an open group has reached its admission limit. ADR-0015 records the scope.
- CAPTURE-001 (f724c04): new GTK-independent prismcast-capture crate owns portal
  session authorization, ephemeral PipeWire FD/node grants, bounded lease/probe
  capacity, cancellation/revocation and orderly worker/session cleanup. Native
  RGBA probe validates caps, buffers and timestamps; runtime/property checks return
  typed errors. ADR-0016 and docs/research/capture-lease-native.md explain ownership.
- Fourteen capture tests cover mocked lifecycle and real headless GStreamer buffer
  evidence. Real portal capture remains unverified: the user was offered a window
  or monitor test, but no target selection has arrived and no dialog was opened.
  The opt-in test reads three frames and closes the session even on probe failure.

## Validation

Combined just ci passed: 406 tests passed, six opt-in tests ignored. just deny
passed. Five separate real-display regressions cover preview, shell, gesture,
scene dialogs and rapid source toggles; see the journal for final results.
Portal test instructions: docs/testing/portal-capture.md. Undo evidence:
docs/testing/undo-history.md. Headless probe evidence does not establish actual
monitor/window capture or UI integration.

## Next task and limits

CAPTURE-002 is ready with a concrete spec in .agent/tasks/CAPTURE-002.yaml:
Core Commands authorize/retry, Core Events/snapshots expose runtime status,
GTK exports the local parent window, and the media graph shares one capture lease
per source across rebuilds. Negotiate source dimensions, support revocation and
cleanup, and require explicit authorization rather than opening pickers from
restored state. Production compositor/UI currently still supports TestPattern;
CAPTURE-001 alone does not add a screen/window source to the application.

Undo/redo remain existing application metadata APIs; canonical Commands and
UI/protocol controls, destructive Add/Remove restoration and persisted history
are follow-ups. Z-order i32 boundary overflow and missing neighbor update events
need a coordinated core change. Existing preview non-atomic edit preflight,
cardinal rotation/Normal blend limitations and CPU rebuild frame interruption
remain. Capture lease caps are bounded; v6 portal serial targeting is deferred.

Prior overlapping edits remain preserved on archive/paused-agent-phase2 (2ce3390)
and its named stash. Do not reapply the alternate backend API wholesale.
Worktrees remain available for review. No secrets or portal grants are persisted.
