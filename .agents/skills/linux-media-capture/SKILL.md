---
name: linux-media-capture
description: Implement or investigate Wayland screen/window capture, xdg-desktop-portal ScreenCast, PipeWire streams, V4L2 cameras, DMA-BUF negotiation, and capture recovery for Prismcast.
---

# Linux capture integration

Use Wayland and PipeWire as the primary platform. Check desktop, compositor, portal backend/interface versions, PipeWire and GStreamer plugins before claiming support. Use official interface documentation and version-matched Rust binding APIs.

Portal operations are asynchronous request/session lifecycles. Follow CreateSession, SelectSources, Start and OpenPipeWireRemote according to the supported interface. Handle response signals, user cancellation, permission denial and session closure. Track the returned file descriptor's ownership and lifetime, and validate how the installed pipewiresrc consumes it and the selected node IDs.

Restore tokens and persistent selection depend on interface/backend support and the user's grant. Store supported tokens through the chosen persistence boundary; fall back to selection when restoration fails. Region crop belongs to the media transform unless the actual backend exposes it.

Treat PipeWire restart, source disappearance, monitor hotplug, portal closure and camera removal as recoverable state transitions with bounded reconnect attempts. Do not repeatedly open permission dialogs through an automatic retry loop. Release sessions, streams and FDs on cancellation and shutdown.

For V4L2 negotiate supported formats, sizes and frame rates. For audio verify channel maps, sample rates, clocking and requested source/monitor semantics. Keep streaming callbacks small and avoid blocking or repeated allocation in per-buffer paths.

For DMA-BUF verify negotiated formats/modifiers and importer/exporter compatibility. Measure fallback copies and GPU transitions. Do not equate Wayland, PipeWire or DMA-BUF availability with end-to-end zero-copy.

Test with the actual supported GNOME/KDE or other target sessions. Separate mock lifecycle tests from hardware/portal integration evidence; Xvfb cannot establish Wayland ScreenCast behavior.

## Official references

- [ScreenCast portal](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.ScreenCast.html)
- [Portal request lifecycle](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.Request.html)
- [PipeWire documentation](https://docs.pipewire.org/)
- [V4L2 documentation](https://www.kernel.org/doc/html/latest/userspace-api/media/v4l/v4l2.html)
