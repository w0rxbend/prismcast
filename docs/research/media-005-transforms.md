# MEDIA-005 CPU transform research

Read upstream [videocrop](https://gstreamer.freedesktop.org/documentation/videocrop/videocrop.html)
and [videoflip](https://gstreamer.freedesktop.org/documentation/videofilter/videoflip.html)
and inspected installed GStreamer 1.28.2 element properties before implementation.
Both accept SystemMemory RGBA. videocrop properties are signed integers;
normalized nonnegative edges must be set explicitly so native negotiation never
sees a zero-size frame. videoflip's deprecated method property is superseded by
video-direction, with native enum nicks identity/90r/180/90l/horiz/vert.

Each placement owns bounded queue -> videocrop -> optional source-axis flip ->
optional cardinal rotation -> compositor request pad. Both signed axes flipped
use180 orientation; one flip uses horiz/vert. Crop happens first. Cardinal
orientation swaps cropped width/height before layout scaling. Backend and UI
must use a single pure layout function for the exact rounded output rectangle.
Nearest-cardinal arbitrary rotation follows existing scene-graph policy and
emits one bounded diagnostic per item per owner lifetime (cleared at source/scene
removal). Native tests must compare asymmetric images, not merely solid colors.

Production graph changes retain the existing NULL barrier and typed preflight
validation. New placement elements are removed after request pads are released.
No GPU/zero-copy or arbitrary-angle support is implied.
