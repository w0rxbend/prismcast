# CAPTURE-001 portal and native ownership research

2026-10-01: GNOME Wayland, xdg-desktop-portal 1.21.1, GNOME backend 50.0,
PipeWire/gstreamer1.0-pipewire 1.6.2. ScreenCast GetAll: version5,
AvailableSourceTypes7, AvailableCursorModes7. No native PipeWire development headers
installed; ashpd plus existing Gst plugin does not need them.

Official [ScreenCast contract](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.ScreenCast.html)
requires ordered CreateSession, SelectSources, Start, OpenPipeWireRemote. Subscribe
Closed before Start. Single-source selection and Embedded cursor are capability
checked; persistence disabled. `size` is compositor coordinate space, not pixel caps.

Version-matched [gstpipewirecore.c](https://github.com/PipeWire/pipewire/blob/1.6.2/src/gst/gstpipewirecore.c)
make_core duplicates fd via fcntl(F_DUPFD_CLOEXEC,3), then pw_context_connect_fd owns
the duplicate. The original integer keys the plugin's shared native core cache and
must remain valid until graph teardown. [gstpipewiresrc.c](https://github.com/PipeWire/pipewire/blob/1.6.2/src/gst/gstpipewiresrc.c)
sets target_id from decimal path; numeric target-object instead denotes serial.
Use session-scoped `path` for v5 node IDs. DONT_RECONNECT prevents silent target reuse.

[ashpd API](https://docs.rs/ashpd/0.13.13/ashpd/desktop/screencast/struct.Screencast.html)
returns OwnedFd. Its default connection is global; request futures internally await
Response before returning public handles. Use dedicated connections to make early
cancellation cleanable, including CreateSession and delayed Start responses.
