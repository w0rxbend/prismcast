# Current state

Fresh repository. Only PLAN.md (the master plan), AGENTS.md (project rules), and the `.agent/` infrastructure exist. No code yet.

## Completed

- SKILL-001: 30 shared project-local development skills installed; see `.agents/SKILLS.md`. Metadata/resource validation and `just ci` passed.

- Git repository initialized on `main`.
- AGENTS.md written (architecture, code standards, workflow, DoD).
- `.agent/` infrastructure created (STATE.yaml, BACKLOG.yaml, JOURNAL.md, DECISIONS.md, tasks/).

## Changed

- n/a

## Architecture decisions

- Crate naming follows the `prismcast-*` list at the end of PLAN.md (supersedes the earlier `studio-*` sketch).
- ADRs ADR-0001..0010 to be authored from PLAN.md (see DECISIONS.md).

## Tests

- n/a

## Known issues

- None yet.

## Exact next task

Wave 1 (parallel): BOOT-001/002/003 (workspace + tooling), RES-001..007 (research notes), ADR authoring.
Wave 2 (after BOOT-001): ARCH-001 domain model, ARCH-002 command/event API in `prismcast-core`.

## Recommended files to read

- PLAN.md (§30 layout, §38 roles, §66 task DAG, §75 standards)
- AGENTS.md
- .agent/BACKLOG.yaml

## Commands to reproduce

```bash
just ci
```
