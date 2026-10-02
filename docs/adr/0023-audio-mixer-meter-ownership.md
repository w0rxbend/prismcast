# ADR-0023: Audio mixer and meter ownership

Status: Accepted (AUDIO-001, 2026-10-02)

The empty audio crate and absent meter producer prevent the existing audio
commands and native meter subscription from controlling a real signal.

Keep pure bounded mixer planning and source-meter data in prismcast-audio.
Implement GstAudioMixer in prismcast-media-gst, preserving ADR-0011 platform
placement. Extend the existing service adapter prismcast-preview with an
independent AudioSession owner thread; it receives application snapshots,
reconciles the native graph and forwards observations through an opaque local
application capability. GTK starts/stops the service and renders data only.
No domain dependency on GStreamer, GTK or Tokio is introduced.

TestPattern gains an explicit audio_test boolean, false by default. Enabled
test tones produce 440 Hz sine audio at amplitude 0.5, normalized to stereo
48 kHz raw audio. Persisted explicit routes select named buses; sources without
routes have source meters but do not enter any bus. Track masks remain output
assignments; this foundation mixes each named bus to a nonplaying fakesink,
with no claim of encoder/track delivery. Per-source gain and mute precede
source metering; solo gates contributions separately per bus. Monitoring
playback, balance, delay and filters require later graph implementations.

Runtime budgets are 32 sources, 8 active buses, stereo channels, bounded
GStreamer branch queues and latest-only observations. Unsupported graph
configuration produces typed failures, not silently ignored processing.
Graph rebuilds use a NULL barrier and release all request pads and handlers.
Gain is checked before conversion or GObject property assignment. Native
callbacks never access GTK, mutate the graph or wait for the application.

Application ingress uses an exclusive owner capability and the revision of
the snapshot reconciled by that owner. Reports after configuration changes,
source removal/disable or owner replacement are rejected. Meter events are
transient Core Events: they use broadcaster sequencing and category/source
filtering, but do not commit an AppState revision, alter undo/persistence, or
clone the full state for every sample. Owner-authenticated invalidation clears
the observation watch after a graph failure without fabricating a signal.
Terminal native failures stop the graph and retry only on a later command
revision. A latest-only app meter watch serves
GTK without unbounded wakeups; the existing native wire shape is reused.
Silence is clamped to -120 dBFS so JSON payloads stay finite.

AudioSession cancellation tears down the graph, revokes the reporting
capability and joins the thread before Core shutdown. Deterministic native
tone measurements and lifecycle tests establish the foundation independently
of microphone/device permissions; PipeWire capture remains CAPTURE-004.
