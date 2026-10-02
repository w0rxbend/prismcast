# AUDIO-001: GStreamer mixer and meter foundation

Research date: 2026-10-02. Installed environment: GStreamer 1.28.2;
workspace gstreamer-rs 0.25 with v1_26, glib 0.22.10.

## Existing seams

`prismcast-core::audio` already owns named buses, source routes and per-source
volume/mute/solo/monitor/balance/sync state. Existing Commands and AudioEvents
carry configuration changes. `prismcast-audio` is a scaffold. There is no live
meter producer. `prismcast-media::AudioLevels` carries peak/rms vectors; native
wire `MeterEvent::Levels` already exists, so telemetry can be exposed later
without changing the native schema. `AppSnapshot` currently carries transient
capture state but no audio meters. Source settings are opaque JSON in core;
video TestPatternSettings currently rejects unknown keys.

## Confirmed platform behavior

Official audiomixer documentation describes timestamp-aware live mixing and
per-request-pad mute/volume. It does not resample; normalize each branch with
`audioconvert ! audioresample ! audio/x-raw,format=F32LE,rate=48000,channels=2,layout=interleaved`.
Explicit output caps prevent first-linked-source negotiation determining format.
Pad volume is restricted to 0..10. Core gain accepts any finite f32, so do not
send arbitrary converted gain into a bounded GObject property.

Source: https://gstreamer.freedesktop.org/documentation/audiomixer/audiomixer.html

The `volume` element has `volume-full-range` since 1.24, available in the target
runtime (verified with gst-inspect). It allows gains above the usual 0..10
range. Validate conversion finiteness before setting properties.

Source: https://gstreamer.freedesktop.org/documentation/volume/index.html

The `level` element posts named element messages at an explicit nanosecond
interval. Peak/rms/decay are per-channel double values in GValueArray; the
Rust extraction is `structure.get::<gst::glib::ValueArray>("peak")` followed by
`value.get::<f64>()`. Set `post-messages=true`, avoiding deprecated `message`.
Both installed inspection and a finite gst-launch probe confirm the message
shape. A 0.5-amplitude sine through gain 0.5 produced stereo peak -12.0412 dBFS,
rms -15.0515 dBFS, then EOS and successful NULL cleanup. Parse message source
identity against retained level objects instead of trusting external names.

Source: https://gstreamer.freedesktop.org/documentation/level/index.html

Dynamic unlinking requires blocked/idle pads; streaming callbacks cannot block,
mutate GTK or change pipeline state. For the first foundation, stopping and
rebuilding the complete graph transactionally is a simpler explicit tradeoff.
Retain/release mixer request pads on rollback, removal and final shutdown.

Source: https://gstreamer.freedesktop.org/documentation/application-development/advanced/pipeline-manipulation.html

## Suggested runtime-agnostic API

Keep mixer policy independent of Tokio/GTK/GStreamer in prismcast-audio. Suggested
`MixerPlan::from_config(config, active_source_ids)` computes source gain and
per-route output inclusion. Solo is per bus, following existing domain docs:
a soloed source on bus B excludes non-soloed sources on B, not unrelated buses.
Explicit mute still silences a soloed source. MonitorOnly excludes output.
TrackMask denotes output tracks, not stereo channel assignment.

A `MeterSnapshot` holds bounded source and bus entries keyed by typed IDs, each
using the existing AudioLevels vectors or fixed two-channel values. Report
post-gain/mute levels and mixed bus levels separately. Publish finite silence
(e.g. -120 dBFS); internal -infinity cannot be faithfully serialized as JSON
numbers. Validate channel shape, NaN/+infinity and stale source identity.

GstAudioMixer owns its graph and request pads exclusively on an OS thread, with
new/start/stop/reconcile/drain_events/latest_levels. Factory construction,
linking, state changes, bus Error/EOS, and rollback return typed errors. No native
graph operation runs while the thread is inside Tokio block_on. A terminal sink
constructor seam permits fakesink by default and appsink for deterministic
sample assertions. No hardware-dependent playback sink is needed for CI.

An app-authenticated audio owner follows the capture-owner capability pattern,
watching latest configuration snapshots and publishing latest-only meters through
a separate watch store. Meter updates should not clone AppState, increment
configuration revisions, write persistence or record undo. UI controls dispatch
existing Core Commands; UI meters read app telemetry. A single pending GTK
wakeup consumes the latest watch value, following existing bridge behavior.

## Limits to make explicit

Do not claim PipeWire capture; CAPTURE-004 is a separate owner/backend task. A
deterministic tone must be explicitly opt-in, not substituted for unavailable
microphones. Optional test-pattern audio needs both video and audio parsers to
accept the shared settings extension. Balance, monitor device playback, sync,
filters and independent tracks are follow-ups unless implemented by this task.
Unsupported runtime settings should report a bounded diagnostic instead of
silently pretending configuration has been applied. A single master bus
prototype must not claim that arbitrary routing works.

## Validation

Test pure gain/solo policy, malformed settings, two-source mixing, measured -6 dB
gain changes, explicit mute silence, per-bus solo, source remove/re-add, repeated
start/stop and request-pad cleanup. Bound queues, bus processing and event history.
A latest-only meter consumer must survive slow/non-consuming UI without retaining
unbounded samples. Validate owner shutdown completion on source/core removal.
A 32-source deterministic stress fixture matches PLAN Phase 5.

## Implementation follow-up

ADR-0023 retains native post-fader source peak/RMS and diagnostic bus levels.
The OBS adapter's input-volume channel representation has three multipliers
(magnitude, peak and input peak), while this foundation supplies only two
post-fader observations. Its translation remains deferred rather than
inventing the additional input measurement. Verified against the installed
obws 0.15 InputVolumeMeter type and upstream meter handling/VolumeMeter
callback contract:

- [OBS meter event handler](https://github.com/obsproject/obs-websocket/blob/master/src/eventhandler/EventHandler_Inputs.cpp)
- [OBS volume meter measurements](https://github.com/obsproject/obs-studio/blob/master/frontend/components/VolumeMeter.cpp)

Track masks are output track assignments, not stereo channel masks. Native
terminal sinks remain fakesinks, so successful metering establishes mixing
and control, not recording or physical playback. Backend implementation
uses a conservative aggregate gain bound f32::MAX / 32 and treats malformed
native measurement messages as terminal failures.
