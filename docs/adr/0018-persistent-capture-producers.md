# ADR-0018: Persistent capture producers and rebuildable preview consumers

Status: Accepted for CAPTURE-002 CPU prototype.

One explicit Core authorization effect creates one ephemeral portal lease and one
independent native producer pipeline per SourceId/generation. A singleton bounded
capture effect receiver prevents duplicate owners or lossy broadcast authorization.
Its media owner also owns the preview compositor. Portal futures run asynchronously;
native graph changes run on the dedicated OS owner thread outside runtime block_on.
Core snapshots invalidate removed, disabled, or superseded sources; they never start
portal selection. Runtime updates carry generation and plain negotiated dimensions.

A pipewiresrc/videoconvert/appsink producer remains PLAYING across scene/placement/
canvas rebuilds. A bounded latest sample bridge feeds one rebuildable appsrc per
shared source, then the existing tee fans out placements. Streaming callbacks only
exchange bounded sample/endpoint metadata and push nonblocking buffers; they never
mutate GTK or Core. No per-placement pixel copies or zero-copy claims are introduced.
Native queues retain one frame each; raw RGBA frames are capped at 128 MiB and each
axis at8192, supporting the observed6144x3456 window (about81 MiB/frame).

Each new appsrc consumer starts a Time segment. Original producer PTS differences
are rebased to that consumer pipeline's current running time; duration is preserved.
Missing/backwards PTS reset the epoch and mark DISCONT instead of inventing a source
cadence. New consumers never inherit stale absolute producer running-time offsets.
Appsrc is live/nonblocking and bounded with downstream leaking. Actual caps/sample
width/height determine placement geometry; portal coordinate size is ignored.

Authorizing, denied, revoked or failed capture placements are skipped with bounded
recoverable diagnostics; unrelated TestPattern placements remain visible. Active
caps changes trigger geometry reconciliation and framework-free runtime dimensions.
Native consumer teardown and producer NULL/destruction precede voluntary lease close.
External revocation can precede teardown; no automatic permission retries occur.

GTK exports an opaque parent identifier locally and holds its export guard through
preview shutdown. AppHandle::authorize_source_capture attaches that identifier to the
explicit Core command's ephemeral envelope. It is neither persisted nor serialized
into protocol source settings. Remote authorization may use no parent window.

Production live capture evidence remains distinct from mock/headless tests. The
CAPTURE-001 manual GNOME window probe has yielded real6144x3456 frames; this ADR does
not claim the new integrated preview path is validated until its separate smoke test.
