# Scene graph → GStreamer compositor mapping

Date: 2026-10-01. Task: `.agent/tasks/ARCH-003.yaml`. Design doc only — implementation
is MEDIA-003 (CPU prototype) and MEDIA-005 (full transform support). See §14 for implemented scope.

Normative inputs:

- Domain model: `crates/prismcast-core/src/scene.rs` (`Scene`, `SceneItem`,
  `Transform`, `Crop`, `Bounds`, `BlendMode`, `Anchor`), `source.rs`
  (`SourceKind::Scene` for nesting), `command.rs` / `event.rs` (Command/Event API).
- PLAN.md §8 (scene graph), §5 (preview/rendering), §59 (undo transactions),
  §76 (command/event invariant).
- ADR-0004 (GStreamer behind backend traits; start with `compositor`, §3 of its
  Decision), ADR-0005 (every mutation a Command, every change an Event).
- RES-003 (`docs/research/gstreamer-capabilities.md`) §2 (compositor elements and
  pad properties), §7 (risks), Conclusion 2 (compositor backend strategy).

## 1. Scope and layering

The scene graph has two halves that must never merge:

- **Domain half** (`prismcast-core`): `Scene` is an ordered list of `SceneItem`s,
  each placing a shared `Source` with transform/crop/opacity/visibility/lock/blend/
  bounds/z-index. Items are kept sorted ascending by `z_index` with insertion order
  as tiebreak (`Scene::sort_items`). This half knows nothing about GStreamer.
- **Media half** (`prismcast-compositor`, MEDIA-003): a `CompositorBackend`
  implementation (per ADR-0004) that maintains a GStreamer `compositor` element
  graph mirroring the *current program scene* (and, in studio mode, the preview
  scene). It consumes `SceneEvent`s and turns them into pad graph mutations.

The mapping in this document is the contract between the two halves. UI, CLI,
WebSocket and IPC never talk to the compositor; they emit Commands and render
Events (ADR-0005, PLAN §76).

```
Controller (GTK/CLI/WS/IPC)
      │  Command::SetSceneItemTransform { .. } etc.
      ▼
Application Core ── AppState::apply(state, cmd) ──► (state', Vec<Event>)
      │                                                │
      │ Arc<AppSnapshot> (reads)                       │ Event::Scene(SceneEvent::ItemUpdated { .. })
      ▼                                                ▼
   Controllers render                    prismcast-compositor actor
                                         (MEDIA-003): diff item vs. pad state,
                                         set pad properties / relink branches
```

## 2. Pipeline shape per scene

One `compositor` element per *live* scene, src caps pinned to the canvas
resolution from the active profile (`capsfilter video/x-raw,width=W,height=H`).
Each visible `SceneItem` owns one **item branch** feeding one compositor sink pad:

```
┌─ item branch (one per SceneItem) ──────────────────────────────┐
│                                                                │
│  source bin ──► videocrop ──► [videoflip] ──► compositor pad   │
│  (per          (Crop)        (90° steps only,  (xpos/ypos/     │
│   SourceId,                   §6)               width/height/  │
│   shared)                                       alpha/zorder/  │
│                                                 operator)      │
└────────────────────────────────────────────────────────────────┘
```

- The **source bin is shared** per `SourceId`, not per item: a `tee` after the
  source's output fans out to every item branch referencing it, and to any
  output/monitor taps. Adding a second item with the same `source_id` adds a tee
  branch, not a new source instance. This is what makes the domain rule "same
  Source usable from multiple scenes via SceneItem references" (ARCH-001
  acceptance) cheap.
- Compositor backend is swappable: `compositor` (CPU, correctness-first default),
  `glvideomixer` (GL/DMA-BUF), `vacompositor` (Intel/AMD VPP) all expose the same
  pad vocabulary `xpos/ypos/width/height/alpha/zorder` — verified in RES-003 §2.
  Everything below is written against that shared vocabulary; backend-specific
  deltas are called out.

## 3. Field-by-field mapping (SceneItem → compositor behavior)

| `SceneItem` field | Compositor behavior |
|---|---|
| `id: SceneItemId` | Not exposed to GStreamer. Index key of the media-side pad registry: `HashMap<SceneItemId, ItemBranch>` held by the compositor actor; appears in `tracing` context (`item_id=`) per AGENTS.md. |
| `source_id: SourceId` | Selects the tee pad of the shared source bin the item branch links to. |
| `transform.position: Vec2` | Compositor pad `xpos`/`ypos` (gint), after anchor adjustment (§4.3). Negative values are legal (item partially off-canvas); `compositor` clips at the output frame. |
| `transform.scale: Vec2` | Multiplies the cropped source size into pad `width`/`height` (§4.2). Ignored when `bounds.kind != None` (§5). Negative component = flip (§6). |
| `transform.rotation: f32` | **Not expressible on compositor pads.** 0/90/180/270 handled by `videoflip` in the item branch; arbitrary angles unsupported in v1 (§6). |
| `transform.anchor: Anchor` | Pure math: shifts `position` before it becomes `xpos`/`ypos` (§4.3). No pad property. |
| `crop: Crop` | `videocrop` element in the item branch: `left`/`top`/`right`/`bottom` properties map 1:1 (§4.1). |
| `opacity: f32` | Compositor pad `alpha` (double, `[0.0, 1.0]`), clamped. |
| `visible: bool` | `false` → pad `alpha := 0.0` (topology-preserving hide, §7). |
| `locked: bool` | **No media effect.** Consumed by UI editing and undo only; the compositor never sees it. Listed here so no field is unaccounted for. |
| `blend_mode: BlendMode` | Compositor pad `operator` where available: `Normal → over`, `Additive → add`. `Multiply`/`Screen` unsupported by all three elements → fallback + warning (§8). |
| `bounds: Bounds` | When `kind != None`, replaces free scaling: effective scale and anchor are *derived* from the bounds rectangle (§5), then flow through the same pad properties. |
| `z_index: i32` | Compositor pad `zorder` (guint), via dense-rank mapping, not raw value (§9). |

Pad property types per `gst-inspect-1.0` (RES-003 §2): `xpos`/`ypos` gint,
`width`/`height` gint, `alpha` double, `zorder` guint; `operator` and
`sizing-policy` exist on `compositor`/`glvideomixer` only.

## 4. Transform and crop math

All math happens in the compositor actor (media side) from the item snapshot in
`SceneEvent::ItemUpdated` plus the source's negotiated caps. The domain stores
f32; pad properties are integers — rounding rules are fixed here so all backends
agree.

### 4.1 Crop first, in source pixels

Let the source's negotiated frame size be `S = (sw, sh)` (from caps). `Crop` is
applied by `videocrop` before anything else:

```
cw = sw − crop.left − crop.right        (clamped: cw ≥ 1)
ch = sh − crop.top  − crop.bottom       (clamped: ch ≥ 1)
```

Edge cases: crop values beyond the source size clamp to a 1×1 remainder rather
than erroring (OBS behaves this way; a 0-size pad would break negotiation). If
caps are not yet known (source still negotiating), the branch stays unlinked and
the item renders nothing until the first caps event; the pad is then configured
from the actual caps. Crop meta (`GstVideoCropMeta`) is *not* used: only
`cudacompositor` consumes it (RES-003 §2), so an explicit `videocrop` is the
portable path.

### 4.2 Then scale → pad width/height

With `bounds.kind == None`:

```
dw = cw · |scale.x|
dh = ch · |scale.y|
pad.width  = max(1, round(dw))
pad.height = max(1, round(dh))
```

The compositor element performs the scaling itself (`width`/`height` pad
properties), so no `videoscale` is needed in the item branch. On `compositor`
this is CPU scaling (RES-003 §2 limitation); on `glvideomixer`/`vacompositor` it
stays on GPU. `sizing-policy` is left at its default (`none`) — we compute sizes
ourselves because we must honor anchor and bounds semantics that the element's
`keep-aspect-ratio` policy cannot express.

`scale` components are clamped to `|s| ≤ 64` as a sanity bound; 0 is legal and
collapses the item to 1 px (kept negotiable), matching OBS's "invisible but
present" behavior.

### 4.3 Anchor → xpos/ypos

`transform.position` names the canvas location of the item's *anchor point*;
compositor `xpos`/`ypos` name the top-left corner of the placed rectangle:

```
ax, ay = anchor fractions, e.g. TopLeft=(0,0), Center=(0.5,0.5), BottomRight=(1,1)

pad.xpos = round(position.x − ax · dw)
pad.ypos = round(position.y − ay · dh)
```

`Anchor::TopLeft` (the default) therefore passes `position` through unchanged.
Rounding is half-away-from-zero on the final integers only; all intermediate
math stays f32 so chained updates don't accumulate error.

### 4.4 Resulting pad update

One `SetSceneItemTransform` or `SetSceneItemCrop` command collapses to at most
five pad property sets (`xpos`, `ypos`, `width`, `height`, unchanged `alpha`)
plus, when crop changed, four `videocrop` property sets. All are live-settable on
a running pipeline — no relink, no state change (GstVideoAggregator pads accept
property changes between buffers).

## 5. Bounds fitting (`Bounds`)

Bounds is an alternative constraint that *derives* scale and anchor; the
compositor sees only the derived integers, so §4 remains the only pad path.

Given bounds rectangle size `B = (bw, bh)` and cropped source `C = (cw, ch)`:

| `BoundsKind` | Derived scale `(sx, sy)` | Overflow |
|---|---|---|
| `None` | use `transform.scale` (§4.2) | — |
| `Stretch` | `(bw/cw, bh/ch)` | aspect ignored |
| `FitInner` | `s = min(bw/cw, bh/ch)`, `(s, s)` | letterboxed inside rect |
| `FitOuter` | `s = max(bw/cw, bh/ch)`, `(s, s)` | overflows rect, clipped by canvas |

The derived placement is anchored at `bounds.alignment` *within the bounds
rectangle*, and the bounds rectangle itself is positioned at
`transform.position` per `transform.anchor`:

```
dw, dh = cw·sx, ch·sy                        (§4.2 with derived scale)
rect.x = position.x − anchor.x · bw          (bounds rect top-left)
rect.y = position.y − anchor.y · bh
pad.xpos = round(rect.x + align.x · (bw − dw))
pad.ypos = round(rect.y + align.y · (bh − dh))
```

`FitOuter` overflow beyond the canvas is clipped by the compositor's output
frame for free; overflow within the canvas is intentional (OBS "scale to outer
bounds" behavior). A canvas resize (profile change) does **not** recompute
bounds: bounds are absolute canvas pixels in the domain, so a resolution change
visually rescales nothing server-side; items keep their pixel geometry. (OBS
keeps pixel geometry too; a "rescale items on canvas change" feature would be a
new command, not a compositor concern.)

## 6. Rotation and flip — limitations of the compositor elements

None of `compositor`, `glvideomixer`, or `vacompositor` has a rotation pad
property (RES-003 §2; verified pad lists). This is the model's sharpest
impedance mismatch, and it is handled as follows:

- **Cardinal rotations** (rotation ≡ 0/90/180/270 mod 360): insert `videoflip`
  (`method=clockwise`/`rotate-180`/`counterclockwise`) into the item branch
  between `videocrop` and the compositor pad, and swap `dw`/`dh` for 90/270
  before §4.2/§4.3 sizing. Works on every backend (videoflip supports
  GL/DMA-BUF passthrough via `video/*` caps negotiation where the hardware
  elements allow it; worst case it forces one memory copy).
- **Arbitrary rotation**: **unsupported in v1.** No stock element rotates video
  by arbitrary angles while preserving GL/DMA-BUF memory. The design reserves
  the path: the domain keeps `rotation: f32` in the persisted schema, and the
  compositor actor quantizes any non-cardinal angle to the nearest cardinal
  step and logs a `tracing::warn!` once per item. The planned resolution is a
  custom Rust GL/Vulkan compositing element (PLAN §5 anticipates this; RES-003
  §7 notes no Vulkan compositor element exists upstream). Until then the UI
  must gray out free rotation.
- **Flip** (PLAN §8 "Flip"): expressed as a negative `scale` component in the
  domain — no new field needed. The compositor actor maps `scale.x < 0` to
  `videoflip method=horizontal-flip`, `scale.y < 0` to `vertical-flip` (both
  combinable with rotation), and uses `|scale|` in §4.2. `videoflip` handles at
  most one method per element, so a combined rotate+flip branch chains two
  `videoflip` elements.

## 7. Visibility

`visible=false` sets pad `alpha := 0.0` and leaves the branch linked. Rationale:

- Hiding/showing is a frequent live operation (studio workflows toggle items
  mid-broadcast); relinking a live aggregator pad costs a negotiation cycle and
  can glitch adjacent frames.
- The CPU/GPU cost of compositing a fully transparent input is real but small;
  RES-003 Conclusion 2 makes `compositor` the correctness-first default, so we
  optimize topology stability over marginal cost.
- Documented optimization for later: items hidden for the entire session (or
  scenes in the background) may be fully unlinked by the actor as a resource
  saving; this is an internal media decision invisible to the domain.

`visible=false` composes multiplicatively with `opacity`: effective pad alpha is
`opacity` when visible, `0.0` when not. The pad `alpha` is never driven above
1.0 or below 0.0 (clamped on the media side; the domain type already documents
the range).

## 8. Blend modes

Domain `BlendMode` maps to the `operator` pad enum where the element has one:

| `BlendMode` | `compositor`/`glvideomixer` `operator` | `vacompositor` |
|---|---|---|
| `Normal` | `over` (default) | n/a — alpha-over only |
| `Additive` | `add` | unsupported |
| `Multiply` | **unsupported** → `over` + warn | unsupported |
| `Screen` | **unsupported** → `over` + warn | unsupported |

RES-003 §2 confirms `operator` ∈ {source, over, add} on `compositor`/
`glvideomixer` and *no* blend operators on `vacompositor`. `Multiply` and
`Screen` therefore degrade to `Normal` with a one-time `tracing::warn!`
per item; the UI should badge these blend modes as "partial support". Full
support is blocked on the same custom GL/Vulkan element as arbitrary rotation
(§6) — both are fragment-shader features. The backend trait must expose a
capability bitset (`supports_blend_modes`, `supports_arbitrary_rotation`) so the
UI can render honest affordances per active backend.

## 9. Z-order

Compositor pad `zorder` is a guint; domain `z_index` is an i32 with ties broken
by insertion order. Mapping raw values would break on negatives and ties, so the
compositor actor assigns **dense ranks**:

```
for (rank, item) in scene.items.iter().enumerate()   // ascending z_index, stable
    pad[item.id].zorder = rank as u32
```

- `scene.items` is already sorted ascending by `z_index` with insertion-order
  tiebreak (`Scene::sort_items`), and GStreamer ties in `zorder` are avoided by
  construction since ranks are unique.
- `RaiseSceneItem`/`LowerSceneItem` swap neighbors in the domain (with the
  tie-bump semantics in `Scene::raise_item`/`lower_item`); the resulting
  `ItemUpdated` events let the actor recompute ranks. Only pads whose rank
  changed are touched — a raise/lower costs exactly two `zorder` sets.
- `zorder` is live-settable on GstVideoAggregator pads; no relink.

## 10. Command → event → pad flow

Every scene mutation follows the same path (ADR-0005):

| Command | SceneEvent emitted | Compositor actor action |
|---|---|---|
| `AddSceneItem` | `ItemAdded { item }` | tee branch → videocrop → pad; configure per §3–§9 |
| `RemoveSceneItem` | `ItemRemoved { item_id, source_id }` | release pad, unlink branch, drop tee pad if last user |
| `DuplicateSceneItem` | `ItemAdded` | same as add |
| `SetSceneItemTransform` | `ItemUpdated` | §4 math → xpos/ypos/width/height (+videoflip relink on rotation/flip class change) |
| `SetSceneItemCrop` | `ItemUpdated` | videocrop props + §4 resize |
| `SetSceneItemVisible` | `ItemUpdated` | pad alpha (§7) |
| `SetSceneItemLocked` | `ItemUpdated` | **ignored** (§3) |
| `SetSceneItemZIndex`, `Raise/LowerSceneItem` | `ItemUpdated` | re-rank → zorder sets (§9) |
| `SetSceneItemOpacity` | `ItemUpdated` | pad alpha |
| `SetSceneItemBounds` | `ItemUpdated` | §5 → §4 math |
| `SetCurrentScene` | `CurrentChanged` | scene switch on the transition bin (out of scope here — transition design is a separate task); until then: swap program scene's pad set |
| `RemoveScene` | `Removed` | teardown only if it was the live scene |

Notes:

- **Coalescing**: a UI drag gesture emits a stream of `SetSceneItemTransform`
  commands inside one undo transaction group (PLAN §59). Pad property sets are
  cheap and idempotent, so the actor applies every update as it arrives — no
  debouncing on the media side. Snapshots/events are what controllers throttle,
  not the compositor.
- **Source caps arrival** (async): when a source's caps first arrive or change
  (e.g. PipeWire renegotiation — RES-003 §7 upstream issue #3147), the actor
  recomputes §4/§5 for *every* item referencing that `SourceId` and updates pads.
  No domain event is emitted: the domain model is unchanged, this is a media-side
  re-evaluation. UI learns about resolution via a separate media-stats channel,
  not via SceneEvents.
- **Idempotency**: the actor keeps last-applied derived values per item; an
  `ItemUpdated` that changes nothing media-visible (e.g. only `locked` flipped)
  produces zero GStreamer calls.

## 11. Groups and nested scenes

PLAN §8 lists Group/Ungroup as required operations, but the ARCH-001 domain model
has **no group type** — a deliberate gap this document must resolve:

- **Editing groups** (OBS-style: multi-select, move as one) are a **UI/core
  concern, not a compositor concern**. A group would be a domain-level container
  of `SceneItemId`s whose members receive a common relative transform. Because it
  does not exist in `prismcast-core` yet, a follow-up ARCH task must either add a
  `Group` domain type + commands or explicitly descope it. **Follow-up flagged
  for the orchestrator.**
- **Nested composition** already exists via `SourceKind::Scene(SceneId)`
  (`source.rs`): a scene rendered as a source. The compositor realizes this as a
  **sub-compositor**: the nested scene gets its own full pipeline per §2 inside a
  `GstBin`, and its output feeds the parent item branch like any other source.
  The nested scene's canvas is its own capped resolution (its output caps), and
  the parent item's crop/scale apply on top unchanged — the §4 math is recursive
  without modification.
- **Cycle hazard**: `SourceKind::Scene(scene_a)` inside `scene_a` (directly or
  transitively) would recurse forever. `RemoveSource` is already rejected while
  referenced (`state.rs` delete policy), but no cycle check exists for
  `AddSource { kind: Scene(id) }` + placement. The compositor actor must enforce
  a maximum nesting depth (proposal: 8) and refuse deeper linkage with a typed
  error; a domain-level cycle rejection in `AppState::apply` is the proper fix
  and is **flagged as a follow-up** (touches `prismcast-core`, outside this
  task's scope).
- A nested scene renders only when reachable from the program (or studio
  preview) scene; otherwise its sub-compositor exists but its pads are idle
  (§7 optimization applies recursively).

## 12. Studio mode and scene lifetime

- Exactly one compositor instance per **live** scene: the program scene always;
  the preview scene additionally in studio mode (`SystemEvent::PreviewSceneChanged`
  exists in the domain already). Non-live scenes have no pads at all — their
  domain state lives only in `AppState`/`AppSnapshot`.
- Making a scene live is a bulk operation: for each item in sorted order, perform
  the `AddSceneItem` action. Item branches of the *previous* program scene are
  torn down after the transition completes (transition mechanics are a separate
  MEDIA/ARCH task; the scene-graph contract is only "pad sets are per-scene and
  rebuilt on switch").
- Because pad state is always derivable from the `SceneItem` snapshots, crash
  recovery or backend swap (compositor → glvideomixer) is a full rebuild from
  the snapshot — there is no media-side scene state worth persisting.

## 13. Backend capability matrix (from RES-003 §2)

| Capability | `compositor` | `glvideomixer` | `vacompositor` |
|---|---|---|---|
| xpos/ypos/width/height/alpha/zorder pads | yes | yes | yes |
| `operator` (blend) | over/add | over/add | none |
| crop meta | no | no | 1.28+ (cudacompositor; VA unverified) |
| Memory | SystemMemory | GL/DMA-BUF/system | VAMemory/system |
| Role | correctness-first default (v1) | zero-copy path (v1.1) | Intel/AMD optimization (optional, rank none) |

Selection policy per RES-003 Conclusion 2: implement against the shared pad
vocabulary, default to `compositor`, probe for the GL path at startup. The scene
graph design above is identical across backends; only the memory negotiation and
the §6/§8 capability bits differ.

## 14. Open follow-ups for the orchestrator

1. **Group domain type** (§11): PLAN §8 requires Group/Ungroup; the domain model
   has no such type. Needs an ARCH task adding `Group` + commands or an explicit
   descope decision.
2. **Scene-nesting cycle check** (§11): `AppState::apply` should reject
   `AddSource { kind: Scene(id) }` graphs that would cycle. Domain-side fix in
   `prismcast-core` (outside ARCH-003 scope).
3. **Capability bitset on `CompositorBackend`** (§8): `supports_blend_modes`,
   `supports_arbitrary_rotation` so UI affordances match the active backend —
   feed into ARCH-004 (media abstraction) if not already covered.
4. **Arbitrary rotation + Multiply/Screen** require the custom GL/Vulkan element
   (§6, §8); blocked on the same media work, tracked by MEDIA-005.


## 14. MEDIA-003 implemented CPU prototype

Native implementation lives in `prismcast-media-gst::GstCompositor`; the
framework-free `prismcast-compositor` crate remains reserved for composition
math. `GstCompositor::new(sink: gst::Element)` accepts an unattached native sink.
GTK adapters create/inspect GTK sinks on the GTK thread and transfer only the
Send element to the media owner. This crate does not depend on GTK.

`sync_snapshot(&[Source], &Scene)` validates the authoritative source list and
selected scene before mutating the graph; `clear_scene()` handles no selected
scene. Source null settings (the domain's initial value) mean test-pattern
settings defaults. MEDIA-003 introduced TestPattern sources, position/scale,
normal opacity, hidden items and stable dense z-order. MEDIA-005 adds crop,
all anchors/bounds, signed flips and cardinal/quantized rotations via shared
geometry (§15). Other blend modes, source filters and nested/non-test-pattern
sources remain explicit errors when placed. Disabled sources do
not run. Each enabled SourceId has one bin and tee; each placed item has one
downstream-leaky two-buffer queue and compositor request pad. Hidden enabled
items retain branches with zero alpha. Output is CPU SystemMemory RGBA with
fixed canvas dimensions, rational frame rate and square pixels. No zero-copy
claim applies.

Topology changes stop the pipeline to NULL before unlinking: this synchronous
barrier joins streaming threads, avoiding live-pad races. The prototype then
reconstructs shared bins, tees and item branches and resumes if previously
running. This intentionally trades continuity for correctness; streaming-safe
incremental updates and retention across scene switches remain future work.
Scene/source names, locked flags and unused source changes do not rebuild an
unchanged rendered graph. `configure_canvas` applies the latest selected profile
configuration and emits renegotiation. Validation rejects dimensions outside
1..8192, nonpositive/oversized rational fps, fps above 240, nonfinite/excessive
geometry, duplicate IDs and more than 256 items/256 cached scenes/4096 sources.

The compositor is force-live with black background: empty scenes and
`clear_scene()` continue producing black frames while running. Bus ERROR/EOS
use an eight-slot nonblocking terminal channel; other native messages are
dropped. The bounded control event queue retains the latest 32 events. Draining
terminal events tears down the graph; errors dominate EOS in either order.
Stop and Drop release both tee and compositor request pads, unlink branches and
remove graph children, including partial native additions after failure. Graph
construction failures report Failed and leave a stopped graph; owners may
retry via start after reconciling an authoritative snapshot.

## 15. Shared transform geometry (MEDIA-005)

`prismcast_compositor::layout_item(&SceneItem, SourceSize)` is the framework-free
rendering contract for native graph properties and UI outline/hit testing.
`ItemLayout.rect` exposes the exact integer pixel rectangle, normalized `crop`
exposes the actual native edge properties, and `rotated_source_size` gives the
intrinsic cropped/rotated dimensions before scale for UI resize tools.
`test_pattern_source_size(&Source)` resolves validated known dimensions, using
1920×1080 for null or missing settings; other source kinds remain explicit errors.

Transform order is crop -> signed source-axis flips -> cardinal rotation ->
absolute canvas-axis scale or bounds fit. A 6×4 source rotated90°, scale(-2,3)
has intrinsic rotated size4×6 and rendered size8×18: the negative X first flips
the original source horizontally, while magnitudes size the rotated output on
canvas X/Y axes. Both negative signs flip180° before rotation. Bounds fitting
uses the rotated cropped dimensions. This clarifies §6's dimension swap before
sizing; do not substitute conventional source-scale-then-rotate formulas.

Native width/height clamp to at least one pixel only after rounding; anchor and
bounds alignment use raw float sizes, including zero/subpixel scales. Oversized
crop edges clamp left/top first, then right/bottom to retain one source pixel,
without summing raw u32 edges. Finite scale magnitudes clamp to64 as specified;
resulting extents >8192 or invalid bounds/nonfinite values fail preflight. The
existing half-away-from-zero position/extent rounding remains unchanged.


### MEDIA-005 native branch implementation

Each item branch now owns queue -> videocrop -> optional source-axis flip ->
optional cardinal rotation -> compositor pad. Native video-direction nicks are
horiz/vert/180 for flips and90r/180/90l for rotation; both signed flips combine
as180 before rotation. The source bin/tee remains shared perSourceId. Crop
properties and outputpad rectangle come exclusively from `layout_item`; crop
u32 edges never reach a signed native property until normalization.

Arbitrary rotation quantizes to the nearest90°; positive45° goes clockwise to90,
315° wraps to0. One tracing warning and BackendEvent::Warning per placed item
warn users without repeating on each topology rebuild. Diagnostic tracking is
bounded by the256 current items and pruned on scene/item removal. Unsupported
blend modes remain rejected rather than silently approximated in this wave.
Zero/signed scales, small fit sizes and clamped crops remain negotiable with
minimum1×1 native output. Full graph updates still use theNULL barrier and may
briefly interrupt frames; no incremental or GPU support is claimed.
