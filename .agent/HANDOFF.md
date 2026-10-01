# Current state

Phase 0 complete; Phase 1 skeleton complete including the GTK shell. Phase 2
(media prototype) is next and is blocked on GStreamer dev packages.
344 tests green, fmt/clippy/deny clean.

## Completed (all)

- BOOT-001..004, RES-001..007, ARCH-000..007 (see git log / BACKLOG.yaml)
- CORE-001..004: prismcast-app actor/dispatcher/broadcaster/undo + persistence
  (XDG layout, atomic writes, .bak recovery, schema v1, debounced actor)
- IPC-001/002: UDS server + prismcast-cli
- WS-001: WebSocket transport (shared session machinery with IPC, token auth,
  disabled by default)
- UI-001: GTK4/Relm4/libadwaita shell (gtk4 0.11/relm4 0.11/adw 0.9) —
  scenes/sources/outputs panels driven purely by commands + events + snapshots

## Architecture decisions (recent)

- GTK↔tokio bridge: commands via `AsyncComponentSender::oneshot_command`
  (tokio sync primitives are executor-agnostic); events via pump task →
  relm4::Sender → snapshot reads (crates/prismcast-ui/src/bridge.rs, app.rs).
- Remote session logic is generic over FrameReader/FrameWriter; IPC
  (length-prefixed MessagePack) and WS (JSON text) share one loop.
- Persistence: envelope structs + retained-document TOML; unknown fields
  survive everywhere; dirty-classification covers all 49 commands.

## Tests

`cargo test --workspace`: 344 pass. `just ci` green. GUI not yet smoke-tested
on a real display.

## Known issues / blockers

- MEDIA-001 blocked: need `libgstreamer1.0-dev libgstreamer-plugins-base-dev`
  (+video/audio dev). Runtime 1.28.2 already installed.
- Follow-ups in STATE.yaml open_questions (PersistenceRecovered event,
  SaveProject command, encoder/service registry home, frame-limit mismatch).

## Exact next task

After gstreamer dev packages: MEDIA-001 (gst init + backend crate
prismcast-media-gst) → MEDIA-002 (test pattern source) → MEDIA-003
(compositor) → MEDIA-004 (preview paintable bridge into UI-001's placeholder)
per docs/architecture/scene-graph.md. Independent of that: UI-002..004 panel
polish, CORE-005 undo refinement, WS-002 (rustls wss://).

## Recommended files to read

- .agent/STATE.yaml, .agent/JOURNAL.md
- docs/architecture/scene-graph.md (compositor mapping for MEDIA-003)
- crates/prismcast-ui/src/app.rs (UI integration point)
- crates/prismcast-remote/src/session.rs (shared transport session)

## Commands to reproduce

```bash
just ci
cargo run -p prismcast-cli -- ping        # needs a running server
cargo run -p prismcast                     # needs a display
```
