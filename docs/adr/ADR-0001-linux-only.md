# ADR-0001: Linux-only platform scope

## Context

Prismcast targets an OBS-class broadcasting and recording feature set. A cross-platform
scope (Windows, macOS, Linux) forces an OS-abstraction layer for capture, encoders,
windowing integration, and device handling, and historically constrains design around the
lowest common denominator. OBS itself carries significant complexity from cross-platform
support. Prismcast explicitly does not want to inherit the "OS abstraction required for
Windows/macOS" or "historic compatibility decisions" from OBS (PLAN §79).

Linux today offers a coherent, modern media stack — PipeWire for audio/video routing,
xdg-desktop-portal for Wayland capture, V4L2 for cameras, VA-API/NVENC for hardware
encoding, GTK4/libadwaita for native GNOME UI, D-Bus and systemd for desktop integration —
that a Linux-only design can target directly.

## Decision

Prismcast is Linux-only. No Windows or macOS ports are planned, and no portability
abstraction layer will be designed for them.

Concretely:

- Capture is PipeWire/xdg-desktop-portal-first with V4L2 for cameras (PLAN §6).
- Hardware encoding targets VA-API and NVENC; software encoders are fallback (PLAN §12).
- IPC is a Unix domain socket (PLAN §21, ADR-0006).
- The desktop UI is GTK4 + libadwaita (PLAN §3, ADR-0002).
- LV2 is a first-class audio plugin format candidate (PLAN §9).

Platform-specific code is allowed to use Linux APIs directly (D-Bus, memfd, DMABUF,
systemd) without indirection, provided it stays behind the media-backend traits
(ADR-0004) and out of the domain crate.

## Alternatives

- **Full cross-platform from day one.** Rejected: roughly doubles capture/encoder/UI
  integration work, delays the vertical-stack-first scope strategy (PLAN §77), and
  re-introduces the abstraction tax this project deliberately avoids.
- **Abstract "OS layer" trait with one Linux implementation, keeping the door open.**
  Rejected as a formal layer: the backend traits in ADR-0004 already isolate GStreamer and
  capture specifics; an additional hypothetical OS layer adds indirection with no consumer.
  The dependency-direction rules (domain ← core ← services ← UI) keep `prismcast-core`
  platform-clean anyway, so a future port is not structurally blocked — it is just not a
  design goal.

## Consequences

- The architecture can exploit DMABUF zero-copy, portal persistence tokens, PipeWire
  node targeting, and Wayland semantics directly instead of through shims.
- CI and testing matrices are Linux-only (GNOME Wayland, KDE Plasma Wayland, X11
  fallback per PLAN §45).
- User base is limited to Linux; this is an accepted product trade-off (PLAN §1).
- Domain code remains free of platform types, so the decision does not leak into
  `prismcast-core`.

## Evidence

- PLAN.md §1 (Product goal: "Linux-only OBS-class broadcasting and recording application").
- PLAN.md §79 (deliberately not copying OBS's OS abstraction and historic compatibility).
- PLAN.md §77 (vertical-stack scope strategy).

## Status

Accepted (2026-09-30)
