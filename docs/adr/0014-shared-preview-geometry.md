# ADR-0014: Share pure placement geometry between rendering and preview editing

- Status: Accepted
- Date: 2026-10-01
- Tasks: MEDIA-005, UI-002

## Context

The native compositor and the preview editor need to agree on rounded placement
rectangles after crop, flips, rotation, scaling, bounds and anchor alignment.
Independent calculations would make selection outlines and pointer hit-testing
diverge from displayed pixels, particularly with rotated or cropped sources.
The existing scene-graph design specifies the mathematical contract, while the
prismcast-compositor crate reserves the scene-composition layer.

## Decision

Implement framework-free geometry in prismcast-compositor, depending only on
prismcast-core. Return typed source dimensions, normalized crop, cardinal
orientation and final rounded render rectangles. Native GStreamer construction
and GTK presentation consume those immutable calculations. Neither GTK nor
GStreamer becomes a dependency of the geometry crate or the domain.

Use source-axis flips before clockwise cardinal rotation, then canvas-axis
scaling/bounds. Keep arithmetic fractional until final rectangle rounding.
Retain the existing CPU resource limits and nearest-cardinal diagnostic policy;
free-angle rotation is not a new capability of this wave.

The preview keeps selection and an in-progress draft as local presentation state.
A completed gesture sends one existing SetSceneItemTransform Command. Commands
and committed snapshots remain authoritative; local drafts never mutate the
application snapshot. An observed scene/profile/item/source change cancels stale
drafts. Locked and unavailable placements cannot be edited. Separate preview
command acknowledgements bound pending edits independently of other controllers.
Before completing a gesture or numeric action, check a fresh core snapshot and
cancel if its captured context changed. Canvas allocation changes also cancel
gestures. This client-side check does not introduce an atomic compare-and-set
command; edits racing after the check retain the existing core command ordering.

## Consequences

Pixel and hit-test geometry has one implementation and can be tested without a
display. TestPattern source dimensions are available from settings in this
prototype; later source backends need a negotiated-dimensions contract before
preview tools support them. Bounds-driven placements can move but pointer resize
and numeric scale editing stay disabled until bounds-specific editing is added.

Graph changes retain the existing prototype NULL barrier and can briefly interrupt
frames. General transform streaming and transaction-safe grouped undo remain
later work. No new persistent schema, wire protocol or core Command is required.
