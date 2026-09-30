# RES-004 — Linux capture research: PipeWire, xdg-desktop-portal, V4L2

Date: 2026-09-30. Author: research agent (RES-004).
Target platforms per PLAN §6: GNOME + KDE on Wayland, Wayland-first, X11 fallback secondary.

All claims below were verified against upstream sources on 2026-09-30. Where a fact
depends on a shipped version, the version is stated. OBS references distinguish the
released baseline (OBS Studio 32.2.2) from master/33.x development code.

---

## 1. xdg-desktop-portal ScreenCast API

Primary reference: [ScreenCast portal interface docs](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.ScreenCast.html)
(currently documents **version 6** of `org.freedesktop.portal.ScreenCast`).
Request/response lifecycle: [Request docs](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.Request.html).

### 1.1 Session lifecycle

The portal dance is strictly ordered and asynchronous (each method answers via a
`org.freedesktop.portal.Request::Response` signal on a per-request object path):

1. `CreateSession(options)` → Response carries `session_handle` (typed `s` by historical accident, kept for compat).
2. `SelectSources(session_handle, options)` — **callable exactly once per session**; invalid input closes the session.
3. `Start(session_handle, parent_window, options)` — shows the backend's picker dialog; Response carries
   `streams (a(ua{sv}))` and, for persistent sessions, `restore_token`.
4. `OpenPipeWireRemote(session_handle, options)` → `fd (h)`, an already-connected PipeWire socket FD.
   The docs say to wrap it with `pw_context_connect_fd`; **only the screencast nodes are visible** through this remote.

Cancellation/denial surfaces as a non-zero `response` code in the Response signal
(1 = user cancelled, 2 = user ended the dialog). The session can die at any time via
`org.freedesktop.portal.Session::Closed` — this must be treated as a normal,
recoverable state transition.

`handle_token` / `session_handle_token` options let the caller precompute the request/session
object paths so the Response signal subscription can be registered *before* the call — this
avoids a race where the reply arrives before subscription. OBS and ashpd both do this.

### 1.2 Interface version history

The effective interface version at runtime is `min(frontend, backend)` — the frontend
derives its advertised version from the backend ([xdg-desktop-portal-wlr#377](https://github.com/emersion/xdg-desktop-portal-wlr/issues/377)),
so always read the `version` property at runtime; never assume.

| ScreenCast version | Feature added | Frontend release | Backend status (2026-09-30) |
|---|---|---|---|
| 1 | MONITOR/WINDOW source types, base flow | (initial) | universal |
| 2 | `cursor_mode` option, `AvailableCursorModes` property | xdg-desktop-portal 1.2.0 ([NEWS](https://raw.githubusercontent.com/flatpak/xdg-desktop-portal/1.12.0/NEWS)) | universal on GNOME/KDE |
| 3 | `source_type` in stream properties | (1.16 era) | universal on GNOME/KDE |
| 4 | `persist_mode`, `restore_token`, stream `id` | xdg-desktop-portal 1.12.0 (2021-12, "screencast: Allow restoring previous sessions", [NEWS](https://raw.githubusercontent.com/flatpak/xdg-desktop-portal/1.12.0/NEWS); commit [ab515cda45](https://github.com/flatpak/xdg-desktop-portal/commits/main/data/org.freedesktop.portal.ScreenCast.xml) 2021-09-28) | GNOME ≥ 42 (backend code present since 2022-03), KDE since Plasma 5.25/6.0 era ([commit d782e10c63](https://github.com/KDE/xdg-desktop-portal-kde/commits/master/src/screencast.cpp), 2022-05-25) |
| 5 | `mapping_id` stream property (RemoteDesktop/libei mapping) | xdg-desktop-portal 1.18.0 (2023-09, "Add a mapping id property to the ScreenCast portal", NEWS) | GNOME ≥ 45 advertises v5 ([screencast.c](https://raw.githubusercontent.com/GNOME/xdg-desktop-portal-gnome/main/src/screencast.c) `xdp_impl_screen_cast_set_version(..., 5)`); KDE only on master (commit [30f7494ab2](https://github.com/KDE/xdg-desktop-portal-kde/commits/master/src/screencast.h), 2026-07) |
| 6 | `pipewire-serial` stream property; node ID deprecated for targeting | xdg-desktop-portal **1.21.2** (2026-05-05, "Add pipewire-serials to ScreenCast Portal streams #1942", [release notes](https://github.com/flatpak/xdg-desktop-portal/releases)) | KDE master only (commit [245ae0d48c](https://github.com/KDE/xdg-desktop-portal-kde/commits/master/src/screencast.h) "Implement screencasting portal v6", 2026-03-26 — first ships in Plasma 6.8, in beta as of 2026-09); **GNOME main (v51) still advertises v5**; xdg-desktop-portal-wlr lacks it ([#377](https://github.com/emersion/xdg-desktop-portal-wlr/issues/377) open 2026-03) |

Consequence: `pipewire-serial` is a forward-looking optimization. As of today, stable GNOME
and stable KDE both cap at v5/v4 respectively, so node-ID-based targeting remains the
shipping reality, with serials used opportunistically when `version >= 6`.

### 1.3 Restore tokens and persistence

Semantics from the interface docs (v4+):

- `persist_mode`: `0` = do not persist (default), `1` = persist while the app runs,
  `2` = persist until explicitly revoked. Only meaningful for ScreenCast sessions;
  RemoteDesktop persistence is a separate path.
- If persistence is granted, `Start`'s Response carries a `restore_token` (**s**).
- **Tokens are single-use**: each restored session's `Start` returns a *new* token that must
  replace the stored one. If the stored session cannot be restored (monitor/window gone,
  permission revoked), the token is silently ignored and the user is prompted normally —
  so token-based restore must always have the picker dialog as fallback, without error UI.
- The stream property `id` (v4) is stable across restored sessions — useful to match a
  restored stream to a stored source configuration.

Known pitfalls (verified issues):

- Transient persistence (`persist_mode=1`) restore regressed in frontend 1.18
  ([flatpak/xdg-desktop-portal#1124](https://github.com/flatpak/xdg-desktop-portal/issues/1124)) —
  prefer `persist_mode=2` like OBS does.
- Backends may restore the wrong monitor when the topology changed
  ([flatpak/xdg-desktop-portal#1371](https://github.com/flatpak/xdg-desktop-portal/issues/1371)).
- OBS issue [#13635](https://github.com/obsproject/obs-studio/issues/13635) (2026-07) shows
  Hyprland's backend generates tokens that never get persisted client-side — token storage
  must happen on *every* Start response, and source settings must be saved immediately.
- Electron/Chromium opens several token-less sessions per share (one per thumbnail),
  producing repeated picker dialogs ([xdg-desktop-portal-hyprland#385](https://github.com/hyprwm/xdg-desktop-portal-hyprland/issues/385)).
  Lesson for Prismcast: open exactly one session per source and reuse the PipeWire remote.

### 1.4 pipewire-serial vs node IDs

From the v6 docs (and PipeWire's own API docs
([pw_stream_connect](https://docs.pipewire.org/group__pw__stream.html),
[streams page](https://docs.pipewire.org/page_streams.html))):

- The node ID in the `streams` tuple is a 32-bit PipeWire object ID that **can be reused
  after node destruction** — across monitor hotplug, resolution/refresh changes, and
  suspend/resume, an active client can silently connect to the wrong stream.
- `pipewire-serial` (v6) is the node's `object.serial`: a **monotonically increasing 64-bit
  identifier, never reused**. It is the correct targeting key.
- Targeting happens via the stream property `PW_KEY_TARGET_OBJECT`, set to the
  `object.serial` (or `node.name`) of the desired node, combined with
  `PW_STREAM_FLAG_AUTOCONNECT`.
- Backends must keep populating the legacy node ID for pre-v6 clients, so a dual-path
  implementation (serial when present, node ID otherwise) is mandatory.

The wlr issue [#377](https://github.com/emersion/xdg-desktop-portal-wlr/issues/377) documents
why this matters most on wlroots compositors, where the backend creates streams directly
with no compositor-level mediation absorbing node churn.

### 1.5 Cursor modes

`AvailableCursorModes` bitmask (v2+): `1` Hidden, `2` Embedded (compositor draws the cursor
into the frames), `4` Metadata (cursor position/shape travels as PipeWire stream metadata;
the client composites it).

OBS 32.2.2's policy ([screencast-portal.c](https://github.com/obsproject/obs-studio/blob/32.2.2/plugins/linux-pipewire/screencast-portal.c),
`select_source`): prefer METADATA; else EMBEDDED if the user wants the cursor visible; else
HIDDEN. Same code on master — no 33.x divergence here.

On the GStreamer side, `pipewiresrc` translates `SPA_META_Cursor` into a
`GstVideoRegionOfInterestMeta` named `"cursor"` on output buffers
([gstpipewiresrc.c](https://github.com/PipeWire/pipewire/blob/master/src/gst/gstpipewiresrc.c)
lines ~760, 889–893). That gives us cursor *position* per frame; the cursor *image* requires
reading the SPA cursor bitmap metadata, which pipewiresrc does not currently expose as a
GStreamer-level object — compositing a custom cursor would need either a libpipewire path or
accepting Embedded mode. It also handles `SPA_META_VideoCrop` and videotransform metadata,
adjusting caps/buffers accordingly.

### 1.6 Source types: monitor vs window vs virtual, multiple, region

- `AvailableSourceTypes` bitmask: `1` MONITOR, `2` WINDOW, `4` VIRTUAL (virtual monitors;
  supported by KDE — wired up 2025 ([commit d9a55b3bb7](https://github.com/KDE/xdg-desktop-portal-kde/commits/master/src/screencast.h))).
- `multiple` (SelectSources option) allows multi-source selection; each selected source
  becomes its own PipeWire stream in the `streams` array.
- Stream properties per stream: `id` (v4, stable across restores), `position (ii)` and
  `size (ii)` in compositor coordinate space (**not necessarily pixel space** — HiDPI/fractional
  scaling caution; monitor streams only), `source_type (u)` (v3), `mapping_id` (v5),
  `pipewire-serial` (v6).
- **There is no region source type in the portal.** Region capture is a compositor crop:
  capture the monitor stream and crop in the media graph (`videocrop` / scene-graph crop).
  PLAN's "Region Crop" requirement therefore belongs to the transform layer, not the portal
  layer. (xdg-desktop-portal-wlr optionally offers region *selection* via external choosers,
  but that is backend-specific and non-standard — do not design around it.)
- Window capture caveat: window identity across sessions is fuzzy. KDE matches windows for
  restore by `appId` + fuzzy title match (Levenshtein, [screencast.cpp](https://raw.githubusercontent.com/KDE/xdg-desktop-portal-kde/master/src/screencast.cpp));
  GNOME refuses to restore windows with too-different titles. Window titles change constantly
  (browser tabs), so restored window capture can silently land on a different window or fail
  to restore. Design UI around "window capture may need re-selection".

### 1.7 Backend implementation matrix (verified 2026-09-30)

| Backend | Ships with | ScreenCast version advertised | Persist/restore | Notes |
|---|---|---|---|---|
| xdg-desktop-portal-gnome 42–44 | GNOME 42–44 | v4 features (restore code present since 42.0) | yes | — |
| xdg-desktop-portal-gnome 45–51 | GNOME 45+ (51 current dev) | **v5** | yes | no `pipewire-serial` yet ([screencast.c](https://raw.githubusercontent.com/GNOME/xdg-desktop-portal-gnome/main/src/screencast.c)) |
| xdg-desktop-portal-kde 6.0–6.7.x | Plasma 6 stable (6.7.x current) | **v4** | yes ([commit 9025472bf8](https://github.com/KDE/xdg-desktop-portal-kde/commits/master/src/screencast.cpp) 2026-06 fixed over-strict persist checks) | known duplicate-stream response bug (see OBS workaround, §4) |
| xdg-desktop-portal-kde master (6.8 beta) | Plasma 6.8, ~Oct 2026 | **v6** | yes | first backend with `pipewire-serial` |
| xdg-desktop-portal-wlr 0.8.x | wlroots compositors (sway etc.) | v4+ (restore tokens added in 0.8.0, [release notes](https://github.com/emersion/xdg-desktop-portal-wlr/releases)) | yes, env-var-capped persist | no serials yet; node churn most acute here |
| xdg-desktop-portal-hyprland | Hyprland | v4-ish | token generation exists but persistence historically flaky ([#385](https://github.com/hyprwm/xdg-desktop-portal-hyprland/issues/385), OBS #13635) | — |

Frontend required for v4: xdg-desktop-portal ≥ 1.12 (Dec 2021 — ubiquitous by now).
For v6/serials: frontend ≥ 1.21.2 (May 2026) **and** a v6 backend — realistically only
Plasma 6.8+ in 2026.

---

## 2. Consuming the stream: GStreamer `pipewiresrc`

The PipeWire project ships the GStreamer plugin in-tree
([src/gst/gstpipewiresrc.c](https://github.com/PipeWire/pipewire/blob/master/src/gst/gstpipewiresrc.c));
PipeWire itself is at **1.6.x** as of 2026-09 ([tags](https://gitlab.freedesktop.org/pipewire/pipewire/-/tags)).
`pipewiresrc` is classed `Source/Audio/Video` — the same element serves screen capture,
camera capture, and audio capture.

Verified properties (master source, `G_PARAM_SPEC` definitions):

| Property | Type | Purpose for Prismcast |
|---|---|---|
| `fd` | int | The portal-provided PipeWire remote FD (`OpenPipeWireRemote`). Presence of `fd` selects portal-remote mode. |
| `path` | string | **Deprecated** (`G_PARAM_DEPRECATED`). Old node-ID targeting. |
| `target-object` | string | Target node **name or serial** to connect to — the modern replacement for `path`. Maps to `PW_KEY_TARGET_OBJECT`. Feed it the `pipewire-serial` (v6) or fall back to the node ID. |
| `client-name`, `client-properties` | string / GstStructure | Identify ourselves to PipeWire/WirePlumber (routing policy sees `application.name` etc.). |
| `stream-properties` | GstStructure | Extra `pw_stream` properties (e.g. `node.autoconnect` behavior tuning). |
| `autoconnect` | bool (default true) | Let PipeWire link us to the target. |
| `always-copy` | bool | **Deprecated**; buffer negotiation decides. |
| `min-buffers` / `max-buffers` | int | Buffer count negotiation with the PipeWire peer. |
| `use-bufferpool` | bool (default: auto — true for video) | DMA-BUF-friendly buffer pooling. |
| `resend-last`, `keepalive-time` | bool / int ms | Re-emit the last frame on EOS/periodically — useful for previews when the source is static (screen idles; portal streams are damage-driven on some backends, e.g. wlr 0.8 "send damage via PipeWire"). |
| `on-disconnect` | enum: `none` (default), `eos`, `error` | **Critical for capture recovery**: controls element behavior when the PipeWire peer (compositor/camera node) disappears. `error` lets our owner actor drive reconnection through the normal bus-error path. |
| `provide-clock` | bool (default true) | pipewiresrc provides the pipeline clock from the PipeWire graph clock — good A/V sync default when mixing PipeWire sources. |

Usage pattern for portal capture (matches OBS and WebKit behavior;
[WebKit change requiring fd+path](https://trac.webkit.org/timeline?from=2022-04-29T03%3A20%3A03-07%3A00&precision=second)):

```text
pipewiresrc fd=<portal_fd> target-object=<serial-or-nodeid> \
            client-properties=...(application.name=Prismcast) \
            on-disconnect=error ! ...
```

Notes:

- Do **not** put a `framerate` caps filter directly after pipewiresrc — the compositor decides
  the capture rate; cap with `videorate` downstream
  ([pattern confirmed in field reports](https://etducky.com/blog/wayland-screen-capture-portal-pipewire)).
- DMA-BUF: pipewiresrc negotiates `video/x-raw(memory:DMABuf)` with modifiers when downstream
  supports it; OBS's libpipewire path enumerates DRM formats/modifiers explicitly. With
  GStreamer, zero-copy depends on the whole downstream chain (compositor → encoder). Per the
  gstreamer-rust skill: never claim zero-copy from element names; measure.
- FD ownership: the portal FD is ours after `OpenPipeWireRemote`; pipewiresrc consumes a copy
  for its connection. Keep the `OwnedFd` alive until the element has opened its connection,
  and close it when the source is torn down.

### Rust crate choices for the portal+PipeWire path

| Concern | Options | Assessment |
|---|---|---|
| Portal D-Bus client | [`ashpd`](https://docs.rs/ashpd) 0.13 (workspace version on master; zbus-based, async, feature `screencast`, GTK4 window-identifier helpers via features `gtk4`/`gtk4_wayland`) | Covers CreateSession/SelectSources/Start/OpenPipeWireRemote, `PersistMode`, `CursorMode`, stream `id`/`mapping_id` ([Stream struct](https://docs.rs/ashpd/latest/ashpd/desktop/screencast/struct.Stream.html)). **Gap: no `pipewire-serial` accessor yet** — needs upstream contribution or a small zbus call for the v6 property. |
| PipeWire client (if needed beyond pipewiresrc) | [`pipewire` (pipewire-rs)](https://docs.rs/crate/pipewire) 0.10.0 (2026-06) | Safe-ish bindings over libpipewire; needed only if we outgrow pipewiresrc (custom cursor bitmaps, per-app audio graph surgery, registry watching without GstDeviceMonitor). Adds main-loop threading concerns — avoid unless required. |
| GStreamer | `gstreamer` crate (gstreamer-rs) | pipewiresrc via `gst::ElementFactory::make("pipewiresrc")`; runtime property/caps probing per skill. |

---

## 3. V4L2 camera capture

### 3.1 Direct V4L2 via `v4l2src`

[v4l2src documentation](https://gstreamer.freedesktop.org/documentation/video4linux2/v4l2src.html)
(plugin `video4linux2`, gst-plugins-good):

- `device=/dev/videoN` selection; `device-name`/`device-fd` introspection.
- `io-mode`: `auto`, `rw`, `mmap`, `userptr`, `dmabuf`, `dmabuf-import` — `dmabuf-import` is
  the zero-copy camera→GPU/encoder path where supported.
- Src pad caps include raw formats, `image/jpeg` (UVC MJPEG — needs `jpegdec`),
  `video/x-h264`/`h265` (UVC 1.5 H.264 cameras — passthrough possible),
  bayer, DV, and `video/x-raw(memory:DMABuf)` with `DMA_DRM` formats.
- Device controls (brightness/contrast/saturation/hue, `extra-controls` GstStructure) are
  runtime properties — we must enumerate them per device rather than hardcode.
- Kernel-level crop properties (`crop-left/top/right/bottom`, `crop-bounds`, since GStreamer 1.22)
  exist for devices with real crop support — rare on UVC; do crop in the scene graph instead.
- Format/framerate negotiation: query the element's pad caps after device open; prefer
  camera-native MJPEG/H.264 when CPU matters, raw YUYV/NV12 otherwise. libv4l2 emulation is
  disabled by default since GStreamer 1.14 for good reasons — do not re-enable.

### 3.2 Cameras through PipeWire (`pipewiresrc`)

PipeWire exposes V4L2 and libcamera devices as PipeWire nodes; `pipewiresrc` can connect to
them by `target-object` (node name/serial). Benefits: unified source model with screen/audio
capture, portal-mediated access for sandboxed apps, WirePlumber-managed routing. OBS 30.1
shipped a "Video Capture Device (PipeWire)" source using this model
([Phoronix on OBS 30.1](https://www.phoronix.com/news/OBS-Studio-30.1),
[OBS linux-pipewire plugin tree at 32.2.2](https://github.com/obsproject/obs-studio/tree/32.2.2/plugins/linux-pipewire)
including `camera-portal.c`). Costs: fewer camera controls exposed than raw V4L2, format
selection less deterministic, and an extra mediation layer that can stall (user reports of
frame-rate capping issues exist).

For **sandboxed (Flatpak)** distribution, camera access goes through the
[Camera portal](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.Camera.html)
(`AccessCamera`, then PipeWire ACLs gate the nodes) — PipeWire camera nodes are the only
path there. As a host app, both paths work.

### 3.3 Hotplug (cameras and audio devices)

- GStreamer: `GstDeviceMonitor` with the v4l2 device provider (udev-backed) emits
  device added/removed; `gst-device-monitor-1.0 Video/Source` demonstrates it. PipeWire nodes
  appear via the PipeWire device provider. Use one monitor per device class and map
  `GstDevice` → element via `gst_device_create_element`.
- Device *disappearance mid-stream* surfaces as a v4l2src/pipewiresrc error or EOS on the bus;
  treat as recoverable: stop branch, mark source unavailable, watch the monitor for return,
  re-arm with bounded retries. Never spin on re-enumeration.

---

## 4. Audio capture via PipeWire

- **Mic/line input and desktop output capture**: with `pipewire-pulse` providing the PulseAudio
  server, `pulsesrc` works everywhere; `pipewiresrc` (`target-object=<node serial/name>`) is
  the native path and avoids the Pulse emulation layer. OBS's built-in Linux audio capture
  (mic + desktop) is still PulseAudio-API-based (`linux-pulseaudio` plugin at
  [32.2.2](https://github.com/obsproject/obs-studio/tree/32.2.2/plugins)) and works through
  pipewire-pulse.
- **Per-application audio**: OBS has no built-in per-app capture on Linux even at 32.2.2;
  the de-facto solution is the third-party
  [obs-pipewire-audio-capture](https://repology.org/project/obs-pipewire-audio-capture/packages)
  plugin, which enumerates PipeWire `Stream/Output/Audio` nodes (application playback
  streams) and connects a capture stream to the chosen node via target-object semantics.
  PLAN's "PipeWire Application Audio" source is therefore a **differentiator, not parity**:
  implementable by enumerating the PipeWire registry (via `pipewire-rs` or the
  GstDeviceMonitor/PipeWire provider), matching nodes by `application.name`/`object.serial`,
  and targeting them from `pipewiresrc` with audio caps.
- Channel maps/sample rates: negotiate explicitly; PipeWire will happily hand you float32
  planar at the node's native rate — normalize early (`audioconvert ! audioresample ! capsfilter`)
  at the source boundary so the mixer sees one canonical format.
- Audio hotplug uses the same device-monitor/registry mechanism as cameras.

---

## 5. X11 fallback options

Reality check: the ScreenCast portal is not Wayland-only. Both GNOME's and KDE's backends
provide portal screen capture under their X11 sessions too (mutter and KWin X11 both expose
PipeWire streams; corroborated by long-standing user reports of "KDE X11 + pipewire and
GNOME X11 + pipewire works", e.g. [Arch forums](https://bbs.archlinux.org/viewtopic.php?id=269671)).
So the **portal+pipewiresrc path is the primary path on both session types**.

If the portal is unavailable/broken (minimal WMs, misconfigured backends — a common real-world
failure), X11-only fallbacks:

- [`ximagesrc`](https://gstreamer.freedesktop.org/documentation/ximagesrc/index.html)
  (gst-plugins-good): full-display capture via XGetImage/XShm, XDamage for incremental
  updates, XFixes for the cursor, default 25 fps fixation. Coarse but dependable.
- OBS's X11 capture ([linux-capture plugin at 32.2.2](https://github.com/obsproject/obs-studio/tree/32.2.2/plugins/linux-capture)):
  `xshm-input` (XCB SHM screen capture) and `xcomposite-input` (per-window via XComposite;
  historically buggy with modern compositing WMs). Under Wayland these only see XWayland
  content — which is why PLAN's Wayland-first stance is correct.

Prismcast recommendation: portal/PipeWire first on both Wayland and X11 sessions of
GNOME/KDE; `ximagesrc` as an explicit, clearly-labeled legacy fallback on X11; no X11-specific
design surface beyond that (PLAN: "Do not design around X11-specific APIs").

---

## 6. How OBS (baseline 32.2.2) does it — comparison

From [plugins/linux-pipewire at tag 32.2.2](https://github.com/obsproject/obs-studio/tree/32.2.2/plugins/linux-pipewire)
(`screencast-portal.c`, `pipewire.c`, `portal.c`):

- Raw GDBus (no libportal). Reads the `version` property and gates persistence on
  `version >= 4`; requests `persist_mode=2`, stores `restore_token` in the source's settings
  via `obs_source_save()` on **every** Start response (single-use token rotation handled
  correctly).
- Cursor policy: METADATA > EMBEDDED (if visible) > HIDDEN (§1.5).
- `multiple=false`, one stream per source; contains an explicit workaround for a KDE backend
  bug that sometimes returns multiple streams — it takes the **last** one.
- libpipewire (not GStreamer) for the stream itself: `pw_context_connect_fd` on the portal FD,
  explicit SPA format/modifier enumeration with DMA-BUF preference, gs (GPU) texture import.
- **Targets streams by the legacy node ID.** Even on master (33.x dev) as of 2026-09 there is
  no `pipewire-serial` usage — the code paths at 32.2.2 and master are identical in this area
  (verified by diffing both refs). Prismcast can leapfrog OBS here by adopting serials from
  day one where the portal provides them.
- Camera: separate "Video Capture Device (PipeWire)" source with Camera-portal support
  (`camera-portal.c`) plus the legacy `linux-v4l2` plugin.
- Audio: PulseAudio API (`linux-pulseaudio`); no per-app capture.

---

## 7. Conclusions for Prismcast

Architecture and implementation implications, roughly in PLAN order:

1. **One portal abstraction, two consumer paths.** Capture acquisition (portal session
   lifecycle, tokens, stream metadata) belongs in a dedicated module (media platform layer),
   producing `(OwnedFd, Vec<PortalStream>)`; the media graph only consumes `fd` +
   `target-object`. This keeps D-Bus async lifecycles out of GStreamer graph code.
2. **Crate choice: ashpd 0.13 + gstreamer-rs pipewiresrc.** ashpd covers the whole session
   flow with zbus and GTK4 window-identifier integration. Track/upstream the missing
   `pipewire-serial` accessor (v6). Avoid a direct libpipewire dependency until a concrete
   need (cursor bitmap compositing, per-app audio surgery) justifies `pipewire-rs` 0.10.
3. **Target by serial, fall back to node ID.** Read the portal `version` property at runtime;
   when ≥ 6 and `pipewire-serial` is present, set pipewiresrc `target-object` to the serial;
   otherwise use the node ID. Persist **both** the restore token and the stream `id` (stable
   across restores) in the source configuration; never persist node IDs. This directly
   implements PLAN's journal wisdom ("PipeWire node IDs are not persistent").
4. **Token rotation is state.** Store the new `restore_token` on every Start (single-use
   semantics), request `persist_mode=2`, and treat restore failure as a silent fall back to
   the picker — never as an error. Rotate-on-restore must be atomic with source-settings
   persistence (schema-versioned, per AGENTS.md persistence rules).
5. **Cursor: request METADATA, composite later.** pipewiresrc already surfaces cursor position
   as ROI metadata — our compositor can draw its own cursor (a future OBS-style differentiator),
   but shipping v1 can simply request EMBEDDED when "show cursor" is on, matching OBS's policy.
6. **Region crop lives in the scene graph.** No portal support exists; `videocrop` or
   compositor-level crop on a monitor stream. Confirm PLAN wording stays "Region Crop" as a
   transform, not a capture mode.
7. **Recovery model per source.** Configure `on-disconnect=error`, and treat bus errors,
   `Session::Closed`, and device-monitor removal uniformly as "source unavailable → bounded
   reconnect → re-select via token → picker as last resort". Do not re-open the picker in an
   automatic retry loop (skill guidance; user-hostile).
8. **Sources matrix**: Screen/Window via portal+pipewiresrc (primary on Wayland *and* GNOME/KDE
   X11); Camera via v4l2src (host install) with pipewiresrc+Camera portal for Flatpak;
   audio via pipewiresrc with pulsesrc fallback; per-app audio via PipeWire registry targeting —
   a genuine advantage over OBS 32.2.2 on Linux.
9. **Risks / open questions**:
   - KDE duplicate-stream bug: be defensive when parsing `streams` (log and pick deterministically).
   - `position`/`size` are in compositor coordinates, not pixels — multi-monitor scene
     auto-layout must map coordinates through the actual negotiated stream resolution.
   - Window-capture restore is fuzzy by design (title matching); UI must tolerate silent
     re-selection needs. Consider surfacing "reselect window" affordances on restore failure.
   - GNOME has no v6 backend timeline; serial adoption will be KDE-first for months.
   - PipeWire restart behavior of gst pipewiresrc reconnect needs prototyping (does the element
     recover the remote, or must we rebuild the branch? — assume rebuild).
   - DMA-BUF end-to-end (portal stream → compositor → encoder) must be measured per GPU/driver;
     keep a CPU-copy fallback path verified.
10. **ADR candidates**: (a) "Portal-first capture on all session types, ximagesrc only as
    legacy X11 fallback" (refines PLAN §6); (b) "ashpd + pipewiresrc as the capture stack,
    serial-based targeting with node-ID fallback"; (c) per-app audio via PipeWire registry
    (new capability beyond OBS baseline — worth an ADR since it shapes the source model).

---

## Appendix: sources

- [ScreenCast portal interface (v6)](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.ScreenCast.html) and
  [backend interface](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.impl.portal.ScreenCast.html);
  [Request lifecycle](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.Request.html)
- [xdg-desktop-portal releases](https://github.com/flatpak/xdg-desktop-portal/releases) (1.21.2: pipewire-serials, 2026-05);
  [NEWS at 1.12.0](https://raw.githubusercontent.com/flatpak/xdg-desktop-portal/1.12.0/NEWS) (session restore);
  [ScreenCast.xml commit history](https://github.com/flatpak/xdg-desktop-portal/commits/main/data/org.freedesktop.portal.ScreenCast.xml)
- Backends: [GNOME screencast.c](https://raw.githubusercontent.com/GNOME/xdg-desktop-portal-gnome/main/src/screencast.c),
  [KDE screencast.h/.cpp](https://github.com/KDE/xdg-desktop-portal-kde/tree/master/src),
  [xdg-desktop-portal-wlr releases](https://github.com/emersion/xdg-desktop-portal-wlr/releases),
  [wlr#377 serial issue](https://github.com/emersion/xdg-desktop-portal-wlr/issues/377)
- PipeWire: [pw_stream_connect](https://docs.pipewire.org/group__pw__stream.html),
  [streams doc](https://docs.pipewire.org/page_streams.html),
  [gstpipewiresrc.c](https://github.com/PipeWire/pipewire/blob/master/src/gst/gstpipewiresrc.c)
- GStreamer: [v4l2src](https://gstreamer.freedesktop.org/documentation/video4linux2/v4l2src.html),
  [ximagesrc](https://gstreamer.freedesktop.org/documentation/ximagesrc/index.html)
- OBS baseline: [linux-pipewire at 32.2.2](https://github.com/obsproject/obs-studio/tree/32.2.2/plugins/linux-pipewire),
  [linux-capture at 32.2.2](https://github.com/obsproject/obs-studio/tree/32.2.2/plugins/linux-capture),
  [OBS 30.1 PipeWire camera source coverage](https://www.phoronix.com/news/OBS-Studio-30.1)
- Rust crates: [ashpd](https://docs.rs/ashpd) ([screencast Stream](https://docs.rs/ashpd/latest/ashpd/desktop/screencast/struct.Stream.html)),
  [pipewire-rs](https://docs.rs/crate/pipewire)
- Known issues: [xdg-desktop-portal#1124](https://github.com/flatpak/xdg-desktop-portal/issues/1124),
  [#1371](https://github.com/flatpak/xdg-desktop-portal/issues/1371),
  [OBS#13635](https://github.com/obsproject/obs-studio/issues/13635),
  [xdph#385](https://github.com/hyprwm/xdg-desktop-portal-hyprland/issues/385)
