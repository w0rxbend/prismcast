---
name: relm4-desktop
description: Implement or debug Prismcast Rust Relm4 components, factories, view macros, component messages, and the GTK-to-core async bridge. Use for desktop presentation changes and Relm4 lifecycle or update problems.
---

# Relm4 desktop development

Read AGENTS.md and the active task. Inspect the actual Relm4, gtk4, glib and libadwaita dependency versions and feature gates before selecting APIs. Use version-matched examples; book examples may contain older widgets or unwrap calls unsuitable for production here.

- Keep component models focused on presentation state, selection, dialogs and snapshots. Send core Commands for user operations; render core Events/Snapshots. Media ownership stays outside components.
- Choose SimpleComponent for synchronous presentation, Component when command outputs are needed, and async components only when their lifecycle requires it. Verify exact trait signatures in the installed version.
- Retain child Controllers for their intended lifetime; forward typed child outputs to parent inputs. Disconnect callbacks and cancel owned background work when a component is destroyed.
- Use factories for repeated independent rows when appropriate; identify scenes/sources by domain IDs rather than treating row positions as permanent IDs. Evaluate GTK list-model virtualization for large lists.
- Inspect view macro watch/track behavior when a property fails to update. Prevent feedback loops when rendering snapshots changes controls whose signals send Commands.
- Keep widgets on the GTK thread. Cross runtime boundaries using owned Send data and a bounded application bridge. Check Relm4 sender semantics instead of assuming its internal channels provide backpressure. Coalesce meters and snapshots before forwarding them to the UI.
- Keep long work out of update and signal handlers. Handle closed channels, stale results and cancellation; stopping a future does not necessarily stop an underlying blocking operation.

Verify the affected crate with cargo check and focused tests; run repository quality gates when available. Exercise repeated component creation/destruction, command failures, and updates after navigation or window closure. Report display-dependent checks separately from compiler checks.

## Sources to consult for the affected API

- [Relm4 book](https://relm4.org/book/stable/)
- [Component messages and controllers](https://relm4.org/book/stable/components.html)
- [Relm4 API](https://docs.rs/relm4/)
- [Upstream examples](https://github.com/Relm4/Relm4/tree/main/examples)
- [GTK main loop and async work](https://gtk-rs.org/gtk4-rs/stable/latest/book/main_event_loop.html)
