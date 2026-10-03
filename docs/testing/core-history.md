# Canonical history — CORE-006

The header’s Undo and Redo buttons activate `win.undo` and `win.redo`.
Both send the same `Command::Undo` / `Command::Redo` used by local and
native socket controllers. Their tooltips show the current operation label.
History is global for one running application and is bounded and transient;
restarting the application begins with empty history.

Ctrl+Z and Ctrl+Shift+Z activate the window actions when focus is outside a
text editor. The window key controller runs in the bubble phase, after child
widgets, and explicitly checks focused Editable/TextView ancestors. Empty
or readonly entries also keep these shortcuts, avoiding an unexpected scene
undo when local text undo is unavailable. GtkText delegates under Entry,
SpinButton and PasswordEntry implement Editable; GTK supplies their own text
undo/redo bindings. See [GTK EventControllerKey](https://docs.gtk.org/gtk4/class.EventControllerKey.html)
and [GTK Text](https://docs.gtk.org/gtk4/class.Text.html).

The UI renders `AppSnapshot::history()` on every snapshot watch wake,
including same-revision group metadata updates. Empty history, an open edit
group and shutdown disable the corresponding actions. Availability is
advisory: replay races, permission failures and invalid inverses still reach
Core and report through the existing command-error toast. The native
[Gio SimpleAction enabled state](https://docs.gtk.org/gio/class.SimpleAction.html)
drives both buttons and the keyboard handler. No UI history stack exists.

## Deterministic checks

Run the headless history tests and real persistence test:

```sh
cargo test -p prismcast-app
cargo test -p prismcast-app --test history_persistence
cargo test -p prismcast-ui shortcut_requires_control_z
```

Core/app coverage checks empty stacks, grouped atomic replay, ownership,
permission checks for the actual inverse/forward operation, history bounds,
rejected replay preserving state/runtime/history, same-revision metadata,
and canonical wrapper dispatch. Capture settings/enable replay invalidates
transient observations; history never recreates capture authorization.

The persistence regression records a mixed transaction containing a scene
rename and output reconnect policy. After the transaction, Undo and Redo,
it flushes and reads the collection and profile files and checks both
values. A new actor initialized with the identical final working state
starts with empty history. This test does not exercise a complete document
hydration/bootstrap path. History itself is absent from persisted data.

Native IPC/WebSocket regressions use real sockets to check the command and
response envelope, emitted Events and committed snapshots across controllers.
Run the focused filters recorded in the task’s integration journal.

## GTK display checks

Run each filter in its own process with a real Wayland display:

```sh
GDK_BACKEND=wayland GSK_RENDERER=cairo G_DEBUG=fatal-criticals \
  cargo test -p prismcast-ui native_window_history_actions_share_controller_history_and_refresh_groups \
  -- --ignored --test-threads=1 --nocapture

GDK_BACKEND=wayland GSK_RENDERER=cairo G_DEBUG=fatal-criticals \
  cargo test -p prismcast-ui text_editors_keep_history_shortcuts_even_readonly_or_empty \
  -- --ignored --test-threads=1 --nocapture
```

The production RelmApp window test creates a scene item, then a distinct
local controller renames its source and changes its transform. Header
buttons and the production key-controller signals replay those changes
through Core. It checks initial empty actions, group begin/end publication
without a revision change, an intentionally stale action rejected by Core
with a visible error toast, unchanged rejected state and disabled actions
during the production joined shutdown.

The focused display test checks Entry’s actual GtkText delegate, SpinButton,
PasswordEntry and TextView focus, including readonly/empty editors. It
emits production key-controller signals to verify text focus passes through,
nontext focus emits canonical Undo/Redo and shutdown blocks shortcuts.
These tests exercise native GTK controllers and actions; they do not inject
physical keyboard events into the compositor or require capture hardware.

Destructive-operation undo and persistent history remain separate work.

## Focused validation (2026-10-03)

Both display filters above passed separately on Wayland with Cairo and
fatal GTK criticals. The UI headless suite passed 29 tests, with 11 display
tests ignored. Package Clippy passed with warnings denied. The coordinator
also passed the real profile/collection persistence regression and owns
the final workspace CI and native socket results.

Final coordinator gates passed on 2026-10-03: `just ci` completed formatting,
workspace all-target Clippy with warnings denied, and 65 test suites totaling
692 passed, zero failed and 20 environment-dependent ignored tests. `just deny`
passed separately (existing duplicate/unmatched audit warnings remain). The two
new ignored GTK regressions were run and passed separately as described above.
Real native transport history tests and CLI subprocess tests are part of CI.
