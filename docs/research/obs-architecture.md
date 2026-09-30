# RES-002 — OBS Studio / libobs Architecture Research

**Task:** RES-002 · **Date:** 2026-09-30 · **Author:** research agent
**Scope (task file):** how libobs structures sources, scenes, scene items, outputs, encoders, services, the audio pipeline, and the obs-frontend boundary; what to keep conceptually, what to redesign for Linux/Rust/remote-first (PLAN §79).

## Baseline and method

Per PLAN.md, the research baseline is **OBS Studio 32.2.2** (released 2026-08-14). All "released" statements below were checked against the `32.2.2` tag of [obsproject/obs-studio](https://github.com/obsproject/obs-studio/tree/32.2.2) and the official release notes. The live [OBS developer documentation](https://docs.obsproject.com/) is built from `master` and already carries 33.x annotations (e.g. `obs_frontend_is_safe_mode_enabled()` is marked "Added in version 33.0" — development-only), so version annotations are called out explicitly wherever they matter.

Primary sources used:

- [Backend Design](https://docs.obsproject.com/backend-design) — official description of libobs threads and pipelines
- API references: [Core](https://docs.obsproject.com/reference-core), [Sources](https://docs.obsproject.com/reference-sources), [Scenes](https://docs.obsproject.com/reference-scenes), [Outputs](https://docs.obsproject.com/reference-outputs), [Encoders](https://docs.obsproject.com/reference-encoders), [Services](https://docs.obsproject.com/reference-services), [Settings](https://docs.obsproject.com/reference-settings), [Modules](https://docs.obsproject.com/reference-modules), [Canvases](https://docs.obsproject.com/reference-canvases), [Frontend API](https://docs.obsproject.com/reference-frontend-api)
- Source tree: [obsproject/obs-studio](https://github.com/obsproject/obs-studio) (`libobs/`, `libobs/media-io/`, `plugins/`, `frontend/`)
- Release notes: [OBS Studio 32.0](https://obsproject.com/blog/obs-studio-32-0-release-notes), [OBS Studio 32.2](https://obsproject.com/blog/obs-studio-32-2-release-notes), [GitHub releases (30.2 multitrack video)](https://github.com/obsproject/obs-studio/releases)
- [obsproject/obs-websocket](https://github.com/obsproject/obs-websocket) (in-tree since OBS 28; v5 protocol spec at `docs/generated/protocol.json`)

---

## 1. Repository and component map

The repo splits into ([root listing, 32.2.2](https://github.com/obsproject/obs-studio/tree/32.2.2)):

| Component | Role |
|---|---|
| `libobs/` | Core: objects, pipelines, graphics abstraction (`graphics/`), `media-io/` (audio-io, video-io, format conversion), `audio-monitoring/` (platform backends: `pulse`, `win32`, `osx`, `null`), hotkeys, properties, settings |
| `libobs-opengl/`, `libobs-d3d11/`, `libobs-metal/` | Pluggable graphics subsystem implementations (OpenGL is the only one on Linux; D3D11 on Windows; Metal for macOS, experimental since 32.0 per the [32.0 notes](https://obsproject.com/blog/obs-studio-32-0-release-notes)) |
| `plugins/` | All sources/outputs/encoders/services as modules: `linux-pipewire`, `linux-v4l2`, `linux-pulseaudio`, `linux-alsa`, `linux-jack`, `obs-ffmpeg`, `obs-outputs` (RTMP etc.), `obs-webrtc` (WHIP), `rtmp-services`, `obs-x264`, `obs-nvenc`, `obs-filters`, `obs-transitions`, `obs-websocket`, `frontend-tools`, … |
| `frontend/` | The Qt6 application (formerly `UI/`; renamed by 32.2.2): `OBSApp.cpp`, `OBSStudioAPI.cpp`, docks, dialogs, settings, importers, plugin-manager |
| `deps/`, `shared/` | Third-party code and shared helpers |

Key takeaway: **everything user-visible except the core pipelines is a plugin**, including every capture source, encoder, and output. libobs is a registry + three pipelines + a graphics abstraction.

## 2. Core object model

Six plugin-definable object types exist ([Backend Design](https://docs.obsproject.com/backend-design)): **Sources** (inputs, filters, transitions), **Outputs**, **Encoders**, **Services** — plus **Scenes/SceneItems** and, since 31.x, **Canvases** as first-class core objects.

Common patterns across all of them:

- **Definition structs** (`obs_source_info`, `obs_output_info`, `obs_encoder_info`, `obs_service_info`): a C vtable — string `id`, capability flag bitmask, and function pointers (`create`, `destroy`, `update`, `get_defaults`, `get_properties`, …). Types are registered by modules at load time (`obs_register_source`, `obs_register_output`, …).
- **Instances are reference-counted** (`obs_source_t`, `obs_output_t`, …) with paired **weak references** (`obs_weak_source_t`, …) to break cycles. Names are unique-ified automatically.
- **Signals and proc handlers**: every object carries a `signal_handler_t` (typed string signals such as `source_activate`, `item_visible`, `stop` with `code`) and a `proc_handler_t` (ad-hoc callable procedures). A global handler on the core emits `source_create`, `source_destroy`, `channel_change`, `hotkey_*`, `canvas_*`, `video_reset` etc. ([Core reference, "Core OBS Signals"](https://docs.obsproject.com/reference-core)).
- **Settings are `obs_data_t`** — JSON-serializable key/value objects with *default* and *autoselect* tiers ([Settings reference](https://docs.obsproject.com/reference-settings)). `obs_data_save_json_safe` writes with a backup of the previous file. There is **no schema versioning**; unknown fields survive round-trips implicitly.
- **Properties** (`obs_properties_t`): each type can describe its settings UI declaratively; the Qt frontend auto-generates widgets from it. This couples plugin metadata to a UI-generation model.

### Capability flags (the real type system)

`obs_source_info.output_flags` ([Sources reference](https://docs.obsproject.com/reference-sources)):

| Flag | Meaning |
|---|---|
| `OBS_SOURCE_VIDEO` / `OBS_SOURCE_AUDIO` | Has video / audio |
| `OBS_SOURCE_ASYNC_VIDEO` | Pushes raw frames via `obs_source_output_video()` (RAM path); A/V auto-synced by mutual timestamps |
| `OBS_SOURCE_COMPOSITE` | Renders child sources itself (scenes, transitions); must implement `audio_render` |
| `OBS_SOURCE_CUSTOM_DRAW` | Opts out of the "render-directly-into-first-filter" optimization (the doc's author note admits this default was a mistake) |
| `OBS_SOURCE_INTERACTION` | Receives mouse/keyboard events (browser source) |
| `OBS_SOURCE_DO_NOT_DUPLICATE` | Never fully duplicated on scene copy |
| `OBS_SOURCE_DO_NOT_SELF_MONITOR` | Prevents monitoring feedback when capturing the monitoring device |
| `OBS_SOURCE_DEPRECATED`, `OBS_SOURCE_REQUIRES_CANVAS` | Lifecycle / canvas gating |

`obs_output_info.flags` ([Outputs reference](https://docs.obsproject.com/reference-outputs)): `OBS_OUTPUT_VIDEO/AUDIO/AV`, `ENCODED` (needs encoders assigned), `SERVICE` (needs a service object), `MULTI_TRACK` (multiple encoded audio tracks), `CAN_PAUSE`.

Key source callbacks: `video_tick` (per-frame elapsed time), `video_render` (GPU path), `filter_video`/`filter_audio` (raw-data filters), `audio_render` (composite mixing), `activate`/`deactivate` (visible on program), `show`/`hide` (visible anywhere), `save`/`load`, `enum_active_sources`. Async sources stop pushing after `destroy` returns — a hard lifetime rule libobs enforces by contract.

## 3. Scenes and scene items

- **A scene is a source** (`obs_scene_t` wraps an internal source with `OBS_SOURCE_COMPOSITE`; `obs_scene_from_source()` / `obs_scene_get_source()` convert). Scene nesting is therefore free; groups are private scenes embedded as items (`obs_group_from_source`).
- **Scene items** (`obs_sceneitem_t`) are the placement records: position, rotation, scale, alignment, bounds (`OBS_BOUNDS_*`: stretch/scale-inner/outer/width/height/max-only), crop, scale filter (point/bilinear/bicubic/lanczos), blend mode and blending method, visibility, lock, selection, per-item **show/hide transitions with durations**, and a `private_settings` `obs_data_t` for frontend data. IDs are `int64_t`; `obs_sceneitem_set_id` is explicitly documented as dangerous ([Scenes reference](https://docs.obsproject.com/reference-scenes)).
- **Duplication modes** (`obs_scene_duplicate`): `OBS_SCENE_DUP_REFS` (shared sources), `OBS_SCENE_DUP_COPY` (deep copy), and private variants — this is how "duplicate scene" and studio-mode previews work.
- Transforms support deferred updates (`obs_sceneitem_defer_update_begin/end`) so UIs can batch matrix recomputation; group transforms auto-fit children unless deferred.
- Signals per scene: `item_add/remove`, `reorder`, `refresh`, `item_visible`, `item_locked`, `item_select/deselect` (documented as "should be replaced"), `item_transform`.

**Canvases** (`obs_canvas_t`, `libobs/obs-canvas.c`): reference-counted containers of scenes that own their own video mix (`obs_canvas_get_video()`), letting different scenes render at different resolutions ([Canvases reference](https://docs.obsproject.com/reference-canvases)). Flags: `MAIN`, `ACTIVATE`, `MIX_AUDIO`, `SCENE_REF`, `EPHEMERAL` (presets `PROGRAM`/`PREVIEW`/`DEVICE`). **Caveat: the docs explicitly mark the Canvas API unstable and still evolving** — treat as in-development even though present in the 32.x tree.

## 4. Video pipeline and threading

libobs spawns **three primary threads** ([Backend Design](https://docs.obsproject.com/backend-design)):

1. **Graphics thread** (`obs_graphics_thread`, `libobs/obs-video.c`) — renders all displays and the final mix on the GPU.
2. **Video thread** (`video_thread`, `libobs/media-io/video-io.c`) — owns the encoded/raw output path.
3. **Audio thread** (`audio_thread`, `libobs/media-io/audio-io.c`) — all audio processing, encoding, output.

Video flow: sources on **output channels** (`obs_set_output_source(channel, …)`, `MAX_CHANNELS = 64`, [libobs/obs-defs.h](https://github.com/obsproject/obs-studio/blob/master/libobs/obs-defs.h)) are drawn to the final texture on the graphics thread; the texture is converted to the output format (typically YUV, optionally GPU-side per `obs_video_info.gpu_conversion`) and queued with timestamp into the video-io cache (**`MAX_CACHE_SIZE = 16`**, [video-io.c](https://github.com/obsproject/obs-studio/blob/master/libobs/media-io/video-io.c)). **If the queue is full, the last frame is duplicated** — this is the visible "encoder lag → skipped frames" behavior. Frames then go to raw outputs and/or video encoders; encoded packets are placed in an **interleave queue per output** to guarantee monotonic timestamp order across audio and video before hitting the output.

The **graphics subsystem is abstracted** (`gs_*` API; module selected via `obs_video_info.graphics_module`, e.g. `"libobs-opengl"`, and cannot change without destroying the OBS context). **Views** (`obs_view_t`) and **displays** (`obs_display_t`) render extra mixes and preview windows off the same pipeline.

## 5. Audio pipeline

([Backend Design](https://docs.obsproject.com/backend-design); constants from [libobs/media-io/audio-io.h](https://github.com/obsproject/obs-studio/blob/master/libobs/media-io/audio-io.h))

- The audio thread ticks every `AUDIO_OUTPUT_FRAMES = 1024` samples (~21.3 ms at 48 kHz), calling `audio_callback` in `libobs/obs-audio.c`.
- Sources push audio via `obs_source_output_audio()` into a per-source circular buffer (`audio_input_buf`); mismatched rate/layout is resampled/remixed with swresample, and **audio filters run before buffering**.
- Each tick takes a **reference snapshot of the audio source tree**; leaves copy their closest-timestamp audio to `audio_output_buf`; composite parents (scenes, transitions) mix children via `audio_render` (this is how transition crossfades work); the root mixes all channels into the final mix.
- Tracks: **`MAX_AUDIO_MIXES = 6`** fixed output mixes, **`MAX_AUDIO_CHANNELS = 8`** max channel count. Speaker layouts up to 7.1. `obs_reset_audio2` (newer API) adds configurable max buffering latency and fixed-vs-dynamic buffering.
- **Monitoring** is a separate per-platform subsystem (`libobs/audio-monitoring/`, PulseAudio backend on Linux), per-source three-state (off / monitor-only / monitor+output); 32.0 added deduplication logic to prevent double audio when monitoring a captured device ([32.0 notes](https://obsproject.com/blog/obs-studio-32-0-release-notes)).
- Volume/mute/meters live in `obs-audio-controls.c` (fader/meter objects attach per source).

## 6. Outputs

([Outputs reference](https://docs.obsproject.com/reference-outputs))

- **Raw vs encoded**: raw outputs get `raw_video`/`raw_audio`/`raw_audio2` callbacks after setting media handlers (`obs_output_set_media`); encoded outputs must have encoders assigned (`obs_output_set_video_encoder`, `obs_output_set_audio_encoder(output, enc, track_idx)` — multi-track by index) and receive `encoded_packet` callbacks, always in monotonic timestamp order.
- **Lifecycle handshake** for implementations: `obs_output_can_begin_data_capture` → `obs_output_initialize_encoders` → `obs_output_begin_data_capture` → (streaming) → `obs_output_end_data_capture` or `obs_output_signal_stop(code)`. Stop codes: `SUCCESS`, `BAD_PATH`, `CONNECT_FAILED`, `INVALID_STREAM`, `ERROR`, `DISCONNECTED`, `UNSUPPORTED`, `NO_SPACE`, `ENCODE_ERROR`. Signals: `start/stop/pause/unpause/starting/stopping/activate/deactivate/reconnect/reconnect_success`.
- **Operational features**: output delay (with `PRESERVE` on reconnect), pause with sample-exact audio truncation, auto-reconnect with **doubling backoff** (`obs_output_set_reconnect_settings`), congestion metric 0.0–1.0, dropped-frame counter, connect-time-ms, `set_last_error` for user-facing strings.
- **Codec/protocol negotiation**: outputs declare `encoded_video_codecs`/`encoded_audio_codecs` and `protocols` (semicolon lists, 29.1+); the core can enumerate outputs by protocol. Packet-level processing hook: `obs_output_add_packet_callback` (31.0); reconnection policy hook: `obs_output_set_reconnect_callback` (31.1, e.g. to refresh an expired stream key).
- **One encoder can feed multiple outputs** (each output has its own interleave queue) — but the *frontend* only ever creates one streaming output (see §8).

Built-in output implementations live in `plugins/obs-outputs` (RTMP/FLV, and since 30.2 enhanced-RTMP multitrack), `plugins/obs-ffmpeg` (recording/muxing), `plugins/obs-webrtc` (WHIP, 30.x+).

## 7. Encoders

([Encoders reference](https://docs.obsproject.com/reference-encoders))

- `obs_encoder_info`: `type` (`OBS_ENCODER_VIDEO`/`AUDIO`), `codec` string ("h264", …), `encode(frame → packet)` callback, capability caps (`DEPRECATED`, `ROI` 30.1+, `SCALING` = encoder wants unscaled frames and scales internally).
- **Format negotiation**: `get_video_info`/`get_audio_info` let an encoder demand a specific format/size/rate; the core converts automatically before encoding. `obs_encoder_set_preferred_video_format` forces conversion only when needed.
- Packets (`encoder_packet`) carry pts/dts/timebase/keyframe/drop_priority/track_idx; `get_extra_data` (codec headers) and `get_sei_data` feed muxers; `get_priming_samples` (32.1+) handles AAC/Opus encoder delay correctly.
- Audio encoders bind to a mixer index (track) at creation (`obs_audio_encoder_create(…, mixer_idx, …)`); video encoders can be scaled per output via `obs_output_set_preferred_size` → `obs_encoder_set_scaled_size` **before** start only.

## 8. Services and the multistreaming gap

([Services reference](https://docs.obsproject.com/reference-services))

- A service is a thin config object: `get_url`, `get_key`, optional username/password, `get_protocol`, `get_connect_info` (server URL / stream key / encryption passphrase, 29.1+), `can_try_to_connect`, `get_supported_*_codecs`, `get_output_type` (preferred output), and `apply_encoder_settings` (service-imposed limits like keyframe interval or bitrate caps). The doc itself carries: *"the service API is incomplete as of this writing."*
- `rtmp-services` ships a big JSON catalog of providers; Twitch/YouTube/Restream integrations are service modules.
- **Multitrack Video** (Twitch Enhanced Broadcasting) arrived in **30.2** (Windows+NVENC only initially; Linux in 31.1; dynamic bitrate in 32.2 per [release notes](https://obsproject.com/blog/obs-studio-32-2-release-notes)) — it is *one service receiving multiple quality ladders of the same canvas*, not independent destinations.
- **There is no native multistreaming in OBS 32.2.2**: the frontend API exposes exactly one streaming output, one recording output, one replay buffer (`obs_frontend_get_streaming_output()` etc., [Frontend API](https://docs.obsproject.com/reference-frontend-api)). Multi-destination streaming exists only via third-party plugins (obs-multi-rtmp style). This validates PLAN's "native multistreaming" as a genuine differentiator.

## 9. The obs-frontend boundary

([Frontend API reference](https://docs.obsproject.com/reference-frontend-api))

The frontend API is a C ABI implemented by the Qt app (`frontend/OBSStudioAPI.cpp`) and consumed by in-process plugins and obs-websocket:

- **State ownership split**: libobs knows sources/scenes/outputs; the *frontend* owns **scene collections, profiles, studio mode (preview/program), transitions dock state, the T-bar, projectors, virtualcam, screenshots, undo/redo** (`obs_frontend_add_undo_redo_action`, 29.1+), themes, and UI docks. Scene switching (`obs_frontend_set_current_scene`) is a frontend concept; libobs only has `obs_set_output_source` on channels.
- **Events**: a flat enum (`obs_frontend_event`: STREAMING_STARTING/STARTED/STOPPING/STOPPED, RECORDING_*, SCENE_CHANGED, SCENE_COLLECTION_*, PROFILE_*, STUDIO_MODE_*, REPLAY_BUFFER_*, VIRTUALCAM_*, THEME_CHANGED, EXIT, …) delivered to registered callbacks — no payload types, no subscription filtering.
- **Persistence**: scene collections are frontend-managed JSON files of `obs_data` (`obs_save_sources` etc.), saved with backup-on-overwrite; profiles hold `basic.ini`-style config plus encoder/output settings; save/load callbacks let plugins piggyback. **No schema version field** in the core format.
- **Qt leaks through the API**: `obs_frontend_get_main_window()` returns `QMainWindow*`, docks take `QWidget*`, projector geometry is Qt base64 encoding. Scripting (Lua/Python) and obs-websocket both sit *above* this API — which is why obs-websocket (v5, JSON request/event protocol with RPC versioning and event subscriptions, [protocol.json](https://github.com/obsproject/obs-websocket/blob/master/docs/generated/protocol.json)) can only expose what the frontend owns.

## 10. Modules and the plugin ABI

([Modules reference](https://docs.obsproject.com/reference-modules))

- Modules are shared libraries exporting `obs_module_load()` etc., discovered via search paths; each registers its object types at load. `obs_open_module` returns `MODULE_INCOMPATIBLE_VER` for mismatches, and **32.0 refuses plugins built for a newer OBS** ([32.0 notes](https://obsproject.com/blog/obs-studio-32-0-release-notes)).
- There is **no stable ABI**: plugins share libobs's address space, refcounting rules, and graphics context; a crashing plugin crashes OBS (mitigated only by crash-handler/sentinel and 32.0's plugin *manager* UI). Unload ordering is contractual ("don't release active objects in `obs_module_unload`").

## 11. Released vs in-development summary

| Feature | Status vs 32.2.2 baseline |
|---|---|
| Core object model (sources/scenes/outputs/encoders/services), channels, 3-thread pipeline | Stable, ancient |
| obs_data JSON settings, properties, signals/proc handlers | Stable |
| 6 audio mixes / 8 channels, monitoring via PulseAudio | Stable (Linux monitoring = Pulse/PipeWire-compat) |
| Multitrack video (single service, quality ladders) | Released 30.2+, Linux since 31.1, dynamic bitrate 32.2 |
| Canvases (`obs_canvas_t`, per-canvas resolution) | Present but **explicitly unstable API**, still being integrated |
| Plugin manager, Hybrid MP4/MOV default, Metal renderer | 32.0 released (Metal = experimental, macOS) |
| `obs_frontend_is_safe_mode_enabled`, other 33.0-annotated APIs | **Development only (33.x)** |
| Native multistreaming to independent destinations | **Does not exist** in OBS (frontend hardcodes one streaming output) |

---

## Conclusions for Prismcast

**Keep conceptually (maps to PLAN §79 "keep" list):**

1. **The six-object domain model is proven.** Source (with Input/Filter/Transition subkinds), Scene as a compositing source, SceneItem as placement record (transform/crop/bounds/blend/visibility/lock + per-item show/hide transitions), Encoder, Output, Service — plus Profile and SceneCollection. Keep the recursive "scene is a source" idea: it makes nesting, groups-as-scenes, and preview-of-scene trivially uniform.
2. **Capability flags, but typed.** Replace C bitmasks with Rust bitflags/enums on the domain traits (`AsyncVideo`, `Composite`, `Interactive`, …). The `CUSTOM_DRAW` footgun (optimization opt-out backwards) is a lesson: make the safe path the default.
3. **Output lifecycle handshake.** OBS's `can_begin → initialize_encoders → begin_data_capture → end/signal_stop(code)` with typed stop reasons maps directly to our `OutputState` machine in `prismcast-output`; adopt the stop-code taxonomy (`ConnectFailed`, `Disconnected`, `Unsupported`, `NoSpace`, `EncoderError`) as typed `thiserror` variants.
4. **Service as constraint negotiator.** `apply_encoder_settings` + declared codec/protocol lists + `can_try_to_connect` is the right shape for our per-destination stream targets; generalize from "one service" to "N targets in the output graph" (ADR-0007).
5. **Reconnect/backoff, congestion, dropped-frames, connect-time as first-class telemetry** on every streaming output — feed these into core events for the remote API.
6. **Encoder↔output decoupling with per-output scaling and format conversion** (`get_video_info`, `set_scaled_size`, preferred format): in GStreamer terms, encoders are shareable branches behind `tee` with per-branch `capsfilter`/`videoscale`. Shared encoder feeding multiple outputs (OBS interleave queues ≈ GStreamer muxer/aggregator behavior).
7. **obs_data's defaults/autoselect tiers** are worth borrowing as a settings-metadata concept — but implement with serde + explicit `schema_version` (AGENTS.md mandates versioning; OBS's lack of it is a known pain).
8. **Deferred transform updates** (`defer_update_begin/end`) = batch command application in our core; we get this free with a command/event core (ADR-0005).

**Redesign / do not copy (maps to PLAN §79 "do not copy" list):**

1. **Frontend boundary.** OBS's split (libobs = engine, Qt frontend owns scene collections/profiles/studio mode/undo; obs-websocket sits on top of the Qt API) is exactly the "frontend-only state changes" PLAN rejects. Prismcast: scene collections, profiles, studio mode, undo, transitions state all live in `prismcast-core` behind the command/event API; GTK/CLI/Web/WS are interchangeable controllers. No Qt-style "get me the main window handle" surface anywhere.
2. **Threading model.** Three global threads + manual refcounting + mutex-protected source trees → replace with GStreamer's per-element streaming threads, pad probes, and our media control actor (owner/actor style per AGENTS.md). The 16-frame video cache with frame duplication is GStreamer `queue` + `videorate` behavior; make queue bounds and drop policy explicit per branch.
3. **Audio model.** OBS: fixed 6 global mixes, 8 channels max, per-source circular buffers, swresample, Pulse monitoring bolted on. Prismcast `prismcast-audio`: typed `AudioBusId` buses, GStreamer `audiomixer` graph, monitoring as just another PipeWire output branch, tracks as muxer-level mapping rather than a global mix bitmask. Keep the *idea* of audio filters on the source path and composite-side mixing (transitions crossfade audio).
4. **Plugin system.** In-process C ABI with no stability guarantee and crash-sharing is unacceptable; PLAN's ADR-0009 (plugin isolation, out-of-process SDK) is confirmed correct. Also note OBS 32.0's plugin manager + version gating as UX worth copying in spirit (discoverability, enable/disable, clear failure reporting like `obs_module_failure_info`).
5. **Graphics abstraction.** No `gs_*` equivalent: GStreamer GL/Vulkan + `gtk4paintablesink` for preview (per gstreamer-rust skill) replaces displays/swapchains. Linux-only means no D3D11/Metal portability tax.
6. **Signals/proc handlers → typed events.** OBS's stringly-typed signal handler with `calldata` becomes typed `CoreEvent`s with ID context (`scene_id=`, `output_id=`), structured via `tracing`. Frontend event enum's flatness (no filtering, no payloads) is what our event subscription model must improve on for remote clients (obs-websocket v5's `EventSubscription` bitfield is the better precedent).
7. **Hotkeys** belong in core as command triggers, not a libobs-style global registry tied to the frontend.

**Cross-stack notes:**

- **Canvases:** OBS is mid-migration to per-canvas resolution and the API is self-declared unstable. PLAN has no canvas concept. Recommend: *defer*; design `SceneCollection` so a future `CanvasId` (per-scene resolution/framerate) can slot in without breaking persisted schema. Worth an open question in the ARCH-001 domain model task.
- **Multistreaming:** OBS's absence of it (single streaming output hardcoded in the frontend) is the market gap PLAN targets; ADR-0007's output graph is validated. Multitrack video (one destination, N quality renditions, 30.2+) is a *separate* feature worth noting for 0.3+ (it implies encoder-ladder support in the output graph, not just N destinations).
- **Do not wrap libobs.** Community Rust bindings exist (`libobs-wrapper`, [docs.rs](https://docs.rs/crate/libobs-wrapper)), but libobs is inseparable from its own graphics/audio subsystems and C ABI — wrapping it would conflict with ADR-0004 (GStreamer), ADR-0001 (Linux-only), and the GTK4 paintable preview path. This research reinforces that ADR-0004 was right.
- **Persistence:** copy `obs_data_save_json_safe`'s atomic-write-with-backup behavior, add `schema_version`, keep unknown-field tolerance (AGENTS.md already mandates both).

**ADR / plan triggers identified:**

1. Audio track/bus model (6 fixed mixes vs typed bus matrix) — should be pinned in an ADR or in ARCH-001/ARCH-002 scope (none of ADR-0001..0010 currently covers the audio graph shape).
2. Canvas deferral decision (above) — record as an open question; no ADR needed yet.
3. No change needed to existing ADRs; this research confirms ADR-0004/0005/0007/0009 directions.
