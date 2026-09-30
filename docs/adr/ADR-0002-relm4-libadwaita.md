# ADR-0002: GTK4 + Relm4 + libadwaita desktop UI with a presentation-only role

## Context

Prismcast needs a desktop UI that is native to GNOME/Wayland, written in Rust, and
compatible with the project's defining invariant (PLAN §76, ADR-0005): every user-visible
operation is a Core Command and every state change produces a Core Event. The same
operations must later work identically from WebSocket, IPC, the web UI, and the CLI
(PLAN §3). A UI toolkit choice therefore has two parts: which toolkit, and what the UI
is *allowed to do*.

The stack chosen in PLAN §3 is gtk4, relm4, libadwaita, gio, glib, gdk4. Relm4 provides
an Elm-like component/message architecture on top of GTK4 and integrates with libadwaita.
For preview rendering, the media graph terminates in `gtk4paintablesink`, exposing the
composed video as a `GdkPaintable` rendered by a GTK `Picture` with GL/DMABUF support
(PLAN §5).

## Decision

1. The desktop application `prismcast-ui` is built with **GTK4 + Relm4 + libadwaita**.
2. Relm4 components hold **presentation state only**. There is no media logic in Relm4
   components, and GTK/GStreamer types must not leak into domain models (PLAN §2, §3).
3. UI interaction follows exactly one path:

   ```
   GTK → Command → Application Service → Domain mutation → Media Engine → Domain Event → GTK update
   ```

   GTK widgets never mutate a GStreamer pipeline or domain state directly.
4. libadwaita is the visual baseline (system/light/dark/custom themes via CSS overlays,
   PLAN §27); the layout is adaptive and does not copy OBS's Qt dock implementation
   (PLAN §28).

## Alternatives

- **Qt (as OBS uses).** Rejected: the project deliberately does not inherit OBS's
  Qt-oriented frontend architecture (PLAN §1, §79); Qt also has weaker Rust bindings and
  no native GNOME look.
- **Iced / egui / other immediate-mode or pure-Rust toolkits.** Rejected: immature
  Wayland/desktop integration relative to GTK4, no libadwaita-equivalent design language,
  and no equivalent of `gtk4paintablesink` for zero-copy GStreamer preview (PLAN §5).
- **Relm4 components owning media handles directly.** Rejected: it breaks frontend
  interchangeability (PLAN §76) and makes remote control a second-class bolt-on.

## Consequences

- UI work is decoupled from media work; UI agents can build against Command/Event types
  without touching GStreamer (enables the Phase 1 demo `prismcast-cli ping` against the
  running GTK app, PLAN §43).
- UI bugs cannot corrupt media state; all mutations are validated by the core.
- GTK access is confined to the GTK main thread (PLAN §57 concurrency model, §75).
- The project depends on GTK4/libadwaita/GStreamer development packages in CI (PLAN §62).

## Evidence

- PLAN.md §3 (Recommended technology stack: Desktop UI; "Use Relm4 only for presentation
  state"; the GTK → Command → … → GTK update flow).
- PLAN.md §2 (Fundamental architecture decision: no media logic in Relm4 components).
- PLAN.md §5 (Preview/rendering strategy via `gtk4paintablesink`).
- PLAN.md §27–28 (Theme system and desktop layout on libadwaita).

## Status

Accepted (2026-09-30)
