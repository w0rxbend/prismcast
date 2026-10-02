# CAPTURE-004: PipeWire audio target identity and capture

Research: 2026-10-02. Inspected host: GStreamer 1.28.2, PipeWire and its
GStreamer plugin 1.6.2, WirePlumber 0.5.13. `libpipewire-0.3.pc` is absent;
use the installed GStreamer plugin and bounded `pw-dump`, avoiding a new
native development-library requirement. No personal microphone or desktop
playback stream was opened during this research.

## Installed source properties

These values were checked with `gst-inspect-1.0 pipewiresrc`:

| Property | Installed type/default | Foundation policy |
| --- | --- | --- |
| target-object | optional string | Exact transient decimal object.serial |
| path | optional string, deprecated | Do not use global node IDs |
| stream-properties | GstStructure | Fixed typed capture and routing keys |
| autoconnect | bool, true | Connect only the explicit target |
| min-buffers | int, 1 | 2 |
| max-buffers | int, INT_MAX | 8 |
| use-bufferpool | bool; automatic defaults vary by media | false for audio |
| on-disconnect | none/eos/error, default none | error |
| provide-clock | bool, true | false; retain the mixer system clock |
| fd | int, -1 | Fresh pinned connection; retain owned FD through NULL |
| do-timestamp | bool, false | Preserve plugin timestamps |
| keepalive-time | int milliseconds, 0 | Keep disabled |
| resend-last | bool, false | Keep disabled |

The source has an ANY caps template. Downstream audio conversion and explicit
F32LE/interleaved/stereo/48 kHz caps prevent accidental video negotiation.
Property availability is checked before setting critical capture controls.

The version-matched plugin resolves target-object as object.serial or a node
name. Deprecated path supplies the global object ID. The input stream always
uses DONT_RECONNECT, with AUTOCONNECT controlled by its property. An unlinked
source can wait inside negotiation; successful state assignment alone does
not demonstrate signal flow. Therefore the owner requires genuine level
measurements within three seconds after graph startup and continues checking
each captured source's callback timestamp. A stream that stops delivering
buffers fails and requires Retry; measured silence remains valid.
[PipeWire 1.6.2 source](https://raw.githubusercontent.com/PipeWire/pipewire/1.6.2/src/gst/gstpipewiresrc.c).

## Input, monitor and one application stream

Read-only discovery classifies `Audio/Source` as input, `Audio/Sink` as output
monitor, and `Stream/Output/Audio` as one application playback stream. Capture
streams and video nodes are excluded. Application grouping and following all
streams of a desktop application are separate work.

Sink monitoring sets `stream.capture.sink=true` and targets the sink; input
and application capture set it false. This is the documented monitor-port
selection mechanism.
[PipeWire loopback documentation](https://docs.pipewire.org/page_module_loopback.html).

WirePlumber permits same-media-type links with opposite directions; it also
permits an input node to monitor the appropriate input node's monitor ports.
Application playback is an output node, so an explicit capture input can
consume it directly without routing its signal through a shared sink.
[Link eligibility policy](https://raw.githubusercontent.com/PipeWire/wireplumber/0.5.8/src/scripts/lib/linking-utils.lua).
The target direction changes to input only for sink-monitor capture.
[Direction policy](https://raw.githubusercontent.com/PipeWire/wireplumber/0.5.8/src/scripts/lib/common-utils.lua).
Numeric target.object resolves object.serial; node.dont-fallback prevents
an unavailable explicit target becoming the default input or output.
[Explicit target policy](https://raw.githubusercontent.com/PipeWire/wireplumber/0.5.8/src/scripts/linking/find-defined-target.lua).

Capture stream properties are fixed: media.type=Audio,
media.category=Capture, media.class=Stream/Input/Audio, an ID/generation-based
node.name, node.dont-fallback=true, node.dont-reconnect=true and
node.dont-move=true. Passing typed GstStructure fields is supported by the
plugin's value-to-string property copying. No user property map is forwarded.
[Stream property copying](https://raw.githubusercontent.com/PipeWire/pipewire/1.6.2/src/gst/gstpipewirestream.c).

## Identity and permission boundary

Persist strict versioned settings with an advisory node.name and explicit
input/output/application mode. Native authorization resolves the exact name
and required class to precisely one current node; absent or duplicated names
fail. Human labels, application.name and application.process.id are display
metadata, not stable selectors.

Freeze object.serial together with PipeWire Core info.cookie and the Core
request generation and the resolved local socket endpoint. Serial alone is insufficient across daemon restart:
new instances can reuse the same node name and serial. Before any signal or
grant rebuild, fresh connected sockets first pin every source to its server;
then one new inventory on that same explicit endpoint verifies every granted identity. A changed
cookie, serial, class or name rejects the old grant and requires another
explicit authorization. Restore, source enable and mixer edits never mint a
grant. Domain IDs and request generations remain distinct from PipeWire IDs.
[PipeWire identifying and routing properties](https://docs.pipewire.org/page_man_pipewire-props_7.html).

The plugin internally shares connections indexed by FD. Its -1/default
connection uses normal context connection; a supplied FD is duplicated before
connecting. A stream-properties remote.name field cannot independently choose
a server after that shared connection exists. Tests therefore run in a child
process with an isolated PipeWire remote environment, avoiding process-global
environment mutation in parallel tests.
[Connection implementation](https://raw.githubusercontent.com/PipeWire/pipewire/1.6.2/src/gst/gstpipewirecore.c).
Fresh sockets are never reused after the plugin consumes their protocol
connection. This closes the inventory-to-open race: a restart after validation
kills the old connection, rather than binding a reused serial on a new daemon.
The original FD remains alive through NULL because the plugin uses `dup`.
Socket shutdown interrupts plugin protocol waits before deliberate teardown.

Endpoint selection supports an absolute PIPEWIRE_REMOTE, or a single socket
name under absolute PIPEWIRE_RUNTIME_DIR/XDG_RUNTIME_DIR. Discovery explicitly
passes `pw-dump --remote <absolute-socket>`; connection and identity lookup
therefore cannot select different servers through client configuration defaults.
Unsupported endpoint forms fail. Unix sockets connect nonblockingly with a
250 ms deadline, including full-listen-backlog handling. A non-consuming peek
rejects known EOF on every pinned connection before any graph source starts.

Discovery starts fixed argv `pw-dump --remote <socket>`, without a shell, on a native owner or
separate discovery thread. Limits: two seconds, 2 MiB output, 128 target nodes.
Stderr is discarded. A single bounded reader is joined after child kill/reap
on timeout or overflow; no helper or reader is detached. Each rebuild reads
one inventory for all eight possible physical grants, rather than eight
successive subprocesses. Diagnostic tone and captured sources share the
existing total limit of 32 sources/eight buses.

## Isolated native evidence and test fixture

The fixture starts a private daemon/socket with protocol-native, metadata,
spa-node-factory, client-node, adapter, link-factory and access modules; it
loads no hardware enumerator. Synthetic audiotestsrc supplies Audio/Source;
support.null-audio-sink supplies Audio/Sink. WirePlumber runs its policy-only
profile, without hardware discovery. Playback test streams explicitly declare
media.type=Audio and media.category=Playback when overriding media.class.
The installed runtime accepts these classes and discovers transient serials.

The initial probe captured real stereo buffers from the synthetic input and
sink monitor; direct capture of a selected Stream/Output/Audio serial also
produced peak approximately -12.0412 dBFS. The monitor probe sets
stream.capture.sink=true; application and input probes set false. All private
children were terminated and reaped. Finite gst-launch probes can print a
buffer-removal error during deliberate NULL teardown with on-disconnect=error;
that is not a startup failure. The owner retires callbacks and flushes bus
traffic before deliberate graph teardown.

The automated private fixture passes real source, monitor and application
capture. A selected 440 Hz, amplitude 0.25 application reads approximately
-12.0412 dBFS while a louder unrelated 997 Hz, amplitude 0.5 sentinel plays on
the same sink. Sink monitoring reads above -4 dBFS; selected application capture
retains -12.0412 dBFS, proving it excludes the shared sink's unrelated stream.
Gain -6.0206 dB produces -18.0618 dBFS; mute produces the finite -120 dBFS floor.
Source removal is terminal, same-name replacement rejects the old grant, and
restored settings alone open no graph. A daemon restart recreates the same
source serial: the already-pinned old connection fails its EOF preflight; the
old cookie grant is rejected and explicit fresh authorization captures again.
The additional direct plugin probe bypasses that preflight and proves supplied
old FDs cannot fall back to the replacement daemon. All private children are
killed and reaped; the outer worker owns a private process group so a timeout
also kills daemon/policy/playback descendants. Runtime directories use mode0700.

Run the routine fixture (approximately 1.6 seconds on this host):

```sh
cargo test -p prismcast-media-gst --test audio_pipewire isolated_pipewire_audio -- --ignored --exact --nocapture
```

The extra direct plugin probe is a separate ignored test
`isolated_pipewire_audio_closed_fd_probe`; it passed with approximately 31.6
seconds total runtime. Pure regressions cover full Unix listen backlog,
missing/ambiguous/wrong-class selection, bounded discovery metadata, cookie
replacement, stale queued observations and fresh measured silence. A separate
preview service fixture exercises Core authorization through native capture,
generation-checked meters, gain/mute, enable without authorization and Retry.
Source Active follows actual level measurements, including genuine silence;
synthetic fallback readings are never supplied for a missing target.

## Practical limits

This foundation selects one currently exposed playback stream. Desktop policy
may deny access or expose no suitable nodes, and sandboxed deployments may
need additional permission plumbing. Isolated tests demonstrate native API
behavior, not microphone permissions or every desktop/sandbox configuration.
No playback, monitor device, recording track or encoded output is introduced.

The installed upstream plugin can block synchronously for approximately 30
seconds when starting a dead supplied FD. Routine EOF preflight avoids known
closed sockets, but a daemon death after the final preflight can still enter
upstream negotiation timeouts. Cancellation then waits for the native call to
return. Discovery's two-second bound and the three-second sample watchdog are
not hard bounds on every GStreamer state transition. This measured dependency
limitation remains explicit; graph sources do not reconnect to a new daemon.
