# CAPTURE-002 runtime integration constraints

The CAPTURE-001 grant is ephemeral and cannot be reconstructed from persisted
source settings. Explicit authorization must pass through the core Command API;
source creation or snapshot restoration alone cannot initiate portal Start.
Runtime source status and negotiated dimensions belong to a separate snapshot
view, not persisted Source fields. Worker updates need generation checks so a
late authorization cannot revive a removed, disabled or superseded source.

A single bounded effect receiver owns capture work. Authorization admission must
fail if no owner can receive it; latest-only runtime watches cannot serve as a
lossless queue of picker requests. Native graph mutation and blocking teardown
remain on the media owner thread, with bounded buffers at callback boundaries.

Current compositor reconstruction uses a NULL barrier and rebuilds shared source
bins. Portal capture must retain a separate producer/lease across placement,
scene and canvas updates, sharing that producer for all placements of SourceId.
If producer and consumer use separate pipelines, their clock origins and buffer
segments must be aligned explicitly; retained producer PTS cannot simply be fed
to a newly started consumer as though both had identical running times.

Portal stream size is compositor-coordinate metadata and can differ from actual
pixels. Only negotiated native caps establish compositor/editor dimensions.
GNOME ScreenCast v5 on this host supplies node IDs; serial targeting is a later
v6 capability. Revocation is external and immediate. Voluntary removal/retry and
shutdown must release native consumers/producers before closing the portal lease.

GTK exports its own window handle on the main thread. Export lifetime must cover
the pending request, and unexport must run on GTK during teardown. The opaque
parent identifier is ephemeral service context; it is not a persisted source
setting or a wire-provided OS resource.

## Primary sources

- ScreenCast lifecycle, stream sizes and versioned targeting:
  https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.ScreenCast.html
- GTK Wayland handle export ownership:
  https://docs.gtk.org/gdk4-wayland/method.WaylandToplevel.export_handle.html
- Existing local ownership evidence: capture-lease-native.md and ADR-0016.

Live monitor/window pixels and portal cancellation are opt-in display evidence.
Headless generators establish graph negotiation and teardown but cannot establish
actual portal capture. No permission dialog is opened during ordinary CI.
