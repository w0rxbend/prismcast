# ADR-0003: Wayland-first capture via xdg-desktop-portal and PipeWire, X11 compatibility where practical

## Context

Screen and window capture is the most platform-sensitive subsystem. On Linux the display
server split (Wayland vs X11) determines which APIs even exist. Under Wayland there is no
protocol-level screen-scraping; the sanctioned route is the xdg-desktop-portal ScreenCast
interface, which returns a PipeWire file descriptor/stream that GStreamer's `pipewiresrc`
can consume (PLAN §6). The portal also provides explicit monitor/window selection,
persistence through restore tokens, and the security model Wayland compositors require.

PipeWire node IDs are not stable across restarts — they can be reused — so sources must be
re-targeted using the `pipewire-serial` property rather than raw node IDs (PLAN §36).

X11 remains deployed; PLAN §1 requires "X11 compatibility where practical" and Phase 3
tests include an X11 fallback (PLAN §45), but X11-specific APIs must not shape the design
(PLAN §6: "Do not design around X11-specific APIs").

## Decision

1. **Wayland is the primary capture target.** Display/window capture uses:

   ```
   xdg-desktop-portal → ScreenCast portal → PipeWire FD/stream → GStreamer pipewiresrc
   ```

2. Required capture functionality: display capture, window capture, region crop, cursor
   modes, persistent portal selection (restore tokens stored in source configuration),
   capture reconnect, monitor hotplug, and window disappearance/reappearance (PLAN §6).
3. Source identity survives restarts via portal restore tokens and `pipewire-serial`
   targeting, never via raw PipeWire node IDs.
4. **X11 is a compatibility path, not a design axis.** Where practical (e.g. X11 sessions
   detected at runtime), capture may fall back to X11-appropriate mechanisms (including
   the portal, which also works on X11, or `ximagesrc` as a last resort), but no
   architecture decision may depend on X11-only APIs.
5. Capture is exercised on GNOME Wayland, KDE Plasma Wayland, and an X11 fallback
   environment (PLAN §45).

## Alternatives

- **X11-first / Xlib-XCapture as the primary design.** Rejected: X11 is in maintenance
  decline on the desktop, Wayland is where security and HDR/fractional-scaling work
  happens, and PLAN §6 mandates Wayland as the primary target.
- **Direct Wayland protocol capture (ext-screencopy / wlr-screencopy).** Rejected as the
  primary path: compositor support is fragmented (GNOME does not implement wlr protocols),
  there is no standardized permission/selection UX, and the portal is the cross-compositor
  sanctioned API. A compositor-specific fast path may be investigated later behind the
  source backend trait, but only as an optimization.
- **PipeWire camera/display enumeration without the portal.** Rejected for screen/window
  capture: the portal is what grants access; bypassing it is not permitted by compositors.

## Consequences

- Capture UX includes a portal selection dialog by design; restore tokens make it a
  once-per-source interaction rather than per session.
- Failure handling must cover portal session expiry, PipeWire restarts, and monitor
  hotplug explicitly (PLAN §61 failure model).
- X11 users get a working but secondary experience; some Wayland-first features (e.g.
  per-window capture via portal) may degrade gracefully on X11.
- All capture code lives behind the source backend traits (ADR-0004), so the portal
  dependency does not leak into the domain.

## Evidence

- PLAN.md §6 (Linux capture architecture: portal ScreenCast → PipeWire → pipewiresrc;
  required functionality list; "Do not design around X11-specific APIs").
- PLAN.md §36 (PipeWire node IDs are reused; target with `pipewire-serial`).
- PLAN.md §45 (Phase 3 test matrix: GNOME Wayland, KDE Plasma Wayland, X11 fallback).
- PLAN.md §61 (failure cases: PipeWire restarts, portal session expires, monitor
  disconnected).

## Status

Accepted (2026-09-30)
