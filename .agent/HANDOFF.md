# Current state

CAPTURE-002 command-driven monitor/window capture preview is implemented and
integrated on agent/CAPTURE-002 with separate Core/API, media and UI worktrees
and independent reviews. Main026c823 was pushed at the user's explicit request
before this wave; current wave integration/push is recorded in the journal.

## Implemented

- AuthorizeSourceCapture is an explicit Core Command. A singleton bounded owner
  receives effects; absent/full/disconnected owners fail admission before changes.
  Ephemeral parent context is local-only. Transient source runtime lives outside
  persisted AppState; Core Events and snapshots expose status/actual dimensions.
  Owner/generation checks prevent stale revival, including retry/remove/disable.
  Authorization is not replayed by transactions, undo or snapshot restoration.
- Persistent pipewiresrc producers retain one portal lease per shared SourceId
  through compositor rebuilds. Bounded RGBA appsink/appsrc consumers share native
  buffers and tee placements; native caps define geometry. Timelines are rebased
  across independent producers/consumers. Unavailable captures leave other content
  visible. Native graph retirement precedes voluntary portal session close.
- UI offers monitor/window source creation and explicit authorize/retry controls.
  GTK-local exported parent guard survives asynchronous media shutdown. Pending,
  terminal and disabled sources gate repeated signals. Negotiated dimensions drive
  preview editing; caps/generation/revocation changes cancel stale drafts.
- ADR-0017/0018 and docs/testing/capture-core-runtime.md, capture-ui.md and
  shared-capture-preview.md document the contract and evidence.

## Evidence and live limitation

Combined fmt/clippy/workspace tests and dependency audit pass; final count is in
JOURNAL. Seven separate real Wayland display regressions pass, including source
buttons and actual parent export; those tests open no permission dialogs.

Actual GNOME window probe passed: three6144x3456 RGBA frames, timestamps and clean
session shutdown. The integrated animated-window preview test opened one picker
but received no completed sharing grant; at120seconds it reported Failed with
"capture authorization timed out" and cleaned up. Integrated live preview pixels
are therefore unverified. User was asked whether available for one retry; do not
open another dialog without their reply. Headless native pixels/producer retention
are tested separately and cannot replace this manual evidence.

Run opt-in: GDK_BACKEND=wayland GSK_RENDERER=cairo G_DEBUG=fatal-criticals cargo test
-p prismcast-preview actual_window_capture_preview_pixels_placement_and_shutdown
-- --ignored --nocapture --test-threads=1. Select the animated window titled
"Prismcast capture test target – select this window". The test expects red/blue
paintable pixels, unchanged grant across hide/show rebuild, source removal and
shutdown. Capture tests must run in separate processes from other GTK tests.

## Next and remaining limits

Coordinate live preview revalidation when user available, then scope CAPTURE-003
V4L2 discovery/camera producer integration. Monitor, KDE and X11 actual capture
remain unverified. CPU RGBA producer frames are capped128MiB and axes8192; queues
are bounded but conversion/composition can copy pixels. No zero-copy claim.
ScreenCast v5 node IDs are supported; v6 serial targeting remains future work.

Canonical Undo/Redo Commands/UI/wire, destructive Add/Remove history, persisted
history, z-order boundary overflow/missing neighbor events and atomic expected
version edits remain follow-ups. Existing cardinal rotation/Normal blend and CPU
rebuild interruption remain. Prior overlapping edits are preserved on
archive/paused-agent-phase2 (2ce3390) and its named stash; do not reapply its alternate
backend API wholesale. Worktrees remain reviewable. Never persist grants/FDs.
