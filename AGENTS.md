# AGENTS.md — Prismcast

Every agent reads this file first, then `.agent/STATE.yaml`, `.agent/HANDOFF.md`, and the active task. PLAN.md is the master plan; this file is permanent project law.

## Product

Linux-only OBS-class broadcasting/recording studio. Rust, GTK4 + Relm4 + libadwaita, Wayland-first, PipeWire-first capture, GStreamer media graph, native multistreaming, remote-first control.

## Layered architecture (inviolable)

```
Interfaces (GTK UI / CLI / WebSocket / Unix IPC / Web UI / Plugins)
   -> Application Core (state, commands, events, persistence, undo)
   -> Domain interfaces
   -> Media Core (sources, filters, scene graph, mixer, encoder graph)
   -> Linux Media Platform (GStreamer, PipeWire, portal, V4L2, VAAPI/NVENC)
```

- No media logic in Relm4 components. UI only sends Commands and renders Events/Snapshots.
- `prismcast-core` (domain) must not depend on GTK, GStreamer, Tokio, or Axum.
- Dependency direction: domain <- core <- services <- UI/API. Never reversed.

## Central invariant (PLAN §76)

Every user-visible operation is a Core Command. Every state change produces a Core Event. GTK, CLI, Web UI, WebSocket, IPC are interchangeable controllers of one API.

## Code standards (PLAN §75)

- No `unwrap()`/`expect()` in production paths (except impossible invariants with a comment). Tests may unwrap.
- No blocking I/O on the Tokio runtime. No GTK access off the GTK main thread.
- No unbounded channels for media/control data. No global mutable singletons. No giant `Arc<Mutex<AppState>>` — owner/actor style, immutable `Arc<AppSnapshot>` for reads.
- Strongly-typed ID newtypes (`SceneId`, `SourceId`, `SceneItemId`, `FilterId`, `OutputId`, `EncoderId`, `ServiceId`, `AudioBusId`, `ProfileId`, `SceneCollectionId`). No raw `String` IDs.
- Typed errors via `thiserror`. Structured logging via `tracing` with ID context (`source_id=`, `output_id=`, ...).
- Do not reuse protocol structs as domain structs.
- Explicit schema versioning in persisted files; never silently discard unknown fields.

## Workspace layout

```
crates/
  prismcast-core        domain model, IDs, commands, events, errors (no tokio/GTK/GStreamer)
  prismcast-app         application core services: actor, dispatcher, broadcaster, undo, persistence (tokio allowed)
  prismcast-media       media engine abstraction traits + control actor
  prismcast-compositor  scene graph composition
  prismcast-audio       audio graph, mixer, meters
  prismcast-output      output graph (recording, streaming, multistream)
  prismcast-protocol    wire protocol types (IPC/WS), versioned
  prismcast-remote      IPC + WebSocket servers, auth
  prismcast-web         axum web UI backend
  prismcast-ui          GTK4/Relm4/libadwaita application (binary: prismcast)
  prismcast-plugin-sdk  extension traits/manifests
  prismcast-cli         studioctl-style CLI (binary: prismcast-cli)
docs/{architecture,adr,research,protocols,testing}
.agent/                 agent state (STATE.yaml, HANDOFF.md, JOURNAL.md, BACKLOG.yaml, tasks/)
```

## Commands

```bash
just fmt        # cargo fmt
just lint       # cargo clippy --workspace --all-targets -- -D warnings
just test       # cargo test --workspace
just deny       # cargo deny check
just ci         # all of the above
```

## Task workflow (PLAN §39)

1. Read AGENTS.md, `.agent/STATE.yaml`, `.agent/HANDOFF.md`.
2. Read active task from `.agent/tasks/<ID>.yaml`; check `allowed_scope` and `depends_on`.
3. Research uncertain external APIs before coding (write `docs/research/*.md`); make an ADR for architectural decisions.
4. Implement only the task scope. Run `just ci`.
5. Commit (`<type>(<TASK-ID>): summary`; WIP checkpoints allowed per PLAN §40).
6. Update `.agent/STATE.yaml`, append to `.agent/JOURNAL.md`, rewrite `.agent/HANDOFF.md`.

## Definition of Done (PLAN §41)

Implementation + unit tests (+ integration where possible) + docs + error handling + clean fmt/clippy + STATE/HANDOFF updated + commit. Feature tasks also need acceptance criteria met.

## Git / parallel agents

- Branch per task: `agent/<TASK-ID>`; integrate into `main` after validation. Use `git worktree` under `.worktrees/` when multiple agents run concurrently.
- Architecture-affecting changes require an ADR in `docs/adr/` first.
- Never commit secrets.
- Standing instruction: always commit AND push to `main` (origin = github.com:w0rxbend/prismcast). Every integration ends with `git push`.
