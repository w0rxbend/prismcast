# CORE-006: native history controller contracts

Research: 2026-10-03. This task changes local application contracts, with no new
external API or dependency. The authoritative sources are the repository's
accepted [ADR-0025](../adr/0025-canonical-history-commands.md), application actor,
native session mapping and CLI client. Undo/redo remain global chronological
history operations rather than controller-specific stacks.

## Canonical write path

[Core Command](../../crates/prismcast-core/src/command.rs) has parameterless
Undo and Redo. The [application actor](../../crates/prismcast-app/src/actor.rs)
owns bounded history and checks the caller's permissions for the actual inverse
or forward action before replay. Actor compatibility helpers delegate to the
canonical command path. Pure domain state application cannot replay history;
it rejects a history command anywhere inside an atomic transaction before
changing its scratch state.

[Native RequestKind](../../crates/prismcast-protocol/src/request.rs) mirrors
these two unit variants as `undo` and `redo`. The
[interface mapper](../../crates/prismcast-remote/src/map.rs) maps them to Core
commands, advertises both in get_version and rejects history transaction
members. The [shared session](../../crates/prismcast-remote/src/session.rs)
already calls `dispatch_with_permissions` for every native mutation. Both Unix
and WebSocket transports therefore use the actor's same authorization and
replay boundary. Best-effort native request batches keep their established
independent-request semantics; atomicity remains an explicit transaction.

This is additive protocol version 1. Existing mutation success is
ResponseData::Empty, with envelope request_type echoing the request tag.
CommandResponse labels are application data and do not imply a new wire result
variant. Replayed actions emit their ordinary Scene/Source/Audio/etc Events.
Neither application HistoryStatus metadata nor labels/stacks/grants enter the
native state snapshot. No wire history query was added.

## Permissions and rejection

Undo/redo require at least one mutation scope or Admin, followed by recursive
permission checks over the entry's actual operations. Read-only access cannot
replay. Audio-only access cannot undo a scene rename. Scene-only access cannot
undo a transaction combining scene rename and mute. These checks apply equally
to Redo. Denials preserve domain state, revision, presentation history and both
stacks. Later trusted replay verifies that denied entries were not consumed.

An empty stack or an open gesture group produces the actor's InvalidInput,
mapped by the existing native error table to invalid_field 400. Permission
denial maps to forbidden 800. Structural native transaction rejection retains
the established invalid_request 100 error policy used for nested transactions
and query members. Wire transactions cannot include Undo/Redo or nested
transactions; rejection precedes any earlier member's state mutation.

Authorization effects are never inverse commands. Restoring advisory source
settings or enabled state through history revokes current capture and cannot
restore a native grant. Source control and history share the canonical commit
invalidation path, owned and tested in the application layer.

## CLI behavior

[The CLI](../../crates/prismcast-cli/src/lib.rs) uses its existing IPC/WebSocket
client selection and authentication path. `undo`/`redo` accept no positional
payload; each sends exactly its matching RequestKind. Human output identifies
the applied operation; `--json` returns the existing empty mutation data.
Unexpected success payloads are transport/protocol failures. Server rejection
exits 2 with the existing typed error summary on stderr, connection failure
exits 1, and success exits 0. No history or media logic runs inside the CLI.

## Deterministic acceptance evidence

[Remote history integration tests](../../crates/prismcast-remote/tests/history.rs)
drive real Unix sockets and loopback WebSockets against real actors:

- Rename, Undo and Redo each return the expected request_type and empty data;
  normal renamed Events have consecutive sequence numbers and match snapshots.
- Read-only, wrong-scope and mixed-scope denial covers both Undo and Redo,
  preserving state/revision/history and leaving trusted replay possible.
- Empty Undo/Redo, open groups and direct/nested atomic history membership fail
  without state changes; an ordinary subsequent Undo still reaches its entry.

[CLI subprocess tests](../../crates/prismcast-cli/tests/cli.rs) run the actual
binary against a real Unix server, covering human/JSON success, resulting scene
names, empty stacks, both replay permission denials, argument rejection and
unchanged history. Existing request mirror tests pin 52 commands, and native
capability tests pin 65 sorted unique request tags at protocol version 1.

Focused commands:

```sh
cargo test -p prismcast-protocol
cargo test -p prismcast-remote --test history
cargo test -p prismcast-cli --test cli
cargo clippy -p prismcast-protocol -p prismcast-remote -p prismcast-cli --all-targets -- -D warnings
```

Destructive history and persisted history remain separate work. GTK availability
uses transient HistoryStatus metadata; race-time errors still come from the
actor. No OBS-compatible history request was introduced.
