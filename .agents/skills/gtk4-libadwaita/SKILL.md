---
name: gtk4-libadwaita
description: Design, implement, review, or debug native Rust GTK4 and libadwaita UI, GObject ownership, actions, CSS, accessibility, and application lifecycle in Prismcast.
---

# GTK4 and libadwaita

Inspect Rust bindings, system GTK/libadwaita versions, Cargo feature gates and the oldest supported deployment environment. Select widgets and APIs supported by that combination. Consult Relm4 guidance when editing components.

Keep all GTK object access on the main thread. Use weak references where callbacks otherwise form ownership cycles; explicitly manage signal handlers, bindings, timers and subscriptions with widget/component lifetime. A GLib main context and Tokio runtime are separate execution environments; confirm which executor owns a future before using runtime-specific APIs.

Use GNOME HIG and libadwaita patterns for navigation, preferences, dialogs and feedback. Preserve Prismcast's preview/scenes/sources/mixer/output workflow from PLAN.md. Choose layouts for this broadcasting workload rather than forcing every control into preference rows.

- Route GActions, shortcuts and widget signals through the same core Command path. Keep action enablement consistent with core state and permissions.
- Use native widgets, symbolic icons and accessible names. Test keyboard focus, long labels, scaling, high contrast and light/dark appearance. Convey recording/streaming state with text or icons as well as color.
- Use GTK CSS and libadwaita semantic styling for custom theme overlays. Verify CSS selectors and properties against GTK documentation; browser CSS is not interchangeable.
- Use GSettings for desktop preferences only where chosen by the project. Keep schema-versioned profiles and scene collections in the core persistence layer.
- Keep the application ID consistent with resources, desktop metadata and packaging. Handle activation, window recreation and orderly shutdown explicitly.
- For freezes or critical warnings, use GTK Inspector, structured logs and an isolated reproduction; do not hide warnings by catching unrelated errors.

Verify UI changes in a real supported desktop session when available. Headless checks cannot prove Wayland capture, GPU rendering or screen-reader behavior.

## Official references

- [gtk4-rs book](https://gtk-rs.org/gtk4-rs/stable/latest/book/)
- [GTK API](https://docs.gtk.org/gtk4/)
- [libadwaita API](https://gnome.pages.gitlab.gnome.org/libadwaita/doc/1-latest/)
- [GNOME HIG](https://developer.gnome.org/hig/)
- [GIO](https://docs.gtk.org/gio/)
