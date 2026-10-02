# ADR-0024: Command-authorized PipeWire audio capture

Status: Accepted (CAPTURE-004, 2026-10-02)

The audio mixer currently supplies diagnostic tones. Physical inputs, sink
monitors and application playback streams need explicit capture ownership;
persisted source settings cannot constitute permission to open them.

Reuse AuthorizeSourceCapture and transient SourceRuntime generations. Route
audio requests to a bounded receiver on the exclusive AudioOwner; the video
CaptureOwner retains its existing scope. Audio runtime reports carry no video
dimensions. The application validates owner family, generation and source
kind. Native audio meter ingress additionally checks the active generation
and exact reconciled revision. Diagnostic-tone ingress remains separate.
Owner loss fails only that owner's source family and invalidates observations.

Introduce framework-free, versioned PipeWireAudioSettings with a bounded
advisory node.name target and input/output/application mode. PipeWireAudioInput
supports input and output-monitor modes; PipeWireAppAudio supports application
mode. Reject unsupported schema versions, unknown fields and kind/mode
mismatches at authorization. Preserve the existing wire source kinds and
runtime shape. Read-only discovery follows the camera discovery precedent,
outside persisted Core state and without opening capture streams.

Freeze settings in each command effect. On the native audio thread resolve
the selected name and matching media class to exactly one object.serial;
missing or ambiguous targets fail explicitly. Serials and grants stay local
and transient. GStreamer pipewiresrc targets the selected serial; only sink
monitor mode sets stream.capture.sink. Disable reconnection/default fallback
and bound native buffers. Revalidate the exact granted identity before graph
rebuilds; never silently rebind to a replacement object with the same name.
Native discovery/process boundaries have output and time limits.

AudioSession retains an explicit allowlist of granted targets. Restore,
enable, routing, gain changes and unrelated snapshots create no grant.
Settings changes, disable/removal and superseding generations revoke grants.
Active is reported only after actual measurements. Terminal native failures
stop the graph, clear measurements and revoke physical grants; reopening
requires another authorization command. Diagnostic tones retain their
existing next-command recovery behavior. Shutdown tears down the native
graph and joins its owner before Core closure.

Keep the shared capture-runtime budget at eight entries and the mixer budget
at 32 total sources/eight buses. Admission at capacity may retire the oldest
terminal runtime observation atomically; never evict Authorizing or Active
entries. Failed admission changes no state and emits no effect.

GTK offers asynchronous target selection, command-driven source creation and
routing, separate Start/Retry capture controls, runtime diagnostics and atomic
route/source removal. It performs no media operations. Native measurements
flow through existing meter Events and native subscriptions. Playback,
monitoring, output encoding and multi-stream grouping by application identity
remain later work; application capture selects one current playback stream.

Validate consent, stale generations, invalidation and failure through injected
service seams. Separately prove real PipeWire input, monitor and application
stream behavior in an isolated daemon/session-manager fixture without opening
personal devices. Hardware, desktop-policy and sandbox evidence must remain
distinct from isolated/headless coverage.
