# ADR-0008: Persistence model — profiles + scene collections under ~/.config, schema versioning with migrations, crash resilience

## Context

Prismcast must persist user projects across restarts, survive crashes mid-write, and let
agents evolve the file format over the project's life. OBS deliberately separates scene
collections (scenes, sources, filters, transitions, audio configuration) from profiles
(video resolution/FPS, outputs, encoders, streaming services, recording settings), and
that separation has proven useful (PLAN §19, §79 keeps both concepts). Restart-restore is
part of milestone 1 (PLAN §67), so persistence is foundational, not an afterthought.

Two failure pressures shape the design: corrupted config files (PLAN §61) and interrupted
writes (kill -9 class events; Phase 6 requires recordings to survive kill -9, PLAN §48 —
the same crash-resilience attitude applies to project state). PLAN §19 mandates explicit
schema versioning, stepwise migrations, and never silently discarding unknown fields.

## Decision

1. **Two top-level persisted concepts** (PLAN §19):
   - `SceneCollection`: scenes, sources, filters, transitions, audio configuration.
   - `Profile`: video resolution/FPS, output configuration, encoders, streaming
     services, recording settings.
2. **Filesystem layout** under `$XDG_CONFIG_HOME/prismcast/` (`~/.config/prismcast/`):

   ```
   profiles/<name>/profile.toml
   collections/<name>/collection.json
   themes/<name>/{theme.toml,style.css,assets/}
   plugins/
   ```

   Profiles are TOML (hand-editable); collections are JSON (machine-managed, golden-tested
   serialization per PLAN §63).
3. **Explicit schema versioning**: every persisted file carries `"schemaVersion": N`.
   Loading migrates stepwise (V1→V2→V3→…); migrations are pure, tested functions.
   **Unknown fields are preserved**, never silently discarded (PLAN §19) — forward
   compatibility for older builds opening newer files is reported, not guessed at.
4. **Crash resilience**: writes are atomic (write to temp file in the same directory +
   fsync + rename); the last good file is never truncated in place. A corrupted primary
   file falls back to the previous good copy with a typed error surfaced to the user
   (PLAN §61 "config file corrupted").
5. **Persistence is owned by a dedicated actor** (persistent-state actor, PLAN §57);
   saving is triggered by the command stream, never directly from the UI.
6. Portal restore tokens and `pipewire-serial` identifiers are stored with source
   configuration so capture sources survive restarts (ADR-0003, PLAN §36).

## Alternatives

- **Single monolithic project file (OBS legacy style).** Rejected: couples scene content
  to output/encoder settings, which PLAN §19 explicitly separates.
- **SQLite database.** Rejected: opaque to users and agents, harder to diff/review in an
  agent-driven repo, and complicates golden testing; the data volume does not justify it.
- **Event-sourced journal as the store.** Rejected (see ADR-0005): replay and migration
  complexity disproportionate to need; snapshots + versioning suffice.
- **Silent best-effort parsing with defaults for unknown fields.** Rejected: violates
  PLAN §19; data loss bugs hide for months.

## Consequences

- Every schema change requires: version bump, a migration function, migration tests, and
  updated golden files.
- Domain IDs (`ProfileId`, `SceneCollectionId`, source/scene IDs) must remain stable
  across persistence, IPC, and WebSocket (PLAN §3).
- Users can hand-edit profiles and check collections into version control.
- The crash-resilience rules (atomic write, fallback, typed corruption error) are
  acceptance-testable.

## Evidence

- PLAN.md §19 (Project persistence: SceneCollection/Profile split, `~/.config/<app>/`
  layout, `schemaVersion`, V1→V2→… migrations, "Never silently discard unknown fields").
- PLAN.md §27 (theme package layout), §36 (restore token stored with source
  configuration), §48/§61 (crash resilience and corrupted-config failure case),
  §57 (persistent-state actor), §63 (golden serialization tests), §67 (restart-restore in
  milestone 1).

## Status

Accepted (2026-09-30)
