# ADR-0025: Canonical history commands

Status: accepted for CORE-006.

## Context

ADR-0015 established a bounded, global chronological history owned by the
application actor. Undo/redo currently enter through special actor messages,
so GTK and native controllers cannot use the canonical Command write path.
PLAN sections 59 and 76 require one history entry per grouped gesture and
interchangeable controllers of the same application API.

## Decision

Add parameterless `Command::Undo` and `Command::Redo`. The application actor
handles them before ordinary domain application. Existing handle helpers
become compatibility wrappers around permission-aware dispatch. Pure AppState
application rejects history commands because AppState owns no history; they
have no domain inverse. A history command anywhere inside an atomic Transaction
is rejected before state changes, including nested transactions. History replay
cannot itself become a new ordinary history entry.

History remains global, bounded and session-transient. Undo/redo require at
least one mutation scope (or Admin), followed by recursive authorization of
all operations in the actual inverse/forward entry. A caller cannot replay
scene changes using only audio privileges or bypass mixed-transaction scopes.
An open gesture group blocks both operations for every controller. Rejected
permission, structure, budget, open-group, empty-history and apply failures
preserve state, runtime observations and both stacks. Successful replay emits
the normal Core Events, publishes the final snapshot and notifies persistence
using the replayed action. The response label identifies `undo` or `redo`.

Publish a small immutable application HistoryStatus alongside AppSnapshot:
optional next undo/redo labels and whether a group is open. It is presentation
metadata, not persisted AppState, authorization proof or protocol domain data.
The labels remain within existing history budgets. Group begin/end refreshes
this metadata without advancing the media/state revision or clearing meters;
ordinary commits carry the latest history metadata. Availability is advisory;
the actor still validates when a command reaches it. GTK uses this metadata
for window Undo/Redo actions, buttons/tooltips and Ctrl+Z / Ctrl+Shift+Z
shortcuts. Requests racing a controller edit receive the existing typed error
and are presented through the established command-error path.

Add native `undo`/`redo` requests, command mappings, advertised capabilities and
CLI subcommands. These are additive protocol version 1 requests, so no version
bump or persisted-file schema change is needed. They return the existing
CommandApplied response or structured error. Native clients observe the same
normal replay Events and snapshots; no history payload or grant crosses the
wire. A wire history query is deferred.

Capture authorization has no inverse and is never stored or replayed. Replayed
source settings/enabled changes use the same commit invalidation as ordinary
commands: transient runtime/grants are revoked and fresh explicit authorization
is required, even if undo restores identical advisory settings. An authorization
request does not clear or restore ordinary history.

## Consequences and limits

No new dependencies or external API changes are required. Existing resource
budgets, global chronology, controller ownership and no-op behavior remain.
Destructive Add/Remove/duplicate undo and history persistence remain deferred.
Older clients discover the two new requests through get_version; their existing
requests and response shapes stay unchanged. Begin/end gesture grouping keeps
its existing local API exception; exposing grouping as canonical commands is
a separate follow-up. This decision does not add OBS-compatible undo requests.
