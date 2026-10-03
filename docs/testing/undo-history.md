# Controller-safe bounded undo history

This page records CORE-005 validation. CORE-006 subsequently exposed canonical
Undo/Redo Commands and GTK/native/CLI controls; see [current history verification](core-history.md)
and [ADR-0025](../adr/0025-canonical-history-commands.md). The budgets and controller
ownership below still apply.

CORE-005 preserves existing AppHandle meta APIs without changing domain,
persistent or wire schemas. Clones share AppControllerId; new_controller creates
an independent controller. Every IPC/WebSocket Session forks its application
handle. This identity owns an open group; it is not an authentication credential.

A successful mutation from another controller closes the owner's group before
recording itself, preserving chronological undo. Foreign End, invalid or
unauthorized commands, admission overflow and successful empty-event no-ops do
not close it. The owner can explicitly End after overflow. At history saturation a command
that already passed structural, payload and authorization admission is probed
on a scratch state; an empty-event no-op succeeds without touching authoritative
state/history. This exceptional path avoids duplicating domain no-op rules. New successful
mutations invalidate redo; rejected commands preserve it.

Defaults are 100 closed undo entries, 8 MiB retained command/label accounting,
256 command nodes per group/transaction, 256 UTF-8 label bytes and nesting depth
16. Effective recursion never exceeds 64 even with a larger configured value.
Node accounting includes nested transaction containers and the finalized group
wrapper. Byte accounting includes bounded serialized command payloads, Command
storage, labels and conservative entry/group framing; commas are charged per
member. This is a history admission budget, not a bound on application state or
existing domain transaction scratch copies. No giant temporary serialized buffer
is allocated. Variable-sized prior inverse payloads are validated by reference
before cloning, including JSON depth checks.

Accepted group creation reserves bookkeeping space and can evict oldest closed
history. Accepted recording evicts oldest closed history as necessary; an open
group's admitted members are never silently removed to accept another member.
Undo/redo also preflight the reverse inverse, which can differ in size from the
stored step. Authorization, resource or application failure preserves the step
and produces no state mutation/event. Large restored settings that cannot fit an
inverse reject reversible changes rather than silently losing undo coverage.

The actor authorizes every inverse operation, including nested mixed-domain
Transactions, using the caller's queued permissions. Having only scene control
cannot undo an audio edit or a mixed scene/audio group. History is globally
chronological; controller identity does not partition the undo timeline.

Validation on 2026-10-01:

```sh
cargo test -p prismcast-app -p prismcast-remote
just ci
just deny
```

Tests exercise cloned/forked controller ownership, real session construction,
foreign failed/unauthorized/no-op boundaries, repeated owner no-ops in a full
group and redo preservation, chronological undo/redo, atomic and
grouped mixed-domain permission denial/retry, redo preservation, label/member/byte
and recursive payload overflow, tight aggregate group accounting, zero-depth
grouping, oversized restored inverse payloads, and repeated failed inverse
application. Destructive restoration, canonical Undo/Redo Commands, protocol
operations and UI controls remain separate follow-ups in ADR-0015.
