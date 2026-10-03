# Current state

CORE-006 is implemented and validated on main (2f327b5): canonical Undo/Redo
Commands and GTK/native/CLI controllers. The next task is CORE-007.
Continue directly on main under the user's standing preference. All 41 auxiliary
worktree histories were already reconciled; preserve their existing local edits.
Assign disjoint files to parallel agents; only the coordinator commits and pushes.

## Canonical history contract (ADR-0025)

- Command::Undo/Redo and matching native requests are parameterless. The actor
  intercepts them in ordinary permission-aware dispatch; compatibility handle
  helpers delegate to that path. Special Undo/Redo actor messages were removed.
- Pure AppState owns no history and rejects these operations, including any
  occurrence inside a nested atomic Transaction before authoritative mutation.
- The initial gate requires any mutation scope or Admin, then every actual
  inverse/forward operation is recursively authorized. required_permission
  returns conservative Admin for history because one static scope cannot
  describe an actor-owned entry. Use Permissions::check and actor dispatch.
- History is one global chronological timeline, with unchanged entry/byte/node/
  label/nesting budgets and controller group ownership (ADR-0015). An open
  gesture group blocks Undo/Redo for everyone. Failed permissions, structure,
  preflight or application preserve state, history, revision and observations.
- Successful replay returns label undo/redo, emits ordinary Core Events, publishes
  the resulting snapshot, clears stale meters and notifies persistence using
  the actual replay action. Mixed collection/profile changes persist correctly.
- AppSnapshot.history() exposes bounded optional undo/redo labels and group_open;
  can_undo/can_redo are advisory presentation checks. Successful group begin/end
  publishes a new immutable snapshot at the same state/runtime revision, without
  clearing meters or invalidating capture. Watch consumers must not suppress
  metadata refresh just because the revision is unchanged.
- Capture authorization never enters history. Replayed source settings/enabled
  changes invalidate transient capture through normal commit logic. Restoring
  advisory settings or enabling a source cannot restore grants or reopen it;
  explicit authorization is required. Authorization preserves existing redo.
- Native protocol version 1 remains additive: 52 mirrored Core Commands and 65
  advertised request kinds. Undo/Redo return existing ResponseData::Empty with
  request_type echo, or structured errors (permission forbidden 800, empty/open
  group invalid_field 400, forbidden transaction structure invalid_request 100).
  No history labels/stacks/availability/grants enter wire snapshots. A wire
  history query and native group APIs remain follow-ups.
- CLI undo/redo uses existing IPC/WS selection/auth; human output identifies
  the applied operation, JSON prints the empty mutation data. Success exits 0,
  transport error exits 1, request/usage rejection exits 2. No payload argument.
- GTK header buttons and win.undo/win.redo actions send Core Commands. Bubble
  Ctrl+Z/Ctrl+Shift+Z guards Editable/TextView ancestors, including readonly or
  empty editors and GtkText delegates. Tooltips show labels; empty history,
  groups and shutdown disable actions. Rejection refreshes availability and
  shows the existing command-error toast. GTK owns no separate history stack.

## Verification

See docs/testing/core-history.md, docs/testing/undo-history.md and
research/core-006-history-controllers.md. Final just ci passed 692 tests, zero failed and 20 environment-dependent ignored;
formatting and workspace all-target Clippy are clean. just deny passed separately
with existing warnings. No dependencies changed. STATE.yaml and JOURNAL.md record
the evidence.

Five app history tests cover mixed scopes on both replays, wrapper/canonical
behavior, open/foreign groups, capacity/no-ops, repeated failed atomic replay,
snapshot identity, same-revision active capture/meters and both capture families'
consent invalidation. Pure domain test rejects direct/nested history application.
Real persistence regression flushes and reads collection/profile files after a
mixed transaction, Undo and Redo; a new actor with identical final working state
starts with empty history (no complete disk-to-AppState bootstrap is claimed).

Three native integration tests each exercise actual Unix and loopback WebSocket
transports: scoped success with consecutive normal Events and matching snapshots,
read-only/wrong/mixed scopes on Undo and Redo, empty/open groups and forbidden
atomic members. CLI subprocess tests exercise the real binary/socket, human/JSON
results, rejection/usage exits and preserved history. Existing auth/TLS/OBS paths
remain unchanged.

Two GTK tests passed in separate real Wayland/Cairo/fatal-critical processes:

```sh
GDK_BACKEND=wayland GSK_RENDERER=cairo G_DEBUG=fatal-criticals cargo test -p prismcast-ui native_window_history_actions_share_controller_history_and_refresh_groups -- --ignored --nocapture --test-threads=1
GDK_BACKEND=wayland GSK_RENDERER=cairo G_DEBUG=fatal-criticals cargo test -p prismcast-ui text_editors_keep_history_shortcuts_even_readonly_or_empty -- --ignored --nocapture --test-threads=1
```

These exercise production header clicks, key-controller signals with actual Core
state assertions, second-controller edits, same-revision group refresh, visible
rejection toast, corrected availability and joined shutdown. They do not inject
physical compositor keyboard events. No new dependencies were added.

## Exact next task

Read .agent/tasks/CORE-007.yaml: atomic conditional scene placement edits. This
closes the UI-002 snapshot-read to dispatch race. Choose and document bounded
preconditions in an ADR before implementation, comparing revisions with entity
preimages. A delayed GTK or native edit must not overwrite a newer transform,
crop, lock, removal or relevant scene/source/runtime context. Preserve normal
Events, bounded undo and explicit capture consent; replay must not reintroduce
stale admission conditions. Destructive undo, groups and persisted history remain
separate tasks. Z-order overflow/missing neighbor events and scene nesting cycle
checks also remain unresolved domain follow-ups.

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
