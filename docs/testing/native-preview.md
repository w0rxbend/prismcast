# Native GTK preview verification

MEDIA-004 connects committed core snapshots to a dedicated media owner through
prismcast-preview. The terminal paintable remains GTK-local; graph construction,
reconciliation and NULL teardown run on the owner OS thread, outside Tokio
execution. Only waiting for snapshots/cancellation/bus-health ticks uses its
thread-local runtime. The 100ms health tick checks backend events; application
state changes use the latest-only snapshot watch without polling. Backend events
are drained before graph reconciliation retires the previous graph.

A reaper joins the media owner before completing shutdown. Window close awaits
that completion, then shuts down the core, then closes GTK. Repeated close
requests stay blocked during shutdown. Preview health uses a latest-only watch
and at most one pending Relm4 wakeup. Backend errors replace video with visible
error text; repairing committed state retries the graph.

## Reproduction

Run headless gates separately:

```sh
just ci
just deny
```

Run each display test in its own process (GTK main-thread initialization belongs
to one test thread). Both tests are ignored by ordinary workspace tests:

```sh
GSK_RENDERER=cairo cargo test -p prismcast-preview native_preview_commands -- --ignored --test-threads=1
GSK_RENDERER=cairo G_DEBUG=fatal-criticals cargo test -p prismcast-ui native_shell_preview -- --ignored --test-threads=1
```

## Observed results

Both commands passed on 2026-10-01 with DISPLAY=:0, WAYLAND_DISPLAY=wayland-0,
native GTK4 4.22.4, libadwaita 1.9.1 and GStreamer 1.28.2. The in-process static
sink reports gst-plugin-gtk4 0.15.2 and no plugin filename, confirming the cached
distro plugin was replaced. Rust bindings unify GTK 0.11.5, GStreamer 0.25.4 and
GLib 0.22.10. The dmabuf feature establishes native GTK >=4.14 as the deployment
floor even if another UI dependency requests a lower GTK feature level.

The session test renders the actual GdkPaintable into a GSK texture and downloads
pixels in memory. Committed commands produce red video, black after hiding the
placement, blue after settings changes, and black after selecting an empty scene.
It requires nonzero incoming video dimensions before accepting a pixel result,
so black during a NULL-barrier reset cannot falsely satisfy the test. It checks
paintable invalidations and profile-selected 1280x720 dimensions. Placing an
unsupported Color source reports Failed; removing it recovers Running and black
video. Shutdown finishes within a five-second test deadline after owner join.

The shell test launches the actual RelmApp/AppModel/CoreBridge, issues timed core
commands, finds the native Picture, verifies received frame dimensions and
invalidations, then closes the real root window twice. The production close
callback completes media and core shutdown and the test joins the core thread.
GTK criticals are fatal in this run; none occurred.

These checks use Cairo GSK rendering and the CPU RGBA compositor. They establish
real-display attachment, command-driven pixels and orderly shutdown. They do not
establish accelerated composition, GPU zero-copy, hardware capture or recording.
The prototype renders TestPattern sources; unsupported source kinds produce a
visible error rather than silently displaying the previous scene.
