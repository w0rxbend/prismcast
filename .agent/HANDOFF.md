# Current state

The native preview milestone is implemented and validated with dynamic agent orchestration. GStreamer development libraries are installed at 1.28.2. The user stopped a previous overlapping agent; its uncommitted edits remain preserved on `archive/paused-agent-phase2` (2ce3390) and in the named stash. Do not reapply its alternate backend API wholesale.

## Completed in this wave

- MEDIA-001: isolated GStreamer initialization/capabilities and lifecycle checks; ADR-0011 sets GStreamer >=1.26, bindings 0.25/GLib 0.22, Rust MSRV 1.93.
- MEDIA-002: validated test-pattern backend and reusable RGBA source bin, bounded events, lifecycle/error/settings tests.
- MEDIA-003: CPU compositor with shared source bins, bounded placement queues, basic move/resize/z-order/visibility, black empty canvas, cleanup and rendered-pixel tests.
- MEDIA-004: prismcast-preview GTK-local paintable adapter and dedicated media owner, command-driven snapshots, static GTK sink, visible health/failure recovery and joined media shutdown before core shutdown. ADR-0013 documents ownership and GTK >=4.14 floor; MPL-2.0 dependency is explicit in deny policy.
- UI-003: scene selection/rename/removal/reorder Commands, labeled focused rename editor, boundary sensitivity, confirmation before non-undoable removal (also Delete).
- UI-004: shared source registry versus placements, show/lock/remove/rename/create/place, partial failure recovery and current checkbox values for rapid toggles.
- BRIDGE-001: entered Tokio startup context, committed-snapshot watches, independent bounded/coalesced root/panel wakeups, synchronous restoration guards and owned task teardown.

## Validation

Final combined `just ci` passed: 367 tests passed, four display tests ignored by the headless suite. `just deny` passed. Separate real-display tests cover downloaded native paintable pixels, unsupported-source failure/recovery, actual RelmApp frame delivery and repeated production window close, rapid source toggles, and scene dialogs. Reproduction/evidence: docs/testing/native-preview.md, source-toggle-signals.md, ui-scene-list.md.

## Next work and limits

UI-002 and MEDIA-005 are now ready; author scoped task specs before implementing. Native preview is attached, but preview editing overlays/input tools remain UI-002 work. Basic position/scale/z-order work; advanced transforms remain MEDIA-005. Prototype topology changes rebuild behind a NULL barrier and can briefly interrupt frames. Only TestPattern sources are supported; other kinds report an error. GPU acceleration, capture, audio, recording and streaming are later phases and are not validated by this wave.

Run the application with `cargo run -p prismcast-ui --bin prismcast`. Agent worktrees remain for review. No remote push was requested. Earlier core/protocol open questions remain in STATE.yaml.
