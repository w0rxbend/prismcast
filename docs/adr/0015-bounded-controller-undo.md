# ADR-0015: Bounded undo history and controller ownership

Status: accepted for CORE-005. The temporary meta-API exception and deferred
canonical/controller integration below are superseded by
[ADR-0025](0025-canonical-history-commands.md); the ownership and budgets remain.

The application actor owns chronological undo/redo history. Handle clones share a
local typed controller identity; `new_controller` explicitly forks that identity.
Each remote session forks its handle, so unrelated clients cannot append to or
close another controller's group. A successful foreign command closes the prior
group before recording itself. Rejected commands and successful no-ops leave groups and redo unchanged.

History has explicit entry, retained-byte, group-member, label and nesting limits.
Before state application the actor validates command structure and prepares an
inverse within the byte budget. A counting serializer writes to a bounded sink,
never to a giant temporary JSON buffer. Variable-sized inverse payloads are
measured by reference before cloning; atomic transactions prepare inverses against
successive scratch states. Open groups reject overflow rather than evict members;
explicit close remains possible. Closed history evicts oldest entries as needed.

Undo and redo retain caller permissions in the queued actor message and authorize
the complete inverse recursively before application. Failed authorization,
preflight or application preserves the original history entry. History remains a
single global chronological timeline, not separate per-controller timelines.

Existing handle meta APIs remain available as the established temporary exception
to canonical Commands. Canonical Undo/Redo Commands are a follow-up. Adding protocol/UI undo operations and
destructive snapshot restoration is deferred; persistent and wire schemas do not
change. Controller identity is local caller context, not authentication proof.
