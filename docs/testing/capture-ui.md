# Capture UI and parent-window ownership

CAPTURE-002 adds Monitor Capture and Window Capture to the source dialog. Creation
and placement send the existing AddSource/AddSceneItem commands; creation and
restoration never authorize capture. Shared-source rows show transient runtime
status, including pending selection, denial, cancellation, revocation and errors.
Only an explicit Authorize Capture / Retry Authorization action sends
AuthorizeSourceCapture. Repeat clicks are gated before a committed snapshot.

GTK exports the realized Wayland parent window once. An owned GTK-local guard
retains the exported handle throughout the preview session; only an opaque
`wayland:<handle>` string crosses the application authorization boundary. The
identifier is neither a source setting nor persisted state. Preview shutdown
acknowledgement precedes per-handle unexport and window destruction. Unsupported
parent export returns a presentation error and allows the optional unparented
portal request; it never performs another export or authorization loop.

Version-matched API inspection used gdk4-wayland 0.11.5 export_handle and
v4_12 drop_exported_handle. The latter releases this particular export instead of
unexporting unrelated exports. The [GTK export contract](https://docs.gtk.org/gdk4-wayland/method.WaylandToplevel.export_handle.html)
and [per-handle cleanup API](https://docs.gtk.org/gdk4-wayland/method.WaylandToplevel.drop_exported_handle.html)
require GTK main-thread ownership. The ashpd GTK adapter was inspected but is not
used; portal requests remain GTK-independent in the capture service.

Capture editor geometry uses Active runtime dimensions negotiated from native
caps. Missing or non-active runtime dimensions make a placement uneditable;
portal coordinate sizes and invented source-size defaults are never used. Runtime
status/generation/dimension changes invalidate local gestures, and numeric action
preflight also checks the newest layout before dispatch.

Real GTK tests use explicit production signals and mocks/command collection, with
no portal dialogs. Actual monitor/window pixels require separately authorized
opt-in portal tests and are not implied by these UI regressions.

Validation (2026-10-01): focused UI tests passed (22 passed, six display tests
ignored in the headless run). The following separate real Wayland tests passed
with GTK criticals fatal; none opened a permission picker:

```sh
GDK_BACKEND=wayland G_DEBUG=fatal-criticals cargo test -p prismcast-ui actual_wayland_export_is_owned_and_released_before_window_close -- --ignored --test-threads=1
GDK_BACKEND=wayland G_DEBUG=fatal-criticals cargo test -p prismcast-ui explicit_capture_button_gates_repeat_signals_and_disabled_sources -- --ignored --test-threads=1
GDK_BACKEND=wayland G_DEBUG=fatal-criticals cargo test -p prismcast-ui production_preview_gesture_signals_commit_once_and_cancel_stale_edits -- --ignored --test-threads=1
GDK_BACKEND=wayland G_DEBUG=fatal-criticals cargo test -p prismcast-ui native_shell_preview_and_window_close_stop_both_owners -- --ignored --test-threads=1
```

The native button test covers all six runtime statuses, disabled sources, repeated
signals and absence of requests from rendering. A mock capture owner test proves
source creation produces no authorization request, explicit authorization carries
only ephemeral parent context, actual reported pixel sizes define editor geometry,
and caps renegotiation/revocation cancel drafts. The shell test proves ordinary
preview and repeated close remain functional with the new parent export guard.

`just ci` passed in the isolated UI worktree, including fmt/clippy/workspace tests
and doctests; `just deny` passed. Concurrent worktrees must use separate target
directories: a shared-target run reached passing runtime tests but another build
removed a dependency artifact before rustdoc; the isolated rerun passed fully.
