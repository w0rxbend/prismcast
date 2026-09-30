# ADR-0005: Command/Event core API — every operation a Command, every change an Event

## Context

Prismcast's defining product goal is exceptionally rich remote control: GTK UI, CLI, web
UI, WebSocket, Unix IPC, hardware controllers, automation, and plugins must all drive the
same application (PLAN §76, §80). If any frontend mutates state directly (the OBS-style
"frontend-only state changes", which PLAN §79 deliberately rejects), frontends diverge,
undo/redo and persistence become unreliable, and remote control is perpetually
second-class.

PLAN §76 states the central invariant:

> Every user-visible operation is a Core Command. Every state change produces a Core
> Event.

The Application Core owns state, commands, events, persistence, and undo/redo (PLAN §2);
interfaces send Commands/Queries and render Events/Snapshots.

## Decision

1. **All mutations go through `Command` types** defined in the domain/core crates (e.g.
   `SetCurrentScene { scene_id }`, `SetSourceVisible { item_id, visible }`,
   `StartOutput { output_id }`, PLAN §20). There is no other write path — including for
   the GTK UI itself.
2. **All state changes are reported as strongly typed `Event`s** in a single hierarchical
   enum (`AppEvent::Scene/Audio/Output/System/…`, PLAN §58). There are no separate event
   definitions for GTK, WebSocket, IPC, or CLI; remote adapters serialize the same events.
3. **Frontends are interchangeable controllers.** GTK, CLI, web UI, WebSocket, Unix IPC,
   automation, and plugins all talk to the identical Command/Query/Event contract
   (PLAN §76). New frontends require no core changes.
4. Commands are the unit of undo/redo: reversible domain operations plus transaction
   groups (a drag gesture = one undo entry, PLAN §59); the remote API optionally supports
   transactions.
5. Reads use immutable `Arc<AppSnapshot>` shared from the core actor; control flows
   Command → core actor → domain mutation → media control actor → engine (PLAN §57).
   No giant `Arc<Mutex<AppState>>`.
6. Hotkeys and any future control surface are built on pre-existing generic commands
   (PLAN §54).

## Alternatives

- **Direct mutation from the GTK UI with a remote API bolted on (OBS-style).** Rejected:
  produces frontend-only state changes (PLAN §79), makes remote control incomplete by
  construction, and breaks undo/persistence invariants.
- **CRDT or event-sourced store.** Rejected for now: full event sourcing adds replay and
  schema-migration complexity disproportionate to the need; events here are notifications
  of committed changes, not the storage format. Persistence is snapshot-based with
  schema versioning (ADR-0008).
- **Per-frontend APIs tailored to each transport.** Rejected: duplicates validation and
  semantics across adapters; adapters must be thin serialization layers (PLAN §58).

## Consequences

- Remote control is complete by construction: anything the UI can do, IPC/WebSocket/CLI
  can do (milestone 1 explicitly tests moving a source via GTK, CLI, and WebSocket,
  PLAN §67).
- The web UI consumes `InitialStateSnapshot` + event stream rather than polling full
  state (PLAN §23); meter-type events are throttled/coalesced (PLAN §56).
- Every command is an authorization checkpoint (PLAN §24 permission model applies
  uniformly).
- Command handlers must be total and validated; malformed remote input produces typed
  errors, not partial mutation.
- Undo/redo, persistence, and automation all hang off the same command stream.

## Evidence

- PLAN.md §76 (Architectural principle — the central invariant).
- PLAN.md §20 (Remote-control architecture: Command/Query/Event; "Every frontend talks to
  this exact contract").
- PLAN.md §2, §80 (layered architecture and final architecture target).
- PLAN.md §58 (single strongly-typed event model), §57 (actor-style concurrency,
  `Arc<AppSnapshot>`), §59 (command-based undo/redo with transaction groups), §23
  (snapshot + event stream), §67 (milestone 1 multi-frontend test).

## Status

Accepted (2026-09-30)
