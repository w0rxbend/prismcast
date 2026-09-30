# Current state

Phase 0 (research + architecture) complete. Phase 1 (skeleton) mostly complete:
workspace, domain model, command/event API, application actor, IPC server + CLI
all implemented and green (273 tests, fmt/clippy/deny clean).

## Completed

- BOOT-001..004: workspace (12 crates), tooling (justfile, deny.toml, CI), core error/ID model, agent infra
- RES-001..007: research notes in docs/research/ (OBS matrix/architecture, GStreamer, capture, encoders, browser, obs-websocket)
- ARCH-000: ADR-0001..0010 in docs/adr/
- ARCH-001/002: prismcast-core domain model + 49-variant Command + Event hierarchy + pure apply() (100 tests)
- ARCH-003/005/006: design docs — scene-graph, output-graph, persistence-model (docs/architecture/)
- ARCH-004: prismcast-media backend trait surface + mocks (19 tests)
- ARCH-007: prismcast-protocol wire types + native-protocol.md (47 tests)
- CORE-001/002/003: prismcast-app — actor, permission dispatcher, broadcaster, undo (39 tests)
- IPC-001/002: prismcast-remote UDS server + prismcast-cli (ping/status/scene list/switch) (40 tests)

## Changed

- AGENTS.md: prismcast-app added to workspace layout.

## Architecture decisions

- prismcast-* crate naming; prismcast-app is the tokio-allowed services layer; core stays pure.
- Audio: named bus matrix + TrackMask, not OBS's 6 fixed mixes.
- IPC: MessagePack (human-readable mode) length-prefixed; local socket default Admin, token auth optional.
- Undo: inverse commands; Add*/Remove* currently non-undoable (CORE-005 follow-up).

## Tests

`cargo test --workspace`: 273 pass. `just ci` green.

## Known issues / blockers

- UI-001 and MEDIA-001 blocked: missing system dev packages `libadwaita-1` and `gstreamer-1.0` (gtk4 present).
- Follow-ups recorded in STATE.yaml open_questions.

## Exact next task

CORE-004 (persistence, per docs/architecture/persistence-model.md) or WS-001
(WebSocket transport reusing prismcast-remote session machinery). After
`apt install libadwaita-1-dev libgstreamer1.0-dev libgstreamer-plugins-*-dev`:
UI-001 + MEDIA-001..005 toward the §67 milestone.

## Recommended files to read

- .agent/STATE.yaml, .agent/BACKLOG.yaml, .agent/JOURNAL.md
- docs/architecture/*.md, docs/protocols/native-protocol.md
- crates/prismcast-app/src/actor.rs (the integration point for all controllers)

## Commands to reproduce

```bash
just ci
cargo run -p prismcast-cli -- ping   # against a running server (see prismcast-remote tests)
```
