# UI-002 GTK preview input research

2026-10-01; GTK Rust0.11.5, native GTK4.22.4, Relm4 0.11.

GTK GestureDrag emits start coordinates, cumulative offsets for updates/end,
and inherited Gesture::cancel. Use one local draft updated synchronously by
these callbacks; only drag-end emits a core command. Cancellation/Escape clears
the draft. Callback wiring captures weak editor references to avoid widget cycles.

DrawingArea's draw function receives allocation dimensions and a Cairo context;
it renders selection decoration only. Overlay hosts it above the existing
Picture. Picture ContentFit::Contain keeps source aspect ratio, so canvas-space
input conversion must account for centered letterbox offsets and active-profile
dimensions. Skip input outside the fitted canvas. Draw clipping follows that
same rectangle. Neither paintable frame dimensions nor gesture offsets define
domain canvas dimensions.

Numeric SpinButtons are presentation drafts until an explicit Apply action.
Snapshot synchronization blocks selector callbacks synchronously. Selection
holds SceneItemId, not a row index; hit testing uses the shared compositor
layout and reversed committed z-order. Lock/visibility/source/profile changes
invalidate an active draft rather than overwrite concurrent commands.

Verified installed gtk4-0.11.5 signatures: GestureDragExt::connect_drag_begin,
connect_drag_update, connect_drag_end accept f64 pairs; GestureExt::connect_cancel
accepts Option<EventSequence>; DrawingAreaExtManual::set_draw_func accepts
allocated i32 width/height. APIs used are below application's GTK >=4.14 floor.

Official references:
- https://docs.gtk.org/gtk4/class.GestureDrag.html
- https://docs.gtk.org/gtk4/class.DrawingArea.html
- https://docs.gtk.org/gtk4/class.Overlay.html
- https://docs.gtk.org/gtk4/enum.ContentFit.html
