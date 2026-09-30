# RES-006 — Browser Source Research (WebKitGTK 6, WPE, CEF)

Research milestone from PLAN §15: **can WebKitGTK 6 efficiently export rendered frames into
DMABUF/GStreamer?** If not, evaluate an isolated browser renderer process and alternatives.

Date: 2026-09-30. Baseline: OBS Studio **32.2.2** (released 2026-08-14) is the reference for
shipped functionality; OBS **33.x** documentation and release candidates are development/master
material and are marked as such below.

## Short answer

**No.** WebKitGTK 6 (the GTK4 widget API) has no public API to hand rendered frames to an
embedder as DMA-BUF or GStreamer buffers. Internally WebKitGTK renders GPU-side and shares
composited buffers between its web and UI processes as DMA-BUF, but the only public frame-extraction
API is `webkit_web_view_get_snapshot()`, an async, CPU-readback snapshot meant for thumbnails — not
a 60 FPS frame source ([WebKitGTK API docs](https://webkitgtk.org/reference/webkitgtk/stable/method.WebView.get_snapshot_finish.html)).

**The efficient path is WPE WebKit**, the toolkit-free sibling port of the same engine, via
GStreamer's WPE source elements. The new **`wpe2` plugin in GStreamer 1.28** (stable, released
2026-01-27) uses the WPE Platform API and imports the web page's GPU buffer as an EGLImage into
`GLMemory` — GPU-resident frames, directly pluggable into our media graph, with browser engine
process isolation included for free. Its gaps today: **no audio**, no `memory:DMABuf` caps, and
no Rust bindings for the WPE control API.

## Version landscape (verified 2026-09-30)

| Component | Current stable | Notes |
|---|---|---|
| WebKitGTK | **2.54.0** (2026-09-16) | webkitgtk-6.0 API stable since 2.40 (2023-03); previous stable series 2.52.x ([release index](https://webkitgtk.org/releases/), [2.54.0 announcement](https://webkitgtk.org/2026/09/16/webkitgtk2.54.0-released.html), [GNOME blog on 6.0 stability](https://blogs.gnome.org/mcatanzaro/2023/03/21/webkitgtk-api-for-gtk-4-is-now-stable/)) |
| WPE WebKit | 2.54.0 | same engine, toolkit-free port; **WPEPlatform is the default (and only needed) integration layer since 2.54**; libwpe + WPEBackend-FDO declared legacy ([WPE architecture](https://wpewebkit.org/about/architecture.html)) |
| GStreamer | **1.28.7** (2026-09-07) | 1.28 introduced the `wpe2` plugin; 1.26 is the previous stable series ([1.28 release notes](https://gstreamer.freedesktop.org/releases/1.28/)) |
| `webkit6` Rust crate | **0.6.1** (2026-03-11) | gtk-rs-family bindings to webkitgtk-6.0; feature flags up to `v2_52`; deps gtk4 0.11 / glib 0.22; ~43k downloads/month ([lib.rs/crates/webkit6](https://lib.rs/crates/webkit6)) |
| OBS 32.2.2 (released baseline) | CEF **127/6533** (Chromium 127) | Chromium 127 is EOL; see security note below ([obs-browser](https://github.com/obsproject/obs-browser), [OBS 33.0 release notes](https://github.com/obsproject/obs-studio/releases)) |
| OBS 33.0 (development) | CEF **150/7871** (Chromium 150) | "Updated CEF from 127/6533 to 150/7871" — unreleased at baseline date ([OBS 33.0 release notes](https://github.com/obsproject/obs-studio/releases), [Linuxiac summary](https://linuxiac.com/obs-studio-33-nears-release-heres-what-to-expect/)) |

Security note carried over from RES-001: OBS 32.2.2's shipped CEF is Chromium 127 (EOL); a
September 2026 SCRT disclosure documents an unsandboxed Chromium 127 RCE against it, with the fix
landing only via the CEF upgrade targeting OBS 33.0. Whatever engine we pick must track security
updates through the distro, not a vendored binary.

## Option A — WebKitGTK 6 widget (`webkit6-rs`)

### What the API offers

`WebKitWebView` is a GTK4 widget. The webkit6 crate exposes it faithfully
([WebViewExt docs](https://world.pages.gitlab.gnome.org/Rust/webkit6-rs/stable/latest/docs/webkit6/prelude/trait.WebViewExt.html)):

- `snapshot()` / `snapshot_future()` → `gdk4::Texture`, BGRA8888, **async one-shot**
  ([get_snapshot_finish](https://webkitgtk.org/reference/webkitgtk/stable/method.WebView.get_snapshot_finish.html)).
- `background_color` setter takes a `GdkRGBA` — alpha 0 gives a transparent background, provided the
  page itself doesn't paint one ([set_background_color](https://webkitgtk.org/reference/webkitgtk/stable/method.WebView.set_background_color.html)).
- JS bridge: `evaluate_javascript`, `call_async_javascript_function` (GVariant args, Promise-aware),
  plus script-message handlers on `WebKitUserContentManager` returning `JSCValue`
  ([migration guide](https://webkitgtk.org/reference/webkitgtk/stable/migrating-to-webkitgtk-6.0.html)).
- Process model: UI process + per-origin web processes + single global network process; **the
  bubblewrap sandbox is mandatory in 6.0** (the enable/disable API was removed; extra paths are
  mounted via `webkit_web_context_add_path_to_sandbox()`); **process swap on cross-site navigation
  is mandatory**; web process death surfaces as the `web-process-terminated` signal and an
  `is-web-process-responsive` property ([migration guide](https://webkitgtk.org/reference/webkitgtk/stable/migrating-to-webkitgtk-6.0.html), [WebView docs](https://webkitgtk.org/reference/webkitgtk/stable/class.WebView.html)).
- The GObject DOM API (`WebKitDOM*`) was **removed without replacement** in 6.0; DOM manipulation
  goes through JavaScript evaluation or the `JavaScriptCore` GLib API. `WebKitWebExtension` was
  renamed `WebKitWebProcessExtension` and upstream warns the whole web-process API "may
  unfortunately be removed in the future" ([migration guide](https://webkitgtk.org/reference/webkitgtk/stable/migrating-to-webkitgtk-6.0.html)) — do not build our JS bridge on it.

### Why it cannot be the media-path frame source

1. **No frame export API.** WebKitGTK's DMA-BUF renderer (default since 2.42, unified across
   Wayland/X11 in the 2.44 cycle) shares the composited page as DMA-BUF between web and UI
   processes, but that buffer is consumed by GTK's renderer inside the widget — there is no public
   handle to it ([Igalia: accelerated compositing rendering](https://blogs.igalia.com/carlosgc/2023/04/03/webkitgtk-accelerated-compositing-rendering/), [What's new in 2.44](https://webkitgtk.org/2024/03/27/webkigit-2.44.html), [Phoronix](https://www.phoronix.com/news/WebKitGTK-DMA-BUF-Rendering)).
2. **Snapshot is thumbnail-grade.** It re-renders/readbacks asynchronously to a `GdkTexture`;
   designed for occasional captures (tab thumbnails), not a 30/60 FPS stream. No damage callbacks
   are exposed for arbitrary widgets in GTK4 — offscreen GDK surfaces and their damage events "have
   no replacement in GTK 4.x" ([GTK 3→4 migration guide](https://docs.gtk.org/gtk4/migrating-3to4.html)), which kills the classic GTK3 `GtkOffscreenWindow` trick (see prior art below).
3. **A widget must be mapped to render.** A browser source that keeps animating while its preview
   is hidden (multiview, scene not active but source still composited into the output) is exactly
   what a desktop widget won't do reliably. PLAN §15 already warns: do not couple browser rendering
   to desktop WebView widgets.
4. **Audio cannot be rerouted.** The web view plays audio through WebKit's internal GStreamer
   playback pipeline to the default audio sink; the public API only exposes `is-muted` and
   `is-playing-audio` ([WebView docs](https://webkitgtk.org/reference/webkitgtk/stable/class.WebView.html)). Capturing that audio for our mixer would require PipeWire per-stream
   capture as a side channel — fragile for per-source mute/volume semantics.

**Verdict:** `webkit6` is the right crate for UI-side web content (browser docks, service login
flows, the in-app web panels) in `prismcast-ui`. It is not a source for the media graph.

## Option B — WPE WebKit via GStreamer (`wpesrc` / `wpe2`)

WPE is the official toolkit-free WebKit port; it renders off-screen by design and hands the
embedder GPU buffers instead of widgets ([WPE architecture](https://wpewebkit.org/about/architecture.html)).

### Legacy plugin: `wpe` (`wpesrc`, `wpevideosrc`, since GStreamer 1.16)

- Source/bin in gst-plugins-bad; `wpesrc` = URI-handling bin with **sometimes audio pads**
  (`audio_%u`, raw F32LE/F64LE/S16LE) around `wpevideosrc`
  ([wpesrc element docs](https://gstreamer.freedesktop.org/documentation/wpe/wpesrc.html?gi-language=c)).
- Video caps: `video/x-raw(memory:GLMemory), format=RGBA` (GPU) or `video/x-raw, format=BGRA`
  (software fallback via WPEBackend-FDO ≥1.6 SHM path)
  ([element docs](https://gstreamer.freedesktop.org/documentation/wpe/wpesrc.html?gi-language=c)).
- Action signals `run-javascript`, `load-bytes`, signal `configure-web-view`; properties
  `location`, `draw-background`.
- **Structural problem:** it is built on `wpe-webkit-1.0` + `wpebackend-fdo-1.0`, the stack WPE
  2.54 just declared legacy, and DMA-BUF output is an unimplemented TODO in the source
  ("DMABuf support (requires changes in WPEBackend-fdo to expose DMABuf planes and fds)"). Avoid
  for new development.

### Current plugin: `wpe2` (`wpevideosrc2`, since GStreamer 1.28, 2026-01-27)

Release notes: "New wpe2 plugin that makes use of the 'WPE Platform API' with support for rendering
into GL and SHM buffers and navigation events (**but not audio yet**)"
([GStreamer 1.28 release notes](https://gstreamer.freedesktop.org/releases/1.28/)).

Verified against the 1.29.2 source (Debian packaging of gst-plugins-bad, `ext/wpe2/`):

- Builds against `wpe-webkit-2.0 >= 2.50` with `wpe/wpe-platform.h`; registers **`wpevideosrc2`**
  (rank none). No `wpesrc2` bin yet — the debug category exists but only the video source is
  registered, consistent with "no audio yet".
- Implements a custom `WPEDisplayGStreamer` (a `WPEDisplay` subclass) that supplies the EGL display
  and DRM device from the GstGL context — i.e. the web page is composited onto our GStreamer GL
  context, no Wayland compositor required.
- Buffer import: `wpe_buffer_import_to_egl_image()` → wrapped as `GstEGLImage`/`GstGLMemory`
  (GPU-resident, DMA-BUF-backed `WPEBufferDMABuf` underneath), with `wpe_buffer_import_to_pixels()`
  SHM fallback for the raw BGRA path (`gstwpethreadedview.cpp`).
- Caps: `video/x-raw(memory:GLMemory), format=RGBA` and `video/x-raw, format=BGRA` — **no
  `memory:DMABuf` caps feature yet** (gstwpevideosrc.cpp). GLMemory is still encoder-friendly: our
  RES-005 matrix shows `nvav1enc`/`nvh264enc` accept CUDA/GL memory directly, and VA encoders take
  DMABuf via `vapostproc` import — so GLMemory is not a hard blocker, but expect one copy/conversion
  on the VA path unless upstream adds DMABuf caps.
- Control surface: properties `location`, `draw-background` (transparency when false + transparent
  page CSS; note WebKit prefers opaque DMA-BUF formats only when the page background is opaque —
  [WebKit bug 270964](https://bugs.webkit.org/show_bug.cgi?id=270964)); signals `wpe-view-created`,
  `configure-web-view`; action signals `load-bytes`, `run-javascript`. Size/framerate come from caps
  negotiation (`fixate`), not properties.
- WPEPlatform buffer model: composited pages are handed over as `WPEBuffer` — `WPEBufferDMABuf` on
  GPU systems, `WPEBufferSHM` as fallback; a headless platform (`WPEDisplayHeadless`) exists for
  off-screen rendering/CI ([WPE architecture](https://wpewebkit.org/about/architecture.html)).

### What WPE gives us for free

- The same multiprocess isolation as WebKitGTK (web/network processes, mandatory sandbox) — a
  crashed renderer kills one element's output, not the studio ([webkitgtk.org](https://webkitgtk.org/)).
- The same GStreamer-based in-page media stack as WebKitGTK: with GStreamer ≥ 1.24, in-page video
  decode/render uses the DMA-BUF sink with DRM modifiers ([What's new in WebKitGTK 2.44](https://webkitgtk.org/2024/03/27/webkigit-2.44.html)).
- Engine security updates ride the distro's `wpewebkit` packages, matching our no-vendored-binary
  stance.

### Gaps and risks

- **No audio in wpe2** (as of 1.28.x). Options: (a) contribute/wait for upstream audio pads —
  Philippe Normand (Igalia) is the active maintainer of both the plugin and WebKit's GStreamer
  backend, so this is a plausible upstream contribution; (b) PipeWire per-stream capture of the
  web process's audio as a stopgap (weak per-source semantics); (c) accept video-only browser
  sources at Phase 11 and ship audio in a follow-up.
- **No Rust bindings for the WPE control API.** The `webkit6` crate targets webkitgtk-6.0 (GTK4);
  there is no gtk-rs-family binding for `wpe-webkit-2.0`. The C API is largely the same
  `WebKitWebView` API ("the central class of the WPE WebKit and WebKitGTK APIs" —
  [WebView docs](https://webkitgtk.org/reference/webkitgtk/stable/class.WebView.html)), so a thin
  internal FFI shim (via `glib`/`gobject` pointers obtained from the `configure-web-view` signal)
  or a small upstream gir-based binding are the options. Open question.
- **Distro availability of the element.** wpe2 needs `wpe-webkit-2.0 ≥ 2.50` at gst-plugins-bad
  build time; several distros build gst-plugins-bad without WPE. Runtime probing
  (`gst_element_factory_make("wpevideosrc2")`) with a clear user-facing feature check is mandatory;
  Flatpak packaging may need to build the plugin + WPE WebKit into the runtime. Open question,
  ties into the packaging work.
- **Young code.** wpe2 is one stable cycle old; expect sharp edges (negotiation quirks, teardown
  races). Prototype early with deterministic test pages, per the gstreamer-rust skill guidance.

## Option C — CEF (the OBS approach)

How OBS's browser source works, for reference ([obs-browser repo](https://github.com/obsproject/obs-browser), [DeepWiki architecture summary](https://deepwiki.com/obsproject/obs-browser/2-browser-source)):

- CEF off-screen rendering (OSR). Released path (OBS 32.2.2, CEF 6533): `OnPaint()` delivers a CPU
  buffer per frame; uploaded to an OBS texture. Hardware path via `OnAcceleratedPaint()` shared
  textures (Windows DXGI, macOS IOSurface); **Linux DMA-BUF shared-texture support is part of the
  CEF 7871 update in development OBS 33.x**, not the released baseline. CEF's GPU-accelerated OSR on
  Linux has a long history of breakage ([electron#41972](https://github.com/electron/electron/issues/41972), [cef#4166](https://github.com/chromiumembedded/cef/issues/4166)).
- Property surface (32.2.2): URL / local file, width, height, FPS, custom CSS (default CSS makes
  the page transparent), shutdown-when-not-visible, refresh-when-scene-active, webpage control
  permission level, reroute-audio ([DeepWiki property table](https://deepwiki.com/obsproject/obs-browser/2-browser-source)) — this is the parity target for our own property schema.
- JS bridge: `window.obsstudio` object with permission levels 0–5, `DispatchJSEvent` for OBS→page
  events, obs-websocket `emit_event` vendor request ([obs-browser README](https://github.com/obsproject/obs-browser)).
- Audio: CEF audio handler → `OnAudioStreamPacket()` → `obs_source_output_audio()`.

Why CEF is a poor fit for Prismcast even though OBS chose it:

1. **Packaging/runtime weight.** CEF is a Chromium fork; binaries come from
   [cef-builds.spotifycdn.com](https://cef-builds.spotifycdn.com/index.html) and OBS vendors its own
   patched fork ([obsproject/cef](https://github.com/obsproject/cef)). We would inherit a second
   entire browser stack alongside the system's WebKit, plus a C++ FFI layer with no maintained
   gtk-rs-grade Rust bindings.
2. **Security posture.** Released OBS ships an EOL Chromium 127 (see version landscape). Tracking
   CEF security ourselves is a recurring tax; WebKitGTK/WPE updates come with the distro.
3. **No GStreamer integration.** CEF frames arrive via callback, not as GstBuffers; every frame
   would cross a hand-rolled upload boundary, and in-page media decode would not share our
   GStreamer hardware decode stack.
4. **Wayland.** obs-browser's docks/service-integration features are disabled on Wayland in OBS;
   CEF+GTK4+Wayland coexistence is its own problem we don't need.

## Option D — isolated renderer process (custom), and prior art

PLAN §15's fallback ("consider an isolated browser renderer process") has working precedents:

- **fzwoch/obs-webkitgtk** (GPL-2.0, 2020): helper process runs WebKitGTK in a GTK3
  `GtkOffscreenWindow`, captures on `damage-event`, writes BGRA frames over a pipe to the OBS
  plugin; hardware acceleration disabled (`WEBKIT_HARDWARE_ACCELERATION_POLICY_NEVER`)
  ([repo](https://github.com/fzwoch/obs-webkitgtk)). Proof that process isolation + SHM frames
  works, but the GTK3 offscreen-window + damage-event mechanism it relies on has no GTK4
  replacement ([GTK migration guide](https://docs.gtk.org/gtk4/migrating-3to4.html)) — this design
  cannot be ported to WebKitGTK 6 as-is.
- **OBS WebKitGTK Browser (BitHeaven, 2026-07)**: current community plugin; isolated renderer
  process, transparent BGRA frames to OBS via shared memory, XComposite/XShm capture with XDamage
  change detection, "hardware-accelerated WebKit compositor using DRM/DMA-BUF", page audio capture,
  automatic shutdown when hidden — but WebKitGTK **4.1 (GTK3)**, X11/XWayland only, Alpine-focused
  ([OBS forum resource](https://obsproject.com/forum/resources/webkitgtk-browser.2607/)). Again:
  the architecture is validated, the toolkit generation is not ours.

If wpe2's gaps (audio) become blocking, the honest fallback is not reviving GTK3 offscreen tricks
but a small WPEPlatform-based renderer helper of our own (the same API wpe2 uses) that pushes
DMA-BUF/SHM frames over PipeWire or GStreamer's `intersrc`/shmsrc into the media graph. That is a
build-it-only-if-needed path; wpe2 already does the hard part.

## Requirement mapping (PLAN §15 list)

| Requirement | wpe2 (1.28) | Notes |
|---|---|---|
| URL | `location` property | also `web+https://` URI handling via gst-play; load progress via `wpe-stats` element messages |
| Local file | `location=file://...` or `load-bytes` action signal | |
| Transparent background | `draw-background=false` + page CSS; RGBA alpha preserved | upstream prefers opaque DMA-BUF formats only when page bg is opaque ([bug 270964](https://bugs.webkit.org/show_bug.cgi?id=270964)) |
| Viewport width/height | caps negotiation (`fixate`) | matches OBS width/height semantics |
| FPS | caps framerate | OBS uses an FPS property; ours is a caps concern |
| Custom CSS | inject via `run-javascript` / user stylesheet at `configure-web-view` | |
| Reload | reload via WebKit API on the view object / set `location` | |
| Shutdown when hidden | destroy/pause the element (pipeline state) | renderer process dies with it |
| Reload when activated | recreate + load | cheap since engine processes spawn fast |
| Audio | **not yet in wpe2** | biggest gap; see risks |
| Sandboxing | mandatory bubblewrap sandbox in WebKitGTK 6.0 / WPE | |
| JS bridge | `run-javascript` action signal; richer bridge needs WPE control API from Rust | no Rust WPE bindings — open question |

## Conclusions for Prismcast

1. **Answer to the PLAN §15 milestone:** WebKitGTK 6 cannot efficiently export frames to
   DMA-BUF/GStreamer through any public API. Treat it as settled: **no browser frames through the
   GTK widget**. `webkit6` (crate 0.6.x) is still the right choice for *UI* web views (docks,
   auth flows) inside `prismcast-ui`.
2. **Browser source = a GStreamer source element, not a widget.** Target **`wpevideosrc2`
   (GStreamer ≥ 1.28, WPE WebKit ≥ 2.50, WPEPlatform)** behind our media-engine source trait.
   Frames arrive as `GLMemory` RGBA (GPU) or raw BGRA (software fallback) — directly compositable
   by our GL compositor and feedable to encoders (NVENC takes GL memory; VA-API needs a
   `vapostproc` import — acceptable, and likely to improve if upstream adds `memory:DMABuf` caps).
3. **Process isolation comes free** from WPE's multiprocess model + mandatory sandbox, satisfying
   PLAN §53's "isolated browser renderer" without a custom helper process. A crashed web process
   surfaces as an element error/EOS on a bounded channel — map it to a Core Event and let the
   source actor restart it.
4. **Audio is the decision point.** wpe2 has no audio pads today (verified in 1.28 release notes
   and 1.29 source). Recommended: prototype video-only first (Phase 11), evaluate upstreaming audio
   to wpe2 (maintainer is the WebKit GStreamer lead — receptive venue), and only if that stalls,
   consider a custom WPEPlatform renderer helper or PipeWire side-channel capture. **This should
   trigger an ADR**: "browser source engine = WPE via wpe2; audio strategy" before Phase 11 starts.
5. **Minimum versions drift upward.** Requiring wpe2 implies GStreamer ≥ 1.28 and WPE WebKit ≥ 2.50
   for this feature — align with RES-005's "1.26 practical floor, 1.28 target" by gating the
   browser source as a 1.28-only, runtime-probed optional feature. Check distro packaging early
   (Fedora/Debian gst-plugins-bad WPE build flags, Flatpak runtime).
6. **Property schema parity with OBS 32.2.2** is achievable: URL/local file, width, height, FPS
   (caps), custom CSS, shutdown-when-not-visible, reload-on-activate, permission-leveled JS bridge.
   Model the JS bridge on OBS's permission levels (None/ReadObs/ReadUser/Basic/Advanced/All) — it
   maps cleanly onto our command/event core: page → `run-javascript` + script messages → Core
   Commands with a per-source permission gate; Core Events → injected JS events.
7. **Open questions for follow-up:** (a) Rust-side control of the WPE `WebKitWebView` — thin FFI
   shim vs. generating a `wpewebkit-rs` gir binding; (b) wpe2 behavior under a headless/session-less
   Wayland context (does `WPEDisplayGStreamer` need our GstGL Wayland display or does it run fully
   headless); (c) latency/jitter characteristics of EGLImage-wrapped frames under load — measure
   per PLAN §53's 60 FPS overlay test; (d) whether to expose `WebKitSettings`
   (`hardware-acceleration-policy`, `enable-2d-canvas-acceleration`, media capabilities) per-source.
8. **Do not pursue CEF.** Rationale: EOL-Chromium security posture in the released baseline,
   vendored-binary packaging tax, no GStreamer frame integration, weak Wayland story, and no
   maintained Rust bindings. WebKitGTK/GTK3 offscreen tricks are likewise dead ends on GTK4.

## Sources

- WebKitGTK stable API docs: [WebKitWebView](https://webkitgtk.org/reference/webkitgtk/stable/class.WebView.html), [get_snapshot_finish](https://webkitgtk.org/reference/webkitgtk/stable/method.WebView.get_snapshot_finish.html), [set_background_color](https://webkitgtk.org/reference/webkitgtk/stable/method.WebView.set_background_color.html), [Migrating to webkitgtk-6.0](https://webkitgtk.org/reference/webkitgtk/stable/migrating-to-webkitgtk-6.0.html)
- WebKitGTK releases/news: [release index](https://webkitgtk.org/releases/), [2.54.0](https://webkitgtk.org/2026/09/16/webkitgtk2.54.0-released.html), [2.52 highlights](https://webkitgtk.org/2026/03/18/webkitgtk-2.52-highlights.html), [What's new in 2.44](https://webkitgtk.org/2024/03/27/webkigit-2.44.html), [2.46 graphics/Skia](https://webkitgtk.org/2024/10/04/webkitgtk-2.46.html), [Skia compositor in 2.54 (Igalia)](https://blogs.igalia.com/carlosgc/2026/09/21/skia-compositor-for-wpe-webkit-and-webkitgtk/), [DMA-BUF renderer (Igalia, 2023)](https://blogs.igalia.com/carlosgc/2023/04/03/webkitgtk-accelerated-compositing-rendering/)
- WPE: [architecture](https://wpewebkit.org/about/architecture.html), [FAQ (WPEPlatform)](https://wpewebkit.org/about/faq.html)
- GStreamer: [1.28 release notes](https://gstreamer.freedesktop.org/releases/1.28/), [wpesrc element docs](https://gstreamer.freedesktop.org/documentation/wpe/wpesrc.html?gi-language=c), gst-plugins-bad 1.29.2 `ext/wpe2/` source via [sources.debian.org](https://sources.debian.org/src/gst-plugins-bad1.0/1.29.2-2/ext/wpe2/)
- Rust bindings: [lib.rs/crates/webkit6](https://lib.rs/crates/webkit6), [webkit6 WebViewExt docs](https://world.pages.gitlab.gnome.org/Rust/webkit6-rs/stable/latest/docs/webkit6/prelude/trait.WebViewExt.html)
- OBS/CEF: [obs-browser](https://github.com/obsproject/obs-browser), [DeepWiki: browser source](https://deepwiki.com/obsproject/obs-browser/2-browser-source), [OBS releases (33.0 CEF 7871)](https://github.com/obsproject/obs-studio/releases), [cef-builds.spotifycdn.com](https://cef-builds.spotifycdn.com/index.html)
- Prior art: [fzwoch/obs-webkitgtk](https://github.com/fzwoch/obs-webkitgtk), [BitHeaven OBS WebKitGTK Browser](https://obsproject.com/forum/resources/webkitgtk-browser.2607/), [Fedora obs-studio-plugin-webkitgtk](https://packages.fedoraproject.org/pkgs/obs-studio-plugin-webkitgtk/)
- GTK4: [Migrating from GTK 3 to GTK 4 (offscreen surfaces removed)](https://docs.gtk.org/gtk4/migrating-3to4.html)
