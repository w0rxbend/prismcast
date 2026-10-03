# Interactive preview editor verification

UI-002 wraps the existing native Picture with a GTK-local selection decoration,
placement selector and numeric/action toolbar. Geometry comes from the same pure
prismcast-compositor layout used by the backend. Input maps through the centered
aspect-preserving canvas rectangle using the active profile dimensions; margins
are excluded from hit testing. Visible, enabled placements are tested from the
highest committed z-order, identified by SceneItemId. Locked placements can be
selected but cannot be edited.

Pointer motion updates only one local draft and queues a redraw. Drag end sends
one SetSceneItemTransformIf (ADR-0026); Escape, Gesture::cancel, allocation
changes and stale scene/profile/item/source snapshots cancel it. Final
submission reads the latest core snapshot even when its GTK wakeup is still
queued. Numeric Apply does the same preflight and checks candidate geometry
before dispatch. Both paths attach the originally captured placement basis as a
PlacementExpectation: item transform/crop/bounds/lock, current scene, active
profile and its video config, and the negotiated source dimensions. The
application actor compares that basis against authoritative state and its
capture runtime in the same dispatch turn, so the courtesy re-read and the
dispatch no longer need to be atomic in the UI: a concurrent accepted command
rejects the stale edit with Error::Conflict instead of being silently
overwritten, and the rejection surfaces through the existing PreviewDispatched
toast. The local preflight stays as early-cancel UX; the core check is the
authority.

A dedicated PreviewDispatched reply releases the pending editor intent; unrelated
command replies do not release it. Snapshots remain authoritative. Bounds sizing
disables scale fields and pointer resize, while moving and cardinal/flip actions
remain available. Free rotation, snapping, grouping and studio tools are outside
this task. Preview failure/status pages disable editing.

## Automated checks

```sh
just ci
just deny
GSK_RENDERER=cairo G_DEBUG=fatal-criticals cargo test -p prismcast-ui production_preview_gesture -- --ignored --test-threads=1
GSK_RENDERER=cairo G_DEBUG=fatal-criticals cargo test -p prismcast-ui native_shell_preview -- --ignored --test-threads=1
```

Passed on 2026-10-01 with the existing Wayland display and GTK4 4.22.4. Eight
headless editor tests cover letterbox/invalid allocation mapping, anchored
resize clamping, signed/cardinal geometry, zero-offset/cancel preservation,
topmost hit selection and 1000 local updates producing one committed actor
revision, remote lock/profile invalidation, a concurrent committed transform
rejecting the stale conditional gesture edit as Error::Conflict while the newer
core value survives, and unrelated commits (scene rename, another item's
transform) not staling the captured basis.

The separate native display test emits actual production GestureDrag callbacks,
then dispatches their resulting Commands through the actor. It asserts every
committed gesture and button command is the conditional
SetSceneItemTransformIf variant and that the real core accepts it, checking the
resulting state. It waits 80 ms in
the GTK main loop between begin and motion to catch selection-driven allocation
or DropDown notification feedback. It checks move and
resize, local high-frequency motion without a command queue, canceled gestures,
latest-core lock preflight with no preceding GTK refresh, allocation cancellation,
placement selection and Apply/Rotate90/FlipX button callbacks, bounds sensitivity,
known-unrenderable numeric rejection and disabled failed-preview controls. It
uses real GTK widgets on a real display; it does not simulate physical mouse
input or assert native pointer-device recognition. GTK criticals are fatal.

The existing actual RelmApp shell test also passed after editor integration,
checking frame arrival, command updates and repeated production window-close
shutdown. Native displayed-pixel evidence remains in native-preview.md and
cpu-transforms.md; the editor does not own a pipeline or renderer.

## Manual exercise

Select a placement via the picker or canvas; drag it to move and drag the
lower-right handle to resize. Use Tab to reach X/Y/scale inputs, then Apply.
Rotate90 and Flip X/Y are existing transform Commands. Check letterboxed
windows, cropped/rotated/signed items, source sharing, locks, remote changes,
keyboard cancellation, failure/recovery, and long source names. Scale fields
scroll horizontally if the toolbar cannot fit. Outline color follows the widget
accent theme; locked outlines are dashed and the hint explains the lock.
