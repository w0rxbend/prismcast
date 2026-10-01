# CAPTURE-003 V4L2 camera discovery and capture research

2026-10-02 (baseline probed 2026-10-01, re-verified); gstreamer-rs 0.25 with the
workspace's v1_26 feature, native GStreamer 1.28.2 (Ubuntu gst-plugins-good).
Narrows RES-004 §3 (docs/research/linux-capture.md) to the camera slice.

GstDeviceMonitor is the discovery mechanism. gst::DeviceMonitor in gstreamer-rs
0.25 wraps gst_device_monitor; add_filter with the class string "Video/Source"
restricts enumeration to video capture devices. The video4linux2 plugin ships
v4l2deviceprovider ("Video (video4linux2) Device Provider", verified via
gst-inspect-1.0 video4linux2), so V4L2 nodes appear as GstDevice objects whose
properties carry "device.path" (the /dev/videoN node), "display-name", and
"device.api" = "v4l2". The api property matters: PipeWire camera nodes are also
advertised under Video/Source by the PipeWire provider, and "device.api"
distinguishes kernel V4L2 nodes from them. One-shot enumeration uses devices()
for a snapshot without starting the monitor. Hotplug uses add_watch/start():
the monitor's bus then emits DeviceAdded/DeviceRemoved messages, each carrying
the GstDevice. gst-device-monitor-1.0 Video/Source demonstrates the same class
filter and is a quick manual probe.

v4l2src opens a node selected by its `device` string property (default
"/dev/video0"); device-name and device-fd are read-only introspection available
after open. The src pad template spans video/x-raw (including
memory:DMABuf/DMA_DRM), image/jpeg, video/x-h264/h265, bayer and more, but the
caps actually offered are native to the opened device: query pad caps after open
and negotiate downstream, never hardcode formats. io-mode defaults to auto;
picture controls (brightness, contrast, saturation, hue, extra-controls) are
per-device runtime properties. Open failure fails the NULL-to-READY state change
and posts a bus ERROR in the typed gst::ResourceError domain: NotFound for a
missing node, Busy when another process holds the device (EBUSY), open/permission
errors otherwise. Mid-capture unplug surfaces later as a bus ERROR from buffer
dequeue failure, after the pipeline was already running. Classification should
prefer pre-open node checks (existence, access mode) and the typed ResourceError
quark over parsing human-readable error text.

Validation machine evidence (2026-10-01, re-confirmed 2026-10-02):
gst-inspect-1.0 v4l2src reports the element installed at rank primary from
plugin video4linux2 1.28.2; ls /dev/video* matches nothing; neither v4l2loopback
nor uvcvideo kernel modules are loaded; gst-device-monitor-1.0 Video/Source
enumerates no devices (only the libcamera provider logs). Hardware evidence
therefore requires a physical webcam or `sudo modprobe v4l2loopback`; mock and
headless lifecycle tests remain the CI baseline, with any real-camera or
loopback probe recorded separately per the task's validation rule.

Live hardware findings (2026-10-02, Anker PowerConf C200, uvcvideo):
- Direct capture works: `v4l2src device=/dev/video2 num-buffers=5 ! fakesink`
  streamed 5 buffers cleanly with no other process holding the node.
- UVC cameras expose sibling metadata nodes: `/dev/video3` fails with "not a
  capture device" (and `/dev/video1`, pre-renumber, with "Cannot identify
  device"). Enumeration must tolerate non-capture nodes; explicit open maps
  them to a typed failure.
- Nodes renumber across replug: the pair moved from video1/video2 to
  video2/video3 within minutes. Persisted device paths are therefore advisory;
  a restored path may point at a missing or wrong device and must surface as
  recoverable "device missing", never auto-opened.
- Even with hardware present, `gst-device-monitor-1.0 Video/Source` listed the
  camera only through the PipeWire and libcamera providers; the GStreamer
  v4l2deviceprovider produced no entries. DeviceMonitor discovery alone cannot
  be trusted to see every camera; enumeration needs a /dev/video* fallback
  scan (with capture-capability tolerance) or multi-provider aggregation.
- A transient "Device failed during initialization / Internal data stream
  error" was observed while the camera re-enumerated mid-probe; retry after
  re-enumeration succeeded. Busy/ unplug races are real and must map to typed,
  retryable statuses.

Verified installed element details (gst-inspect-1.0 v4l2src, GStreamer 1.28.2):
`device` is readable/writable string, default "/dev/video0"; `device-fd` is a
read-only integer, -1 until open; `device-name` is a read-only string; io-mode
enum offers auto, rw, mmap, userptr, dmabuf, dmabuf-import. APIs used are within
the workspace's declared gstreamer v1_26 feature floor.

Official references:
- https://gstreamer.freedesktop.org/documentation/video4linux2/v4l2src.html
- https://gstreamer.freedesktop.org/documentation/gstreamer/gstdevicemonitor.html
- https://docs.rs/gstreamer/0.25/gstreamer/struct.DeviceMonitor.html
