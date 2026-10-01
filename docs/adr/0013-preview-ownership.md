# ADR-0013: GTK preview attachment and media ownership

Status: Accepted for MEDIA-004 prototype

## Context

The preview must display the current committed scene while all interfaces keep
using core Commands. GTK objects are main-thread objects; backend traits are
Send. Relm4 messages must not become frame queues, and blocking graph operations
must run off GTK and Tokio execution threads.

## Decision

Introduce prismcast-preview as an integration adapter. It creates and retrieves
the statically registered gtk4paintablesink paintable on the GTK thread, then
passes only its Send gst::Element sink to a dedicated media owner thread. The UI
attaches the GTK-local paintable to gtk::Picture and keeps no GStreamer imports.
The adapter consumes AppHandle::subscribe_snapshots, retaining only latest
committed snapshots and rebuilding/diffing through the media backend contract.
No polling is used to synchronize application state. Backend bus polling, if
required for observed pipeline failures, is separate from state synchronization.

The owner performs graph mutations outside Tokio execution. It waits for watch
changes and cancellation on a thread-local runtime, then leaves block_on before
calling backend code. Stop is an explicit cancellation signal with completion
acknowledgement after pipeline NULL and owned-resource cleanup. The GTK loop
stays alive during asynchronous shutdown so sink main-context callbacks can
complete. Window close stops media before stopping core and closing GTK.

Use matching gst-plugin-gtk4 0.15 (Rust library gstgtk4), gtk4 0.11,
gstreamer 0.25 and GLib 0.22; register statically before element construction.
Select static, waylandegl and dmabuf features. The dmabuf feature requires
native GTK >=4.14 (tested here on 4.22.4); UI-only lower feature declarations
do not lower that application deployment floor. Support system-memory frames for
this CPU compositor prototype; these features enable later memory paths but do
not establish zero-copy behavior. Add MPL-2.0 to cargo-deny because the sink is
MPL-2.0; project-owned code remains MIT OR Apache-2.0.

Failures have typed adapter errors and latest-only preview status. Frames use
sink-owned bounded notification/pending-frame machinery, never core events or
Relm4 channels. Empty scene output remains valid black video; new demo sources
are always created/placed with AddSource and AddSceneItem Commands.

## Consequences and validation

The new adapter is the only GTK/GStreamer integration owner; domain and generic
media traits retain no GTK dependencies. A native display integration test runs
explicitly, with bounded deadlines, core-driven mutations, paintable/frame
observations and ordered shutdown. Headless gates are recorded separately.

## Verified upstream references

- https://raw.githubusercontent.com/GStreamer/gst-plugins-rs/0.15/video/gtk4/Cargo.toml
- https://raw.githubusercontent.com/GStreamer/gst-plugins-rs/0.15/video/gtk4/src/sink/imp.rs
- https://raw.githubusercontent.com/GStreamer/gst-plugins-rs/0.15/video/gtk4/examples/gtksink.rs
