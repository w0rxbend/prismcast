# Linux Rust Broadcasting Studio — Agentic Development Plan

## 1. Product goal

Build a Linux-only OBS-class broadcasting and recording application with:

- Rust as the application language.
- GTK4 + Relm4.
- libadwaita native GNOME UI.
- Wayland-first architecture.
- X11 compatibility where practical.
- PipeWire-first capture.
- GStreamer-based media graph.
- GPU accelerated composition where possible.
- Streaming + recording.
- Native simultaneous multi-streaming.
- Extensible sources, filters, encoders, services and outputs.
- Complete remote-control API.
- Local IPC.
- WebSocket API.
- Browser-based remote UI.
- CLI control.
- Theme/style system.
- Profiles and scene collections.
- Crash-resilient persistence.
- Plugin/extensibility model.

This should eventually provide most functionality users expect from OBS without inheriting OBS's Qt-oriented frontend architecture.

Research baseline: OBS Studio 32.2.2, released August 14, 2026. The OBS development documentation currently already exposes 33.x development documentation, so agents must distinguish released functionality from development/master functionality.

---

# 2. Fundamental architecture decision

Do NOT put media logic into Relm4 components.

Use four layers:

```text
┌──────────────────────────────────────────────────────────────┐
│                         Interfaces                           │
│                                                              │
│ GTK/Relm4 │ Web UI │ CLI │ WebSocket │ Unix IPC │ Plugins   │
└──────────────────────────────┬───────────────────────────────┘
                               │
                        Commands / Queries
                               │
┌──────────────────────────────▼───────────────────────────────┐
│                       Application Core                       │
│                                                              │
│ State │ Commands │ Events │ Projects │ Profiles │ Undo/Redo │
│ Automation │ Authorization │ Persistence                    │
└──────────────────────────────┬───────────────────────────────┘
                               │
                         Domain interfaces
                               │
┌──────────────────────────────▼───────────────────────────────┐
│                         Media Core                           │
│                                                              │
│ Sources → Filters → Scene Graph → Mixer → Encoder Graph     │
│                                      │                       │
│                         Recording / Streaming / Preview      │
└──────────────────────────────┬───────────────────────────────┘
                               │
┌──────────────────────────────▼───────────────────────────────┐
│                     Linux Media Platform                     │
│                                                              │
│ GStreamer │ PipeWire │ xdg-desktop-portal │ V4L2 │ VAAPI   │
│ NVENC │ ALSA/PipeWire │ WebKitGTK │ systemd │ D-Bus        │
└──────────────────────────────────────────────────────────────┘
```

OBS uses essentially the same high-level separation through `libobs`: sources, outputs, encoders and services.

---

# 3. Recommended technology stack

## Core

```text
Rust stable
Tokio
serde
serde_json
thiserror
tracing
tracing-subscriber
uuid
indexmap
slotmap / generational-arena where stable object IDs are required
```

Avoid leaking GTK/GStreamer types into domain models.

Domain:

```rust
SceneId
SourceId
SceneItemId
FilterId
OutputId
EncoderId
ServiceId
AudioBusId
ProfileId
SceneCollectionId
```

IDs must remain stable across IPC, WebSocket and persistence.

---

## Desktop UI

```text
gtk4
relm4
libadwaita
gio
glib
gdk4
```

Relm4 is compatible with GTK4 and libadwaita and provides an Elm-like component/message architecture.

Use Relm4 only for presentation state.

Never:

```text
GTK widget -> mutate GStreamer pipeline directly
```

Instead:

```text
GTK
 ↓
Command
 ↓
Application Service
 ↓
Domain mutation
 ↓
Media Engine
 ↓
Domain Event
 ↓
GTK update
```

This is critical because later the same operation must work from WebSocket, IPC and web UI.

---

# 4. Media engine

Recommended first implementation:

```text
GStreamer + gstreamer-rs
```

instead of building capture, synchronization, decoding, encoding, muxing and networking from scratch.

GStreamer has maintained Rust bindings and already provides the primitives required for pipelines and plugins.

Suggested internal abstraction:

```rust
trait SourceBackend
trait VideoFilterBackend
trait AudioFilterBackend
trait CompositorBackend
trait EncoderBackend
trait OutputBackend
trait StreamingServiceBackend
```

GStreamer becomes one implementation:

```text
GstSourceBackend
GstCompositorBackend
GstEncoderBackend
...
```

This preserves the possibility of replacing specialized portions later.

---

# 5. Preview/rendering strategy

Phase 1:

```text
GStreamer compositor
        ↓
gtk4paintablesink
        ↓
GdkPaintable
        ↓
GTK Picture
```

`gtk4paintablesink` supports GTK4 `Paintable`, GL textures and direct Linux DMABUF rendering with sufficiently recent GTK versions. This is particularly valuable for avoiding unnecessary CPU copies.

Composition can initially use:

```text
compositor
```

then investigate:

```text
glvideomixer
VA compositor
Vulkan composition
custom GStreamer Rust element
```

`compositor` already exposes position, dimensions, alpha and z-order per input, which maps naturally onto scene items.

Do not write a custom renderer before proving that GStreamer composition is insufficient.

---

# 6. Linux capture architecture

Wayland must be the primary target.

## Screen/window capture

Use:

```text
xdg-desktop-portal
    ↓
ScreenCast portal
    ↓
PipeWire FD / PipeWire stream
    ↓
GStreamer pipewiresrc
```

The portal explicitly provides monitor/window selection and PipeWire streams, including persistence through restore tokens.

Required functionality:

```text
Display Capture
Window Capture
Region Crop
Cursor modes
Persistent portal selection
Capture reconnect
Monitor hotplug
Window disappearance/reappearance
```

Do not design around X11-specific APIs.

---

# 7. Source model

Core abstraction:

```rust
Source {
    id,
    kind,
    name,
    enabled,
    settings,
    video,
    audio,
    filters,
}
```

Initial source implementations:

```text
PipeWire Display
PipeWire Window
V4L2 Camera
PipeWire Audio Input
PipeWire Application Audio
Media File
Image
Image Slideshow
Color
Text
Browser
Scene
Test Pattern
Network Stream
```

OBS's basic source model includes capture, audio, browser, images and other scene content; scenes contain references to sources rather than owning isolated copies of them.

Your domain should preserve this concept:

```text
Source
   ↑
SceneItem reference
   ↑
Scene
```

Therefore the same source may appear in multiple scenes.

---

# 8. Scene graph

A scene should be a graph/container of `SceneItem` objects:

```rust
SceneItem {
    id: SceneItemId,
    source_id: SourceId,

    transform: Transform,
    crop: Crop,
    opacity: f32,
    visible: bool,
    locked: bool,
    blend_mode: BlendMode,
    bounds: Bounds,
    z_index: i32,
}
```

Transform:

```rust
Transform {
    position,
    scale,
    rotation,
    anchor,
}
```

Required operations:

```text
Move
Resize
Crop
Rotate
Flip
Center
Fit to canvas
Stretch
Reset transform
Lock
Hide/show
Duplicate
Group
Ungroup
Reorder
Copy/Paste
Undo/Redo
```

OBS's scene model similarly stores transforms on scene items rather than directly on source definitions.

---

# 9. Audio engine

Treat audio as its own graph rather than a property of video.

Domain:

```text
AudioSource
AudioBus
AudioRoute
AudioFilterChain
AudioMonitor
AudioTrack
```

Mixer functionality:

```text
volume
mute
solo
meter
peak meter
monitoring
balance
sync offset
track routing
```

OBS exposes mixer levels, meters, mute and monitoring separately.

Initial filters:

```text
Gain
Compressor
Limiter
Noise Gate
Expander
EQ
Delay
Polarity Inversion
RNNoise
```

OBS currently exposes a similar filter pipeline including compressor, expander, gain, limiter, noise gate and noise suppression.

Later:

```text
LV2
VST3
PipeWire filter-chain integration
```

Linux-first means LV2 should be considered a first-class plugin format.

---

# 10. Output graph — crucial difference from OBS

Do not model streaming as:

```rust
Option<StreamingOutput>
```

Model:

```text
OutputGraph
 ├── RecordingOutput
 ├── TwitchOutput
 ├── YouTubeOutput
 ├── CustomRTMPOutput
 ├── SRTOutput
 ├── WHIPOutput
 └── VirtualCameraOutput
```

An output has:

```rust
Output {
    id,
    video_encoder,
    audio_encoders,
    service,
    reconnect_policy,
    state,
    statistics,
}
```

This makes multistreaming native rather than an extension.

---

# 11. Native multistream

Example:

```text
                         ┌── Twitch RTMP
Scene → Video Mixer ─────┼── YouTube RTMP
        │                ├── Custom RTMP
        │                └── Recording MKV
        │
        └── Audio buses
```

Optimization:

If two destinations require exactly the same:

```text
resolution
FPS
codec
profile
bitrate
GOP
color format
```

share one encoded stream:

```text
Renderer
   ↓
Encoder
   ↓
Encoded packet tee
   ├── Twitch mux/output
   └── YouTube mux/output
```

Otherwise:

```text
Renderer
 ├── Encoder A → Twitch
 └── Encoder B → YouTube
```

Each destination needs independent:

```text
state
network queue
reconnect
statistics
error handling
credentials
latency
rate-control policy
```

One broken output must never stop another output.

---

# 12. Encoding

Initial codecs:

```text
H.264
HEVC
AV1
VP9 where needed
AAC
Opus
```

Hardware abstraction:

```text
NVIDIA NVENC
VA-API
Intel hardware encoding
software x264/x265/AV1 fallback
```

GStreamer already exposes hardware VA-based encoders including H.264, H.265 and AV1.

Agent research must benchmark:

```text
CPU copies/frame
GPU→CPU transitions
DMA-BUF preservation
encoder latency
composition latency
memory consumption
```

Zero-copy should be a project-level performance requirement.

---

# 13. Recording

Required:

```text
MKV
MP4
fragmented MP4
WebM where appropriate
```

Features:

```text
simultaneous stream + recording
separate recording encoder
multiple audio tracks
split recording by duration
split recording by size
automatic filename formatting
remux MKV → MP4
chapter markers
pause/resume
```

OBS supports separate recording encoders and multiple audio tracks, currently up to six in its standard workflow.

Your internal architecture should not artificially limit tracks to six.

---

# 14. Replay buffer

Implement as:

```text
encoded circular packet buffer
```

rather than raw frames.

```text
Encoder
   ↓
Ring Buffer<EncodedPacket>
   ↓ save request
Muxer
   ↓
File
```

Configuration:

```text
duration
memory limit
output format
hotkey/API save
```

---

# 15. Browser Source

For Linux-native integration investigate:

```text
WebKitGTK 6
webkit6 Rust bindings
```

WebKitGTK 6 is specifically the GTK4 API generation, and Rust bindings exist.

Required browser-source functionality:

```text
URL
local file
transparent background
viewport width/height
FPS
custom CSS
reload
shutdown when hidden
reload when activated
audio
sandboxing
JS bridge
```

OBS's browser source exposes comparable URL/local file, viewport, FPS, CSS and lifecycle options.

Research milestone required before implementation:

```text
Can WebKitGTK efficiently export rendered frames into DMABUF/GStreamer?
```

If not, consider an isolated browser renderer process.

Do not couple browser rendering directly to desktop WebView widgets.

---

# 16. Filters

Architecture:

```text
Source
   ↓
Filter
   ↓
Filter
   ↓
Filter
   ↓
Scene compositor
```

Generic:

```rust
trait Filter {
    fn descriptor(...)
    fn settings_schema(...)
    fn process(...)
}
```

Video MVP:

```text
Crop/Pad
Scale
Color Correction
Chroma Key
Color Key
Luma Key
Mask
Opacity
Sharpen
Delay
Scroll
```

Audio MVP:

```text
Gain
Compressor
Limiter
Gate
Expander
Noise suppression
Delay
```

OBS exposes essentially this style of ordered filter chains.

---

# 17. Transitions

Domain:

```rust
Transition {
    kind,
    duration,
    settings,
}
```

MVP:

```text
Cut
Fade
Swipe
Slide
Stinger
```

Then:

```text
per-scene transitions
transition matrix
quick transitions
transition override
```

---

# 18. Studio Mode

Explicit states:

```text
ProgramScene
PreviewScene
```

Commands:

```text
SetPreviewScene
TransitionToProgram
SwapPreviewProgram
```

Never hide this distinction in UI state.

The remote API must be able to control both independently.

---

# 19. Project persistence

Use two concepts analogous to OBS:

```text
SceneCollection
Profile
```

Scene collection:

```text
Scenes
Sources
Filters
Transitions
Audio configuration
```

Profile:

```text
Video resolution
FPS
Output configuration
Encoders
Streaming services
Recording settings
```

OBS intentionally separates scene collections from output profiles.

Suggested format:

```text
~/.config/<app>/
    profiles/
        twitch-1080p/
            profile.toml

    collections/
        development-stream/
            collection.json

    themes/
    plugins/
```

Use explicit schema versioning:

```json
{
  "schemaVersion": 4
}
```

Provide migrations:

```text
V1 → V2
V2 → V3
V3 → V4
```

Never silently discard unknown fields.

---

# 20. Remote-control architecture

This should be one of the defining features.

Everything is expressed as:

```text
Command
Query
Event
```

Example:

```rust
enum Command {
    SetCurrentScene { scene_id: SceneId },
    SetSourceVisible { item_id: SceneItemId, visible: bool },
    SetSourceVolume { source_id: SourceId, db: f32 },
    StartOutput { output_id: OutputId },
    StopOutput { output_id: OutputId },
}
```

Events:

```rust
enum Event {
    SceneChanged,
    SceneItemUpdated,
    AudioLevel,
    OutputStarted,
    OutputStopped,
    OutputStatistics,
    SourceAdded,
    SourceRemoved,
}
```

Every frontend talks to this exact contract.

---

# 21. IPC

Primary local IPC:

```text
Unix Domain Socket
```

For example:

```text
$XDG_RUNTIME_DIR/<app>/control.sock
```

Protocol:

```text
length-prefixed MessagePack
```

or:

```text
CBOR
```

with explicit protocol version.

Advantages:

```text
fast
local
low overhead
systemd friendly
easy CLI integration
permissions through filesystem
```

Optional later:

```text
D-Bus adapter
```

for desktop integration.

---

# 22. WebSocket API

Expose:

```text
ws://127.0.0.1:...
wss://...
```

Protocol concepts should intentionally resemble obs-websocket:

```text
Hello
Identify
Identified
Request
RequestResponse
RequestBatch
Event
Subscriptions
```

obs-websocket 5.x already validates this model and additionally supports RPC versioning, PubSub/event subscriptions, batches, and JSON/MessagePack concepts.

Strong recommendation:

Implement an additional:

```text
obs-websocket compatibility adapter
```

eventually.

This gives existing:

```text
Stream Deck tools
mobile clients
automation scripts
bots
home automation
```

a migration path.

Do NOT make the internal domain protocol identical to obs-websocket.

Instead:

```text
Core Command API
     ↑
     ├── Native WS adapter
     ├── OBS websocket adapter
     ├── Unix IPC adapter
     └── CLI adapter
```

---

# 23. Remote Web UI

Backend:

```text
axum
tokio
tower
rustls
WebSocket
```

Web UI features:

```text
stream start/stop
recording start/stop
scene switching
preview thumbnails
source visibility
audio faders
mute
output statistics
multi-stream status
replay buffer
studio mode
transition
source properties
```

Design the protocol so the web UI receives incremental events.

Never periodically download the entire project state.

Example:

```text
InitialStateSnapshot
+
Event stream
```

---

# 24. Authentication and security

Remote server configuration:

```text
disabled by default
bind address
TLS
token authentication
permissions
```

Permission model:

```text
Read
ControlScenes
ControlAudio
ControlOutputs
ModifyConfiguration
Admin
```

Tokens:

```text
controller
readonly-dashboard
admin
```

A web client should not automatically receive filesystem-level operations.

---

# 25. CLI

Example:

```bash
studioctl status

studioctl scenes list

studioctl scene switch gaming

studioctl source mute mic

studioctl output start twitch

studioctl output start youtube

studioctl record start
```

CLI talks through IPC.

It must not initialize GTK or GStreamer itself.

---

# 26. Plugin architecture

Do not start with dynamically loaded native `.so` Rust plugins.

First define stable extension interfaces.

Categories:

```text
SourceProvider
FilterProvider
OutputProvider
EncoderProvider
ServiceProvider
AutomationProvider
UIExtension
```

Stage 1:

built-in Rust registry.

Stage 2:

out-of-process plugins using IPC.

Stage 3:

WASM plugin ABI where applicable.

Out-of-process plugins are safer than unstable Rust dynamic ABI.

---

# 27. Theme system

Use libadwaita as baseline rather than replacing its semantics.

Support:

```text
system
light
dark
custom
```

Theme package:

```text
theme.toml
style.css
assets/
```

Variables:

```text
accent
warning
error
streaming
recording
audio-meter
canvas-background
panel-background
```

User themes:

```text
~/.config/<app>/themes/<name>/
```

Use GTK CSS overlays.

Avoid hard-coded colors inside widgets.

---

# 28. Desktop layout

Recommended principal window:

```text
┌───────────────────────────────────────────────────────────┐
│ HeaderBar        Profile | Collection | Stream status    │
├───────────────────────────────────────────────────────────┤
│                                                           │
│                     Preview Canvas                        │
│                                                           │
├──────────────┬───────────────────┬────────────────────────┤
│ Scenes       │ Sources           │ Audio Mixer            │
│              │                   │                        │
├──────────────┴───────────────────┴────────────────────────┤
│ Transition             │ Outputs / Controls               │
└───────────────────────────────────────────────────────────┘
```

Use detachable/optional panels later.

Do not attempt to copy OBS's Qt dock implementation.

Prefer adaptive libadwaita UI.

---

# 29. Main OBS feature-parity matrix

Agents maintain:

```text
docs/research/obs-feature-matrix.md
```

with:

| Domain | Feature | Priority |
|---|---|---|
| Scenes | scene management | P0 |
| Scenes | transforms | P0 |
| Scenes | groups | P1 |
| Sources | display capture | P0 |
| Sources | window capture | P0 |
| Sources | camera | P0 |
| Sources | audio input/output | P0 |
| Sources | image/media | P0 |
| Sources | browser | P1 |
| Audio | mixer | P0 |
| Audio | filters | P1 |
| Video | filters | P1 |
| Output | recording | P0 |
| Output | RTMP streaming | P0 |
| Output | multistream | P0 |
| Output | replay buffer | P1 |
| Output | multi-track audio | P1 |
| Streaming | reconnect | P0 |
| Streaming | service profiles | P0 |
| UI | studio mode | P1 |
| UI | multiview | P2 |
| UI | projectors | P2 |
| UI | statistics | P1 |
| Config | profiles | P1 |
| Config | scene collections | P1 |
| Control | hotkeys | P1 |
| Control | WebSocket | P0 |
| Control | IPC | P0 |
| Control | Web UI | P1 |
| Control | CLI | P1 |
| Plugins | extensions | P2 |
| Linux | virtual camera | P1 |
| Linux | PipeWire capture | P0 |
| Linux | Wayland | P0 |
| UX | themes | P1 |

The matrix must always contain:

```text
OBS behavior
Our desired behavior
Status
Dependencies
Tests
Known differences
```

---

# 30. Repository architecture

Recommended Cargo workspace:

```text
studio/
├── Cargo.toml
├── AGENTS.md
├── README.md
├── rust-toolchain.toml
├── deny.toml
├── justfile
│
├── crates/
│   ├── studio-domain/
│   ├── studio-core/
│   ├── studio-state/
│   ├── studio-media/
│   ├── studio-media-gst/
│   ├── studio-capture-linux/
│   ├── studio-audio/
│   ├── studio-output/
│   ├── studio-streaming/
│   ├── studio-recording/
│   ├── studio-protocol/
│   ├── studio-ipc/
│   ├── studio-websocket/
│   ├── studio-web/
│   ├── studio-plugin-api/
│   ├── studio-config/
│   ├── studio-ui/
│   └── studio-cli/
│
├── apps/
│   ├── studio/
│   └── studioctl/
│
├── web/
│
├── docs/
│   ├── architecture/
│   ├── adr/
│   ├── research/
│   ├── protocols/
│   └── testing/
│
└── .agent/
```

Dependency rule:

```text
domain
 ↑
core
 ↑
services
 ↑
UI / API
```

`studio-domain` must not depend on:

```text
GTK
GStreamer
Tokio
Axum
```

where avoidable.

---

# 31. Agentic development system

The repository itself becomes the shared memory.

Never depend on:

```text
Claude conversation history
Codex session history
Kimi context
```

A new agent must be able to start with zero previous model context.

Required files:

```text
AGENTS.md

.agent/
├── STATE.yaml
├── HANDOFF.md
├── JOURNAL.md
├── BACKLOG.yaml
├── CURRENT_TASK.yaml
├── DECISIONS.md
└── tasks/
```

---

# 32. AGENTS.md

Contains permanent project rules:

```text
architecture
coding standards
allowed dependencies
module boundaries
test requirements
commit rules
performance requirements
security requirements
documentation requirements
commands
```

Every agent reads this first.

It changes rarely.

---

# 33. STATE.yaml

Machine-readable resumable state:

```yaml
schema_version: 1

project_phase: media-foundation

head_commit: 75ab91e

current_objective:
  id: MEDIA-021
  title: PipeWire display capture

completed:
  - CORE-001
  - CORE-002
  - MEDIA-001

in_progress:
  - MEDIA-021

blocked: []

next_candidates:
  - MEDIA-022
  - MEDIA-023

decisions:
  - ADR-0004
  - ADR-0007

verification:
  cargo_fmt: pass
  cargo_clippy: pass
  cargo_test: pass

last_agent:
  model: codex
  session: unknown
```

This is the authoritative resume pointer.

---

# 34. Task format

Each task:

```yaml
id: MEDIA-021

title: PipeWire display capture

goal:
  Implement Wayland display capture through xdg-desktop-portal.

depends_on:
  - MEDIA-004

allowed_scope:
  - crates/studio-capture-linux
  - crates/studio-media-gst

acceptance:
  - portal selector appears
  - PipeWire stream starts
  - stream survives UI resize
  - cancellation handled
  - source removal handled
  - automated tests added

research_required:
  - portal ScreenCast v6
  - GStreamer pipewiresrc

validation:
  - cargo test
  - cargo clippy --workspace --all-targets -- -D warnings

status: ready
```

Agents should work from atomic tasks rather than vague instructions like:

```text
implement OBS
```

---

# 35. HANDOFF.md

Every coding agent ends by rewriting:

```text
# Current state

## Completed
...

## Changed
...

## Architecture decisions
...

## Tests
...

## Known issues
...

## Exact next task
...

## Recommended files to read
...

## Commands to reproduce
...
```

Maximum roughly 2–4 pages.

It is optimized for the next model.

---

# 36. JOURNAL.md

Append-only development log.

Example:

```text
2026-10-01 MEDIA-021

Investigated xdg ScreenCast portal restore_token behavior.

Decision:
store restore token with source configuration.

Problem:
PipeWire node IDs are not persistent.

Use pipewire-serial instead.
```

This prevents agents repeatedly rediscovering the same issue.

The portal documentation explicitly warns that PipeWire node IDs can be reused and recommends targeting using the `pipewire-serial` property.

---

# 37. ADRs

Every significant decision becomes:

```text
docs/adr/ADR-0007-gstreamer-media-engine.md
```

Format:

```text
Context
Decision
Alternatives
Consequences
Evidence
Status
```

Essential ADRs initially:

```text
ADR-0001 Linux-only
ADR-0002 Relm4/libadwaita
ADR-0003 Wayland-first
ADR-0004 GStreamer media engine
ADR-0005 Command/Event core API
ADR-0006 Unix-socket IPC
ADR-0007 output graph for multistreaming
ADR-0008 project persistence model
ADR-0009 plugin isolation strategy
ADR-0010 OBS WebSocket compatibility layer
```

---

# 38. Multi-agent roles

Do not allow every model to randomly modify everything.

Use logical roles.

## Orchestrator

Responsibilities:

```text
read repository state
select next tasks
split work
detect blockers
integrate changes
update STATE
```

Should generally not implement large features.

---

## Research agent

Produces:

```text
docs/research/*.md
```

Research areas:

```text
OBS behavior
GStreamer
PipeWire
portal APIs
encoding
service protocols
browser rendering
hardware acceleration
```

Must include source links and concrete conclusions.

---

## Architecture agent

Creates:

```text
ADRs
interfaces
crate boundaries
protocols
domain models
```

No large implementation unless necessary as prototype.

---

## Implementation agents

Specializations:

```text
Core
Media
Linux capture
Audio
Outputs
GTK UI
Remote API
Web UI
Plugins
```

---

## Verification agent

Does not add features unless repairing defects.

Checks:

```text
correctness
tests
architecture
unsafe Rust
locking
deadlocks
memory lifetime
latency
error handling
API consistency
```

---

# 39. Model-independent workflow

Claude, Codex, Kimi, Gemini or another coding agent should receive the same startup instruction:

```text
1. Read AGENTS.md.
2. Read .agent/STATE.yaml.
3. Read .agent/HANDOFF.md.
4. Read the active task.
5. Read referenced ADRs.
6. Inspect git status and HEAD.
7. Continue the active task if incomplete.
8. Otherwise select the highest-priority ready task.
9. Research uncertain external APIs before coding.
10. Implement only the task scope.
11. Run validation.
12. Update documentation.
13. Commit.
14. Update STATE.yaml.
15. Rewrite HANDOFF.md.
```

This is the central mechanism that makes agents interchangeable.

---

# 40. Interruption protocol

At any interruption:

```text
SIGINT
token limit
model crash
provider rate limit
manual user interruption
```

the agent should, whenever possible:

1. Stop starting new work.
2. Make the working tree compilable if feasible.
3. Run the smallest useful validation.
4. Write `.agent/HANDOFF.md`.
5. Update `.agent/STATE.yaml`.
6. Record unfinished work.
7. Commit a checkpoint.

Example:

```text
wip(MEDIA-021): checkpoint PipeWire portal integration
```

A WIP commit is better than leaving undocumented dirty state.

The next model resumes from the repository.

---

# 41. Definition of Done

No task is complete merely because code exists.

Required:

```text
implementation
unit tests
integration tests where possible
documentation
observability
errors handled
no new clippy warnings
format clean
state updated
handoff updated
commit created
```

Feature tasks additionally need acceptance tests.

---

# 42. Development phases

## Phase 0 — Research and architecture

No large UI implementation.

Deliver:

```text
OBS feature matrix
GStreamer capabilities matrix
Linux capture matrix
encoding matrix
remote protocol design
domain model
ADRs
benchmark harness
```

Exit criteria:

```text
architecture can express:
scene
source
filter
audio
output
multistream
recording
remote command
```

---

# 43. Phase 1 — Skeleton

Implement:

```text
workspace
domain crate
event bus
command bus
config system
persistence
Relm4 shell
logging
error model
IPC skeleton
```

Demo:

```text
studioctl ping
```

returns status from running GTK application.

This proves UI and backend are properly separated.

---

# 44. Phase 2 — Media prototype

Implement:

```text
test-pattern source
GStreamer pipeline
scene
compositor
preview
GTK paintable
```

Demo:

```text
two video sources
move one source
resize it
change z-order
```

No streaming yet.

---

# 45. Phase 3 — Linux capture

Implement:

```text
PipeWire
portal monitor capture
portal window capture
V4L2 camera
audio input
audio output/application capture
device discovery
hotplug
```

Test on:

```text
GNOME Wayland
KDE Plasma Wayland
X11 fallback
```

---

# 46. Phase 4 — Scene editor

Implement:

```text
scene list
source list
canvas selection
drag
resize
crop
rotate
snap
z-order
locking
visibility
groups
undo/redo
```

Commands go through application core.

No UI-only mutations.

---

# 47. Phase 5 — Audio

Implement:

```text
mixer
meters
mute
gain
monitoring
audio routing
filters
sync delay
multiple tracks
```

Stress tests:

```text
32 simultaneous audio sources
```

---

# 48. Phase 6 — Recording

Implement:

```text
encoding
muxing
MKV
multiple audio tracks
recording status
pause
remux
statistics
```

Reliability test:

```text
kill -9 during recording
```

MKV recording should remain recoverable.

OBS similarly recommends MKV partly for resilience against interrupted recording.

---

# 49. Phase 7 — Streaming

Implement generic streaming:

```text
RTMP/RTMPS
service abstraction
reconnect
backoff
statistics
credentials
```

Then service presets:

```text
Twitch
YouTube
custom RTMP
```

Later:

```text
SRT
RIST
WHIP
```

---

# 50. Phase 8 — Multistream

Implement:

```text
N simultaneous outputs
shared encoders
dedicated encoders
independent errors
independent reconnect
individual statistics
```

Critical test:

```text
Twitch network sink dies
YouTube continues uninterrupted.
```

Another:

```text
Recording continues while every remote stream reconnects.
```

---

# 51. Phase 9 — Remote control

Implement:

```text
Unix IPC
CLI
native WebSocket protocol
event subscriptions
authentication
batch operations
```

Then build:

```text
obs-websocket compatibility adapter
```

Compatibility testing should use existing OBS WebSocket clients.

---

# 52. Phase 10 — Remote Web UI

Start small:

```text
outputs
scenes
audio
sources
statistics
```

Then:

```text
studio mode
transitions
source properties
scene editor
```

Do not build a second independent application state model.

Web UI must be only another remote client.

---

# 53. Phase 11 — Browser source

Implement isolated browser renderer.

Test:

```text
transparent web page
animated CSS
video playback
audio
WebGL
alerts
60 FPS overlay
```

Measure:

```text
CPU
GPU
memory
frame latency
frame drops
```

---

# 54. Phase 12 — OBS-level UX functionality

Add:

```text
Studio Mode
Replay Buffer
Transitions
Hotkeys
Profiles
Scene Collections
Multiview
Projectors
Statistics
Automatic reconnect
Stream delay
Virtual camera
```

OBS exposes hotkeys for streaming/recording, sources, scenes and replay buffer operations, so these operations should already exist as generic commands before hotkey support is added.

---

# 55. Phase 13 — Extensibility

Implement:

```text
plugin manifest
plugin discovery
plugin process
plugin IPC
capability negotiation
API versioning
```

Then investigate:

```text
WASM plugins
Rust native plugins
Lua scripting
```

Prefer stable message protocols over Rust ABI.

---

# 56. Performance targets

Make them explicit from day one.

Preview:

```text
60 FPS stable
```

Control latency:

```text
IPC command p95 < 10 ms
```

Scene switch:

```text
command → applied < one output frame when no transition
```

Memory:

```text
avoid duplicate raw frame buffers
```

Media:

```text
DMABUF wherever possible
no GPU→CPU→GPU round trip without justification
```

Audio:

```text
bounded queues
no allocation in critical per-buffer paths where feasible
```

Remote events:

```text
meter events throttled/coalesced
```

Never emit 60 × number-of-audio-sources JSON events per second to every WebSocket client blindly.

---

# 57. Concurrency model

Separate domains:

```text
GTK main thread
Tokio runtime
GStreamer streaming threads
media control actor
persistent-state actor
WebSocket tasks
```

Do not protect application state with one giant:

```rust
Arc<Mutex<AppState>>
```

Prefer owner/actor style.

Example:

```text
Command
   ↓
Core actor
   ↓
Domain mutation
   ↓
Media control actor
   ↓
GStreamer
```

Snapshots can be shared as immutable:

```rust
Arc<AppSnapshot>
```

---

# 58. Event model

Events should be strongly typed.

Example:

```rust
enum AppEvent {
    Scene(SceneEvent),
    Source(SourceEvent),
    Audio(AudioEvent),
    Output(OutputEvent),
    System(SystemEvent),
}
```

Remote adapters serialize these.

Do not create separate event definitions for:

```text
GTK
WebSocket
IPC
CLI
```

---

# 59. Undo/redo

Use commands with reversible domain operations:

```text
MoveSceneItem
ResizeSceneItem
RenameSource
AddSource
DeleteSource
```

Maintain transaction groups:

```text
drag start
100 pointer movements
drag end
```

should produce one undo entry, not 100.

Remote API should optionally support transactions.

---

# 60. Observability

Use structured tracing.

Every long-lived object carries ID context:

```text
source_id
output_id
scene_id
request_id
```

Example:

```text
output_id=youtube-main
service=youtube
state=reconnecting
attempt=4
backoff_ms=8000
```

Expose metrics internally:

```text
render FPS
dropped frames
encoder latency
encoder queue
network bitrate
network dropped frames
audio underruns
memory usage
GPU upload/copy count
```

Remote UI should consume the same metrics.

---

# 61. Failure model

Agents must explicitly design failure cases.

Examples:

```text
camera removed
monitor disconnected
PipeWire restarts
portal session expires
encoder crashes
WebKit process crashes
network disappears
stream server rejects authentication
recording disk fills
plugin process dies
config file corrupted
GPU device disappears
```

Each component should have:

```text
Running
Degraded
Recovering
Failed
Stopped
```

where applicable.

A single source failure should not crash the program.

---

# 62. CI

Minimum:

```text
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo deny check
cargo audit
```

Additional:

```text
cargo llvm-cov
cargo nextest
```

Linux integration environment should install:

```text
GTK4
libadwaita
GStreamer
PipeWire
portal development packages
WebKitGTK
```

---

# 63. Testing layers

Unit:

```text
domain transformations
routing
serialization
state changes
```

Integration:

```text
GStreamer pipeline
recording
encoding
IPC
WebSocket
```

Golden:

```text
project serialization
protocol messages
```

End-to-end:

```text
start daemon/app
create source
create scene
start recording
change scene
stop recording
inspect result
```

Performance:

```text
1080p60
1440p60
4K60
multi-output
many browser sources
many audio sources
```

---

# 64. Agent research loop

When a task touches an uncertain subsystem:

```text
Research
 ↓
Write research note
 ↓
Make ADR if architectural
 ↓
Prototype
 ↓
Benchmark
 ↓
Decide
 ↓
Production implementation
```

Agents must not hallucinate external APIs.

For example:

```text
"implement zero-copy WebKit browser source"
```

must begin with research/prototype because that capability determines the architecture.

---

# 65. Research backlog for OBS parity

Initial research agents should independently inspect:

```text
OBS source architecture
OBS scene graph
OBS output architecture
OBS audio pipeline
OBS filters
OBS transitions
OBS replay buffer
OBS studio mode
OBS WebSocket API
OBS profiles
OBS scene collections
OBS virtual camera
OBS browser source
OBS recording
OBS reconnect behavior
OBS hotkeys
OBS plugin system
```

The result is not source-code cloning.

The result should be:

```text
Behavior
User expectation
Domain model
Required API
Implementation proposal
Priority
```

---

# 66. First 30 agent tasks

Recommended starting DAG:

```text
RES-001 OBS feature inventory
RES-002 OBS architecture research
RES-003 GStreamer capability research
RES-004 Linux capture research
RES-005 encoder capability matrix
RES-006 browser-source research
RES-007 obs-websocket analysis

ARCH-001 domain model
ARCH-002 command/event API
ARCH-003 scene graph
ARCH-004 media abstraction
ARCH-005 output graph
ARCH-006 persistence model
ARCH-007 remote protocol

BOOT-001 Cargo workspace
BOOT-002 quality tooling
BOOT-003 tracing/error model
BOOT-004 agent state infrastructure

CORE-001 application actor
CORE-002 command dispatcher
CORE-003 event broadcaster
CORE-004 persistence
CORE-005 undo/redo

MEDIA-001 initialize GStreamer
MEDIA-002 test source
MEDIA-003 compositor
MEDIA-004 GTK preview bridge
MEDIA-005 transform support

UI-001 Relm4 application shell
UI-002 preview panel
UI-003 scene list
UI-004 source list

IPC-001 Unix socket server
IPC-002 studioctl ping/status
```

These can already be distributed across several coding agents.

---

# 67. First meaningful milestone

Avoid defining:

```text
"UI opens"
```

as milestone 1.

Milestone 1 should demonstrate architectural correctness:

```text
1. Launch GTK app.
2. Test-pattern video appears.
3. Create two sources.
4. Compose them.
5. Move source via GTK.
6. Move source via CLI.
7. Move source via WebSocket.
8. GTK updates after every remote operation.
9. Save project.
10. Restart.
11. Project restores.
```

If this works, the core architecture is sound.

---

# 68. Second milestone

Real Linux capture:

```text
1. Add Wayland display capture.
2. Portal opens.
3. Display appears.
4. Add microphone.
5. Mixer operates.
6. Record MKV.
7. Control visibility/mute remotely.
```

---

# 69. Third milestone

Actual broadcaster:

```text
1. PipeWire display.
2. Camera.
3. Microphone.
4. Scenes.
5. Scene transitions.
6. Audio filters.
7. RTMP stream.
8. MKV recording.
9. WebSocket control.
10. CLI.
```

At this point it becomes useful software.

---

# 70. Fourth milestone

Differentiating feature set:

```text
YouTube + Twitch simultaneously
independent output status
remote Web UI
remote audio mixer
output recovery
shared encoder optimization
```

This is where the project stops being merely an OBS clone.

---

# 71. Agent orchestration command

A generic runner can eventually behave like:

```bash
./agent run \
    --model codex \
    --until-blocked \
    --max-tasks 10
```

Next:

```bash
./agent resume --model claude
```

Next:

```bash
./agent resume --model kimi
```

Each invocation uses only:

```text
Git
AGENTS.md
.agent/*
docs/*
tests
```

as shared state.

The model provider becomes replaceable infrastructure.

---

# 72. Orchestrator algorithm

Conceptually:

```text
while project not blocked:

    read STATE

    if current task exists:
        resume current task
    else:
        select highest priority task
        whose dependencies are complete

    inspect related research
    inspect ADRs

    if information insufficient:
        create research task
        execute research
        persist findings

    implement task

    run validation

    if failed:
        repair or document blocker

    if successful:
        commit
        mark task complete

    update STATE
    update HANDOFF
```

The orchestration system should never rely on an LLM remembering the previous iteration.

---

# 73. Branch strategy for several simultaneous agents

Use:

```text
main
agent/<task-id>
```

Example:

```text
agent/MEDIA-021
agent/UI-014
agent/API-008
```

One orchestrator integrates completed branches after validation.

Agents must not share a single mutable working tree simultaneously.

Use:

```text
git worktree
```

Example:

```text
.worktrees/
    MEDIA-021/
    UI-014/
    API-008/
```

This gives Claude, Codex and Kimi physically separated workspaces.

---

# 74. Parallelization rules

Safe:

```text
media research + UI shell
WebSocket protocol + GStreamer capture
documentation + tests
```

Unsafe:

```text
two agents redesigning domain model
two agents changing protocol schema
two agents changing same compositor abstraction
```

Architecture-affecting work goes through ADR first.

---

# 75. Code standards

Recommended:

```text
No unwrap() in production paths.
No expect() except impossible invariant with explanation.
No blocking filesystem/network operations on async runtime.
No GTK access outside GTK main thread.
No unbounded channels for media/control data.
No global mutable singleton state.
No raw IDs represented as String.
No giant AppState mutex.
No direct UI→GStreamer mutation.
No protocol structs reused as domain structs.
```

Require:

```text
explicit newtypes
typed errors
bounded queues
cancellation
structured concurrency
resource ownership
```

---

# 76. Architectural principle

The most important invariant for the entire project:

```text
Every user-visible operation is a Core Command.
Every state change produces a Core Event.
```

Consequently:

```text
GTK UI
CLI
Web UI
WebSocket
Unix IPC
hardware controller
automation
plugin
```

become interchangeable controllers.

That directly satisfies the goal of exceptionally rich remote control.

---

# 77. Scope strategy

Do not attempt complete OBS parity initially.

Build vertically:

```text
capture
→ composition
→ preview
→ recording
→ streaming
→ remote control
```

before implementing breadth such as:

```text
50 filters
scripts
plugin marketplace
multiview
advanced projectors
```

The first release should have a small but production-grade vertical stack.

---

# 78. Recommended MVP

Version `0.1`:

```text
GTK4/Relm4/libadwaita
Wayland/PipeWire
display capture
window capture
V4L2 camera
microphone
media/image source
scenes
transforms
audio mixer
basic filters
H.264
AAC
MKV recording
RTMP streaming
Twitch/YouTube
native multistream
Unix IPC
CLI
WebSocket
profiles
scene collections
themes
```

Version `0.2`:

```text
browser source
studio mode
transitions
replay buffer
multiple audio tracks
remote Web UI
hardware encoder tuning
OBS WebSocket compatibility
```

Version `0.3`:

```text
virtual camera
SRT
WHIP
RIST
plugin API
LV2/VST3
multiview
projectors
advanced transitions
```

---

# 79. What I would deliberately NOT copy from OBS

Do not copy:

```text
Qt-oriented frontend architecture
single-stream assumptions
frontend-only state changes
plugin ABI constraints
OS abstraction required for Windows/macOS
historic compatibility decisions
```

Keep the concepts that have proven useful:

```text
Source
Scene
SceneItem
Filter
Encoder
Output
Service
Profile
SceneCollection
Transition
```

and redesign the implementation for:

```text
Linux
Wayland
PipeWire
Rust
remote-first control
native multistreaming
```

---

# 80. Final architecture target

```text
                         ┌─────────────────┐
                         │  GTK / Relm4 UI │
                         └────────┬────────┘
                                  │
┌────────────┐  ┌─────────┐  ┌───▼────┐  ┌────────────┐
│ Remote Web │  │   CLI   │  │  Core  │  │ Automation │
└──────┬─────┘  └────┬────┘  └───┬────┘  └─────┬──────┘
       │              │           │             │
       └──── WebSocket/IPC ───────┼─────────────┘
                                  │
                           Command/Event API
                                  │
                     ┌────────────▼────────────┐
                     │     Domain Model       │
                     └────────────┬────────────┘
                                  │
                         Media abstraction
                                  │
        ┌─────────────────────────▼──────────────────────┐
        │                GStreamer Core                 │
        │                                               │
        │ Sources → Filters → Mixer → Compositor       │
        │                             │                 │
        │                             ▼                 │
        │                        Encoder Graph          │
        └─────────────────────────────┬─────────────────┘
                                      │
           ┌──────────────────────────┼───────────────────────┐
           │                          │                       │
           ▼                          ▼                       ▼
      Recording                  Twitch                  YouTube
           │
           ▼
       Replay buffer
```

This architecture gives the project a realistic path toward OBS-class capabilities while making multistreaming, remote operation, Linux/Wayland integration and agent-driven maintainability first-class design constraints rather than later additions.


prismcast/
├── prismcast-core
├── prismcast-media
├── prismcast-compositor
├── prismcast-audio
├── prismcast-output
├── prismcast-protocol
├── prismcast-remote
├── prismcast-web
├── prismcast-ui
├── prismcast-plugin-sdk
└── prismcast-cli



This is initial PLAN, feel free to extend and use OBS studio codebase (open-source one) for referrence and sneakpicking.
