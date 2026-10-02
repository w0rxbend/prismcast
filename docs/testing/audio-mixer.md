# Audio mixer foundation — AUDIO-001

Use `cargo run -p prismcast-ui` and the Audio Mixer panel’s **Add test tone**
button. This explicitly creates an unplaced TestPattern source with
`{"audio_test": true}` and routes it to the master bus through Core Commands.
The sine is 440 Hz, amplitude 0.5, stereo 48 kHz. At unity gain, source peak
is approximately -6.02 dBFS and RMS approximately -9.03 dBFS. The visual
test-pattern parser accepts the same opt-in setting; normal video patterns
remain silent. Visual finite `num_buffers` does not limit audio duration.

Gain, mute and solo controls send existing Core Commands. Source meters
measure after source gain/mute and before per-bus solo, so a nonsolo source
may still show its signal while being excluded from one bus. Solo affects
only buses shared with a soloed source. Explicit mute remains effective.
Source routes with empty track masks do not feed buses. **Remove test tone**
sends one atomic Core Transaction removing all routes and the source; a
referenced source causes the entire transaction to roll back.

Outputs terminate in nonplaying fakesinks in this foundation. There is no
speaker monitoring, microphone capture, encoding or recording yet. Device
input/application capture is CAPTURE-004. Balance, monitoring modes, sync
delay and filters on active tones yield typed runtime failures. Correcting
the configuration retries once on the next command revision; timer ticks
do not reopen a failed graph. Named buses are mixed, while output track
encoding/delivery remains future work.

The owner accepts up to 32 active tones and 8 buses. Native branch queues
hold at most 8 buffers or 100 ms. At maximum fanout there are 288 queues
and 544 owned request pads; observations occupy at most 40 latest slots
and one bounded terminal diagnostic. Every native bus message is dropped
after inspection, so a nonconsuming native bus cannot accumulate history.
Gain is rejected above f32::MAX / 32 to retain aggregate float headroom.
Effective signal changes rebuild through NULL and may briefly interrupt
audio; unrelated scene/name changes preserve the graph.

Meters enter the bounded actor queue using an exclusive local capability
and the reconciled snapshot revision. Reports for stale owners/revisions
and removed/disabled sources fail. The latest app meter watch holds at
most 32 sources with up to 8 finite channels each; native tones use stereo.
Silence is -120 dBFS. Observations neither clone/publish command state nor
change revision, persistence or undo/redo. GTK health and meter notifications
retain one pending wakeup per consumer and update existing widgets.

Native IPC/WebSocket meter clients explicitly subscribe to `meter`; the
existing `levels` wire shape is reused. Source filters and minimum delivery
intervals apply. Pending measurements are invalidated by configuration
events; ordinary mixer changes preserve throttle cadence. A producer’s
settings change, disable or removal retires its meter window. Sequence
lag also discards cached telemetry. OBS meter translation and GetStats
remain separate follow-ups: native post-fader RMS/peak do not supply OBS’s
distinct input peak, and audio meters do not establish output statistics.
A graph fault clears local observations; native clients see meter updates
cease until recovery, with no fabricated silence/error payload.

## Verification (2026-10-02)

`just ci` and `just deny` passed on the combined main tree. The headless
workspace suite passed 659 tests, with 12 environment-dependent tests
ignored. Cargo deny reported existing duplicate/unmatched warnings, with
advisories, bans, licenses and sources passing. No new external dependency
was introduced.

Focused fixtures cover real source gain/RMS/mute, measured per-bus solo,
unrouted/empty-track sources, 32-source slow consumption, disable/removal,
repeated stop/start, invalid processing/gain/settings, native bus failure
priority, invalid observation failure, request-pad release and callback
removal. App tests cover revocation, input shape/bounds, latest-store
capacity, source/revision races, observation invalidation, snapshot identity,
undo/redo and Core Event filtering. Real-socket native tests cover opt-in,
source filtering, latest-value throttling and disabled-source pending
measurement cancellation.

The owned-service test measures a real tone across commands and joined
restart. A private supervision fixture injects a terminal poll error after
real readings: measurements clear at the same revision, no timer retry
occurs, a later gain command restores measured output, and cancellation
joins the worker.

The GTK audio-panel display regression passed on Wayland with Cairo and
fatal GTK criticals: programmatic refresh dispatches nothing, controls
emit gain/mute/solo commands, meter refresh preserves widget identity and
slider edits, missing data clears readings, and a two-route tone deletion
executes atomically. Run separately:

```sh
GDK_BACKEND=wayland GSK_RENDERER=cairo G_DEBUG=fatal-criticals \
  cargo test -p prismcast-ui audio_controls_send_commands_and_meter_updates_preserve_widgets \
  -- --ignored --nocapture --test-threads=1
```

The actual RelmApp shell regression additionally clicks the production
Add test tone button, waits for its committed route and real measurements,
and repeats window close while both preview and audio owners are live.
Core closure must occur after their teardown and leave the meter watch empty.

```sh
GDK_BACKEND=wayland GSK_RENDERER=cairo G_DEBUG=fatal-criticals \
  cargo test -p prismcast-ui native_shell_preview_and_window_close_stop_both_owners \
  -- --ignored --nocapture --test-threads=1
```

Headless and display evidence does not establish microphone/application
capture, playback, remote servers in desktop bootstrap, recording, or
GNOME/KDE/X11 cross-desktop validation.
