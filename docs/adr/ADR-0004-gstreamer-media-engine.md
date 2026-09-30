# ADR-0004: GStreamer as the first media engine, behind backend traits

## Context

Prismcast needs capture, synchronization, decoding, encoding, muxing, composition, and
networking. Building all of that from scratch is infeasible for the vertical-stack-first
scope strategy (PLAN §77). GStreamer has maintained Rust bindings (gstreamer-rs) and
already provides the pipeline/plugin primitives required, including PipeWire capture
(`pipewiresrc`), GTK4 preview (`gtk4paintablesink` with GL/DMABUF support), a `compositor`
element that maps naturally onto scene items (position, dimensions, alpha, z-order), and
hardware VA-based encoders for H.264/H.265/AV1 (PLAN §4, §5, §12).

At the same time, hard-coding GStreamer into the domain would leak a media framework into
every layer and preclude replacing specialized portions later (PLAN §4) — and would
violate the rule that the domain crate must not depend on GStreamer (PLAN §30).

## Decision

1. **GStreamer + gstreamer-rs is the first and reference media engine** (PLAN §4).
2. All media functionality is accessed through backend traits defined in
   `prismcast-media`:

   ```rust
   trait SourceBackend
   trait VideoFilterBackend
   trait AudioFilterBackend
   trait CompositorBackend
   trait EncoderBackend
   trait OutputBackend
   trait StreamingServiceBackend
   ```

   GStreamer provides the first implementations (`GstSourceBackend`,
   `GstCompositorBackend`, `GstEncoderBackend`, …) in a separate crate
   (`prismcast-media-gst` role), so specialized portions can be replaced later.
3. Composition starts with the GStreamer `compositor` element; `glvideomixer`, VA
   compositors, Vulkan composition, or a custom Rust element are only investigated after
   `compositor` is proven insufficient (PLAN §5). No custom renderer before that proof.
4. Preview uses `gtk4paintablesink → GdkPaintable → GTK Picture` to preserve DMABUF
   zero-copy where possible (PLAN §5); zero-copy is a project-level performance
   requirement (PLAN §12), validated by benchmarking CPU copies/frame, GPU→CPU
   transitions, DMA-BUF preservation, encoder/composition latency, and memory (PLAN §12).
5. GStreamer runs on its own streaming threads, coordinated by a media control actor;
   domain code never sees GStreamer types (PLAN §57).

## Alternatives

- **Custom media engine in Rust.** Rejected: capture/sync/decode/encode/mux/network from
  scratch contradicts the scope strategy and performance targets; years of work before
  feature parity.
- **FFmpeg (via bindings) as the engine.** Rejected as the core: FFmpeg excels at
  codec/muxing but lacks GStreamer's live pipeline model, PipeWire source integration,
  and GTK paintable sinks, which are central to capture and preview. FFmpeg may still be
  used under a backend trait for specific encode/decode needs.
- **Direct trait-less GStreamer use throughout the codebase.** Rejected: leaks a media
  framework into domain/UI, blocks future substitution, and violates the crate dependency
  rules (PLAN §30).

## Consequences

- Media work requires GStreamer expertise and GStreamer dev packages in CI (PLAN §62).
- The backend traits are a stable internal seam; a second engine (or custom Rust
  elements) can be introduced without touching domain, UI, or protocol code.
- Performance work (zero-copy, DMABUF) is constrained by GStreamer element capabilities;
  gaps must be closed with custom elements, not architectural workarounds.
- Research tasks (RES-003 GStreamer capability research, RES-005 encoder matrix, PLAN
  §66) gate media implementation choices.

## Evidence

- PLAN.md §4 (Media engine: GStreamer + gstreamer-rs as first implementation; backend
  trait list; `Gst*` implementations preserve replaceability).
- PLAN.md §5 (Preview via `gtk4paintablesink`; composition starting with `compositor`;
  "Do not write a custom renderer before proving that GStreamer composition is
  insufficient").
- PLAN.md §12 (hardware encoders via VA; zero-copy as project-level requirement).
- PLAN.md §30 (`studio-domain`/`prismcast-core` must not depend on GStreamer).

## Status

Accepted (2026-09-30)
