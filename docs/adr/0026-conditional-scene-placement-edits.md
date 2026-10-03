# ADR-0026: Atomic conditional scene placement edits

Status: accepted for CORE-007.

## Context

UI-002 added gesture and numeric placement editing over the preview canvas.
Both paths compute an absolute `Transform` from a basis captured earlier:
the item's current transform/crop/bounds, the source's negotiated native
dimensions, the active profile's video configuration and the current scene.
The final submission re-reads the latest snapshot as a courtesy, but the read
and the dispatch are separate operations (docs/testing/preview-editor.md,
ADR-0014): another controller can commit a transform, crop, lock or removal —
or change the scene/profile/capture context — between the last read and the
actor applying the command. The late edit then silently overwrites the newer
state with a value computed from a stale basis.

The application actor serializes all mutations, so an atomic
compare-and-apply inside the actor closes this race. What the actor compares
is the design question.

### Revision versus entity preimage

Option A: **global state revision precondition** (`expected_revision: u64`
from `AppSnapshot::revision()`). Trivial to implement, but far too coarse:
every committed command bumps the one global revision, so an unrelated scene
rename, audio route change or a different item's edit rejects the placement
edit. Under multi-controller load spurious conflicts dominate. The wire
`snapshot` does not even carry the revision today, so native clients would
additionally need a protocol schema addition.

Option B: **per-entity version counters**. Requires new version fields on
persisted domain types (`SceneItem`, `Scene`, profile), a persisted-file
schema migration, and version maintenance in every mutation path. Large blast
radius for one command family.

Option C (chosen): **typed entity-preimage expectation**. The command carries
the exact basis values the edit was computed from; the owner applies the edit
only if the basis still holds. Unrelated commits (other items, renames,
audio, meter revisions) do not invalidate the edit. Every required value is
already present in the wire `StateSnapshot` (full scene items, profiles with
video config, `source_runtime` dimensions, `current_scene`) — controllers
obtain the context without any runtime grant or new query. No persisted
schema changes.

## Decision

Add one new canonical command:

```rust
Command::SetSceneItemTransformIf {
    scene_id: SceneId,
    item_id: SceneItemId,
    transform: Transform,
    expect: PlacementExpectation,
}
```

`PlacementExpectation` is a flat, bounded, `Copy + PartialEq` domain struct —
the placement-edit basis:

- `transform: Transform`, `crop: Crop`, `bounds: Bounds`, `locked: bool` —
  the item preimage the geometry was derived from. A concurrent transform,
  crop or bounds change, or the item becoming locked, rejects the edit.
  `visible`, `opacity` and `z_index` are deliberately excluded: they do not
  alter the geometric basis of a transform edit.
- `current_scene: SceneId` — the scene the controller was previewing.
- `active_profile: ProfileId` and `video: VideoConfig` — the canvas basis
  (conservative full video-config equality, matching the existing GTK draft
  invalidation).
- `source_dimensions: Option<SourceDimensions>` — the negotiated native
  pixels of the item's source; `None` expects no active dimensions.

Item identity comes from the command's `scene_id`/`item_id`; a missing scene
or item keeps the ordinary `NotFound` semantics. Any expectation mismatch is
a new typed `Error::Conflict`, distinct from validation (`InvalidInput`) and
authorization (`Unauthorized`).

Atomicity and placement of the check:

- Pure `AppState::apply` enforces the state preimage (item, current scene,
  profile/video) before any mutation, so the domain contract is self-contained
  and the scratch-replay paths (transactions, saturation probes) stay honest.
- The application actor additionally compares `source_dimensions` against its
  owned capture runtime in the same dispatch turn, before inverse preparation.
  Because the actor owns both the state and the runtime and serializes all
  mutations, check-and-apply is atomic. A rejected conditional edit changes
  nothing: no state, no events, no revision bump, no snapshot publication, no
  history entry, no meter clear, no capture invalidation, no persistence
  notification.
- Conditional commands are top-level only. A `SetSceneItemTransformIf` inside
  an atomic `Transaction` is rejected before any state change, like history
  commands, because per-member runtime admission cannot be honored by the
  existing transaction replay contract.

History integration (ADR-0025): the command requires `ControlScenes`, emits
the ordinary `SceneEvent::ItemUpdated`, and shares the `transform scene item`
label. Its domain inverse is the **unconditional** `SetSceneItemTransform`
with the pre-application value, so history never stores a precondition and
Undo/Redo replay cannot re-check a stale expectation (notably runtime
dimensions, which can change without clearing the redo stack). No-op behavior
is unchanged: an expectation-matching edit equal to the current value emits no
events and records no history entry. Capture authorization is untouched:
successful commits invalidate transient capture through the normal commit
logic only.

Protocol (additive version 1, no bump): one new advertised request kind
`set_scene_item_transform_if` with a typed wire `PlacementExpectation` (UUIDs
plus the existing wire `Transform`/`Crop`/`Bounds`/`VideoConfig`/
`SourceDimensions`), mapped to the canonical command. `Error::Conflict` maps
to the existing `state_conflict` 500 code with a `field` naming the mismatched
expectation member; transaction membership is rejected with `invalid_request`
(the same mapping as history-command members). The obs-websocket adapter keeps
issuing unconditional edits; a core `Conflict` surfacing there maps to the
existing invalid-resource-state status.

GTK: gesture `finish()` and the numeric/action controls keep capturing their
originating context as they do today, but submit `SetSceneItemTransformIf`
carrying that context instead of the unconditional command. The local draft
staleness preflight stays as UX; the core check is the authority. Rejections
surface through the existing command-error toast and snapshot refresh.

The existing `SetSceneItemTransform` (and all other placement commands)
remain supported unchanged for deliberate unconditional control. The
expectation type is designed to be reusable if conditional crop/bounds
variants are ever needed; they are out of scope.

## Consequences and limits

No new dependencies and no persisted-schema changes. One new core error
variant, one new command variant, one new wire request kind (advertised list
grows from 65 to 66). Deterministic owner race tests must prove stale GTK and
native edits cannot clobber newer changes and that unrelated commits do not
invalidate a pending conditional edit. Known unrelated follow-ups remain
open: destructive undo, gesture grouping over the wire, z-order boundary
overflow and missing neighbor events, and scene-nesting cycle checks.
