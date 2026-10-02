# Current state

CAPTURE-004 is implemented and validated on main (6150504). Continue directly
on main: the user explicitly requested merging all worktree histories first
and direct-main swarm development. All 41 auxiliary worktree heads remain
ancestors of main; existing worktrees/local edits remain preserved. Assign
agents disjoint files; coordinator alone stages, commits and pushes.

## PipeWire audio ownership (ADR-0024)

- Strict core PipeWireAudioSettings: schema_version=1, bounded advisory
  node.name target and input/output/application mode. PipeWireAudioInput
  covers input and sink monitor; PipeWireAppAudio selects one playback stream.
  Unknown fields/versions and kind/mode mismatches reject authorization.
- AuthorizeSourceCapture freezes settings and routes audio effects into a
  bounded AudioOwner request receiver. Video CaptureOwner remains isolated.
  Existing ControlScenes permission applies; native meters require Read.
- AudioRuntimeHandle.report_capture uses generation/status/diagnostic with
  dimensions=None. report_capture_levels requires active generation and exact
  reconciled revision. Tone report_levels cannot impersonate physical capture.
  Runtime/status events remain transient and grants never enter persisted state.
- Shared runtime table caps at eight entries; new admission may atomically
  retire the oldest terminal observation. Never evict live entries. Failed
  admission changes no state/effect. Owner/receiver loss fails only its family.
- capture::audio resolves exact class/name to transient serial, daemon cookie
  and absolute endpoint. Discovery uses fixed pw-dump --remote argv, two-second
  deadline, 2 MiB output and 128 targets; reader/child cleanup is joined.
- connect_authorized_audio_sources opens fresh owned sockets, verifies the
  entire grant set after connecting against that same endpoint and gives FDs
  to pipewiresrc. Retained originals pin the daemon epoch through native NULL.
  Full Unix backlog waits are bounded at 250 ms. No fallback/reconnection or
  same-name/serial rebinding occurs. Fresh sockets are used for every rebuild.
- GstAudioMixer.reconcile_authorized retains the existing pure mixer planning,
  stereo48k gain/mute/per-bus solo, bounded queues/latest observations and
  32-total-source/eight-bus limits. Ordinary reconcile opens only explicit tones.
  EOF preflight precedes Playing; deliberate teardown flushes callbacks and
  shuts down protocol sockets before NULL to wake native waits.
- AudioSession holds only explicit grants, stops invalidated graphs before
  slow discovery, checks late resolver completion/cancellation, activates only
  after measurements, and handles concurrent edits during failure cleanup.
  Failure/stall revokes physical grants and clears meters; Retry is explicit.
  Three-second source freshness watchdog rejects stale queued observations;
  five-second service backstop handles missing first data. Real silence counts.
- GTK has async microphone/system/application picker, separate Add/Start/Retry,
  Enabled control, diagnostics and atomic all-route removal. Enable never
  authorizes. Source creation validates first and compensates config failure
  through another Core Command. No GTK media logic was introduced.

## Verification and limits

Final just ci passed: 679 tests passed, 18 environment-dependent ignored;
format and all-target Clippy clean. just deny passed separately with existing
warnings. socket2 became a direct capture dependency but was already locked;
no new external package was added. See docs/testing/pipewire-audio.md and
research/capture-004-pipewire-audio.md for official sources and exact commands.

Nine app and five supervision tests cover consent, generation/revision,
family isolation, bounded admission, delayed resolver/revocation and concurrent
fault cleanup. Native WebSocket test proves permission rejection, audio Active
null dimensions, meter generation and pending failure invalidation.

Real private native fixture passed input, sink monitor and application signal,
unrelated louder-sentinel exclusion, gain/mute, target removal/replacement,
daemon restart with reused serial, closed old socket and fresh authorization
(~1.56s). Final real Core/AudioSession fixture passed actual capture, finite
stereo meters, gain/mute, disable/enable without automatic reopening, new
Retry generation and joined shutdown (~1.25s). Private subprocess environments,
mode0700 runtime directories and process-group cleanup open no hardware.

Separate Wayland/Cairo/fatal-critical GTK tests passed picker/error/no-target
states, enable/start command separation, atomic removal and actual tone/live
meter/repeated-close application flow. Existing AUDIO-001 foundation and
native meter source filtering/throttling remain intact (ADR-0023).

An additional supplied-old-FD native probe passed: replacement capture cannot
occur, but upstream pipewiresrc can block synchronous startup about 30 seconds
(~31.6s fixture total). EOF preflight avoids known dead sockets; daemon death
after that last check can still delay cancellation until native return. The
state-settlement deadline does not bound every synchronous plugin call.

Physical microphones, desktop/session-manager policies and Flatpak permissions
remain unverified. Application capture selects one current playback stream.
A pause which stops buffers can trigger the conservative freshness watchdog
and require Retry; one failure currently revokes all physical audio grants.
Monitoring/playback, balance, sync delay, filters and encoded tracks remain
unsupported; bus outputs end in nonplaying fakesinks. OBS pre-fader input peak
and output statistics remain separate follow-ups. Desktop remote-server
bootstrap also remains absent; transport tests establish delivery separately.
Do not claim the overall broadcasting application complete.

## Exact next task

Read .agent/tasks/CORE-006.yaml: canonical Undo/Redo Core Commands with GTK,
native socket and CLI integration. This closes the central command invariant
and PLAN phase-4 scene-editor gap before larger audio/output work. Existing
actor history methods and CORE-005 limits are the starting point. Research
permission/group/replay semantics and accept an ADR before implementation.
Capture authorization must never be replayed; history changes to source
settings/enable revoke grants and require explicit new authorization.

Read core command/state, app actor/dispatch/undo, native protocol request and
coverage tests, remote mapping/session and GTK actions. Preserve history
limits, transaction-controller ownership and permission checks for the actual
inverse/forward action. Destructive undo and persisted history are separate.

## Other pending evidence

CAPTURE-002 integrated live window preview is still unverified: earlier picker
grants selected the wrong windows. Run raw
actual_window_capture_consumer_frames_show_fixture_pixels, then integrated
actual_window_capture_preview_pixels_placement_and_shutdown in separate
Wayland/Cairo/fatal-critical processes with --ignored --nocapture
--test-threads=1, coordinated with the user choosing the small flashing window
"Prismcast capture test target – select this window". Monitor/KDE/X11 capture,
USB unplug UX and live UI camera preview remain unverified. Camera paths can
renumber; stable identity and wire-exposed discovery are follow-ups.
