# RES-005 — Encoder capability matrix (Linux, GStreamer)

Research note for Prismcast (PLAN §12). Scope: H.264 / HEVC / AV1 / VP9 and
AAC / Opus encoder availability on Linux via GStreamer — NVENC, VA-API,
Intel QSV, software (x264/x265/SVT-AV1) — element names, zero-copy / DMA-BUF
paths, latency characteristics, and recommended defaults per GPU vendor.

## Version baselines (as of 2026-09-30)

| Component | Baseline | Notes |
|---|---|---|
| GStreamer stable | **1.28.7** (2026-09-07); 1.28.0 released 2026-01-27 | [1.28 release notes](https://gstreamer.freedesktop.org/releases/1.28/) |
| Previous stable series | 1.26.11 (2025-03 … 2026-03) | [1.26 release notes](https://gstreamer.freedesktop.org/releases/1.26/) |
| GStreamer in development | 1.29.x → 1.30, expected Q4/2026 | [1.28 notes, "Schedule for 1.30"](https://gstreamer.freedesktop.org/releases/1.28/) |
| gstreamer-rs | **0.25.4** (2026-09-21), cargo features `v1_16`…`v1_30` | [crates.io gstreamer](https://crates.io/crates/gstreamer), [docs.rs features](https://docs.rs/crate/gstreamer/0.25.4/features) |
| OBS baseline (per PLAN) | **32.2.2** (released 2026-07) | [obsproject/obs-studio @ 32.2.2](https://github.com/obsproject/obs-studio/tree/32.2.2) |
| Intel VA-API driver | intel-media-driver (iHD), up to Nova Lake | [intel/media-driver](https://github.com/intel/media-driver) |

Throughout, "stable" means shipped in a released GStreamer/OBS version;
"development" means only on GStreamer `main` (1.29/1.30) or OBS 33.x docs.

---

## 1. Element inventory — video encoders

Element/plugin names verified against the current GStreamer plugin
documentation ([plugins_doc.html](https://gstreamer.freedesktop.org/documentation/plugins_doc.html)),
the per-plugin index pages, and the 1.26/1.28 release notes.

### 1.1 NVIDIA NVENC — plugin `nvcodec` (gst-plugins-bad)

[nvcodec plugin index](https://gstreamer.freedesktop.org/documentation/nvcodec/index.html)

| Codec | Elements | Since | Rank |
|---|---|---|---|
| H.264 | `nvh264enc`, `nvautogpuh264enc` | 1.18 / auto-GPU variants 1.22+ | primary + 1 |
| HEVC | `nvh265enc`, `nvautogpuh265enc` | 1.18 | primary + 1 |
| AV1 | `nvav1enc`, `nvautogpuav1enc` | **1.26** ([1.26 notes](https://gstreamer.freedesktop.org/releases/1.26/): "NVCODEC AV1 video encoder element") | primary + 1 |
| VP9 | **none** — NVDEC has VP9 decode (`nvvp9dec`); NVENC has never offered VP9 encode | — | — |
| JPEG | `nvjpegenc`, `nvautogpunvenc` (auto-GPU variant is 1.28) | 1.28 | — |

Sink pad caps of `nvav1enc` ([element doc](https://gstreamer.freedesktop.org/documentation/nvcodec/nvav1enc.html)):
`video/x-raw(memory:CUDAMemory)`, `(memory:D3D12Memory)` (Windows),
`(memory:GLMemory)`, and plain system memory; formats
`NV12, P010_10LE, VUYA, RGBA, RGBx, BGRA, BGRx, RGB10A2_LE` — i.e. the
encoder consumes CUDA or GL textures directly (zero-copy from a GPU-side
compositor) and can do RGB→NV12 conversion on-GPU.

1.28 additions relevant to us: `nvencoder` gained an `emit-frame-stats`
property (per-frame QP stats via a `frame-stats` signal) and interlaced
handling improvements; `nvh264enc`/`nvh265enc` gained a conditional
`num-slices` property ([1.28 notes](https://gstreamer.freedesktop.org/releases/1.28/)).

Requires the proprietary NVIDIA driver; elements fail registration if the
driver/NVENC is unavailable (runtime capability, per skill guidance — check
with `gst-inspect-1.0 nvcodec`).

### 1.2 VA-API — plugin `va` (gst-plugins-bad), Intel + AMD (+ others)

[va plugin index](https://gstreamer.freedesktop.org/documentation/va/index.html),
[1.26 notes — VA section](https://gstreamer.freedesktop.org/releases/1.26/)

| Codec | Elements | Notes |
|---|---|---|
| H.264 | `vah264enc`, `vah264lpenc` | `lp` = low-power (VDEnc/HuC fixed-function path on Intel) |
| HEVC | `vah265enc`, `vah265lpenc` | 8/10-bit depending on driver |
| AV1 | `vaav1enc` (+ `vaav1lpenc` on drivers that expose it, seen in the wild but driver-dependent) | encode needs DG2/Alchemist or newer (Intel), RDNA3/VCN4 or newer (AMD) — see §3 |
| VP9 | `vavp9enc` | added in **1.26** ([MR !3293 "Implement the VA VP9 encoder"](https://discourse.gstreamer.org/t/setting-up-hardware-acceleration-vaapi-for-webrtc/3700), [1.26 notes mention VA VP9 encoder improvements](https://gstreamer.freedesktop.org/releases/1.26/)); **absent from the generated element docs** because `va` elements are registered dynamically per driver capability — treat as driver-conditional |
| VP8 | `vavp8enc` | added in **1.26** ("VP8 video encoder") |
| JPEG | `vajpegenc` | added in 1.26 |

Sibling elements for the zero-copy path: `vapostproc` (scale/CSC on VA
surfaces), `vacompositor`, `vadeinterlace`.

Example properties on `vah264enc` ([element doc](https://gstreamer.freedesktop.org/documentation/va/vah264enc.html)):
`rate-control` ∈ {cbr, vbr, vcm, cqp, icq, qvbr}, `bitrate`, `cpb-size`,
`key-int-max`, `b-frames`, `b-pyramid`, `ref-frames`, `min-qp`/`max-qp`,
`target-usage` (speed/quality trade-off, 1=best quality … 7=fastest),
`trellis`, `mbbrc`. Sink pads: `video/x-raw(memory:VAMemory)` **or** plain
system memory (`NV12`) — system-memory frames are imported into VA surfaces
by the element. 1.26 enabled ICQ/QVBR for H.264/HEVC/VP9/AV1 encoders,
improved B-pyramid handling, trellis control, and encoder throughput via
output delay ([1.26 notes](https://gstreamer.freedesktop.org/releases/1.26/)).

**Deprecation note:** the old `gstreamer-vaapi` plugin (`vaapih264enc` etc.)
was deprecated in 1.26 (rank demoted to None, no autoplugging) and **removed
in 1.28** ("GStreamer-VAAPI has been removed in favour of the va plugin",
[1.28 notes](https://gstreamer.freedesktop.org/releases/1.28/)). Prismcast
must target `va` only.

### 1.3 Intel QSV — plugin `qsv` (gst-plugins-bad, oneVPL)

[qsv plugin index](https://gstreamer.freedesktop.org/documentation/qsv/index.html):
`qsvh264enc`, `qsvh265enc`, `qsvav1enc`, `qsvvp9enc`, `qsvjpegenc` (plus
decoders). Registration is hardware-dependent (`gst-inspect-1.0 qsv`).

- Backed by Intel oneVPL (libvpl), which on modern Intel graphics drives the
  same silicon as iHD/VA-API. On Linux the `va` plugin and `qsv` plugin are
  largely equivalent in codec coverage; `qsv` is the path oneVPL-specific
  features land in first. 1.26 added D3D12-memory interop for QSV encoders
  (Windows-relevant).
- The legacy `msdk` plugin (`msdkh264enc`, `msdkh265enc`, `msdkav1enc`,
  `msdkvp9enc`, …) still exists in the docs but is the older MediaSDK-based
  stack; `qsv` supersedes it. Do not build on `msdk`.

### 1.4 Vulkan Video — plugin `vulkan` (experimental, watch item)

`vulkanh264enc` exists ([plugins_doc](https://gstreamer.freedesktop.org/documentation/plugins_doc.html));
1.28 added "Vulkan H.264 encoding support" plus runtime-generated pad
template caps so caps reflect actual hardware ([1.28 notes](https://gstreamer.freedesktop.org/releases/1.28/)).
AV1/VP9 Vulkan **decode** landed; HEVC encode is not shipping yet. This is
the potential single cross-vendor encode API long-term, but today it is
H.264-only and young — treat as development/experimental, not a default.

### 1.5 AMD AMF — not a Linux option

`amfh264enc`, `amfh265enc`, `amfav1enc` exist in gst-plugins-bad but the AMF
plugin targets Windows (Direct3D11/12); AMD hardware encoding on Linux is
VA-API-only (radeonsi/VCN) in practice.

### 1.6 Software encoders

| Codec | Element | Plugin / package | Rank | Notes |
|---|---|---|---|---|
| H.264 | `x264enc` | gst-plugins-**ugly** (`x264`) | primary | property `tune` incl. `zerolatency`, `speed-preset`, `bitrate`, `key-int-max`; 1.26 added `nal-hrd` ([1.26 notes](https://gstreamer.freedesktop.org/releases/1.26/)). Patent-encumbered → ugly; distros like Fedora ship via RPM Fusion |
| H.264 | `openh264enc` | gst-plugins-bad | — | Cisco OpenH264; royalty-bearing but binary-royalty-covered; quality below x264 |
| HEVC | `x265enc` | gst-plugins-bad (`x265`) | — | heavy CPU cost; fine for offline/recording |
| AV1 | `svtav1enc` | gst-plugins-bad (`svtav1`), since 1.22 | secondary | [plugin doc](https://gstreamer.freedesktop.org/documentation/svtav1/index.html); preset-based speed/quality; realistic for 1080p60 realtime at fast presets on modern CPUs |
| AV1 | `av1enc` (libaom) | gst-plugins-bad (`aom`) | — | highest quality/bit, slowest; `usage`/`cpu-used` realtime modes exist but slower than SVT at equal quality |
| AV1 | `rav1enc` (rav1e) | gst-plugins-**rs** (`rav1e`) | — | [plugins_doc](https://gstreamer.freedesktop.org/documentation/plugins_doc.html); Rust; between aom and SVT in speed |
| VP9 | `vp9enc` | gst-plugins-good (`vpx`, libvpx) | — | only practical VP9 software encoder; `deadline`/`cpu-used` for realtime |
| H.264 fallback via FFmpeg | `avenc_*` family | gst-libav | marginal rank | e.g. `avenc_libx264` when distro omits ugly plugins |

## 2. Element inventory — audio encoders

| Codec | Element | Plugin | Notes |
|---|---|---|---|
| Opus | `opusenc` | gst-plugins-**base** (`opus`) | the default choice for streaming/recording where the container/protocol allows (WebM, MKV, WebRTC); low latency, excellent quality/bit |
| Opus | `avenc_opus` | gst-libav | FFmpeg-native Opus; lower quality than libopus historically |
| AAC | `fdkaacenc` | gst-plugins-bad (`fdkaac`) | best-quality AAC on Linux; FDK-AAC license is non-free-ish → some distros (Fedora) don't ship it ([Fedora HWAccel wiki notes non-free codec exclusions](https://fedoraproject.org/wiki/Hardware_Video_Acceleration)) |
| AAC | `avenc_aac` | gst-libav | FFmpeg native AAC; universally available via gst-libav; adequate for RTMP-style streaming; 1.26 allows runtime channel-config changes for it |
| AAC | `voaacenc`, `faac` | gst-plugins-bad | legacy/lower quality; avoid |
| MP3 | `lamemp3enc` | gst-plugins-ugly | only for legacy outputs |

No hardware audio encoding exists on Linux; this entire section is CPU.

## 3. Hardware capability by GPU vendor

### 3.1 NVIDIA (NVENC, proprietary driver)

Per [NVIDIA's Ada Lovelace AV1 blog](https://developer.nvidia.com/blog/improving-video-quality-and-performance-with-av1-and-nvidia-ada-lovelace-architecture/)
and the [NVENC support matrix](https://developer.nvidia.com/video-encode-and-decode-gpu-support-matrix-new):

| Codec | First NVENC generation with encode | Example GPUs |
|---|---|---|
| H.264 8-bit | Kepler (NVENC 1st gen) | GTX 600/700+ |
| HEVC 8/10-bit | Maxwell 2nd gen (GM20x) / Pascal full | GTX 950/960+, GTX 10xx+ |
| AV1 8/10-bit | **Ada Lovelace (NVENC 8th gen)** | RTX 40xx, and Blackwell (RTX 50xx) |
| VP9 | never | — |

Consumer GeForce cards historically cap concurrent NVENC sessions
(officially 5 on current drivers, unofficially patchable); relevant for
multistream with per-destination encoders — prefer one encoded stream shared
by destinations (see Conclusions). Ada added 10-bit 8K60 for AV1/HEVC;
B-frames and lookahead are supported on Pascal+.

The `nvcodec` plugin needs a reasonably current driver (CUDA/NVENCODE API);
there are known breakage events when new major drivers change CUDA symbols
(e.g. [gst-cudanvrtc failure with driver 595](https://github.com/games-on-whales/wolf/issues/405)) —
pin/validate the driver version in support docs.

### 3.2 Intel (iHD VA-API driver, intel/media-driver)

Condensed from the official
[intel/media-driver decoding/encoding feature table](https://github.com/intel/media-driver)
(E = full encode, Es = shader-assisted encode, LP = VDEnc low-power):

| Codec | Encode since | Formats |
|---|---|---|
| H.264 | Broadwell (Es), VDEnc/LP on SKL+ | 8-bit |
| HEVC 8-bit | Skylake (Es); E/LP from ICL | — |
| HEVC 10-bit | Kaby Lake (Es); E/LP from ICL | — |
| HEVC 422/444 | 8/10/12-bit E from ICL/TGL depending on sub-format | up to 12-bit on TGL+ |
| VP9 8/10-bit | Kaby Lake | incl. 444 on KBL+ |
| AV1 8/10-bit | **DG2 / Alchemist (Arc A-series)**; MTL/LNL/BMG and newer continue | — |

Caveats from the same source: low-power encode rate control (CBR/VBR)
requires HuC firmware — auto-loaded from Alder Lake onward, but needs
`i915.enable_guc=2` on TGL/RKL/ICL and earlier; the legacy `i965` driver
(libva-intel-driver, Gen 9 and older) is a different, more limited stack;
"free kernel" driver builds drop some Es shader-encode modes. From Meteor
Lake the EncSlice/EncSliceLP split is unified behind `VAEntrypointEncSlice`,
so the `lpenc` vs non-`lp` distinction may collapse to one registration on
newer parts.

### 3.3 AMD (radeonsi VA-API, Mesa; VCN)

| Codec | Encode since | Source |
|---|---|---|
| H.264 8-bit | VCE/VCN generations back to Polaris/RX 4xx (VCN1 on Raven Ridge+) | [rigaya/VCEEnc hardware table](https://github.com/rigaya/VCEEnc) |
| HEVC 8-bit | Polaris (VCE 3.4); 10-bit from VCN2 (Renoir / RDNA1) | same |
| AV1 | **RDNA3 / VCN4 (RX 7000 series)** — Mesa VA-API AV1 encode merged 2023-04, LTR added in Mesa 24.1 | [Phoronix: AMD Adds AV1 Video Encoding To Mesa VA-API](https://www.phoronix.com/news/AMD-Mesa-AV1-VA-API-Encode), [Phoronix: Mesa 24.1 AV1 LTR](https://www.phoronix.com/news/Mesa-24.1-AV1-Encode-LTR-AMD) |
| VP9 | none (VCN has no VP9 encoder) | — |

RDNA3 note: early VCN4 firmware/kernel enablement lacked AV1 encode
([Wccftech, 2022](https://wccftech.com/amd-rdna-3-gfx11-gpu-patches-enable-vcn4-support-but-lack-av1-encoding/));
it arrived in userspace via Mesa later. Treat AV1-on-AMD as
"Mesa ≥ 23.1-ish + VCN4", and verify at runtime.

### 3.4 NVIDIA via VA-API

There is no first-party NVIDIA VA-API driver. The community
[elFarto/nvidia-vaapi-driver](https://github.com/elFarto/nvidia-vaapi-driver)
is an NVDEC-backed VA-API implementation aimed at Firefox **decode**; it is
not a recommended encode path. On NVIDIA, use `nvcodec` directly.

## 4. Zero-copy / DMA-BUF paths

Goal per PLAN §12: zero CPU copies/frame from capture to encoder.

1. **Capture side (Wayland/PipeWire).** `pipewiresrc` delivers
   `video/x-raw(memory:DMABuf)` (with `DMA_DRM` format + DRM modifier
   negotiation — the `drm-format` field landed in **1.24**;
   [1.24 notes](https://gstreamer.freedesktop.org/releases/1.24/),
   [Igalia: DMABuf modifier negotiation](https://blogs.igalia.com/vjaquez/dmabuf-modifier-negotiation-in-gstreamer/)).
2. **VA path (Intel/AMD).** Canonical chain:
   `pipewiresrc (DMABuf) → vapostproc → video/x-raw(memory:VAMemory) → va{h264,h265,av1}lp?enc`.
   `vapostproc` imports DMABufs into VA surfaces and does scale/CSC on-GPU;
   the `va` encoders' documented sink caps are `memory:VAMemory` **or**
   system memory ([vah264enc doc](https://gstreamer.freedesktop.org/documentation/va/vah264enc.html)) —
   direct `memory:DMABuf` sink caps are **not** advertised, so `vapostproc`
   is the import point. Whether import is truly zero-copy depends on
   modifier agreement between the PipeWire buffer and the VA driver
   (i915 X/Y-tiled usually imports on Intel; otherwise the driver copies
   internally). The general contract is documented in
   [GStreamer's DMABuf design doc](https://gstreamer.freedesktop.org/documentation/additional/design/dmabuf.html).
3. **NVENC path (NVIDIA).** Capture DMABuf → GL import (`gstgl`) →
   `nv*enc` accepts `memory:GLMemory`/`memory:CUDAMemory` directly
   ([nvav1enc sink caps](https://gstreamer.freedesktop.org/documentation/nvcodec/nvav1enc.html));
   alternatively `cudaupload` after a GL→CUDA interop step. On NVIDIA,
   PipeWire streams can also be delivered as GL textures via EGL import.
   `cudacompositor` (1.26) / `cudaconvertscale` keep composition on-GPU.
4. **Compositor handoff.** If Prismcast composes in GL (per PLAN §5),
   both vendor paths accept GL- or GPU-memory input without a readback;
   for VA, add a `gldownload`/`vapostproc` boundary only when the compositor
   can't produce VA-compatible DMABufs. 1.28's new `udmabuf` allocator and
   `glupload` udmabuf uploader help the *software* decode/source → GPU
   direction ([1.28 notes](https://gstreamer.freedesktop.org/releases/1.28/)),
   not the capture→encode direction.
5. **Fallback copies.** Any `videoconvert`/sysmem hop costs a full frame
   copy per hop (~8 MB/frame at 1080p RGBA, ~31 MB at 4K) — the PLAN §12
   benchmark list (CPU copies/frame, GPU→CPU transitions, DMA-BUF
   preservation, encoder latency, memory) maps directly onto these boundary
   choices; verify each pipeline with `GST_DEBUG=*dmabuf*,va:7` /
   gst-stats, not from element names alone (per `.agents/skills/gstreamer-rust`).

## 5. Latency characteristics

Live streaming defaults should disable reordering/lookahead; recording can
allow them.

| Encoder | Latency-relevant knobs (verified property names) |
|---|---|
| `nv*enc` (nvcodec) | `zerolatency` (no reordering delay), `rc-lookahead` (0 for live), `bframes` (0 for live), `tune` ∈ {default, high-quality, low-latency, ultra-low-latency}, `multi-pass`, `gop-size`, `strict-gop` ([nvav1enc doc](https://gstreamer.freedesktop.org/documentation/nvcodec/nvav1enc.html)) |
| `va*enc` | `b-frames` (0 for live), `b-pyramid`, `ref-frames` (3 default), `target-usage` (higher = faster/lower quality), `num-slices`; 1.26 "output delay" change improves throughput — watch its latency effect ([vah264enc doc](https://gstreamer.freedesktop.org/documentation/va/vah264enc.html), [1.26 notes](https://gstreamer.freedesktop.org/releases/1.26/)) |
| `qsv*enc` | oneVPL low-power modes; similar B-frame/lookahead knobs |
| `x264enc` | `tune=zerolatency` (removes lookahead/B-frame delay), `speed-preset`, `key-int-max`, `bframes`, `sliced-threads`; classic OBS "veryfast/zerolatency" for live |
| `x265enc` | much higher latency/CPU; `tune=zerolatency` exists; generally avoid for live on CPU |
| `svtav1enc` | preset 0–13 style speed ladder (higher = faster); realtime 1080p60 feasible at fast presets on ≥8-core modern CPUs; latency grows with lookahead/level-of-parallelism settings ([svtav1 plugin doc](https://gstreamer.freedesktop.org/documentation/svtav1/index.html)) |
| `vp9enc` (libvpx) | `deadline=realtime`, `cpu-used` 5–8 for live; frame-parallel off for latency |
| Audio | `opusenc` frame duration as low as 2.5 ms — negligible; AAC adds one frame (~21 ms at 48 kHz) + encoder delay |

B-frames and lookahead are the dominant encode-side latency contributors on
every hardware path; GOP size doesn't add latency but controls
recoverability and multistream join time. For quality-per-bit, hardware
encoders roughly rank (at live settings): NVENC ≥ QSV ≈ VA(Intel) >
VA(AMD), with x264 medium beating all of them if CPU is abundant
(consistent with OBS community consensus and NVIDIA's own benchmarks in the
[Ada blog](https://developer.nvidia.com/blog/improving-video-quality-and-performance-with-av1-and-nvidia-ada-lovelace-architecture/);
treat exact ordering as workload-dependent and validate with the new 1.28
`vmaf` element).

## 6. OBS 32.2.2 baseline mapping (Linux)

Verified against the [plugins tree at tag 32.2.2](https://github.com/obsproject/obs-studio/tree/32.2.2/plugins):

| OBS plugin (32.2.2) | Encoders exposed on Linux | Prismcast GStreamer equivalent |
|---|---|---|
| `obs-x264` | x264 | `x264enc` |
| `obs-ffmpeg` (`obs-ffmpeg-nvenc.c`, `obs-ffmpeg-vaapi.c`, `obs-ffmpeg-av1.c`, `obs-ffmpeg-openh264.c`) | FFmpeg NVENC H.264/HEVC, FFmpeg VA-API H.264/HEVC, SVT-AV1, AOM-AV1, OpenH264 | `nv*enc`, `va*enc`, `svtav1enc`, `av1enc`, `openh264enc` (or `avenc_*` fallbacks) |
| `obs-nvenc` (`nvenc-cuda.c`, `nvenc-opengl.c`; "jim-nvenc") | Native NVENC H.264/HEVC/AV1 — native (non-FFmpeg) NVENC incl. AV1 landed for Linux in OBS 30.2 ([9to5Linux](https://9to5linux.com/obs-studio-30-2-released-with-nvenc-av1-support-on-linux-unified-pipewire-source)) | `nvh264enc`/`nvh265enc`/`nvav1enc` with GL/CUDA memory |
| `obs-qsv11` | QSV H.264/HEVC/AV1 via oneVPL | `qsv*enc` or `va*enc` |
| `obs-ffmpeg` audio | FFmpeg AAC (+ Opus etc.) | `fdkaacenc` / `avenc_aac`, `opusenc` |

Feature parity for the matrix task: Prismcast matches or exceeds OBS on
Linux by exposing VA-API VP9 (`vavp9enc`, 1.26+) and VA-API JPEG, which OBS
does not expose. OBS's AMF path is Windows-only — irrelevant for us.

## 7. Recommended defaults per GPU vendor

Live-streaming profile (latency-first); recording profile can relax B-frames
and presets.

| GPU | Video default | Settings sketch | Audio |
|---|---|---|---|
| NVIDIA Ada/Blackwell (RTX 40/50) | `nvav1enc` where the service accepts AV1, else `nvh264enc` | `tune=low-latency` (or `zerolatency=true`), `bframes=0`, `rc-lookahead=0`, rc-mode cbr/vbr, `multi-pass=single` | `opusenc` (MKV/WebM/WebRTC), `fdkaacenc`/`avenc_aac` (RTMP/MP4) |
| NVIDIA pre-Ada (Pascal–Ampere) | `nvh264enc`; `nvh265enc` for recording/HEVC-capable services | same latency knobs | same |
| Intel Arc / MTL+ (iHD) | `vaav1enc` (or `vaav1lpenc` when registered); fallback `vah264lpenc` | `b-frames=0`, `target-usage=6` live / `4` recording, rc=icq or cbr | same |
| Intel iGPU pre-Arc | `vah264lpenc` (VDEnc low-power) / `vah264enc`; HEVC 10-bit from ICL for recording | same; check HuC for rate control on ≤TGL | same |
| AMD RDNA3+ (VCN4, Mesa ≥23.1) | `vaav1enc` if runtime-confirmed, else `vah264enc`/`vah265enc` | as Intel VA | same |
| AMD pre-RDNA3 | `vah264enc`; `vah265enc` from VCN2 for recording | as Intel VA | same |
| CPU-only | `x264enc tune=zerolatency speed-preset=veryfast` live; `svtav1enc` fast preset only on beefy CPUs; `x265enc`/`av1enc` offline-only | — | same |
| VP9 (YouTube/WebRTC) | `vavp9enc` on KBL+ Intel if driver registers it; otherwise `vp9enc deadline=realtime` | — | Opus |

Cross-cutting defaults: NV12 8-bit pipeline for streaming; P010_10LE only
for HEVC/AV1 HDR recording; GOP = 2×framerate for live (join latency vs
bitrate efficiency); one shared encoded stream per (codec, resolution,
bitrate-class) for multistream rather than N encoder instances (consumer
NVENC session caps make this mandatory on NVIDIA).

## 8. Risks and open questions

- **Driver-conditional registration** of all `va`/`qsv`/`nvcodec` elements:
  capability probing must happen at runtime (`gst-inspect` / registry),
  never assumed from GPU model alone (per `.agents/skills/gstreamer-rust`).
  `vavp9enc` and `vaav1lpenc` are absent from the generated docs for exactly
  this reason.
- **vavp9enc doc gap:** added in 1.26 per release notes and
  [MR !3293](https://discourse.gstreamer.org/t/setting-up-hardware-acceleration-vaapi-for-webrtc/3700)
  but missing from plugins_doc.html — confirm element name/behavior on a
  KBL+ machine before committing to VA-VP9 support.
- **True zero-copy VA import** depends on DRM modifier agreement; some
  PipeWire→VA paths silently copy inside the driver. Needs a benchmark task
  (PLAN §12 list) with `GST_TRACER` stats before we claim zero-copy.
- **NVENC session limits** on consumer GPUs constrain multistream with
  per-destination encoders.
- **NVIDIA driver churn** can break `nvcodec` at runtime (CUDA symbol
  changes; e.g. driver 595 incident linked above). Consider surfacing
  "encoder unavailable after driver update" as a first-class core event.
- **AAC packaging:** `fdkaacenc` (best quality) is not shipped by Fedora and
  some other distros (license); plan `avenc_aac` (gst-libav) as the
  guaranteed fallback and probe both.
- **Vulkan Video encode** (`vulkanh264enc` in 1.28) is the strategic
  cross-vendor endgame — track 1.29/1.30 development but do not design it
  in as a requirement today.
- **HuC/firmware prerequisites** for Intel low-power rate control on older
  kernels need detection and a user-facing hint, not a silent quality
  regression.
- **1.26 "output delay" VA encoder change** improved throughput; its effect
  on live latency should be measured before we default live profiles to
  B-frames>0 anywhere.

## 9. Conclusions for Prismcast

1. **Encoder abstraction (`prismcast-media` / `prismcast-output`):** model
   an `EncoderKind { Nvenc, VaApi, Qsv, Software }` ×
   `Codec { H264, Hevc, Av1, Vp9 }` capability matrix, populated at runtime
   by registry probing (`gst::ElementFactory::find` + pad-template caps +
   a 1-frame smoke encode), not by static GPU tables. The static tables in
   §3 are defaults/marketing, not truth on a given machine.
2. **Element choice order:** NVIDIA → `nvcodec` (`nvh264enc`/`nvh265enc`/`nvav1enc`);
   Intel/AMD → `va` (`vah264lpenc` preferred on Intel iGPU, `vaav1enc` on
   Arc/VCN4 when registered); QSV (`qsv*enc`) as an alternate Intel path
   only if `va` shows problems in benchmarking; software fallback
   `x264enc` → `svtav1enc` → `x265enc` (offline). Never touch
   `gstreamer-vaapi` (removed in 1.28) or `msdk`.
3. **Zero-copy pipeline contract:** capture branch must negotiate
   `video/x-raw(memory:DMABuf)` with DMA_DRM modifier caps (GStreamer ≥1.24);
   VA encode inserts `vapostproc` as the import boundary; NVENC encode
   accepts GL/CUDA memory directly from the compositor. Encode this as
   graph-construction logic with an explicit, logged fallback to a sysmem
   path, plus tracer-based copy counting in CI/bench runs.
4. **Minimum versions:** GStreamer **1.26** is the practical floor
   (NVENC AV1, VA VP8/VP9/JPEG encoders, QVBR/ICQ on VA, gstreamer-vaapi
   deprecation); target **1.28** for udmabuf, webrtcsink VA-encoder support,
   `emit-frame-stats`, and the removal-era cleanliness. gstreamer-rs **0.25.x**
   with cargo feature `v1_26` (or `v1_28` if we require it) matches this.
5. **Audio:** ship `opusenc` as primary; AAC via `fdkaacenc` when present,
   `avenc_aac` fallback; expose both in profiles since RTMP-style services
   demand AAC.
6. **Multistream design:** shared encoded streams (tee after encoder) as
   the default; per-destination encoders only as opt-in, with a consumer-NVENC
   session-limit guard.
7. **Profile schema:** live defaults = no B-frames, no lookahead,
   `tune=zerolatency`-equivalents per backend (property names differ —
   `zerolatency` (nv), `tune=zerolatency` (x264), `b-frames=0`+`target-usage`
   (va)); recording defaults allow B-frames, ICQ/QVBR or const-quality modes.
   The profile domain model in `prismcast-core` needs per-backend property
   bags, not a single flat encoder config.
8. **Bench task (follow-up, PLAN §12):** build a `gst`-based bench harness
   measuring copies/frame, GPU↔CPU transitions, encode latency (frame-in to
   bitstream-out), and RSS for each available backend at 1080p60/4K60;
   gate the "zero-copy" claim on it.
9. **ADR trigger:** the vendor/backend selection policy (§3 defaults +
   runtime probing order) and the shared-encoder multistream rule are
   architecture-level decisions → worth an ADR (`docs/adr/`) once the
   output-graph task starts. Also consider an ADR on minimum GStreamer
   version (1.26 vs 1.28 floor), since it determines whether we can rely on
   VA VP9 encode and NVENC AV1 unconditionally.
