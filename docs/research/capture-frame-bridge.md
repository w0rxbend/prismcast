# CAPTURE-002 persistent CPU bridge research

Official [appsrc](https://gstreamer.freedesktop.org/documentation/app/appsrc.html)
provides live Time-format streams, nonblocking push, max-buffers/max-bytes limits
and downstream leaky mode. GStreamer1.28/gstreamer-app0.25 support these properties.
Do not use need-data to repeat a latest frame in an unpaced loop: producer callbacks
push only genuinely captured samples. appsink max-buffers1 with dropping retains a
bounded latest sample; appsrc max-buffers1/downstream leaking prevents consumer stalls.

[GStreamer clock documentation](https://gstreamer.freedesktop.org/documentation/application-development/advanced/clocks.html)
defines running-time as clock time minus base-time. For an independent producer and
rebuilt consumer, preserve source PTS deltas while choosing the first consumer PTS
from current consumer running-time. Use a fresh epoch/segment and DISCONT on first,
missing or regressed source timestamps. Preserve buffer duration; clear DTS for raw
video. Old consumer endpoint references can briefly survive callback retirement;
NULL consumers reject pushes and cannot revive the stopped graph.

Observed GNOME portal window negotiated6144x3456 RGBA (81MiB/frame) in the prior
manual probe. Bound raw size128MiB, each axis8192, queuesoneframe; cloned GstBuffer
references share existing bytes rather than deep copying per placement. The CPU
videoconvert/compositor can still copy pixels and no zero-copy result is claimed.

Review follow-up: [GstSegment](https://gstreamer.freedesktop.org/documentation/gstreamer/gstsegment.html)
converts buffer positions into producer running-time using segment start/base;
stream-time origin is a different coordinate. The bridge now validates a TIME
sample segment with rate/applied-rate exactly1.0 and calls to_running_time before
consumer epoch rebasing. Missing segments, non-unit/reverse segments, and a present
PTS outside its segment fail explicitly. Missing PTS still starts a DISCONT epoch
using current consumer running-time. Capture segments with other rates are deferred.
