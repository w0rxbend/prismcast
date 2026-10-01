# CPU compositor verification

Run `cargo test -p prismcast-media-gst compositor`. GStreamer >=1.26 with
compositor/videotestsrc (plugins-base) and coreelements is required; no display,
audio device, hardware encoder, or network is used.

Real fakesink pad probes map negotiated RGBA frames and inspect two pixels.
Tests observe red/blue composition, positive scaling/position, hidden opacity,
z-order edits with negative input ranks, item removal, scene switching, and
repeated stop/restart. Same-source placements share one bin and tee. Each graph
change waits for fresh consecutive matching frames to avoid accepting queued
pixels from the preceding graph. Empty/no-selected scenes and disabled sources
produce ongoing black output. Metadata-only snapshot changes retain the same
bin/tee native identity.

Canvas changes verify actual output buffer dimensions and rational frame-rate
negotiation. Stop checks old tee pads, compositor request pads and leftover
pipeline children. Invalid transforms, unknown source IDs, unsupported source
kinds and invalid fps leave the existing valid graph intact. Native ERROR/EOS
messages in both orders verify error health cannot downgrade to Stopped.

This correctness prototype reconstructs topology under a NULL barrier; these
checks do not claim uninterrupted streaming, zero-copy rendering, or full
transform support. Native GTK paintable tests belong to MEDIA-004.
