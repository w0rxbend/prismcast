# Current state

AUDIO-001 is implemented and validated on main (90d200b). The user explicitly
requested merging every existing worktree before implementation and working
directly on main. All 41 auxiliary worktree heads are now ancestors of main;
worktrees/archive/local edits remain preserved. Continue direct-main work
with disjoint file ownership and coordinator-only commits/pushes. See
`docs/testing/worktree-reconciliation.md`.

## Audio foundation (ADR-0023)

- Pure bounded planning and source/bus meter types in prismcast-audio. Native
  GstAudioMixer lives in prismcast-media-gst; domain/app remain native-free.
- Explicit TestPattern audio_test boolean defaults false. Real 440 Hz
  amplitude-0.5 stereo48k tones are independent of visual finite buffers.
  Named routes, source gain/mute and per-bus solo produce measured changes.
  Source meters are post-gain/mute but before bus solo.
- AudioSession in the existing preview service adapter owns a separate graph
  thread, bounded polling/callback storage and cancellation-aware reporting.
  Native request pads/handlers release through NULL teardown; joined audio
  and preview shutdown precedes Core closure.
- Exclusive app owner capability and exact reconciled revision protect meter
  ingress. Transient Core Meter Events and latest watches leave AppSnapshot
  identity/revision, persisted project and undo/redo unchanged. Graph faults
  clear observations; a later command revision retries, never a timer loop.
- Native IPC/WS reuses the existing meter schema with opt-in, source filtering
  and latest-value throttling. State events discard stale pending readings;
  mixer changes preserve minimum delivery cadence. Producer lifecycle ends
  retire meter windows. OBS translation remains deferred.
- GTK placeholder replaced with gain/mute/solo and measured peak/RMS. Add
  test tone uses AddSource/SetSourceSettings/SetAudioRoute; remove uses an
  atomic Transaction over all routes and source. High-rate watches keep one
  pending notification and retain widgets/ongoing gain edits.

## Evidence and limits

Combined final just ci: 659 tests passed, 12 environment-dependent ignored.
just deny passed with pre-existing duplicate/unmatched warnings. No new
external crates. Real native tests cover gain/RMS/mute, per-bus solo,
32-source slow consumption, lifecycle, request-pad/handler release and bus
failure priority. Eight app ingress tests cover owner/revision/bounds/filtering
and no persistence/undo/snapshot churn. Native socket tests cover meter
opt-in, source filters, coalescing and source disable invalidation.

A real service fixture injects a poll failure after native measurements:
same-revision watch clear, Failed status, no timer retry, next-command recovery
and joined shutdown pass. Wayland/Cairo/fatal-critical GTK tests verify
command signals, retained widgets, atomic two-route removal, and the actual
RelmApp Add tone → real meters → repeated-window-close flow. Detailed commands
and runtime budgets: `docs/testing/audio-mixer.md`.

Runtime budget: 32 diagnostic sources, 8 buses, stereo native processing.
Signal/routing changes rebuild through NULL and briefly interrupt audio.
Balance, monitoring playback, sync delay and filters are typed unsupported
configurations. Bus mixes terminate at nonplaying fakesinks: output track
encoding/delivery, recording and physical playback are future work. Native
clients see updates cease during graph failure rather than fabricated silence.
OBS InputVolumeMeters needs distinct input-peak measurements; GetStats needs
output runtime statistics. Desktop remote-server bootstrap also remains a
follow-up; socket tests establish transport delivery independently.

## Next work

Scope CAPTURE-004 (PipeWire audio input/output/application capture) against
the new mixer/session/capability foundation. No CAPTURE-004 YAML exists yet;
research external APIs and consent/lifecycle contracts before coding and
accept an ADR first. Extend media backends and command-authorized capture
ownership rather than substituting tones for missing microphones. Subsequent
phase work still includes full scene editing, monitoring/routing/filter
processing, recording, streaming and output graph integration.

CAPTURE-002 integrated live window preview remains unverified. Run the raw
consumer probe `actual_window_capture_consumer_frames_show_fixture_pixels`,
then integrated `actual_window_capture_preview_pixels_placement_and_shutdown`
in separate Wayland/Cairo/fatal-critical processes with --ignored --nocapture
--test-threads=1, coordinated with the user choosing the small flashing window
"Prismcast capture test target – select this window". Earlier grants selected
wrong windows. Monitor/KDE/X11 capture, unplug UX and live UI camera preview
remain unverified. Do not claim the overall application complete.
