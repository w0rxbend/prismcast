# ADR-0017: Explicit capture commands and transient runtime state

Status: accepted for CAPTURE-002.

AuthorizeSourceCapture is the only command that requests a portal picker, including
explicit retries. PipeWireDisplay and PipeWireWindow sources must exist and be
enabled. Authorization effects are rejected inside atomic Transactions and have
no undo inverse. Restoration, enabling and scene changes never open a picker.

The application actor owns a transient SourceRuntime map in AppSnapshot, separate
from persisted AppState and source settings. CaptureGeneration is a monotonically
allocated token. Runtime status, sanitized bounded diagnostics and actual negotiated
pixel dimensions produce Source RuntimeChanged Events and snapshots, without undo
recording or persistence notification. No portal grant, FD or node is serialized.

A single trusted capture owner attaches through an actor RPC. It receives requests
through a bounded channel and latest snapshots through watch. Missing, disconnected
or full request receivers reject authorization before publishing Authorizing. The
owner reports through an opaque capability and the actor checks both owner identity
and current source generation. Source removal, disable and settings changes clear
runtime; late completions cannot revive it. Owner Drop is observed through a separate
oneshot liveness signal, so cleanup does not depend on a full actor message queue.
Disconnected active requests become recoverable Failed status.

The media owner cancels a request/lease when a watched source is removed, disabled
or its generation changes. Watch coalescing is safe because cancellation derives
from current state, not a possibly dropped edge notification. Retry creates a new
generation; asynchronous old completions fail validation. Active requires actual
negotiated nonzero pixel dimensions, bounded to the compositor's 8192-pixel axes.

GTK exports its parent locally and passes the bounded opaque identifier as ephemeral
application-envelope context through authorize_source_capture. The Core Command and
wire request contain SourceId only. Per-request parent context is immutable and never
logged or persisted. IPC/WebSocket can explicitly authorize with no local parent.

Protocol changes add the explicit request and runtime state/event mapping; live
permission tokens remain out of wire settings. Existing undo metadata API exceptions
remain as ADR-0015 describes. Native producer/lease lifecycle is ADR-0018's responsibility.
