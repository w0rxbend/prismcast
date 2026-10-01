# ADR-0019: Lease-free V4L2 camera capture

Status: accepted for CAPTURE-003.

## Context

CAPTURE-002 established explicit capture authorization for portal-based
PipeWireDisplay and PipeWireWindow sources (ADR-0017) and persistent native
producers feeding rebuildable preview consumers (ADR-0018). CAPTURE-003 adds
V4L2 cameras. Cameras need no portal: there is no picker, session, lease, FD or
node grant, and no parent window. The design question is how a lease-free source
kind fits the explicit command/runtime contract without weakening its guarantees.

## Decision

AuthorizeSourceCapture is reused unchanged as an explicit open-device effect for
SourceKind::V4l2Camera (crates/prismcast-core/src/source.rs:65), which already
exists with the settings convention {"device": "/dev/video0"}. Authorization
stays user-initiated, generation-guarded through the existing transient
SourceRuntime (crates/prismcast-core/src/capture.rs), and cancellable; it is
never replayed by snapshot restore, undo, transactions or scene changes. The
kind gates are extended, not replaced: core state admission
(crates/prismcast-core/src/state.rs), the app actor report gate
(crates/prismcast-app/src/actor.rs authorize_capture) and the preview capture
owner's valid()/authorize() (crates/prismcast-preview/src/capture_owner.rs)
admit the new kind. No portal session, lease, FD, node grant or parent window is
involved; no new Core Command or Event is added.

Persisted identity is only the validated device path in source settings:
absolute, under /dev/, at most 255 bytes, no control characters. Snapshot
restore never opens a device; a restored camera source simply has no runtime,
exactly like a restored portal source.

Device discovery is a read-only system query outside Core state, following the
encoder-registry precedent (EncoderRegistry::probe in
crates/prismcast-media/src/encoder.rs): gst::DeviceMonitor with class filter
"Video/Source" enumerates devices with bounded metadata (device path, display
name) and reports add/remove changes. Discovery publishes snapshots through a
bounded watch channel for the UI picker; it never mutates Core state, never
emits Core Events, and never authorizes anything. Wire-exposed device listing is
explicitly deferred as a protocol schema change follow-up.

Native open and lifecycle run on the dedicated media owner OS thread per
ADR-0018, never on Tokio workers. One persistent v4l2src producer per SourceId
reuses FrameProducer::start(gst::Element) and CaptureFeed
(crates/prismcast-capture/src/producer.rs) plus the kind-agnostic
GstCompositor::sync_capture_feeds (crates/prismcast-media-gst/src/compositor.rs);
the producer survives compositor rebuilds. Negotiated native caps drive runtime
dimensions within the existing 8192-pixel-axis and 128 MiB CPU frame bounds.
Source removal, disable, device settings change and shutdown stop the native
producer and release the device; stale generations cannot revive capture.

Errors map to the existing status vocabulary through pre-open node checks,
preferred over parsing GStreamer error text: a missing node (ENOENT) maps to
Failed with a device-missing message, permission denied (EACCES) to Denied, a
busy node (EBUSY) to Failed with a device-busy message, and mid-capture loss (a
producer bus error after Active) to Revoked. If string or quark matching proves
unreliable, typed CaptureError variants (for example DeviceMissing/DeviceBusy)
are added in prismcast-capture rather than deepening heuristics. There are no
automatic reopen loops; retry is explicit and creates a new generation.

CaptureStatus itself is untouched (Authorizing, Active, Cancelled, Denied,
Revoked, Failed); no protocol change. The UI may present a kind-aware label such
as "Opening camera…" in presentation only. Hotplug scope is deliberately narrow:
the UI picker refreshes on add/remove, and disconnect of an active device maps
to Revoked. There is no auto-switching to another device.

## Consequences

Cameras go through the same explicit, generation-guarded Core contract as portal
capture, so remote controllers can authorize them without any local parent
context, and all controllers observe identical runtime statuses. Restored
projects never grab a camera implicitly; the user re-authorizes explicitly. The
device path is not a stable identity (kernel node numbering changes across
reboot and replug), so re-authorization after topology change may require
re-selecting in the picker; this is accepted as predictable behavior rather than
hidden reopen logic. Discovery results are UI-local until the protocol follow-up
exposes them. Mock/headless lifecycle and error-mapping tests gate CI; real
camera or v4l2loopback evidence is opt-in and recorded separately
(docs/research/v4l2-gst-device-monitor.md).

## Alternatives considered

Camera capture through PipeWire nodes and pipewiresrc (the OBS 30.1 model, and
the mandatory path under the Camera portal for a sandboxed Flatpak) was
considered and deferred: it unifies the source model with screen capture but
exposes fewer camera controls, less deterministic format selection, and an extra
mediation layer (RES-004 §3.2). Direct v4l2src is the host-install answer;
PipeWire camera capture remains a separate future source path. Automatic reopen
on disconnect or device reappearance was rejected as user-hostile surprise
device grabbing. Persisting a stable udev identity (serial, vendor/product)
instead of the device path was deferred; bounded path validation is sufficient
for this slice, and stable identity is a follow-up if replug confusion proves
real.
