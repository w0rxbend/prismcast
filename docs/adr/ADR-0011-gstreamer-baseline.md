# ADR-0011: Native backend placement and compatibility baseline

Status: Accepted (MEDIA-001, 2026-10-01)

## Decision

Implement the Linux GStreamer platform in `prismcast-media-gst`, behind the
existing media traits. Domain and application crates retain no native dependency.
Use gstreamer-rs 0.25 with `v1_26`: GStreamer 1.26 is the minimum development and
runtime baseline, while 1.28 is the preferred deployment target. Newer optional
elements are discovered rather than assumed present. The current machine has
GStreamer core/video/audio 1.28.2 development libraries.

Declare workspace Rust 1.93, correcting the obsolete 1.85 declaration: existing
Relm4 0.11 requires 1.93, GTK4 0.11 and GLib 0.22 require 1.92. GStreamer 0.25
uses the same GLib 0.22 family, so the later paintable bridge can share GObject
types. Keeping the older GStreamer bindings would introduce incompatible GLib
types. Raising the native baseline to 1.28 now would unnecessarily exclude 1.26
systems before any required 1.28 API is used.

Initialization and inventory are synchronous platform operations for a future
media owner thread, never GTK or Tokio worker threads. Registry inventory reports
registered plugin metadata and element factories; presence alone does not promise
device access, codec licensing, instantiation, or hardware acceleration. Missing
optional plugins do not prevent initialization. Consumers explicitly require the
factories their graph needs and receive typed failures.

## Consequences

CI needs native GStreamer core and plugins-base development packages >= 1.26.
Production does not call global GStreamer deinit (other graphs may still exist).
Each graph owner must transition to NULL on EOS, failure and drop.

## References

- [gstreamer-rs 0.25](https://docs.rs/gstreamer/0.25.2/gstreamer/)
- [Initialization](https://gstreamer.freedesktop.org/documentation/application-development/basics/init.html)
- Manifest inspection: relm4 0.11.0 / gtk4 0.11.5 / glib 0.22.10.
