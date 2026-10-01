# ADR-0016: Ephemeral portal capture leases

Status: Accepted for CAPTURE-001 foundation; controller integration follows in CAPTURE-002.

A GTK-independent prismcast-capture service uses ashpd 0.13 with only tokio and
screencast features. Each explicitly requested monitor/window authorization owns
a dedicated D-Bus connection, portal session, and PipeWire remote FD. Four leases
at most exist, including pending requests, cleanup and retained revoked FDs.
Each lease admits only one simultaneous native probe. One latest-value watch per
lease carries status; no frame data enters control channels. Dropped pending
requests/leases and broker shutdown cancel workers. Terminal status publishes
immediately; a separate typed cleanup acknowledgement completes Lease.close.
Cooperative native usage stops before voluntary Session.Close; a wedged graph
triggers bounded forced revocation, while its original FD remains retained. Authorization and cleanup have
deadlines; session closure dominates late completion. No automatic retries,
restore tokens, domain mutations, or dialogs from snapshots occur in this slice.

Dedicated connections are necessary because ashpd waits for portal response
internally before returning Request/Session: cancellation before CreateSession's
response otherwise leaves no public handle to close. Explicit Session.Close and
connection shutdown clean both known sessions and outstanding inaccessible requests.
A worker owns cleanup independently of its caller's dropped future.

Native probing borrows a live lease. Installed PipeWire 1.6.2 duplicates the provided
FD in gstpipewirecore.c with F_DUPFD_CLOEXEC; retain the original throughout graph
NULL/destruction. Portal v5 returns node IDs: use pipewiresrc deprecated `path` for
that ID, not `target-object` (numeric values there are object serials). IDs are never
persisted. Portal v6 serial support is deferred pending binding support.

Actual pixel dimensions come from negotiated sample caps, never portal coordinate
size. Probe uses bounded appsink and CPU RGBA conversion, and claims no zero-copy.
Core commands/events, compositor capture registry, UI parent-window export, persisted
restore tokens, and negotiated geometry propagation remain CAPTURE-002 work.

Session Closed subscription is acknowledged after CreateSession and before
SelectSources/Start. ashpd cannot expose the session before the create response;
a closure in that narrow subscription gap causes later portal operations to fail
and cleanup, rather than a claim of pre-creation race-free subscription.
