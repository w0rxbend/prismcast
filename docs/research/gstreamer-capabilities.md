# RES-003 — GStreamer capability research

Date: 2026-09-30. Task: `.agent/tasks/RES-003.yaml`. Maps to PLAN.md §4 (media engine), §5 (preview/rendering), §11–14 (outputs, encoding, recording, replay buffer).

Method: upstream documentation and release notes (primary sources), crates.io metadata, and local verification against the installed GStreamer 1.28.2 (`gst-inspect-1.0`, Ubuntu resolute packages). Anything marked "local check" was verified on this machine; runtime availability of hardware/protocol elements is per-system and must be probed at startup.

## Version landscape (as of 2026-09-30)

| Component | Current stable | In development | Notes |
|---|---|---|---|
| GStreamer core/plugins | 1.28 series, latest 1.28.7 (2026-09-07); 1.28.0 released 2026-01-27 | 1.29.x → 1.30, likely Q4 2026 | [1.28 release notes](https://gstreamer.freedesktop.org/releases/1.28/); [1.26 notes](https://gstreamer.freedesktop.org/releases/1.26/) (1.26.0: 2025-03-11); [1.24 notes](https://gstreamer.freedesktop.org/releases/1.24/) |
| gstreamer-rs bindings | 0.25.x (0.25.4, 2026-09-21), MSRV 1.92 | 0.26.0-alpha on main | [crates.io gstreamer versions](https://crates.io/crates/gstreamer/versions); 1.28 notes: "the latest release of the bindings (0.24) has already been updated for the new GStreamer 1.28 APIs, and works with any GStreamer version starting from 1.14" |
| gst-plugins-rs (Rust plugins) | 0.15.x (0.15.4, 2026-09-22), MSRV 1.92 | 0.16.0-alpha on main | [gst-plugins-rs repo](https://github.com/GStreamer/gst-plugins-rs); crates.io versions for [gst-plugin-isobmff](https://crates.io/crates/gst-plugin-isobmff/versions), [gst-plugin-webrtc](https://crates.io/crates/gst-plugin-webrtc/versions) |
| gst-plugin-gtk4 | 0.15.x (0.15.2, 2026-05-11); Ubuntu ships 0.14.4 | — | [crates.io gst-plugin-gtk4](https://crates.io/crates/gst-plugin-gtk4/versions) |
| gtk4-rs (UI side) | 0.11.x (0.11.5, 2026-09-20), gtk-rs-core 0.22 | 0.12.0-alpha | [crates.io gtk4](https://crates.io/crates/gtk4/versions) |
| PipeWire (provides pipewiresrc) | 1.6.x (1.6.8, 2026-07); Ubuntu ships 1.6.2 | — | [openSUSE package changelog](https://www.rpmfind.net/linux/RPM/opensuse/16.1/ppc64le/gstreamer-plugin-pipewire-1.6.8-160099.1.1.ppc64le.html) |

Version alignment matters: gst-plugins-rs 0.15 pins gtk4 0.11 / gtk-rs-core 0.22 ([gst-plugins-rs 0.15 Cargo.toml](https://raw.githubusercontent.com/GStreamer/gst-plugins-rs/0.15/Cargo.toml)), so an in-process GTK4 app embedding gst-plugin-gtk4's paintable must use gtk4-rs 0.11.x. gstreamer-rs works with any system GStreamer ≥ 1.14 and exposes newer APIs via feature flags (`v1_24`, `v1_28`, ...) — see the [gstreamer crate docs](https://docs.rs/gstreamer/latest/gstreamer/).

## 1. gstreamer-rs binding status

- The bindings are the official, maintained safe API for both application code and writing new elements in Rust ([docs.rs/gstreamer](https://docs.rs/gstreamer/latest/gstreamer/), [gstreamer-rs repo](https://gitlab.freedesktop.org/gstreamer/gstreamer-rs)). Licensed MIT/Apache-2.0.
- Current stable crate series is 0.25 (first release 2026-02-20, latest 0.25.4 2026-09-21), MSRV Rust 1.92; the previous 0.24 series (MSRV 1.83) already covered GStreamer 1.28 APIs ([crates.io API data](https://crates.io/api/v1/crates/gstreamer/versions), [1.28 release notes](https://gstreamer.freedesktop.org/releases/1.28/)). Our `rust-toolchain.toml` tracks `stable`, so MSRV 1.92 is not a constraint.
- System requirement is GStreamer ≥ 1.14 at build/runtime; per-version API is gated behind cargo features (`v1_22`, `v1_24`, `v1_26`, `v1_28`). We should pin features to our declared minimum runtime version (proposal: `v1_26` minimum, detect 1.28 features at runtime).
- Sub-crates we will need: `gstreamer`, `gstreamer-base`, `gstreamer-video`, `gstreamer-audio`, `gstreamer-app`, `gstreamer-pbutils`, `gstreamer-allocators` (DMA-BUF/udmabuf), `gstreamer-gl` + `gstreamer-gl-wayland`/`gstreamer-gl-egl` (GL interop for preview).
- Plugins can be written in Rust via `gst-plugin-*` crates (cdylib or statically registered rlib) — this is how we would implement Prismcast-specific elements (scene graph bridge, stats) without C.
- Local check (GStreamer 1.28.2, Ubuntu): all core/base/good/bad elements referenced below exist except where noted; `gst-plugins-rs` elements (isobmff, rswebrtc, gopbuffer) and NVENC elements are **not** present in Ubuntu's default packages — see §8.

## 2. Video compositors

All three general-purpose compositors expose the same scene-item-shaped pad properties — verified locally via `gst-inspect-1.0`: `xpos`, `ypos`, `width`, `height`, `alpha`, `zorder` (plus `operator`, `sizing-policy` on `compositor`/`glvideomixer`). This maps 1:1 onto PLAN §8 scene items.

| Element | Plugin / module | Memory types (sink) | Strengths | Limits / risks |
|---|---|---|---|---|
| `compositor` | gst-plugins-base ([docs](https://gstreamer.freedesktop.org/documentation/compositor/index.html)) | SystemMemory only (video/x-raw, huge format list incl. 10/12/16-bit and float) | CPU-only, always available, live mixing via GstVideoAggregator, per-pad `operator` (source/over/add), `sizing-policy=keep-aspect-ratio`, `background` (checker/black/white/transparent), `max-threads` | Every input is downloaded to system memory — breaks the zero-copy goal when inputs are DMA-BUF/GL; scaling is CPU. Fine as correctness-first default (PLAN §5), not the final path. 1.28 had a `force-live` segfault regression, fixed in 1.28.x ([1.28 notes](https://gstreamer.freedesktop.org/releases/1.28/)) |
| `glvideomixer` | gst-plugins-bad OpenGL ([docs](https://gstreamer.freedesktop.org/documentation/opengl/glvideomixer.html)) | GLMemory, DMABuf (`DMA_DRM`), SystemMemory, GLTextureUploadMeta | GPU compositing, accepts DMA-BUF directly, same pad geometry properties, blend constants | Requires a GstGL context shared with the rest of the pipeline and with the preview sink; EGL/Wayland setup complexity; GL debug surface is worse than CPU. Historically the standard zero-copy mixer choice |
| `vacompositor` | gst-plugins-bad `va` ([docs](https://gstreamer.freedesktop.org/documentation/va/vacompositor.html)) | VAMemory + SystemMemory (NV12/I420/P010/RGBA…) | VPP-accelerated compose+scale, output stays in VAMemory → feeds `vah264enc`/`vah265enc` without copies | Rank `none` (never autoplugged; must be explicitly selected). Only Intel/AMD VA-API; per-pad geometry exists (docs example shows `sink_1::xpos/width/alpha`), but no blend operators and format set is limited. `interpolation-method` since 1.26; background-color property only in 1.30 dev ([1.29 notes](https://gstreamer.freedesktop.org/releases/1.29/)) |
| `cudacompositor` | gst-plugins-bad `nvcodec` ([docs](https://gstreamer.freedesktop.org/documentation/nvcodec/index.html)) | CUDAMemory | NVIDIA-only path pairing with `nv*enc`; gained crop-meta support in 1.28 | CUDA context lifecycle; only useful when NVENC is also used |

Notes:
- `va` is the only supported VA-API plugin: gstreamer-vaapi was deprecated in 1.26 and **removed in 1.28** ([1.26 notes](https://gstreamer.freedesktop.org/releases/1.26/), [1.28 notes](https://gstreamer.freedesktop.org/releases/1.28/)). The full `va` element roster (decoders, encoders, `vapostproc`, `vadeinterlace`, `vacompositor`) is listed in the [va plugin docs](https://gstreamer.freedesktop.org/documentation/va/index.html).
- There is no Vulkan compositor element yet (Vulkan work in 1.26–1.28 is decoders/encoders and `vulkansink`; PLAN's "Vulkan composition" bullet remains custom-element territory).
- 1.28 added a shared thread-pool `GstContext` "for video conversion and compositing" resource sharing ([1.28 notes](https://gstreamer.freedesktop.org/releases/1.28/)) — worth adopting for CPU compositing performance.

## 3. gtk4paintablesink and the DMA-BUF preview path

[gst-plugin-gtk4 docs](https://gstreamer.freedesktop.org/documentation/gtk4/index.html) (element `gtk4paintablesink`, Rust, in gst-plugins-rs as `gst-plugin-gtk4`):

- Provides a `GdkPaintable` (read-only `paintable` property) → render in `gtk::Picture`/`gtk::Video`-style widgets. This is exactly the PLAN §5 phase-1 preview path.
- Sink pad templates (verified against docs): `video/x-raw(memory:DMABuf, format=DMA_DRM)`, `video/x-raw(memory:GLMemory)` (RGBA/RGB), `video/x-raw` system memory (packed RGB formats), each optionally with `GstVideoOverlayComposition` meta.
- GL texture rendering needs the crate built with `wayland`, `x11glx` or `x11egl` cargo features. **Direct DMA-BUF rendering needs GTK ≥ 4.14 and the crate's `dmabuf` cargo feature** (`dmabuf = ["gst-allocators", "gtk_v4_14", "gst-video/v1_24"]` — [video/gtk4 Cargo.toml](https://raw.githubusercontent.com/GStreamer/gst-plugins-rs/main/video/gtk4/Cargo.toml)). Direct dmabuf import with GTK 4.14 landed in GStreamer 1.24 ([1.24 notes](https://gstreamer.freedesktop.org/releases/1.24/)).
- 1.28 additions: YCbCr memory texture formats, better color-state fallbacks, and the sink now **proposes a udmabuf buffer pool/allocator upstream** when upstream asks for system memory — letting software-decoded/system-memory sources be imported by GL/Vulkan/compositor without copies ([1.28 notes](https://gstreamer.freedesktop.org/releases/1.28/)). `glupload` gained the same udmabuf uploader.
- The paintable object exposes `background-color`, `force-aspect-ratio`, `scaling-filter`, `orientation`, and an injectable `gl-context` (construct-only) — the last one is our hook to share the app's GDK GL context with the GStreamer GL pipeline.
- Ubuntu packages it as `gstreamer1.0-gtk4` (0.14.4 locally). For guaranteed features we should depend on the `gst-plugin-gtk4` crate (0.15.x) and statically register the plugin in-process, keeping paintable type identity with our gtk4-rs 0.11 app.
- Caveat from the skill file, confirmed by docs: "DMABuf support" ≠ guaranteed zero-copy — the actual number of copies depends on negotiated allocator/pool and GTK renderer. Must be measured per PLAN §12 benchmarking requirement.

## 4. pipewiresrc

`pipewiresrc`/`pipewiresink` ship with **PipeWire itself** (package `gstreamer1.0-pipewire` / `pipewire-gstreamer`), not with GStreamer — see [PipeWire README](https://gitlab.freedesktop.org/pipewire/pipewire). This is the ingest element for xdg-desktop-portal ScreenCast streams (connect via `fd` + node path) and for camera/audio nodes.

Properties verified locally (PipeWire 1.6.2): `fd`, `path` (deprecated string form), `client-name`, `client-properties`, `stream-properties`, `autoconnect`, `do-timestamp`, `keepalive-time`, `resend-last`, `min-buffers`/`max-buffers`, `provide-clock`, `on-disconnect` (enum none/eos/error), `automatic-eos`. Src caps are `ANY`; format is negotiated with the PipeWire node (DMA-BUF modifiers negotiated since PipeWire matured; GStreamer side gained explicit-modifier `DMA_DRM` negotiation in 1.24, [1.24 notes](https://gstreamer.freedesktop.org/releases/1.24/)).

Known caveats (upstream issues):
- Resolution/format changes of the remote stream are historically not handled cleanly: [PipeWire #3147](https://gitlab.freedesktop.org/pipewire/pipewire/-/issues/3147). Prismcast must handle renegotiation robustly.
- Non-monotonic timestamps with keepalive enabled: [PipeWire #3149](https://gitlab.freedesktop.org/pipewire/pipewire/-/issues/3149) — relevant to `keepalive-time` use for screen capture.
- `provide-clock=true` exposes the PipeWire stream clock as pipeline clock; recent PipeWire releases re-enabled this (see [Arun Raghavan's PipeWire/GStreamer notes](https://arunraghavan.net/feed/) and [Collabora's hackfest writeup](https://www.collabora.com/news-and-blog/blog/2024/06/05/hacking-on-the-pipewire-gstreamer-elements/)).

## 5. Hardware encoders

### NVIDIA — `nvcodec` plugin (gst-plugins-bad)

[Element roster](https://gstreamer.freedesktop.org/documentation/nvcodec/index.html): `nvh264enc`, `nvh265enc`, `nvav1enc` (CUDA mode), `nvautogpu{h264,h265,av1}enc` (auto GPU select), plus decoders, `cudaconvert`/`cudascale`, `cudacompositor`, `cudaipc{sink,src}`. Requires NVIDIA GPU + CUDA at runtime; elements do not register otherwise (local check on this CUDA-less machine: all `nv*` missing — probe the registry at startup, don't assume). 1.28 added `num-slices` (device-dependent), `emit-frame-stats`/`frame-stats` signal (per-frame QP monitoring — useful for our stats pipeline), and interlace handling improvements ([1.28 notes](https://gstreamer.freedesktop.org/releases/1.28/)).

### VA-API — `va` plugin (gst-plugins-bad), Intel/AMD

Encoders: `vah264enc`, `vah264lpenc` (low-power), `vah265enc`, `vah265lpenc`, `vaav1enc` (since 1.24, [1.24 notes](https://gstreamer.freedesktop.org/releases/1.24/)), `vajpegenc`, `vavp8enc`. AV1 enc registration depends on driver support (local check: `vaav1enc` absent despite `va` plugin present). `vah264enc` properties verified locally (1.28.2): `rate-control`, `bitrate`, `cpb-size`, `key-int-max`, `b-frames`, `b-pyramid`, `target-usage`, `min/max-qp`, `qos`, `min-force-key-unit-interval`, `cc-insert` — a superset of what OBS's VA-API path exposes; force-key-unit events are supported (needed for replay buffer and split recording).

### Vulkan video

1.28 added Vulkan Video AV1/VP9 decode and **H.264 encode** (`vulkanh264enc` family), still young; treat as experimental alternative, not a primary backend ([1.28 notes](https://gstreamer.freedesktop.org/releases/1.28/)).

### Software fallback

`x264enc` (gpl), `x265enc` (gpl), `svtav1enc` (since 1.24), `vp9enc` — all present locally. `openh264enc` as non-GPL H.264 fallback.

## 6. Muxers and recording plumbing

| Need (PLAN §13) | Element | Status |
|---|---|---|
| MKV (multi-track, crash-safe) | `matroskamux` (gst-plugins-good) | Request pads `audio_%u`/`video_%u`/`subtitle_%u` — unlimited audio tracks (local check). `streamable=true` writes no index/duration → survives crashes; AAC/Opus/FLAC/raw PCM and H.264/H.265/AV1/VP9 accepted. 1.28.4 fixed ReferenceBlock writing for non-keyframes ([1.28 notes](https://gstreamer.freedesktop.org/releases/1.28/)) |
| MP4 | `mp4mux` (gst-plugins-good `isomp4`), or Rust `isomp4mux` | `mp4mux` gained E-AC3 in 1.28; Rust `isomp4mux` gained caps changes and raw audio ([1.28 notes](https://gstreamer.freedesktop.org/releases/1.28/)) |
| Fragmented MP4 | Rust `isofmp4mux` (+ `cmafmux`, `dashmp4mux`, `onviffmp4mux`) | In gst-plugins-rs: 1.28 merged the `fmp4` and `mp4` Rust plugins into a single `isobmff` plugin ([1.28 notes](https://gstreamer.freedesktop.org/releases/1.28/), [isobmff docs](https://gstreamer.freedesktop.org/documentation/isobmff/index.html), [fmp4 docs](https://gstreamer.freedesktop.org/documentation/fmp4/index.html)). Features: manual-split via serialized events, `send-force-keyunit`, split-at-running-time (1.26, [1.26 notes](https://gstreamer.freedesktop.org/releases/1.26/)); fragment-splitting fixes through 1.28.4. **Not packaged by Ubuntu** (local check: absent) |
| Split by duration/size | `splitmuxsink` (gst-plugins-good) | `max-size-time`, `max-size-bytes`, `max-size-timecode`, `send-keyframe-requests`, pluggable `muxer`/`muxer-factory`, `async-finalize` (verified locally) |
| Replay buffer (PLAN §14) | custom `Ring Buffer<EncodedPacket>` or `gopbuffer` | `gopbuffer` (gst-plugins-rs `generic/gopbuffer`, since ~2022, MPL-2.0) buffers the last GOP(s) of an encoded stream ([source](https://github.com/GStreamer/gst-plugins-rs/tree/main/generic/gopbuffer)); 1.28 added H.266 support. Our per-output ring buffer with memory/duration limits still needs to be our own element — `gopbuffer` is GOP-quantized and a good reference implementation |
| Remux MKV→MP4 | `matroskademux` ! `mp4mux` transmux pipeline | trivially expressible; 1.28 matroskademux handles 4K+ uncompressed blocks ([1.28 notes](https://gstreamer.freedesktop.org/releases/1.28/)) |

EOS discipline: fragmented and non-fragmented MP4 muxers must receive EOS to finalize (or use `qtmoovrecover`); this matches the skill guidance to wait for muxer finalization with a deadline.

## 7. Streaming sinks

| Protocol | Element | Plugin / status |
|---|---|---|
| RTMP(S) | `rtmp2sink` (sink caps `video/x-flv`, feed from `flvmux`) | gst-plugins-bad `rtmp2` ([docs](https://gstreamer.freedesktop.org/documentation/rtmp2/rtmp2sink.html)). Properties: `async-connect`, `chunk-size`, `peak-kbps` (pacing), `stats` structure (bytes/acks), `stop-commands`. Old librtmp `rtmpsink`/`rtmpsrc` **deprecated in 1.28, removal scheduled next cycle** ([1.28 notes](https://gstreamer.freedesktop.org/releases/1.28/)) |
| Enhanced RTMP (H.265/AV1, multitrack) | `eflvmux` ! `rtmp2sink` | New in 1.28: FLV H.265 + multitrack per Enhanced RTMP v2 spec ([1.28 notes](https://gstreamer.freedesktop.org/releases/1.28/)) |
| SRT | `srtsink` (ANY caps; feed `mpegtsmux`) | gst-plugins-bad `srt` ([docs](https://gstreamer.freedesktop.org/documentation/srt/srtsink.html)). Caller/listener/rendezvous modes, `latency` (default 125 ms), `passphrase`/`pbkeylen` encryption, `streamid`, `auto-reconnect` (default true), `wait-for-connection`, `stats` structure, caller accept/reject signals. `connection-key` port sharing is **1.30 (dev only)** — do not rely on it |
| WHIP | `whipclientsink` (webrtcsink-based, WHIP client signaller) | gst-plugins-rs `rswebrtc` ([docs](https://gstreamer.freedesktop.org/documentation/rswebrtc/whipclientsink.html)). Video pads accept raw (incl. VAMemory/CUDAMemory/GLMemory — internal encode) **or pre-encoded** H.264/H.265/VP8/VP9/AV1; audio raw or Opus. 1.28 deprecated the older `whipsink`/`whepsrc` (webrtchttp) in favour of `whipclientsink`/`whepclientsrc` ([1.28 notes](https://gstreamer.freedesktop.org/releases/1.28/); background: [Asymptotic's WHIP/WHEP post](https://arunraghavan.net/2024/09/gstreamer-and-webrtc-http-signalling/)) |
| WebRTC (full) | `webrtcsink` / `webrtcbin` | 1.28: webrtcsink gained renegotiation and VA encoder support ([1.28 notes](https://gstreamer.freedesktop.org/releases/1.28/)); `webrtcbin` is the low-level C element ([docs](https://gstreamer.freedesktop.org/documentation/webrtc/index.html)) |
| HLS/DASH (recording-adjacent) | `hlscmafsink`, `hlssink3`, `hlsmultivariantsink`, `dashsink` | New/refreshed in 1.26 ([1.26 notes](https://gstreamer.freedesktop.org/releases/1.26/)) |

Multistream isolation (PLAN §11): each output branch gets its own queue with explicit bounds + its own sink; encoded-stream sharing via a `tee` after the encoder is safe only when codec/profile/rate-control/resolution/fps/color/GOP match — the muxer/sink pairs (`flvmux`+`rtmp2sink`, `mpegtsmux`+`srtsink`) differ per destination anyway. `rtmp2sink`'s `peak-kbps` pacing and `stats`, and `srtsink`'s `stats`, give per-output statistics without custom probes.

## 8. Local verification snapshot (Ubuntu, GStreamer 1.28.2, no NVIDIA GPU)

Found: `compositor`, `glvideomixer`, `vacompositor`, `gtk4paintablesink` (gst-plugin-gtk4 0.14.4), `pipewiresrc` (PipeWire 1.6.2), `vah264enc`, `vah265enc`, `x264enc`, `x265enc`, `svtav1enc`, `matroskamux`, `mp4mux`, `splitmuxsink`, `rtmp2sink`, `srtsink`.
Missing: `nvh264enc`/`nvh265enc`/`nvav1enc` (no CUDA), `vaav1enc` (driver), `isofmp4mux`/`isomp4mux`/`cmafmux`, `whipsink`/`whipclientsink`, `webrtcsink`, `gopbuffer` — all the missing non-hardware ones are **gst-plugins-rs**, which Ubuntu does not package (`apt-cache search plugins-rs` → nothing). Fedora packages `gstreamer1.0-plugins-rs` equivalents more completely ([Fedora packages](https://packages.fedoraproject.org/)).

Implication: `just ci`/dev setups and any Flatpak/releng work must build gst-plugins-rs 0.15.x ourselves (meson subproject or cargo-c), at least: `isobmff`, `rswebrtc`, `gtk4`, `gopbuffer`.

## Conclusions for Prismcast

1. **Binding baseline**: depend on `gstreamer` 0.25.x (MSRV 1.92, fine with `channel = "stable"`), feature-gate to a declared minimum runtime of GStreamer 1.26, runtime-detect 1.28 features (udmabuf gtk4 path, `eflvmux`, shared task-pool context). State this minimum in an ADR; PLAN §4 currently has no version floor.
2. **Compositor strategy**: implement `GstCompositorBackend` against the shared pad-property vocabulary (`xpos/ypos/width/height/alpha/zorder`) that `compositor`, `glvideomixer`, `vacompositor` all expose — the backend can swap elements without changing the domain model. Start with `compositor` (correctness), add `glvideomixer` next (DMA-BUF input support pairs with gtk4paintablesink's DMABuf pad), treat `vacompositor` as an Intel/AMD optimization (rank none, limited formats, no blend ops). This may warrant an ADR addendum to ADR-0004.
3. **Preview**: in-process static registration of `gst-plugin-gtk4` 0.15 with features `wayland` + `dmabuf`, requiring GTK ≥ 4.14 for the direct-DMA-BUF path; keep a system-memory fallback. Verify copy counts with tracers (`GST_TRACERS=stats`/`leaks`, pad push timings from 1.26) rather than assuming zero-copy.
4. **Encoders**: probe at runtime (`gst::ElementFactory::find` + device monitor); expose `nvh264enc/nvh265enc/nvav1enc` (CUDA), `vah264{,lp}enc/vah265{,lp}enc/vaav1enc` (VA), `x264enc/x265enc/svtav1enc` (software) behind `EncoderBackend`. Use force-key-unit events (supported by va and nv encoders) for replay-buffer save and splitmux alignment.
5. **Recording**: default MKV via `matroskamux streamable=true` (crash-safe, unlimited audio tracks — satisfies PLAN §13's "not artificially limited to six"); MP4/fMP4 via Rust `isobmff` (`isomp4mux`/`isofmp4mux`) — we must build gst-plugins-rs since distros lag. Use `splitmuxsink` for time/size splitting. Replay buffer: custom ring-buffer element in Rust (PLAN §14 design), `gopbuffer` as reference.
6. **Streaming**: `rtmp2sink` (+`eflvmux` where Enhanced RTMP is wanted), `srtsink` (avoid 1.30-only `connection-key`), `whipclientsink` for WHIPOutput — never the deprecated `whipsink`. Per-output bounded queues + per-sink stats properties give PLAN §11 isolation and statistics without custom instrumentation.
7. **Risks/open questions**:
   - gst-plugins-rs not packaged by Ubuntu/Debian → build/packaging work (affects BOOT/releng tasks; candidate ADR on vendoring strategy).
   - `pipewiresrc` renegotiation on stream format change and keepalive timestamping have known upstream issues (#3147, #3149) — capture recovery design (RES/ARCH capture tasks) must include re-probe/reconnect logic.
   - `vacompositor` capability spread across drivers is unknown; needs a hardware test matrix before committing to it as more than optional.
   - WHIP/WHEP elements in rswebrtc are young (rank none, active bug reports e.g. multi-stream disconnects against some servers) — flag WHIPOutput as best-effort in early milestones.
   - No Vulkan compositor element exists; if GL proves insufficient, a custom Rust Vulkan/GL element is the fallback (PLAN §5 already anticipates this).

## Sources

- [GStreamer 1.28 release notes](https://gstreamer.freedesktop.org/releases/1.28/) · [1.26](https://gstreamer.freedesktop.org/releases/1.26/) · [1.24](https://gstreamer.freedesktop.org/releases/1.24/) · [1.29 dev notes](https://gstreamer.freedesktop.org/releases/1.29/)
- [gstreamer on crates.io](https://crates.io/crates/gstreamer) / [docs.rs](https://docs.rs/gstreamer/latest/gstreamer/) · [gst-plugins-rs](https://github.com/GStreamer/gst-plugins-rs) · [gst-plugin-gtk4 on crates.io](https://crates.io/crates/gst-plugin-gtk4)
- Element docs: [compositor](https://gstreamer.freedesktop.org/documentation/compositor/index.html) · [glvideomixer](https://gstreamer.freedesktop.org/documentation/opengl/glvideomixer.html) · [va plugin](https://gstreamer.freedesktop.org/documentation/va/index.html) / [vacompositor](https://gstreamer.freedesktop.org/documentation/va/vacompositor.html) · [nvcodec](https://gstreamer.freedesktop.org/documentation/nvcodec/index.html) · [gtk4paintablesink](https://gstreamer.freedesktop.org/documentation/gtk4/index.html) · [srtsink](https://gstreamer.freedesktop.org/documentation/srt/srtsink.html) · [rtmp2sink](https://gstreamer.freedesktop.org/documentation/rtmp2/rtmp2sink.html) · [whipclientsink](https://gstreamer.freedesktop.org/documentation/rswebrtc/whipclientsink.html) · [isobmff](https://gstreamer.freedesktop.org/documentation/isobmff/index.html) · [fmp4](https://gstreamer.freedesktop.org/documentation/fmp4/index.html) · [webrtcbin](https://gstreamer.freedesktop.org/documentation/webrtc/index.html)
- [Asymptotic — WHIP and WHEP with GStreamer](https://arunraghavan.net/2024/09/gstreamer-and-webrtc-http-signalling/) · [Collabora — Hacking on the PipeWire GStreamer elements](https://www.collabora.com/news-and-blog/blog/2024/06/05/hacking-on-the-pipewire-gstreamer-elements/)
- PipeWire issues [#3147](https://gitlab.freedesktop.org/pipewire/pipewire/-/issues/3147), [#3149](https://gitlab.freedesktop.org/pipewire/pipewire/-/issues/3149)
- Local verification: `gst-inspect-1.0` on GStreamer 1.28.2 / PipeWire 1.6.2 / gst-plugin-gtk4 0.14.4 (Ubuntu resolute), this machine, 2026-09-30.
