# PipeWire audio capture (CAPTURE-004)

ADR-0024 defines command-authorized audio capture. Source settings contain
`schema_version: 1`, `target` (the advisory PipeWire node name) and `mode`
(`input`, `output` or `application`). Input/output modes use
PipeWireAudioInput; application mode uses PipeWireAppAudio. An application
target represents one current playback stream, not every stream owned by
an application process.

Runtime prerequisites are the GStreamer `pipewiresrc` plugin, the `pw-dump`
utility and a PipeWire session manager with compatible target-linking policy.
Discovery has a two-second deadline, a 2 MiB JSON byte cap and at most 128
audio targets. Missing tools/server, malformed metadata, ambiguity and quota
exhaustion fail explicitly. Discovery creates no capture stream and runs
outside GTK and Tokio. The native grant also binds the daemon cookie, so
rebuild validation detects a restart even when node serials are reused. Each
graph uses fresh connected Unix socket FDs, with post-connect identity checks
against that same endpoint, retained until native teardown. A disconnect
cannot reconnect to a replacement daemon by reusing its serial.

Adding a source, restoring a project and enabling it do not open a device.
Start capture sends AuthorizeSourceCapture. The audio owner resolves the
frozen settings to one current node and retains its serial only in memory.
Actual measurements activate the runtime. Settings changes, disable/removal,
owner loss and terminal failures revoke capture; Start/Retry is required to
open it again. Gain/routing rebuilds retain authorization only for the same
native identity. Capture uses the existing ControlScenes permission, as do
other AuthorizeSourceCapture requests; meter subscriptions require Read.
Invalidated capture stops before resolving more queued targets. Native source
watchdogs treat three seconds without a buffer measurement as terminal;
the service adds a five-second first-data backstop. Genuine silence produces
measurements and does not expire. An application pause that stops buffer
delivery can require Retry when playback resumes.

The owner checks connected sockets for EOF immediately before native Playing.
The installed PipeWire GStreamer plugin can nevertheless block a synchronous
startup for approximately 30 seconds if its daemon dies after that final
check. Cancellation waits for this native call to return; the two-second
state-settlement deadline does not bound the call itself. The direct
closed-FD probe confirmed this upstream behavior. Normal measured capture
and joined shutdown complete promptly in the isolated regressions.

Audio Active runtime dimensions are absent. Video Active runtimes still
require negotiated pixel dimensions. Meter events retain the existing wire
schema and remain transient, without persistence or undo history changes.
Audio runtime reports are generation-bound; native meter reports also carry
the exact current snapshot revision. Ordinary controllers cannot manufacture
owner capabilities or submit measurements.

The shared runtime table retains at most eight observations. New admissions
may atomically retire the oldest terminal observation; eight live captures
reject another admission. Mixer limits remain 32 total sources/eight buses,
with stereo 48 kHz normalization and bounded branch queues/latest readings.

## Validation evidence

Final combined `just ci` passed: 679 tests passed, 18 environment-dependent
tests ignored; formatting and workspace all-target Clippy with warnings denied
are clean. `just deny` passed with existing duplicate/unmatched warnings.
`socket2` is now a direct capture dependency but was already in Cargo.lock;
no new external package was added.

Evidence is separated by boundary:

| Check | Result |
| --- | --- |
| Nine app capture tests | Frozen effects, permissions, modes/settings, generation/revision meters, invalidation, family isolation and atomic runtime capacity passed |
| Native pure tests | Bounded metadata/selection, serial/cookie replacement, full Unix backlog deadline, stale callbacks and genuine silence passed |
| Five owner supervision tests | No implicit capture, measured activation, explicit failure/timeout retry, stale resolver completion, revocation before blocked discovery and concurrent fault-time edits passed |
| Real native private fixture | Input, sink monitor, selected application, louder unrelated sentinel exclusion, gain/mute, removal/replacement and reused-serial daemon restart passed in about 1.56 seconds |
| Direct supplied-old-FD plugin probe | Old connection failed rather than capturing the replacement daemon; upstream startup timeout measured, about 31.6 seconds total |
| Real Core/AudioSession private fixture | Explicit authorization to real finite stereo meters, measured gain/mute, disable/enable without reopening, new-generation retry and joined shutdown passed on final code in about 1.25 seconds |
| Native WebSocket test | Read-only authorization denied without effects; audio Active dimensions null, generation-bound meters and failed-capture pending-meter invalidation passed |
| Wayland GTK tests | Picker selection/error/no-target states, separate enable/start, no refresh feedback, atomic route/source removal and actual tone/meter/repeated-close application flow passed with Cairo and fatal criticals |

Injected measurements in socket tests establish transport and authorization
contracts; they are not evidence of physical capture. The two real capture
fixtures open only synthetic nodes on a private hardware-free daemon. Their
test workers use separate environments and private process groups; timeout
cleanup terminates fixture descendants.

Run the normal native and service fixtures independently:

```sh
cargo test -p prismcast-media-gst --test audio_pipewire isolated_pipewire_audio -- --ignored --exact --nocapture
cargo test -p prismcast-preview --test audio_pipewire actual_pipewire_audio_session_authorization_measurements_invalidation_and_shutdown -- --ignored --exact --nocapture --test-threads=1
```

The additional slow dependency probe is opt-in:

```sh
cargo test -p prismcast-media-gst --test audio_pipewire isolated_pipewire_audio_closed_fd_probe -- --ignored --exact --nocapture
```

Run GTK tests in separate real-display processes, with `GDK_BACKEND=wayland`,
`GSK_RENDERER=cairo` and `G_DEBUG=fatal-criticals`, using `--ignored --nocapture
--test-threads=1`. Test filters are
`audio_picker_handles_missing_targets_and_submits_selected_mode_without_starting_capture`,
`audio_capture_controls_separate_enable_start_and_atomic_removal`, and
`native_shell_preview_and_window_close_stop_both_owners`.

## Remaining platform evidence

Isolated PipeWire tests do not establish microphone hardware, desktop policy,
Bluetooth channel changes, GNOME/KDE sandbox permissions or Flatpak behavior.
Live personal devices are not needed for the deterministic fixture. Selecting
a sink captures its monitor; selecting an application captures one playback
stream. No fallback to a default microphone/sink is permitted when a target
disappears. Multi-stream application grouping remains later work.

Monitoring/playback, encoded output tracks and OBS pre-fader input peak
measurements remain outside this task. Bus output still ends in nonplaying
fakesinks. One native source failure currently stops the shared audio graph
and revokes all physical grants; each needs explicit retry. A graph rebuild
may briefly interrupt audio. Video/camera capture
limits and the pending coordinated window-picker validation remain unchanged.
