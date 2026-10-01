# ADR-0011: GStreamer runtime floor, binding version, and backend placement

## Context

ADR-0004 chose GStreamer + gstreamer-rs as the first media engine behind backend
traits, but left three questions open that every media task (MEDIA-001 onward)
depends on (STATE.yaml open questions, RES-003 §"Conclusions" #1):

1. **Runtime floor.** PLAN.md §4 declares no GStreamer version floor. The
   encoder research (RES-005) shows the practical floor is feature-driven:
   `nvav1enc` (NVENC AV1) and the `va` plugin's VP9/VP8/JPEG encoders
   (`vavp9enc`, `vavp8enc`, `vajpegenc`) all landed in **1.26**, and
   gstreamer-vaapi was deprecated in 1.26 in favour of `va`. The preferred
   deployment target is **1.28**: it removes gstreamer-vaapi, adds the unified
   Rust `isobmff` plugin, Enhanced-RTMP `eflvmux`, `nvav1enc` frame-stats
   signals, the shared compositing thread-pool context, and the
   `gtk4paintablesink` udmabuf path (RES-003). Meanwhile key elements are
   machine-dependent (all `nv*` need CUDA; gst-plugins-rs elements like
   `isofmp4mux`/`whipclientsink` are not distro-packaged), so presence can never
   be assumed at compile time.
2. **Binding version and Rust MSRV.** gstreamer-rs stable is 0.25.x (0.25.4,
   2026-09-21) with MSRV Rust 1.92; it supports any system GStreamer ≥ 1.14 and
   gates newer APIs behind cargo features (`v1_24`, `v1_26`, `v1_28`). The
   workspace already uses GTK4 0.11 / Relm4 0.11, which require GLib 0.22 and
   Rust 1.92/1.93 respectively; gstreamer-rs 0.25 uses the same GLib 0.22
   family, so the later `gtk4paintablesink` bridge can share GObject types with
   the UI. An older gstreamer-rs would drag in an incompatible GLib.
3. **Backend placement.** ADR-0004 mandates trait isolation and forbids
   GStreamer in `prismcast-core`, but the concrete crate that holds the
   `Gst*` implementations had to be fixed.

## Decision

1. **Runtime floor: GStreamer 1.26 minimum, 1.28 preferred target.**
   `prismcast-media-gst` refuses to initialize on a runtime older than 1.26
   (typed `GstInitError::UnsupportedVersion`). Features only present in 1.28
   (udmabuf gtk4 path, `eflvmux`, shared task-pool context, `isobmff`) are
   **detected at runtime** from the registry inventory, never assumed; graphs
   explicitly require the factories they need and get typed failures.
2. **Bindings: gstreamer-rs 0.25 with the `v1_26` feature** (matching the
   declared floor). **Declared Rust MSRV: 1.93**, correcting the obsolete 1.85
   workspace declaration — it is dependency-derived (Relm4 0.11 requires 1.93;
   GTK4 0.11, GLib 0.22, and gstreamer 0.25 require 1.92).
3. **Placement: all GStreamer code lives in the new crate
   `crates/prismcast-media-gst`**, which implements the `prismcast-media`
   backend traits (`GstSourceBackend`, ...). `prismcast-core` and
   `prismcast-media` stay free of GStreamer, GTK, Tokio, and Axum dependencies;
   only the media control actor (a dedicated owner thread, never GTK or Tokio
   worker threads) drives the backend.
4. **Initialization and inventory are explicit.** `GstRuntime::initialize()` is
   fallible and idempotent (GStreamer init is repeatable; the process never
   calls the global deinit). The `GstCapabilities` snapshot reports the runtime
   version, registered plugins, the full element-factory list, and a structured
   `ElementInventory` (compositors, preview sinks, capture, VA/NV/Vulkan/
   software encoders, muxers, streaming sinks, browser) probed from the
   registry at startup. Inventory entries are observations: presence does not
   promise device access, codec licensing, or hardware acceleration. Missing
   optional plugins never prevent initialization.

## Alternatives

- **Floor at 1.28 now.** Rejected: no required 1.28-only API is used yet, and a
  1.28 floor would exclude 1.26/1.27 systems for no functional gain. Runtime
  detection gives the same safety with wider coverage. Revisit when a 1.28-only
  element becomes mandatory (e.g. `isobmff` recording default).
- **Floor at 1.24.** Rejected: loses NVENC-AV1 and the VA VP9/VP8/JPEG encoder
  family (RES-005), which are part of the PLAN §12 encoder story, and keeps
  supporting the deprecated gstreamer-vaapi era for no consumer.
- **gstreamer-rs 0.24 (MSRV 1.83).** Rejected: pins an older GLib family
  incompatible with the GTK4 0.11 / GLib 0.22 UI stack; the paintable bridge
  (MEDIA-004) must share GObject types with the in-process GTK app.
- **GStreamer code inside `prismcast-media` or `prismcast-app`.** Rejected:
  re-introduces the leak ADR-0004 forbids (media framework types crossing the
  domain seam) and blocks substituting engines later.
- **Assuming element presence from documentation.** Rejected: `nv*` elements do
  not register without CUDA, `vaav1enc` is driver-conditional, and Ubuntu does
  not package gst-plugins-rs (RES-003 §8). Only registry probing is honest.

## Consequences

- CI and dev machines need GStreamer core/plugins-base **dev packages ≥ 1.26**
  (1.28.2 verified locally). Tests probe and skip/require explicitly rather than
  assuming optional elements.
- Every consumer of a media graph must tolerate an element being absent:
  capability UI is driven by `GstCapabilities`, and graph construction fails
  with typed `GstError::MissingFactory` instead of panicking.
- Each graph owner transitions its pipeline to NULL on EOS, failure, and drop;
  no global GStreamer deinit is ever called (other graphs may still exist).
- Bumping the floor to 1.28 later is an ADR addendum plus deleting runtime
  detection branches — no architectural change.
- gst-plugins-rs elements we need but distros lack (`isobmff`, `rswebrtc`,
  `gtk4`, `gopbuffer`) require a vendoring/build decision (open question in
  STATE.yaml, candidate follow-up ADR).

## Evidence

- RES-003 `docs/research/gstreamer-capabilities.md`: version landscape
  (gstreamer-rs 0.25.4, MSRV 1.92, feature flags `v1_26`/`v1_28`; gst-plugins-rs
  0.15 pins gtk4 0.11 / GLib 0.22), compositor/encoder/muxer/sink element
  facts, §8 local verification snapshot (1.28.2; `nv*`/`vaav1enc`/gst-plugins-rs
  elements absent — probe, don't assume), conclusion #1 (floor proposal).
- RES-005 `docs/research/encoder-matrix.md`: `nvav1enc` and VA VP9/VP8/JPEG
  encoders added in 1.26; gstreamer-vaapi deprecated 1.26 / removed 1.28.
- `docs/research/media-001-initialization.md`: `gst::init()` is fallible and
  repeatable; manifests confirm gstreamer 0.25.4 / GLib 0.22 MSRV 1.92, Relm4
  0.11 MSRV 1.93; local toolchain rustc 1.98.1, GStreamer dev 1.28.2.
- ADR-0004 (engine behind backend traits), PLAN.md §4–§5 (engine and preview
  strategy), §12 (hardware encoders), §30 (domain stays GStreamer-free),
  §57 (media threading), §61 (failure model).

## Status

Accepted (2026-10-01)
