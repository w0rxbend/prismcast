# RES-001 — OBS Studio feature inventory (baseline: 32.2.2)

Date: 2026-09-30
Task: RES-001 (PLAN §29, §54, §65, §79)
Status of upstream at time of writing: **OBS Studio 32.2.2 is the latest stable release** (released 2026-08-14; [GitHub releases](https://github.com/obsproject/obs-studio/releases), [obs-versions.com current version tracker](https://obs-versions.com/current-version)). Anything labelled 33.x (e.g. the CEF 128+ upgrade, merged 2026-09-17 into master) is **in-development** and is called out explicitly below ([SCRT disclosure, 2026-09-22](https://blog.scrt.ch/2026/09/22/how-one-twitch-chat-message-became-code-execution-on-a-streamers-pc/)).

Method: feature facts are verified against (a) official OBS release notes on obsproject.com, (b) the OBS knowledge base (obsproject.com/kb), and (c) the source tree of the `32.2.2` tag itself (plugin CMakeLists / submodule pins), which is authoritative for what actually ships. "OBS behavior" below describes **released** 32.2.2 functionality.

Legend: **Status** refers to Prismcast implementation status (project is in phase 0, so everything is `Not started`). Priorities are taken from PLAN §29.

---

## 1. Relevant upstream release timeline (30.2 → 32.2.2)

| Release | Date | Items relevant to the matrix |
|---|---|---|
| 30.2 | 2024-07 | Multitrack Video (Twitch "Enhanced Broadcasting"), initially Windows+NVENC only; Hybrid MP4 recording format (beta); native NVENC on Linux incl. AV1; shared-texture encode for NVENC/QSV/VAAPI on Linux; unified PipeWire screen+window capture into one "Screen Capture" source; enhanced RTMP/FLV multi-track audio+video; HEVC for WebRTC output; audio-only/video-only WHIP; new theme system. ([OBS 30.2 release notes](https://obsproject.com/blog/obs-studio-30-2), [GitHub releases](https://github.com/obsproject/obs-studio/releases)) |
| 31.0 | 2024-12 | CEF (Chromium) 127 (branch 6533) on all platforms; NVIDIA Blur/Background Blur filters; preview zoom/scrollbars; Amazon IVS service; first-party YouTube chat docks; scene items switched to **relative coordinates** (auto-converted on load); **FTL removed**; automatic scene switcher **disabled on Wayland**; display/window capture on Linux no longer captures implicitly. ([OBS 31.0 release notes](https://obsproject.com/blog/obs-studio-31-0-release-notes)) |
| 31.1 | 2025-07 | Multitrack Video on **Linux** and macOS (Apple Silicon); **additional canvases** for Multitrack Video output; explicit-sync support for PipeWire screen capture; hardware-accelerated browser source on Linux (disabled on NVIDIA); QVBR rate control for VA-API; UI font-size/density options. ([GamingOnLinux full changelog mirror](https://www.gamingonlinux.com/2025/07/obs-studio-31-1-0-released-with-multitrack-video-for-linux-explicit-sync-support-for-pipewire/)) |
| 32.0 | 2025-09 | Basic plugin manager; **Hybrid MP4/MOV made the default recording container** (out of beta); Hybrid MOV (ProRes on macOS, HEVC/H.264+PCM everywhere); default bitrate 2500 → 6000 kbps; audio deduplication rework incl. multiple canvases; improved PipeWire video-capture format selection; refuse to load plugins built for newer OBS. ([OBS 32.0 release notes](https://obsproject.com/blog/obs-studio-32-0-release-notes)) |
| 32.1 | 2025-11 (per [blog index](https://obsproject.com/blog)) | **Audio mixer overhaul** (vertical default layout, pinning, monitoring toggle, show-not-in-scene sources, studio-mode preview sources); **WebRTC simulcast**; more undo/redo actions; **partial Canvases support in obs-websocket**; new "add source" groundwork; browser local-file security hardening. ([OBS 32.1 release notes](https://obsproject.com/blog/obs-studio-32-1-release-notes)) |
| 32.2 | 2026-07-21 | New Add Source dialog; **SDR→HDR compose filter**; dynamic bitrate for Multitrack Video; missing-file support for filters; **NVIDIA driver ≥ 570 required** (Video Codec SDK 13); multitrack config limited to Custom service. ([OBS 32.2 release notes](https://obsproject.com/blog/obs-studio-32-2-release-notes)) |
| 32.2.1 / 32.2.2 | 2026-07 / 2026-08-14 | Hotfixes only (game capture hook, plugin loading on first start after update, macOS 12 blocked). ([GitHub releases](https://github.com/obsproject/obs-studio/releases)) |

Bundled component pins at tag `32.2.2` (from submodule SHAs in the [plugins tree](https://github.com/obsproject/obs-studio/tree/32.2.2/plugins)):

- **obs-websocket 5.7.4** ([CMakeLists at pinned SHA](https://raw.githubusercontent.com/obsproject/obs-websocket/1ef34bf48110c2a18184e50e41cd0b1a855e2147/CMakeLists.txt)) — includes `RequestHandler_Canvases` / `EventHandler_Canvases`.
- **obs-browser** (submodule) ships **Chromium 127.0.6533.120 / V8 12.7.224.18 with the CEF sandbox disabled** (`no_sandbox = true`) — security analysis in §6. Upgrade to CEF 128+ merged to master 2026-09-10/17, targeted at **33.0 (development)**, not in any stable release. ([SCRT disclosure](https://blog.scrt.ch/2026/09/22/how-one-twitch-chat-message-became-code-execution-on-a-streamers-pc/))

---

## 2. What actually ships in OBS 32.2.2 (source-tree verified)

Enumerated from the `plugins/` tree and per-plugin CMakeLists at tag `32.2.2` ([plugins/CMakeLists.txt](https://raw.githubusercontent.com/obsproject/obs-studio/32.2.2/plugins/CMakeLists.txt)):

**Sources / encoders / outputs**

- `image-source` — image, image slideshow, color source. In 32.2, directory add includes `.webp` ([32.2 notes](https://obsproject.com/blog/obs-studio-32-2-release-notes)).
- `obs-text` + `text-freetype2` — text rendering (FreeType2 on Linux).
- `obs-ffmpeg` — Media Source (`obs-ffmpeg-source.c`); FFmpeg recording muxer (`ffmpeg-mux`); SRT/RIST MPEG-TS output (`obs-ffmpeg-mpegts.c`, enabled by default: `ENABLE_NEW_MPEGTS_OUTPUT=ON`); HLS mux (`obs-ffmpeg-hls-mux.c`); VAAPI encoders on Linux (`obs-ffmpeg-vaapi.c`); AOM/SVT AV1 (`obs-ffmpeg-av1.c`); OpenH264; FFmpeg audio encoders. ([obs-ffmpeg CMakeLists](https://raw.githubusercontent.com/obsproject/obs-studio/32.2.2/plugins/obs-ffmpeg/CMakeLists.txt))
- `obs-x264`, `obs-nvenc` (native NVENC incl. Linux since 30.2), `obs-qsv11` (QSV incl. Linux), `coreaudio-encoder` (macOS), `obs-libfdk` (optional FDK-AAC). No AMD AMF on Linux (AMF is Windows-only in OBS; on Linux AMD = VAAPI).
- `obs-outputs` — RTMP/RTMPS stream (`librtmp` bundled, mbedTLS), enhanced RTMP HEVC (`rtmp-hevc.c`) and AV1 (`rtmp-av1.c`), FLV output, hybrid MP4/MOV muxer (`mp4-mux.c`), null output; links `happy-eyeballs` (RFC 8305 connection racing) and `bpm`. ([obs-outputs CMakeLists](https://raw.githubusercontent.com/obsproject/obs-studio/32.2.2/plugins/obs-outputs/CMakeLists.txt))
- `obs-webrtc` — **WHIP output + WHIP service only** (no WHEP ingest), via libdatachannel ≥ 0.20 + curl. Simulcast added in 32.1. ([obs-webrtc CMakeLists](https://raw.githubusercontent.com/obsproject/obs-studio/32.2.2/plugins/obs-webrtc/CMakeLists.txt))
- `obs-browser` — Browser Source + browser docks (CEF). Hardware accel on Linux since 31.1, disabled on NVIDIA. ([31.1 changelog](https://www.gamingonlinux.com/2025/07/obs-studio-31-1-0-released-with-multitrack-video-for-linux-explicit-sync-support-for-pipewire/))
- `vlc-video` — VLC source (playlist playback; only if VLC installed).
- `aja`, `decklink` (+ `-output-ui`) — SDI capture/output hardware. Niche; ignore for Prismcast.
- `nv-filters` — NVIDIA RTX audio effects (noise suppression w/ VAD since 32.0), background removal/blur. Windows/NVIDIA-centric.
- `linux-capture` — X11 capture: XSHM display capture, Xcomposite window capture (X11 sessions / XWayland only).
- `linux-pipewire` — Wayland/portal capture: `screencast-portal.c` (unified screen/window via xdg-desktop-portal ScreenCast), `camera-portal.c` (PipeWire camera via Camera portal, requires PipeWire ≥ 0.3.60). Requires PipeWire ≥ 0.3.33, Gio ≥ 2.76. Explicit sync since 31.1. ([linux-pipewire CMakeLists](https://raw.githubusercontent.com/obsproject/obs-studio/32.2.2/plugins/linux-pipewire/CMakeLists.txt))
- `linux-v4l2` — V4L2 device capture (`v4l2-input.c`) **and** virtual camera output (`v4l2-output.c`, writes to a v4l2loopback device). ([linux-v4l2 CMakeLists](https://raw.githubusercontent.com/obsproject/obs-studio/32.2.2/plugins/linux-v4l2/CMakeLists.txt))
- `linux-pulseaudio` — PulseAudio input + output capture **only** (`pulse-input.c`; no per-application capture source on Linux — Windows has "Application Audio Capture" in `win-wasapi`, macOS has it in `mac-capture`; Linux has no equivalent in 32.2.2). ([linux-pulseaudio CMakeLists](https://raw.githubusercontent.com/obsproject/obs-studio/32.2.2/plugins/linux-pulseaudio/CMakeLists.txt))
- `linux-alsa`, `linux-jack`, `sndio`, `oss-audio` — additional audio device backends.
- `rtmp-services` — service catalog (`services.json`) + service-specific handling (Twitch, YouTube, IVS, Restream, etc.).
- `frontend-tools` — automatic scene switcher (disabled on Wayland since 31.0), output timer, captions, scripts entry point. Scripting itself (Lua/Python) lives in libobs (`obs-scripting`).
- `obs-websocket` — remote control API (see Control rows below).

**Filters** — from [obs-filters CMakeLists](https://raw.githubusercontent.com/obsproject/obs-studio/32.2.2/plugins/obs-filters/CMakeLists.txt) (every non-obvious mapping to UI names is per the [Filters guide](https://obsproject.com/kb/filters-guide)):

- Video/effect: Apply LUT (`color-grade-filter.c`), Chroma Key, Color Correction, Color Key, Crop/Pad, Image Mask/Blend (`mask-filter.c`), Luma Key, Render Delay (`gpu-delay.c`), Scaling/Aspect Ratio (`scale-filter.c`), Scroll, Sharpen (`sharpness-filter.c`), HDR Tone Mapping (`hdr-tonemap-filter.c`), **SDR→HDR compose** (`sdr-on-hdr-filter.c`, new in 32.2), Video Delay/Async (`async-delay-filter.c`).
- Audio: Compressor, Expander, Gain, Invert Polarity, Limiter, Noise Gate, 3-Band Equalizer (`eq-filter.c`), Noise Suppression (SpeexDSP / RNNoise via `cmake/speexdsp.cmake` + `cmake/rnnoise.cmake`), VST 2.x (`obs-vst`, uses `vst_header/aeffectx.h` — the VST2 SDK header; **no VST3 in core OBS as of 32.2.2**; [obs-vst CMakeLists](https://raw.githubusercontent.com/obsproject/obs-studio/32.2.2/plugins/obs-vst/CMakeLists.txt)).

**Transitions** — from [obs-transitions CMakeLists](https://raw.githubusercontent.com/obsproject/obs-studio/32.2.2/plugins/obs-transitions/CMakeLists.txt): Cut, Fade, Fade to Color, Luma Wipe, Slide, Stinger (video-file transition), Swipe. Transition duration configurable; Quick Transitions in Studio Mode. ([OBS Studio overview guide](https://obsproject.com/kb/obs-studio-overview))

---

## 3. Feature-parity matrix (PLAN §29 domains)

Status values: `Not started` (no Prismcast code exists yet; phase 0). Dependencies reference our workspace crates (AGENTS.md) and upstream stack components.

### Scenes

| Feature | Pri | OBS 32.2.2 behavior | Desired Prismcast behavior | Status | Dependencies | Tests | Known differences |
|---|---|---|---|---|---|---|---|
| Scene management | P0 | Global, flat list of scenes; scenes are also sources (nesting allowed); names globally unique across scenes+sources; add/remove/rename/duplicate/reorder via dock ([overview guide](https://obsproject.com/kb/obs-studio-overview)) | Same semantics, but all mutations as Core Commands with Core Events; scene = typed `SceneId`, nesting allowed; names need not be globally unique (IDs are newtypes) | Not started | prismcast-core, prismcast-compositor | Unit: command/event round trip, nested scene render; snapshot consistency | OBS uses name-as-identity; we use typed IDs + display names |
| Scene item transforms | P0 | Per-item position/scale/rotation/crop, alignment, bounding-box type, scale filtering, blending mode/method; Edit Transform dialog; relative coordinates since 31.0; undo/redo for these since 32.1 ([31.0 notes](https://obsproject.com/blog/obs-studio-31-0-release-notes), [32.1 notes](https://obsproject.com/blog/obs-studio-32-1-release-notes)) | Same transform model (pos/scale/rot/crop/alignment/bounds/blend), stored relative to canvas; undo from day one via command journal | Not started | prismcast-core (transform model), prismcast-compositor | Property tests: transform math vs reference; undo/redo round trip | Undo is architected in, not retrofitted |
| Groups | P1 | Groups nest items, can be resized as a unit; group bounds bugfixes still landing in 32.2 ([32.2 notes](https://obsproject.com/blog/obs-studio-32-2-release-notes)) | Same concept | Not started | prismcast-core | Group bounds/resize unit tests | — |
| Multiple canvases | P2 | Added 31.1 (scoped to Multitrack Video output); websocket canvas API "partial" since 32.1 ([31.1 changelog](https://www.gamingonlinux.com/2025/07/obs-studio-31-1-0-released-with-multitrack-video-for-linux-explicit-sync-support-for-pipewire/), [32.1 notes](https://obsproject.com/blog/obs-studio-32-1-release-notes)) | Single canvas initially; design domain model so a second canvas (vertical output) is additive, not a rework | Not started | prismcast-core, prismcast-compositor | Model-level: canvas dimension independence | OBS bolted canvases onto a single-canvas model; we should reserve the concept early |

### Sources

| Feature | Pri | OBS 32.2.2 behavior | Desired Prismcast behavior | Status | Dependencies | Tests | Known differences |
|---|---|---|---|---|---|---|---|
| Display capture (Wayland) | P0 | PipeWire "Screen Capture" source via xdg-desktop-portal ScreenCast, unified screen+window selection since 30.2, explicit sync since 31.1, DMA-BUF formats ([linux-pipewire CMakeLists](https://raw.githubusercontent.com/obsproject/obs-studio/32.2.2/plugins/linux-pipewire/CMakeLists.txt)) | Same portal+PipeWire path as the only Wayland route; capture restored/renamed as first-class `ScreenCaptureSource` | Not started | xdg-desktop-portal, PipeWire ≥ 0.3.60, GStreamer pipewiresrc (RES-004) | Integration: capture on GNOME/KDE Wayland; frame integrity; portal revocation recovery | OBS 32.2 had PipeWire-on-NVIDIA failures ([32.2 notes](https://obsproject.com/blog/obs-studio-32-2-release-notes)) — test NVIDIA explicitly |
| Window capture (Wayland) | P0 | Same portal ScreenCast mechanism (window type) | Same | Not started | as above | Window resize/close while capturing | — |
| Display/window capture (X11) | P1 | XSHM display + Xcomposite window capture (`linux-capture`) | Support via XWayland/X11 sessions, lower priority; do not design around it | Not started | X11, GStreamer ximagesrc | X11 session capture test | Wayland-first per PLAN |
| Camera | P0 | V4L2 device source (`linux-v4l2`, udev hotplug); PipeWire camera via Camera portal (PipeWire ≥ 0.3.60) ([linux-pipewire CMakeLists](https://raw.githubusercontent.com/obsproject/obs-studio/32.2.2/plugins/linux-pipewire/CMakeLists.txt)) | Both paths behind one `CameraSource`; hotplug events → Core Events | Not started | V4L2, PipeWire camera portal, GStreamer v4l2src/pipewiresrc | Device hotplug; format/framerate enumeration; disconnect mid-stream | OBS had PipeWire camera framerate bugs as late as 32.1 ([32.1 notes](https://obsproject.com/blog/obs-studio-32-1-release-notes)) |
| Audio input/output capture | P0 | PulseAudio input + output capture (device-level only); ALSA, JACK optional ([linux-pulseaudio CMakeLists](https://raw.githubusercontent.com/obsproject/obs-studio/32.2.2/plugins/linux-pulseaudio/CMakeLists.txt)) | PipeWire-native capture of devices **and per-application streams** | Not started | PipeWire, GStreamer pipewiresrc | Per-app routing test; device hotplug | **Planned improvement over OBS: per-app audio capture does not exist in OBS on Linux** (Windows/macOS only) — PipeWire makes it natural for us |
| Image / slideshow | P0 | Static image, directory slideshow (incl. .webp since 32.2), color source | Same via GStreamer image decoders | Not started | prismcast-media, GStreamer | Decode matrix (png/jpg/webp/gif) | — |
| Media source | P0 | FFmpeg-based file/URL playback, looping, seek slider, hardware decode option, network buffering; HDR playback fixes as late as 32.1 | GStreamer playbin/uridecodebin-backed `MediaSource` with same controls | Not started | prismcast-media (GStreamer) | A/V sync on loop; seek; HDR file tone mapping | OBS uses FFmpeg directly; we use GStreamer (RES-003) |
| Text | P1 | FreeType2 text (Linux), from string or file | Pango-based text (native in GTK stack) or GStreamer textoverlay | Not started | Pango / GStreamer | Rendering, file-watch reload | — |
| Browser source | P1 | CEF-based web page/overlay; Chromium 127, **unsandboxed** in 32.2.2 (see §6); hwaccel on Linux (not NVIDIA); local-file access hardened in 32.1 | Deliberately deferred decision — see §6 and RES-006; if shipped, sandboxed and isolated by construction | Not started | RES-006 outcome | Security review gate | We will not ship an unsandboxed Chromium |
| VLC source | P3 | Playlist playback if VLC installed | Skip — Media Source covers it via GStreamer | Won't do | — | — | Deliberate non-copy |
| Game capture | n/a | Windows-only (`win-capture`); no Linux equivalent in core OBS (community: obs-vkcapture) | Out of scope initially; Wayland game capture is an unsolved upstream problem; revisit via vk-layer/obs-vkcapture research later | Won't do (P3 backlog) | — | — | Known OBS gap on Linux |

### Audio

| Feature | Pri | OBS 32.2.2 behavior | Desired Prismcast behavior | Status | Dependencies | Tests | Known differences |
|---|---|---|---|---|---|---|---|
| Mixer | P0 | Per-source volume/mute, VU meters, 6 assignable tracks, monitoring (off / monitor-only / monitor+output); overhauled UI in 32.1 (vertical default, pinning, hidden/not-in-scene sources) ([32.1 notes](https://obsproject.com/blog/obs-studio-32-1-release-notes), [overview guide](https://obsproject.com/kb/obs-studio-overview)) | Mixer as audio bus graph (`AudioBusId` already a planned ID type); meters streamed as Events; monitoring per bus | Not started | prismcast-audio, GStreamer audiomixer | Meter accuracy, routing matrix, dedup test | OBS mixer state desynced under websocket/plugin edits until 32.2 ([32.2 notes](https://obsproject.com/blog/obs-studio-32-2-release-notes)) — our command/event invariant prevents this class of bug by design |
| Audio deduplication | P1 | Dedup across nested scenes/groups/canvases reworked in 32.0; monitor+output double-capture prevention ([32.0 notes](https://obsproject.com/blog/obs-studio-32-0-release-notes)) | Source instances mixed once by construction (bus graph, not per-scene-instance summation) | Not started | prismcast-audio | Nested-scene audio dedup regression test | Avoid OBS's patch-on-patch history here |
| Audio filters | P1 | Gain, noise gate, suppression (Speex/RNNoise), compressor, limiter, expander, EQ, polarity, VST2 (see §2) | GStreamer-based equivalents (audiocheblimit, webrtcdsp? — decided in RES-003); VST via plugin host later | Not started | prismcast-audio | DSP correctness vs reference | No VST3 in OBS core either; LV2/CLAP is the Linux-native opportunity |
| Sync offset | P1 | Per-source audio sync offset (Advanced Audio Properties); Render Delay / async video delay filters | Delay as explicit pipeline element with typed duration property | Not started | prismcast-audio | A/V sync measurement test | — |

### Video / filters

| Feature | Pri | OBS 32.2.2 behavior | Desired Prismcast behavior | Status | Dependencies | Tests | Known differences |
|---|---|---|---|---|---|---|---|
| Effect filters | P1 | LUT, chroma/luma/color key, color correction, crop/pad, mask/blend, scale, scroll, sharpen, HDR tonemap, SDR→HDR compose (32.2) | Subset first: crop, scale, color correction, chroma key, LUT — mapped to GStreamer elements (RES-003) | Not started | prismcast-media | Per-filter render regression frames | — |
| HDR | P2 | SDR→HDR compose, HDR tonemap filters; HDR capture/encode support on the pipeline | SDR-only P0; HDR tracked as open question (GStreamer HDR support is element/version-dependent) | Not started | RES-003 | — | — |

### Output

| Feature | Pri | OBS 32.2.2 behavior | Desired Prismcast behavior | Status | Dependencies | Tests | Known differences |
|---|---|---|---|---|---|---|---|
| Recording | P0 | Containers: Hybrid MP4/MOV (**default since 32.0**, crash-recoverable, supports chapters/splitting), MKV, FLV, TS, M3U8, plus legacy MP4/MOV ([32.0 notes](https://obsproject.com/blog/obs-studio-32-0-release-notes), [obs-ffmpeg CMakeLists](https://raw.githubusercontent.com/obsproject/obs-studio/32.2.2/plugins/obs-ffmpeg/CMakeLists.txt)) | Fragmented MP4 (fMP4) and MKV as defaults via splitmuxsink-style muxing; EOS-finalization protocol per gstreamer-rust skill | Not started | prismcast-output, GStreamer (mp4mux/matroskamux) | Crash-recovery test (kill -9 mid-recording, file must decode); multi-track audio in file | OBS learned crash-safety the hard way (hybrid MP4); we start with fragmented/recoverable containers |
| RTMP(S) streaming | P0 | librtmp-based, mbedTLS for RTMPS; enhanced RTMP for HEVC/AV1 and multi-track audio (30.2); happy-eyeballs connection racing ([obs-outputs CMakeLists](https://raw.githubusercontent.com/obsproject/obs-studio/32.2.2/plugins/obs-outputs/CMakeLists.txt)) | GStreamer rtmp2sink (FLV/enhanced-RTMP capability check in RES-003) per destination | Not started | prismcast-output, GStreamer rtmp2 | Loopback RTMP server test; TLS; enhanced RTMP HEVC | — |
| Multistream | P0 | **Not in OBS**: one stream output at a time (multitrack video = multiple qualities to *one* service, not multiple destinations); third-party plugins fill the gap | Native `OutputGraph` of independent outputs (PLAN §10/§11): per-output encoder, service, reconnect policy, state, stats; encoded-stream tee when params match | Not started | prismcast-output | Two-destination parallel stream test; slow-destination isolation (bounded queues) | **Core architectural divergence from OBS — deliberate** |
| Multitrack video | P2 | Twitch Enhanced Broadcasting; Windows+NVENC (30.2) → macOS/Linux (31.1); dynamic bitrate (32.2); config limited to Custom service in 32.2 | Defer; revisit when Twitch publishes non-NVENC requirements stable on Linux | Not started | service-specific | — | Don't conflate with multistream |
| Replay buffer | P1 | RAM/disk ring buffer of encoded stream, save-on-hotkey via same muxer ([overview guide](https://obsproject.com/kb/obs-studio-overview)) | Same concept on top of recording pipeline (in-memory GOP ring) | Not started | prismcast-output | Buffer length accuracy; save-while-recording | — |
| Multi-track audio | P1 | Up to 6 tracks recorded into file; multi-track over enhanced RTMP to supporting services | Multi-track recording early; multi-track streaming later | Not started | prismcast-output | Track isolation test in recording | — |
| SRT / RIST | P2 | MPEG-TS over SRT/RIST output + SRT in Media Source; several SRT crash fixes in 32.0 ([obs-ffmpeg CMakeLists](https://raw.githubusercontent.com/obsproject/obs-studio/32.2.2/plugins/obs-ffmpeg/CMakeLists.txt)) | SRT output via GStreamer srtserversink (P2) | Not started | GStreamer srt | SRT listener/caller interop vs OBS/ffmpeg | — |
| WHIP / WebRTC | P2 | WHIP output+service (libdatachannel), audio-only/video-only, HEVC (30.2), simulcast (32.1), STUN restored (31.0) ([obs-webrtc CMakeLists](https://raw.githubusercontent.com/obsproject/obs-studio/32.2.2/plugins/obs-webrtc/CMakeLists.txt)) | WHIP via GStreamer whipsink (P2); WHEP ingest as a *source* is a differentiator OBS lacks | Not started | GStreamer webrtc (whip) | WHIP interop vs reference server | OBS 32.2.2 has no WHEP input |
| HLS output | P3 | HLS mux in obs-ffmpeg (used for recording + some services) | GStreamer hlssink2 later | Not started | — | — | — |
| Virtual camera (Linux) | P1 | Program/Preview/Scene/Source selectable feed written to a v4l2loopback device (`v4l2-output.c`) ([linux-v4l2 CMakeLists](https://raw.githubusercontent.com/obsproject/obs-studio/32.2.2/plugins/linux-v4l2/CMakeLists.txt), [virtual camera guide](https://obsproject.com/kb/virtual-camera-guide)) | Same source-selection model; PipeWire virtual camera node instead of v4l2loopback where consumers support it, v4l2loopback fallback | Not started | prismcast-output, PipeWire, v4l2loopback | Consumer apps (browsers, Meet) see camera; format negotiation | PipeWire virtual cameras still poorly supported by consumers → v4l2loopback likely required; open question |

### Streaming behavior

| Feature | Pri | OBS 32.2.2 behavior | Desired Prismcast behavior | Status | Dependencies | Tests | Known differences |
|---|---|---|---|---|---|---|---|
| Automatic reconnect | P0 | Configurable retry delay + max retries (Advanced settings); multitrack reconnect fixed repeatedly through 31.x–32.0 ([overview guide](https://obsproject.com/kb/obs-studio-overview)) | Per-output `reconnect_policy` in the Output struct (PLAN §10) with backoff; reconnect state machine emits Events | Not started | prismcast-output | Fault-injection: server drop, RST, timeout; verify isolation of other outputs | OBS is single-stream; our reconnect must be per-destination |
| Stream delay | P1 | Global stream delay setting (Advanced); multitrack delay support 31.1 | Per-output delay | Not started | prismcast-output | Delay duration measurement | Per-output vs global |
| Dynamic bitrate | P2 | "Dynamically change bitrate when dropping frames"; extended to multitrack in 32.2 | Encoder-level dynamic bitrate per output (encoder capability permitting) | Not started | prismcast-output | Congestion simulation | — |
| Service profiles | P0 | `rtmp-services` catalog (servers, recommended settings, auth integrations for Twitch/YouTube/IVS) ([plugins tree](https://github.com/obsproject/obs-studio/tree/32.2.2/plugins)) | Versioned service catalog JSON + `ServiceId`; OAuth device-flow for major services later | Not started | prismcast-core, prismcast-output | Catalog schema validation; unknown-field tolerance (schema versioning law) | — |

### UI

| Feature | Pri | OBS 32.2.2 behavior | Desired Prismcast behavior | Status | Dependencies | Tests | Known differences |
|---|---|---|---|---|---|---|---|
| Studio mode | P1 | Preview/Program split, transition or quick-transition to swap, preview editing invisible to output ([overview guide](https://obsproject.com/kb/obs-studio-overview)) | Same, but preview/program are Core states (controllable remotely), UI is one controller | Not started | prismcast-core, prismcast-ui | Remote (WS) studio-mode transition equals UI behavior | OBS studio mode is frontend state; ours is core state |
| Scene transitions | P1 | Cut/Fade/Fade-to-color/Luma-wipe/Slide/Stinger/Swipe; duration; per-scene transition override; quick transitions ([obs-transitions CMakeLists](https://raw.githubusercontent.com/obsproject/obs-studio/32.2.2/plugins/obs-transitions/CMakeLists.txt)) | Cut/fade/luma-wipe first via compositor; stinger later | Not started | prismcast-compositor | Frame-accurate transition duration; mid-transition re-trigger | — |
| Multiview | P2 | Grid view of scenes + preview/program (long-standing; blank-multiview fixes still landing in 32.0) | P2, after core stabilizes | Not started | prismcast-ui | — | — |
| Projectors | P2 | Fullscreen/windowed projector of preview/program/scene/source (source projector selectable) | Same via GTK windows on target monitor | Not started | prismcast-ui | Multi-monitor placement | Wayland fullscreen-on-monitor quirks |
| Statistics | P1 | Stats dock: CPU, render/encode lag, dropped frames, bitrate, per-output data | Stats as Core Events/snapshot data, rendered by any controller (incl. CLI/Web) | Not started | prismcast-core | Stat accuracy vs measured pipeline | OBS stats are UI-side; ours come from core |
| Undo/redo | P1 | Since 28.0; extended to more scene-item actions in 32.1 (still incomplete coverage) | Command journal from day one (PLAN mandates undo in core) | Not started | prismcast-core | Journal replay equals live state | Full coverage by construction |
| Missing-files handling | P1 | Missing Files dialog (recursive search, 31.1); filters got missing-file support in 32.2 | Core Event for missing asset + UI resolution flow | Not started | prismcast-core | Missing file on load → event | — |

### Config

| Feature | Pri | OBS 32.2.2 behavior | Desired Prismcast behavior | Status | Dependencies | Tests | Known differences |
|---|---|---|---|---|---|---|---|
| Profiles | P1 | Profile = output/video/audio/service settings; app vs user config split in 31.0 ([profiles guide](https://obsproject.com/kb/profiles), [31.0 notes](https://obsproject.com/blog/obs-studio-31-0-release-notes)) | `ProfileId`-keyed settings bundles, schema-versioned | Not started | prismcast-core (persistence) | Migration test from v1 schema; unknown-field preservation | OBS JSON is loosely versioned; ours must be explicitly versioned (project law) |
| Scene collections | P1 | Collection = scenes/sources/layout, JSON file, import/export ([scene collections guide](https://obsproject.com/kb/scene-collections)) | Same + import of OBS scene collections (compatibility path, P2) | Not started | prismcast-core | Round-trip fidelity; OBS import fuzz test | OBS import is a differentiator for adoption |
| Auto scene switcher | P3 | frontend-tools; **disabled on Wayland** (cannot enumerate windows) ([31.0 notes](https://obsproject.com/blog/obs-studio-31-0-release-notes)) | Defer; Wayland needs portal/window-ID cooperation — likely automation-via-IPC instead of built-in | Won't do (P3) | — | — | Do not clone a feature that is broken-by-platform |
| Output timer | P3 | frontend-tools stopwatch for stream/record | Automation via CLI/IPC covers it | P3 | — | — | — |

### Control

| Feature | Pri | OBS 32.2.2 behavior | Desired Prismcast behavior | Status | Dependencies | Tests | Known differences |
|---|---|---|---|---|---|---|---|
| WebSocket API | P0 | obs-websocket 5.7.4, JSON-RPC-ish request/response + event subscriptions + batch requests; categories: General, Config, Sources, Scenes, Inputs, SceneItems, Filters, Transitions, Outputs, Record, Stream, MediaInputs, Ui, Canvases (partial); disabled by default, password auto-generated ([obs-websocket CMakeLists](https://raw.githubusercontent.com/obsproject/obs-websocket/1ef34bf48110c2a18184e50e41cd0b1a855e2147/CMakeLists.txt), [protocol docs](https://github.com/obsproject/obs-websocket/blob/master/docs/generated/protocol.md)) | Native WS server exposing the Core Command/Event API (same shapes as IPC); auth on by default; see RES-007 | Not started | prismcast-protocol, prismcast-remote | Protocol conformance; auth required; event subscription filters | OBS bolted WS onto frontend; ours IS the core API |
| IPC | P0 | None (websocket only; no local socket API) | Unix-domain socket IPC, same protocol as WS | Not started | prismcast-remote | Same conformance suite as WS | Pure addition vs OBS |
| Web UI | P1 | None in core (browser docks are web content, not control UI) | axum web UI driving the same API | Not started | prismcast-web | E2E: web scene switch equals UI | Addition vs OBS |
| CLI | P1 | None in core (obs-cli via websocket is third-party) | prismcast-cli driving the same API | Not started | prismcast-cli | Golden-path script test | Addition vs OBS |
| Hotkeys | P1 | Global hotkeys for stream/record/replay/scene/source ops ([overview guide](https://obsproject.com/kb/obs-studio-overview)); Linux hotkeys still get fixes in 31.1 | Hotkey layer maps to Core Commands only (PLAN §54); Wayland global shortcuts need portal/XDG shortcuts — open question | Not started | prismcast-ui (+portal) | Hotkey→command mapping test | Wayland global input capture is restricted; likely portal GlobalShortcuts or in-app only at first |

### Plugins / extensibility

| Feature | Pri | OBS 32.2.2 behavior | Desired Prismcast behavior | Status | Dependencies | Tests | Known differences |
|---|---|---|---|---|---|---|---|
| Plugin system | P2 | Native C modules, ABI-tied; version-gated loading (32.0); basic plugin manager UI (32.0); Lua/Python scripting in libobs | Out-of-process plugins with versioned message protocol (PLAN §55); WASM/native/Lua investigated later | Not started | prismcast-plugin-sdk | Plugin crash isolation test | PLAN §79: do not copy plugin ABI constraints |
| Browser docks | P3 | CEF panels (YouTube chat etc.) | Skip; web UI covers remote needs | Won't do | — | — | — |

### Linux platform

| Feature | Pri | OBS 32.2.2 behavior | Desired Prismcast behavior | Status | Dependencies | Tests | Known differences |
|---|---|---|---|---|---|---|---|
| PipeWire capture | P0 | Portal ScreenCast + camera (see Sources); explicit sync 31.1; NVIDIA failure fixed in 32.2 | Same path, GStreamer pipewiresrc | Not started | RES-004 | See Sources rows | — |
| Wayland session | P0 | Supported via portal capture; some features degrade (scene switcher off, global hotkeys limited) | Wayland-first, X11 secondary | Not started | RES-004 | GNOME + KDE Wayland smoke matrix | — |
| Virtual camera | P1 | v4l2loopback output (see Output row) | PipeWire node + v4l2loopback fallback | Not started | RES-004 | See Output row | — |

### UX / themes

| Feature | Pri | OBS 32.2.2 behavior | Desired Prismcast behavior | Status | Dependencies | Tests | Known differences |
|---|---|---|---|---|---|---|---|
| Themes | P1 | Base theme + variants system (30.2), density/font options (31.1), custom Qt CSS | libadwaita light/dark + accent; no arbitrary CSS theming initially | Not started | prismcast-ui | Screenshot regression | Different theming model (platform-native vs skinnable) |

---

## 4. Linux-relevant gaps and pain points in OBS 32.2.2 (opportunities for Prismcast)

1. **No per-application audio capture on Linux** — only whole-device PulseAudio capture; Windows/macOS have app capture ([linux-pulseaudio CMakeLists](https://raw.githubusercontent.com/obsproject/obs-studio/32.2.2/plugins/linux-pulseaudio/CMakeLists.txt)). PipeWire gives us per-node streams essentially for free.
2. **No game capture on Linux** — `win-capture` is Windows-only; the community obs-vkcapture fills the gap via Vulkan/OpenGL layers.
3. **Automatic scene switcher disabled on Wayland** ([31.0 notes](https://obsproject.com/blog/obs-studio-31-0-release-notes)).
4. **Single stream output** — multistreaming requires third-party plugins; OBS's "multitrack video" is multi-quality to one service, not multi-destination (our headline differentiator, PLAN §10–11).
5. **PipeWire/NVIDIA fragility** — capture failure on NVIDIA fixed only in 32.2 ([32.2 notes](https://obsproject.com/blog/obs-studio-32-2-release-notes)); browser-source hwaccel disabled on NVIDIA (31.1).
6. **State desync under remote control** — mixer state desync via websocket/plugins fixed only in 32.2; evidence for our command/event invariant (PLAN §76).
7. **Browser source is a security liability** — see §6.

## 5. What OBS does that PLAN deliberately will not copy (cross-check vs PLAN §79)

Confirmed against §79: Qt-frontend-coupled state changes (mixer desync bug class), single-stream output model, plugin ABI coupling (32.0's version-gating shows the maintenance cost), OS-abstraction burden, legacy migration debt (31.0 removed pre-28.1 migrations). Domain *concepts* (Source/Scene/SceneItem/Filter/Encoder/Output/Service/Profile/SceneCollection/Transition) all map cleanly to our planned crates.

## 6. Security findings (affect browser-source planning, RES-006)

- OBS 32.2.2 (latest stable) embeds **Chromium 127.0.6533.120 / V8 12.7.224.18** (from July 2024) and runs CEF with **`no_sandbox = true`**. A 2026-09-22 disclosure demonstrated **remote code execution from a Twitch chat message** via an XSS-prone overlay + a known-exploited V8 bug (CVE-2024-7971), on stock OBS settings. Fixes (CEF 128+ upgrade, sandbox re-enablement) were merged to master 2026-09-10/17 and target **33.0 — development, unreleased** ([SCRT disclosure](https://blog.scrt.ch/2026/09/22/how-one-twitch-chat-message-became-code-execution-on-a-streamers-pc/)).
- obs-websocket is **disabled by default and auto-generates a password** when enabled ([SCRT disclosure](https://blog.scrt.ch/2026/09/22/how-one-twitch-chat-message-became-code-execution-on-a-streamers-pc/)). Our remote API must likewise default to authenticated + localhost-only binding.

## 7. Conclusions for Prismcast

1. **Recording: start with fragmented MP4 + MKV only.** OBS converged on crash-recoverable "Hybrid MP4/MOV" as default in 32.0 after years of corrupt-file complaints. Our equivalent is GStreamer `mp4mux` (fragmented mode) / `matroskamux` behind splitmuxsink-style management; skip non-fragmented MP4 entirely. (GStreamer element confirmation is RES-003's job.)
2. **Output graph is our core divergence — keep it.** OBS 32.2.2 confirms single-stream + reconnect-per-app design with recurring reconnect/race fixes (30.2, 31.0, 31.1). Per-output `reconnect_policy`, `state`, `statistics` (PLAN §10) directly addresses this. No change to plan; this research strengthens the rationale.
3. **Audio: design the bus graph to make OBS's dedup/mixer-desync bug classes impossible.** Mixer state desync via remote control was fixed only in 32.2; audio dedup was reworked in 32.0. Both are symptoms of frontend-owned state and per-instance summation. Our command/event invariant + bus-graph mixer avoids both by construction — write regression tests named after these OBS bugs.
4. **Per-app audio capture is a cheap, real differentiator on Linux.** OBS on Linux still only captures whole devices. PipeWire per-node capture should be a P1 `ApplicationAudioSource` — verify node/stream enumeration feasibility in RES-004.
5. **Browser source: do not commit to CEF.** The Sept-2026 RCE disclosure (unsandboxed Chromium 127 in the current stable release) validates caution. Alternatives to evaluate in RES-006: sandboxed WebKitGTK (`webkitgtk` 6.x has multi-process sandboxing), out-of-process CEF with sandbox on, or no browser source in v1. This may warrant an **ADR** once RES-006 completes.
6. **Remote-first is validated.** OBS's control surface is a bolt-on (obs-websocket 5.7.4, disabled by default); feature requests around canvas support ("partial" in 32.1) show the cost of retrofitting. Our WS/IPC/CLI/Web as interchangeable controllers of one command/event API remains the right call. Auth-on-by-default and localhost-binding-by-default are requirements, matching OBS's hardening direction.
7. **Hotkeys on Wayland are a risk item.** OBS still fixes Linux hotkey issues (31.1) and global hotkeys need the XDG GlobalShortcuts portal or compositor-specific support. Flag for ARCH/UI tasks: hotkey commands must exist in core first (PLAN §54 already says this); global capture mechanism is an open question.
8. **Scene collection import from OBS is a P2 adoption feature worth scheduling.** OBS scene collections are plain JSON with documented-enough structure; 31.0's relative-coordinates change shows we must version-test against 32.x files specifically.
9. **Canvases: reserve in the domain model now, implement later.** OBS retrofitted multiple canvases (31.1, scoped to multitrack; websocket support "partial" in 32.1). A `CanvasId` in the domain model costs little now and avoids their rework; vertical-canvas output (Shorts/TikTok) is the motivating use case.
10. **Service catalog**: copy the *idea* of `rtmp-services` (curated, versioned service JSON with recommended settings) but with explicit schema versioning per project law.
11. **No action needed on**: VST (OBS core is VST2-only; LV2/CLAP hosting is the Linux-native answer, P2+), FTL (dead), Decklink/AJA (niche), game capture (unsolved upstream; keep P3 backlog), Qt themes (libadwaita native look instead).

### Open questions to hand to follow-up tasks

- RES-003: GStreamer equivalents for each filter/encoder/output listed here; fMP4 + multi-track muxing support; whipsink maturity; enhanced-RTMP (HEVC/AV1/multitrack) capability of `rtmp2sink`.
- RES-004: PipeWire per-app audio node enumeration; portal GlobalShortcuts for hotkeys; PipeWire virtual camera vs v4l2loopback consumer compatibility; NVIDIA explicit-sync capture testing.
- RES-006: browser-source engine decision (→ ADR).
- RES-007: obs-websocket 5.7.4 request/event surface — decide compatibility vs clean-slate protocol (→ likely ADR input).
