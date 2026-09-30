---
name: linux-desktop-packaging
description: Package and verify Prismcast Rust GTK4/libadwaita desktop builds, native library requirements, desktop/AppStream metadata, GResources, and Flatpak portal/runtime integration.
---

# Linux desktop packaging

Inspect the selected deployment format and oldest supported runtime before choosing GTK/libadwaita/GStreamer APIs or Cargo feature gates. Use Cargo.lock for reproducible application builds. Check system libraries through pkg-config and confirm required GStreamer plugins inside the deployment environment, not only on the host.

- Keep the application ID consistent across application startup, desktop filename, AppStream ID, icons, resources and D-Bus activation where configured.
- Validate desktop entries and AppStream metadata using their validators. Install icons, GResources and gettext catalogs in the expected package paths.
- Keep build-time development libraries separate from runtime requirements. Confirm codec, sink, source, TLS and hardware plugin availability in the final package.
- For Flatpak choose the runtime/SDK and Rust extension compatible with the project's MSRV. Declare reproducible sources and use an offline/vendored Cargo build appropriate to the manifest.
- Choose sandbox access for the implemented feature set. Use portals for desktop capture and user-selected files; verify the PipeWire FD handoff within the sandbox. Host device/plugin support does not imply sandbox availability.
- Keep profile/scene persistence compatible with XDG and the sandbox's mapped locations. Test upgrades against older schema-versioned data.

Verify installation, launch from the desktop, repeated activation, resources/icons/translations, capture permissions and recording destinations in the final package. Run a clean package build and report missing runtime capabilities explicitly. Publishing to a store or remote distribution is a separate authorized action.

## Official references

- [Flatpak documentation](https://docs.flatpak.org/)
- [Desktop entry specification](https://specifications.freedesktop.org/desktop-entry-spec/latest/)
- [AppStream documentation](https://www.freedesktop.org/software/appstream/docs/)
- [GResource](https://docs.gtk.org/gio/struct.Resource.html)
- [Portal documentation](https://flatpak.github.io/xdg-desktop-portal/docs/)
