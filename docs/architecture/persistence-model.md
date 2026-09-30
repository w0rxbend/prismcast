# Persistence Model

Status: design (implementation task: CORE-004)
Basis: PLAN.md §19 (project persistence), §27 (themes), §48/§61 (crash resilience, failure
model), §57 (concurrency model), §63 (golden tests), §67 (restart-restore milestone);
ADR-0003 (portal restore tokens), ADR-0005 (command/event core), ADR-0007 (output graph),
ADR-0008 (persistence decision); `docs/research/obs-architecture.md` §"Persistence".

This document defines how Prismcast persists projects: where files live, what format each
file uses, how schemas evolve, how writes survive crashes, and which actor owns the work.
The domain aggregates referenced below live in `crates/prismcast-core/src/`; this document
maps every one of them to its on-disk home.

## 1. Filesystem layout

Root: `$XDG_CONFIG_HOME/prismcast/`, defaulting to `~/.config/prismcast/`. The root path is
resolved once at startup and injected into the persistence actor (never read from the
environment mid-run, so tests can point it at a tempdir).

```text
~/.config/prismcast/
    prismcast.toml                     # app-level pointer file (active profile/collection)
    profiles/
        twitch-1080p/
            profile.toml               # hand-editable
            profile.toml.bak           # last known-good copy
    collections/
        development-stream/
            collection.json            # machine-managed
            collection.json.bak        # last known-good copy
    themes/
        <name>/{theme.toml, style.css, assets/}    # PLAN §27; not covered further here
    plugins/                           # plugin-managed; not covered here
```

Rules:

- **Directory name = slugified entity name.** The directory is a human-facing handle; the
  authoritative identity is the `ProfileId` / `SceneCollectionId` UUID stored *inside* the
  file (PLAN §3 / ADR-0008: IDs stable across persistence, IPC, WebSocket). On load, the
  in-file ID wins over the directory name. A rename operation rewrites the file first, then
  renames the directory; a crash between the two is harmless because identity comes from the
  file content.
- **Slugification** is lowercase ASCII, spaces → `-`, other non-`[a-z0-9-_]` stripped,
  collisions suffixed `-2`, `-3`, …. Slugification is a pure, tested function.
- **`.bak` files** sit next to their primary in the same directory (same filesystem —
  required for the atomic-rename and fallback semantics in §5–§6). They are refreshed only
  from files that have been verified to parse (§6).
- File permissions: `profile.toml` and its `.bak` are created `0600` because they carry
  stream keys (`Service.key`, see §3). Directories `0700` for the same reason.
- **Secrets at rest.** `SecretString` (`output.rs`) serializes transparently so stream keys
  survive restart, but is plaintext on disk. The `0600` permission is the entire protection
  for now; keyring/secret-service storage is an explicit follow-up (see §9). `SecretString`
  still never appears in logs (`Display`/`Debug` redact).

## 2. Two file families, two formats

ADR-0008: profiles are TOML (hand-editable), collections are JSON (machine-managed,
golden-tested per PLAN §63). The reasons are user-facing, not technical, and they drive
different unknown-field strategies (§4):

| | `profile.toml` | `collection.json` |
|---|---|---|
| Aggregate | `Profile` + output graph (§3) | `SceneCollection` + session state (§3) |
| Format | TOML | JSON (pretty-printed, 2-space, trailing newline; byte-stable for golden tests and VCS diffs) |
| Edited by | users (hand edits expected) | the app only |
| Unknown fields | preserved via retained TOML document | preserved via captured `serde_json::Value` |
| Schema field | `schemaVersion = N` (top-level key) | `"schemaVersion": N` (top-level key) |
| Current schema | 1 | 1 |

### Envelope

Both files wrap their domain payload in a **persisted envelope**. Envelopes are new
serialize-only structs in the persistence layer — they are *not* the domain structs and are
never used as such (AGENTS.md: no reusing wire/persisted structs as domain structs; the
converse also holds). The domain types (`Profile`, `SceneCollection`, …) keep deriving
serde so envelope fields can embed them directly.

```rust
// persistence layer (application core), not prismcast-core
#[derive(Serialize, Deserialize)]
struct CollectionFileV1 {
    #[serde(rename = "schemaVersion")]
    schema_version: u32,            // == 1
    id: SceneCollectionId,
    name: String,
    scenes: Vec<Scene>,             // domain types embedded verbatim
    sources: Vec<Source>,
    transition: Transition,
    audio: AudioMixerConfig,
    session: SessionState,          // see §3
    #[serde(flatten)]
    unknown: serde_json::Map<String, serde_json::Value>,  // §4
}

#[derive(Serialize, Deserialize)]
struct SessionState {
    current_scene: Option<SceneId>,
    studio_mode: Option<StudioMode>,
}
```

`SessionState` (current scene + studio mode) is persisted *inside* `collection.json`,
mirroring OBS, which stores the current scene in the collection file. These are
collection-scoped UI semantics, not profile data; PLAN §19's collection contents list is a
minimum, not an exclusion. Switching collections restores their last current scene.

## 3. Aggregate → file mapping

Every persisted `prismcast-core` aggregate and its home:

| Domain type (crate file) | File | Path within file |
|---|---|---|
| `Profile`, `VideoConfig` (`project.rs`) | `profiles/<name>/profile.toml` | top level / `[video]` |
| `Output`, `OutputKind`, `ReconnectPolicy` (`output.rs`) | `profile.toml` | `[[outputs]]` |
| `EncoderSettings` (`output.rs`) | `profile.toml` | `[[encoders]]` |
| `Service`, `SecretString` (`output.rs`) | `profile.toml` | `[[services]]` |
| recording settings | `profile.toml` | `[recording]` (inside `Profile.settings`-shaped extra config) |
| `SceneCollection` (`project.rs`) | `collections/<name>/collection.json` | whole envelope |
| `Scene`, `SceneItem`, `Transform`, `Crop`, `Bounds`, `Anchor`, `BlendMode`, `Vec2` (`scene.rs`) | `collection.json` | `scenes[]` |
| `Source`, `SourceKind`, `FilterId` refs (`source.rs`) | `collection.json` | `sources[]` |
| `Transition`, `TransitionKind` (`transition.rs`) | `collection.json` | `transition` |
| `AudioMixerConfig`, `AudioBus`, `AudioRoute`, `TrackMask`, `AudioMixerState`, `MonitorMode` (`audio.rs`) | `collection.json` | `audio` |
| `SceneId` of current scene, `StudioMode` (`project.rs`) | `collection.json` | `session` |
| active `ProfileId`/`SceneCollectionId` | `prismcast.toml` | top-level pointer file |
| `Canvas`, `CanvasId` (`scene.rs`) | — | reserved stub, not persisted yet (RES-002); V1 schema leaves room |

Per-aggregate notes:

- **Outputs/encoders/services belong to the profile**, per PLAN §19 (output configuration,
  encoders, streaming services, recording settings are profile data) — even though
  `AppState` (`state.rs`) currently holds them in a top-level `outputs` map. CORE-004 must
  either nest them under the active profile in `AppState` or index them by `ProfileId`;
  the persisted form is fixed by this document regardless: they live in `profile.toml`.
- **`Output.state` is runtime-only and is never persisted.** The envelope writes each output
  without its lifecycle state; a loaded output always starts as `OutputState::Stopped`.
  Persisting `Reconnecting { attempt: 3 }` would resurrect a dead session's retry counter.
- **`Source.settings`, `Transition.settings`, `EncoderSettings.settings`, `Profile.settings`
  are open `serde_json::Value` blobs.** They are validated by their owning implementation,
  not the persistence layer, and pass through load/save untouched. This is the first line
  of unknown-field preservation: per-kind keys unknown to this build survive round-trips.
- **Portal restore tokens and `pipewire-serial`** live inside `Source.settings` for PipeWire
  capture sources (ADR-0003, PLAN research §36/portal notes). The persistence layer treats
  them as opaque settings; nothing special is needed at the file level.
- **`prismcast.toml`** (pointer file) is minimal: `schemaVersion`, active profile slug,
  active collection slug. If it is lost or corrupt, recovery is trivial — pick the first
  profile/collection on disk — so it gets atomic writes but no `.bak`.

Sketch of `profile.toml`:

```toml
schemaVersion = 1
id = "7c9e6679-7425-40de-944b-e07fc1f90ae7"
name = "twitch-1080p"

[video]
width = 1920
height = 1080
fps_num = 60
fps_den = 1

[[encoders]]
id = "…"
codec = "h264"
bitrate_kbps = 6000
keyframe_interval = 120
settings = { preset = "veryfast", rate_control = "cbr" }

[[services]]
id = "…"
name = "Twitch"
url = "rtmps://live.twitch.tv/app"
key = "live_…"            # SecretString; file is 0600

[[outputs]]
id = "…"
kind = "rtmp"
name = "Twitch main"
video_encoder = "…"       # EncoderId UUID string
audio_encoders = ["…"]
service = "…"             # ServiceId UUID string, optional

[outputs.reconnect_policy]
max_retries = 10
initial_backoff_ms = 1000
max_backoff_ms = 30000

[recording]
format = "mkv"
path = "~/Videos/prismcast"
```

## 4. Versioning, migrations, unknown fields

### Schema versioning

- Every persisted file carries `schemaVersion: N` (PLAN §19). `N` is a monotonic integer,
  **per file family** (profile and collection versions evolve independently).
- The running build supports exactly one *current* version per family and a migration chain
  up to it. Both constants live in one module (`CURRENT_PROFILE_SCHEMA`,
  `CURRENT_COLLECTION_SCHEMA`) so a bump is a single edit plus a migration.
- `schemaVersion` is read from the raw document *before* any typed deserialization. A file
  with no `schemaVersion` is treated as version 0 = invalid (pre-versioning files never
  shipped), yielding a typed corruption error rather than a guess.

### Stepwise migrations

Migrations are pure functions on the **raw document**, not on typed structs:

```rust
// One step, Vn → Vn+1. Pure: no I/O, no clock, no randomness.
type Migration = fn(serde_json::Value) -> Result<serde_json::Value, MigrationError>;

const COLLECTION_MIGRATIONS: &[Migration] = &[
    migrate_collection_v1_to_v2, // appended in the commit that bumps the schema
    // …
];
```

Load pipeline:

```text
read file
  → parse as raw document (serde_json::Value / toml document)
  → read schemaVersion
  → if version > CURRENT:  Err(NewerSchema { found, supported })   // report, never guess
  → if version < CURRENT:  apply MIGRATIONS[version..] in order    // V1→V2→V3→…
  → deserialize migrated document into the envelope struct
  → domain validation (referential integrity, §7)
```

Rules (ADR-0008 consequences):

- Every schema bump requires: version constant bump, one migration function, migration unit
  tests (Vn fixture → Vn+1 expectation), updated golden files (PLAN §63). CI fails on a
  bump without all four.
- Migrations never drop data. If a field is renamed, the migration moves it; if a field is
  retired, the migration moves it under an `x-legacy/<retired-field>` key in the envelope's
  `unknown` map rather than deleting it.
- After a successful load-with-migration, the file is **re-saved at the current version**
  through the normal atomic-write path (§5), so migration cost is paid once. The pre-write
  `.bak` refresh (§6) preserves the old-version file until the new one is verified.

### Unknown-field preservation

PLAN §19 / ADR-0008: never silently discard unknown fields. Mechanism differs by format:

- **JSON (collections):** the envelope's `#[serde(flatten)] unknown:
  Map<String, Value>` captures every top-level key this build does not know. Nested domain
  structs (`Scene`, `SceneItem`, `Source`, …) get the same treatment in their *persisted*
  form — note this means the persisted structs for collections are thin per-type wrappers
  with a flatten map, not raw domain-struct serialization. On save, the map is emitted
  back verbatim. Round-trip property (golden test): `save(load(bytes)) == bytes` for any
  well-formed V-current file, including files containing fields from the future.
- **TOML (profiles):** `#[serde(flatten)]` capture works but loses comments and ordering,
  which matters for a hand-edited file. Profiles therefore round-trip a **retained
  document**: load parses into a `toml` document tree, the loader reads known keys out of
  it, and the saver *patches the retained tree* with the in-memory state instead of
  serializing from scratch. Unknown sections/keys — and user comments adjacent to known
  values — survive byte-for-byte. Hand edits made while Prismcast is not running are thus
  first-class input, not an error case.
- **Forward compatibility** (older build, newer file) is reported via
  `Error::Persistence("file schema version N is newer than supported M …")` — a typed,
  user-surfaced error, never a best-effort parse (ADR-0008 rejected alternatives).

## 5. Atomic writes

Every persisted file is written with the same algorithm (write-to-temp + fsync + rename,
per ADR-0008 and `obs_data_save_json_safe`'s backup behavior in
`docs/research/obs-architecture.md` §"Persistence"):

```text
1. Serialize the envelope fully into an in-memory buffer.
   (Serialization failure leaves the filesystem untouched.)
2. Open <file>.tmp-<pid> in the same directory with O_EXCL,
   permissions 0600 (profiles) / 0644 (collections).
3. Write the whole buffer, then fsync the temp file.
4. rename(<file>.tmp-<pid>, <file>)        // atomic on POSIX, same directory ⇒ same fs
5. fsync the containing directory.         // persist the rename itself
```

Properties and edge cases:

- **The last good file is never truncated in place** (ADR-0008): a crash at any point
  leaves either the old file or the new file, never a torn one. A leftover
  `*.tmp-<pid>` from a crashed writer is ignored on load and reaped on the next save
  (delete any temp file older than the current process's start time).
- **Kill -9 mid-save** (PLAN §48 attitude, §61 "config file corrupted"): the worst case is
  the directory fsync (step 5) never ran, so the rename may be lost — the old file is
  still intact and `.bak` still holds the previous verified copy. Nothing is half-written.
- **Same-directory temp** is mandatory: `rename(2)` is only atomic within one filesystem.
  The temp file lives next to the target, not in `/tmp`.
- **Directory rename** (entity renamed by the user) is ordered file-write → directory
  rename → pointer-file update, so every intermediate state is loadable (§1: identity is
  in-file).
- Writes are issued from the persistence actor on the Tokio runtime via `spawn_blocking`
  (fsync is blocking syscall work; AGENTS.md forbids blocking I/O on runtime threads).

## 6. Corruption recovery

Failure case from PLAN §61: "config file corrupted". Load of each file is a small state
machine:

```text
load(path):
    parse primary
      ok              → verify → use; refresh .bak (atomic copy) if .bak != primary
      parse/verify err → parse path.bak
            ok   → restore: typed warning event + copy .bak over primary (atomic write),
                   continue with the .bak content
            fail → typed error Error::Persistence; DO NOT delete or overwrite anything;
                   quarantine both files aside (<file>.corrupt-<timestamp>) only once the
                   user confirms starting fresh, so evidence is never destroyed silently
```

Rules:

- **`.bak` is refreshed only from verified-good content** — a file that parsed, migrated,
  and validated. A `.bak` never contains bytes that were never successfully loaded; this is
  what makes the fallback trustworthy (ADR-0008: "falls back to the previous good copy").
- Recovery is **surfaced**, not silent: the core emits a typed event (exact variant to be
  added in CORE-004, e.g. `SystemEvent::PersistenceRecovered { path, reason }`) so GTK/CLI/
  WebSocket all show "recovered <name> from backup, changes since <mtime> were lost".
- **Total loss** (both files bad, or both absent for a referenced entity): startup
  continues with a fresh default `Profile`/`SceneCollection` (`AppState::new()` seeds
  exactly these) and a prominently surfaced error. Prismcast must always boot (PLAN §61:
  a single component failure must not crash the program).
- **Validation beyond syntax** (referential integrity, §7) failing counts as corruption for
  this flow.
- **Schema newer than supported** is *not* corruption: it takes the `NewerSchema` path (§4)
  and the file is left byte-identical.

## 7. Referential integrity on load

Deserialization proves shape, not sense. After a collection loads, the persistence layer
validates, and treats violations as corruption (§6):

- every `SceneItem.source_id` resolves to a `sources[]` entry;
- every `Source.filters[]` `FilterId` resolves (filters are stub-stage; rule activates when
  filters land);
- `SourceKind::Scene(scene_id)` targets an existing scene, and scene-source references are
  acyclic;
- every `AudioRoute.source_id` / `bus_id` resolves; mixer entries reference existing sources;
- `session.current_scene` / `studio_mode.{program,preview}` resolve (or are reset to
  `None`/first scene with a warning — session state is expendable and never corrupts the
  load);
- for profiles: `Output.video_encoder`/`audio_encoders`/`service` resolve into
  `[[encoders]]`/`[[services]]`;
- ID uniqueness within each entity class.

## 8. Persistence actor ownership

PLAN §57 names a dedicated **persistent-state actor**; ADR-0008 §5: saving is triggered by
the command stream, never directly from the UI. Design:

```text
controller (GTK/CLI/WS/…)          core actor                     persistence actor
        │  Command                     │                                │
        ├─────────────────────────────►│ apply → Events                 │
        │                              │ classify:                      │
        │                              │  · touches profile-only data?  │── SaveProfile { snapshot } ──►│
        │                              │  · touches collection data?    │── SaveCollection { snapshot } ►│
        │                              │  · session/volatile? (no save) │        (bounded mpsc)         │
        │                              │                                │ debounce → serialize →
        │                              │                                │ spawn_blocking(atomic write)
        │  Event (saved/recovered/     │◄── PersistenceEvent ───────────┤
        │   failed with typed error)   │                                │
```

- **Ownership:** the persistence actor is the *only* component that opens files under
  `~/.config/prismcast/`. It owns the root path, the slugification map (ID ↔ directory),
  write debouncing, and the `.bak` lifecycle. No other code touches the config tree;
  controllers go through commands, the core goes through messages.
- **Triggering from the command stream:** `state::apply` already centralizes mutation
  (ADR-0005). The core actor classifies each applied command: mutations to scenes/sources/
  transition/audio mark the active *collection* dirty; mutations to outputs/encoders/
  services/video mark the active *profile* dirty; `AddProfile`/`SelectProfile`/
  `AddSceneCollection`/`SelectSceneCollection` additionally schedule a pointer-file write.
  Explicit `SaveProject` (the milestone-1 verb, PLAN §67 step 9) forces an immediate flush.
- **Coalescing:** dirty marks are debounced (~500 ms trailing edge, capped by a max delay
  of a few seconds) so a 100-command drag transaction group (PLAN §59) produces one write,
  not 100. Shutdown performs a final synchronous flush; the app does not exit with pending
  dirty state without logging it.
- **Snapshots, not references:** `SaveProfile`/`SaveCollection` carry an immutable,
  already-cloned snapshot of the aggregate (`Arc`-shared within the core process). The
  actor never calls back into core state, so no lock coupling exists between domains
  (PLAN §57 owner/actor style; no `Arc<Mutex<AppState>>`).
- **Bounded channel:** the command→actor channel is bounded (AGENTS.md: no unbounded
  channels); backpressure simply coalesces — a full channel means a newer save supersedes
  the queued one, which is always safe because snapshots are whole aggregates.
- **Runtime state stays out:** audio levels, output statistics, `OutputState`, portal
  sessions are never persisted (§3). Restart-restore (PLAN §67) reconstructs them by
  re-running commands/restarting outputs from persisted config.
- **Concurrency within the actor:** one write per file at a time (per-file sequence
  numbers; a stale snapshot whose sequence was superseded is dropped before write).

## 9. Test plan (for CORE-004, per PLAN §63)

- **Golden files** under `tests/golden/`: one V1 `profile.toml` and one V1
  `collection.json`, byte-compared against serialization output (serialization stability),
  and loaded to assert structure (deserialization stability).
- **Round-trip with unknowns:** files containing extra top-level and nested keys (and, for
  TOML, comments) must survive `load → save` unchanged.
- **Migration tests:** one fixture per historical version, migrated to current, compared
  against the current golden.
- **Crash injection:** fault-inject each step of §5 (fail before rename, after rename,
  before dir fsync) and assert a loadable file always remains; kill the writer process
  mid-save and assert primary-or-backup loads.
- **Corruption matrix:** truncated file, valid-JSON-invalid-schema, wrong `schemaVersion`
  (0, future), broken referential integrity → each yields the expected typed error or the
  `.bak` fallback, and recovery events fire.
- **Restart-restore e2e:** the PLAN §67 sequence (save project → restart → project
  restores, sources/scenes/current scene intact).

## 10. Follow-ups / open questions

- **Secret storage:** move `Service.key` out of `profile.toml` into the desktop secret
  service (Secret Service API over D-Bus), persisting only a key reference. File format
  anticipates this: a future `key_ref = "secret:…"` field replaces inline `key` via a
  normal schema migration.
- **Core event variant** for persistence recovery/failure (§6, §8) must be added to
  `prismcast-core` in CORE-004 — deliberately not pre-defined here to keep this task
  doc-only.
- **`AppState` outputs nesting** (§3): CORE-004 decides between `Profile.outputs` vs.
  `outputs_by_profile: IndexMap<ProfileId, …>`; persisted form is unaffected.
- **Import/export** of collections (shareable project files) is out of scope here but
  should reuse the envelope + migration machinery verbatim.
