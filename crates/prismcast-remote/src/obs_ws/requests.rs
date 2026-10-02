//! obs `requestType` → Core Command/Query translation (OBSWS-001 request
//! slice; ADR-0020 §b/§c/§e; RES-007 §Requests).
//!
//! Per ADR-0020 §b every mutation pivots through the **native** wire type
//! ([`RequestKind`]) and the existing [`map::command_from_wire`] mapping, so
//! the adapter inherits the native drift guards, validation, and permission
//! checks (`AppHandle::dispatch_with_permissions` with the session's
//! authenticated permissions). Queries read the latest [`AppSnapshot`]
//! through the permission-checked `AppHandle::query` path (never the queue).
//!
//! ## Status-code mapping (obs `RequestStatus` groups, RES-007)
//!
//! | condition | code |
//! |---|---|
//! | success | 100 |
//! | unknown `requestType` | 204 (session layer, [`execute`] returns `None`) |
//! | missing `requestData` field | 300 |
//! | field of the wrong JSON type | 401 |
//! | numeric field out of range | 402 |
//! | invalid field value (bad enum string, generic `InvalidInput`) | 400 |
//! | output already running / core "cannot start" | 500 |
//! | output not running / core "cannot stop" / absent primary on status+stop | 501 |
//! | studio mode not active (preview/transition requests) | 506 |
//! | name/number/uuid resolution failure, unknown transition | 600 |
//! | duplicate target name (`CreateScene`, `SetSceneName`, `SetInputName`) | 601 |
//! | permission denied (`Unauthorized`) | 703 |
//! | core actor shut down | 207 |
//! | everything else (media/IO/persistence) | 701 |
//!
//! ## Documented divergences from upstream
//!
//! - `Sleep` is accepted as a standalone request (upstream registers it for
//!   batches only), which keeps [`AVAILABLE_REQUESTS`] truthful.
//! - `sourceWidth`/`sourceHeight` in scene-item transforms are `0` until the
//!   source reports capture dimensions (no capture runtime yet).
//! - Output runtime metrics (`outputBytes`, frame counters, duration,
//!   timecode, congestion) are `0`/"00:00:00.000": the OutputGraph does not
//!   track them yet (stats are OBSWS-002+).
//! - `SetInputVolume` with `inputVolumeMul: 0` maps to −100 dB, not −∞:
//!   the core requires finite gains.
//! - Scene/input/output names are deduplicated by the core on add/rename, so
//!   the 601 duplicate-name answers defend against state-restore edge cases.
//! - `outputKind`/`inputKind` strings are Prismcast's own identifiers (there
//!   is no OBS plugin registry behind them); `unversionedInputKind` mirrors
//!   `inputKind`.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use prismcast_app::dispatch::{Permissions, Query, QueryResponse};
use prismcast_app::snapshot::AppSnapshot;
use prismcast_app::{AppHandle, HandleError};
use prismcast_core::error::Error;
use prismcast_core::event::{Event, SceneEvent};
use prismcast_core::id::{OutputId, SourceId};
use prismcast_core::output::{Output, OutputKind, OutputState};
use prismcast_core::project::VideoConfig;
use prismcast_core::scene::{Anchor, Bounds, BoundsKind, Scene, SceneItem, Transform, Vec2};
use prismcast_core::source::SourceKind;
use prismcast_core::state::AppState;
use prismcast_core::transition::TransitionKind;
use prismcast_protocol::data;
use prismcast_protocol::request::RequestKind;

use crate::map;

use super::names::{self, ItemIdMap};
use super::proto::{self, RequestStatus};

/// `Sleep` requests may delay a serial batch by at most this many
/// milliseconds (upstream's `sleepMillis` cap).
const MAX_SLEEP_MILLIS: u64 = 50_000;

/// Every request type this adapter implements, advertised in `GetVersion`'s
/// `availableRequests` (ADR-0020 §d). Sorted; drift-guarded by
/// `tests/obs_ws_requests.rs` (every entry must dispatch to a non-204
/// answer through the real socket path).
pub(crate) const AVAILABLE_REQUESTS: &[&str] = &[
    "CreateScene",
    "CreateSceneItem",
    "GetCurrentPreviewScene",
    "GetCurrentProgramScene",
    "GetCurrentSceneTransition",
    "GetInputList",
    "GetInputMute",
    "GetInputVolume",
    "GetOutputList",
    "GetOutputStatus",
    "GetRecordStatus",
    "GetSceneItemId",
    "GetSceneItemList",
    "GetSceneItemTransform",
    "GetSceneList",
    "GetStreamStatus",
    "GetStudioModeEnabled",
    "GetVersion",
    "RemoveScene",
    "RemoveSceneItem",
    "SetCurrentPreviewScene",
    "SetCurrentProgramScene",
    "SetCurrentSceneTransition",
    "SetInputMute",
    "SetInputName",
    "SetInputVolume",
    "SetSceneItemEnabled",
    "SetSceneItemTransform",
    "SetSceneName",
    "SetStudioModeEnabled",
    "Sleep",
    "StartOutput",
    "StartRecord",
    "StartStream",
    "StopOutput",
    "StopRecord",
    "StopStream",
    "ToggleInputMute",
    "ToggleOutput",
    "ToggleRecord",
    "ToggleStream",
    "TriggerStudioModeTransition",
];

/// Everything a request translation needs: the core handle, the session's
/// authenticated permissions, and the server-wide scene-item ID registry.
pub(crate) struct RequestContext<'a> {
    /// The application core.
    pub app: &'a AppHandle,
    /// Permissions granted at `Identify` (mapped to app scopes).
    pub permissions: Permissions,
    /// Shared `sceneItemId` registry.
    pub item_ids: &'a ItemIdMap,
}

/// The translated outcome of one request: an obs [`RequestStatus`] plus an
/// optional `responseData` payload (omitted on failure).
pub(crate) struct RequestOutcome {
    /// The outcome status.
    pub status: RequestStatus,
    /// `responseData`; `None` on failure or for payload-less successes.
    pub data: Option<Value>,
}

/// Handler result: the optional payload on success, the status on failure.
type Handler = Result<Option<Value>, RequestStatus>;

/// Executes one obs request. Returns `None` for request types this adapter
/// does not implement — the session layer answers those with the typed 204
/// stub (`unknown_request_status`).
pub(crate) async fn execute(
    ctx: &RequestContext<'_>,
    request_type: &str,
    request_data: Option<&Value>,
) -> Option<RequestOutcome> {
    let result = match request_type {
        "GetVersion" => get_version(),
        "Sleep" => execute_sleep(request_data).await,
        // Scenes
        "GetSceneList" => get_scene_list(ctx),
        "GetCurrentProgramScene" => get_current_program_scene(ctx),
        "SetCurrentProgramScene" => set_current_program_scene(ctx, request_data).await,
        "GetCurrentPreviewScene" => get_current_preview_scene(ctx),
        "SetCurrentPreviewScene" => set_current_preview_scene(ctx, request_data).await,
        "CreateScene" => create_scene(ctx, request_data).await,
        "RemoveScene" => remove_scene(ctx, request_data).await,
        "SetSceneName" => set_scene_name(ctx, request_data).await,
        // Scene items
        "GetSceneItemList" => get_scene_item_list(ctx, request_data),
        "GetSceneItemId" => get_scene_item_id(ctx, request_data),
        "CreateSceneItem" => create_scene_item(ctx, request_data).await,
        "RemoveSceneItem" => remove_scene_item(ctx, request_data).await,
        "SetSceneItemEnabled" => set_scene_item_enabled(ctx, request_data).await,
        "GetSceneItemTransform" => get_scene_item_transform(ctx, request_data),
        "SetSceneItemTransform" => set_scene_item_transform(ctx, request_data).await,
        // Studio mode
        "GetStudioModeEnabled" => get_studio_mode_enabled(ctx),
        "SetStudioModeEnabled" => set_studio_mode_enabled(ctx, request_data).await,
        "TriggerStudioModeTransition" => trigger_studio_mode_transition(ctx).await,
        // Inputs
        "GetInputList" => get_input_list(ctx, request_data),
        "GetInputMute" => get_input_mute(ctx, request_data),
        "SetInputMute" => set_input_mute(ctx, request_data).await,
        "ToggleInputMute" => toggle_input_mute(ctx, request_data).await,
        "GetInputVolume" => get_input_volume(ctx, request_data),
        "SetInputVolume" => set_input_volume(ctx, request_data).await,
        "SetInputName" => set_input_name(ctx, request_data).await,
        // Transitions
        "GetCurrentSceneTransition" => get_current_scene_transition(ctx),
        "SetCurrentSceneTransition" => set_current_scene_transition(ctx, request_data).await,
        // Outputs by name
        "GetOutputList" => get_output_list(ctx),
        "GetOutputStatus" => get_output_status(ctx, request_data),
        "StartOutput" => start_output(ctx, request_data).await,
        "StopOutput" => stop_output(ctx, request_data).await,
        "ToggleOutput" => toggle_output(ctx, request_data).await,
        // Stream/record singletons (designated primaries, ADR-0020 §e)
        "GetStreamStatus" => singleton_status(ctx, Singleton::Stream),
        "StartStream" => singleton_start(ctx, Singleton::Stream).await,
        "StopStream" => singleton_stop(ctx, Singleton::Stream).await,
        "ToggleStream" => singleton_toggle(ctx, Singleton::Stream).await,
        "GetRecordStatus" => singleton_status(ctx, Singleton::Record),
        "StartRecord" => singleton_start(ctx, Singleton::Record).await,
        "StopRecord" => singleton_stop(ctx, Singleton::Record).await,
        "ToggleRecord" => singleton_toggle(ctx, Singleton::Record).await,
        _ => return None,
    };
    Some(match result {
        Ok(data) => RequestOutcome {
            status: RequestStatus::ok(),
            data,
        },
        Err(status) => RequestOutcome { status, data: None },
    })
}

// --- field extraction (300 missing, 401 wrong type, 402 out of range) ---

fn missing(field: &str) -> RequestStatus {
    RequestStatus::error(
        proto::status::MISSING_REQUEST_FIELD,
        format!("missing required request field `{field}`"),
    )
}

fn wrong_type(field: &str, expected: &str) -> RequestStatus {
    RequestStatus::error(
        proto::status::INVALID_REQUEST_FIELD_TYPE,
        format!("request field `{field}` must be {expected}"),
    )
}

fn invalid_value(field: &str, detail: impl std::fmt::Display) -> RequestStatus {
    RequestStatus::error(
        proto::status::INVALID_REQUEST_FIELD,
        format!("request field `{field}` is invalid: {detail}"),
    )
}

fn not_found(detail: impl std::fmt::Display) -> RequestStatus {
    RequestStatus::error(proto::status::RESOURCE_NOT_FOUND, detail.to_string())
}

fn req_str<'v>(data: Option<&'v Value>, field: &str) -> Result<&'v str, RequestStatus> {
    let Some(value) = data.and_then(|d| d.get(field)) else {
        return Err(missing(field));
    };
    value.as_str().ok_or_else(|| wrong_type(field, "a string"))
}

fn opt_str<'v>(data: Option<&'v Value>, field: &str) -> Result<Option<&'v str>, RequestStatus> {
    match data.and_then(|d| d.get(field)) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_str()
            .map(Some)
            .ok_or_else(|| wrong_type(field, "a string")),
    }
}

fn req_bool(data: Option<&Value>, field: &str) -> Result<bool, RequestStatus> {
    let Some(value) = data.and_then(|d| d.get(field)) else {
        return Err(missing(field));
    };
    value
        .as_bool()
        .ok_or_else(|| wrong_type(field, "a boolean"))
}

fn opt_bool(data: Option<&Value>, field: &str) -> Result<Option<bool>, RequestStatus> {
    match data.and_then(|d| d.get(field)) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_bool()
            .map(Some)
            .ok_or_else(|| wrong_type(field, "a boolean")),
    }
}

fn req_u64(data: Option<&Value>, field: &str) -> Result<u64, RequestStatus> {
    let Some(value) = data.and_then(|d| d.get(field)) else {
        return Err(missing(field));
    };
    numeric_u64(value, field)
}

fn opt_u64(data: Option<&Value>, field: &str) -> Result<Option<u64>, RequestStatus> {
    match data.and_then(|d| d.get(field)) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => numeric_u64(value, field).map(Some),
    }
}

fn numeric_u64(value: &Value, field: &str) -> Result<u64, RequestStatus> {
    if let Some(number) = value.as_u64() {
        return Ok(number);
    }
    if value.is_number() {
        return Err(RequestStatus::error(
            proto::status::REQUEST_FIELD_OUT_OF_RANGE,
            format!("request field `{field}` must be a non-negative integer"),
        ));
    }
    Err(wrong_type(field, "a number"))
}

fn opt_f64(data: Option<&Value>, field: &str) -> Result<Option<f64>, RequestStatus> {
    match data.and_then(|d| d.get(field)) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_f64()
            .map(Some)
            .ok_or_else(|| wrong_type(field, "a number")),
    }
}

/// The `sceneItemTransform` object of a `SetSceneItemTransform` request.
fn req_object<'v>(data: Option<&'v Value>, field: &str) -> Result<&'v Value, RequestStatus> {
    let Some(value) = data.and_then(|d| d.get(field)) else {
        return Err(missing(field));
    };
    if value.is_object() {
        Ok(value)
    } else {
        Err(wrong_type(field, "an object"))
    }
}

// --- core access helpers ---

/// The latest snapshot through the permission-checked query path.
fn snapshot(ctx: &RequestContext<'_>) -> Result<Arc<AppSnapshot>, RequestStatus> {
    match ctx.app.query(&Query::GetSnapshot, ctx.permissions) {
        Ok(QueryResponse::Snapshot(snapshot)) => Ok(snapshot),
        Ok(other) => Err(RequestStatus::error(
            proto::status::REQUEST_PROCESSING_FAILED,
            format!("unexpected query response variant: {other:?}"),
        )),
        Err(error) => Err(status_for_core_error(&error)),
    }
}

/// Maps a core error onto an obs status (see the module's mapping table).
fn status_for_core_error(error: &Error) -> RequestStatus {
    match error {
        Error::NotFound(message) => {
            RequestStatus::error(proto::status::RESOURCE_NOT_FOUND, message.clone())
        }
        Error::Unauthorized(message) => {
            RequestStatus::error(proto::status::CANNOT_ACT, message.clone())
        }
        Error::InvalidInput(message) => {
            let code = if message.starts_with("cannot start output") {
                proto::status::OUTPUT_RUNNING
            } else if message.starts_with("cannot stop output") {
                proto::status::OUTPUT_NOT_RUNNING
            } else if message == "studio mode is disabled" {
                proto::status::STUDIO_MODE_NOT_ACTIVE
            } else {
                proto::status::INVALID_REQUEST_FIELD
            };
            RequestStatus::error(code, message.clone())
        }
        Error::Protocol(message) => {
            RequestStatus::error(proto::status::INVALID_REQUEST_FIELD, message.clone())
        }
        Error::Media(message) | Error::Io(message) | Error::Persistence(message) => {
            RequestStatus::error(proto::status::RESOURCE_ACTION_FAILED, message.clone())
        }
    }
}

/// Pivots a native request kind through `map::command_from_wire` and
/// dispatches it with the session's permissions; returns committed events.
async fn dispatch(
    ctx: &RequestContext<'_>,
    kind: RequestKind,
) -> Result<Vec<Event>, RequestStatus> {
    let command = map::command_from_wire(kind).map_err(|wire| {
        RequestStatus::error(
            proto::status::REQUEST_PROCESSING_FAILED,
            format!("internal mapping failure: {}", wire.message),
        )
    })?;
    match ctx
        .app
        .dispatch_with_permissions(command, ctx.permissions)
        .await
    {
        Ok(response) => Ok(response.events),
        Err(HandleError::Shutdown) => Err(RequestStatus::error(
            proto::status::NOT_READY,
            "core actor is shut down",
        )),
        Err(HandleError::Core(error)) => Err(status_for_core_error(&error)),
    }
}

/// Resolves `sceneName` from the request data against the snapshot.
fn resolve_scene<'a>(
    state: &'a AppState,
    data: Option<&Value>,
) -> Result<&'a Scene, RequestStatus> {
    let name = req_str(data, "sceneName")?;
    names::scene_by_name(state, name).ok_or_else(|| not_found(format!("scene '{name}'")))
}

/// Resolves `inputName` against the snapshot.
fn resolve_source(state: &AppState, data: Option<&Value>) -> Result<SourceId, RequestStatus> {
    let name = req_str(data, "inputName")?;
    names::source_by_name(state, name)
        .map(|source| source.id)
        .ok_or_else(|| not_found(format!("input '{name}'")))
}

/// Resolves `outputName` against the snapshot.
fn resolve_output(state: &AppState, data: Option<&Value>) -> Result<OutputId, RequestStatus> {
    let name = req_str(data, "outputName")?;
    names::output_by_name(state, name)
        .map(|output| output.id)
        .ok_or_else(|| not_found(format!("output '{name}'")))
}

/// Resolves `sceneName` + numeric `sceneItemId` to a live scene item:
/// the number must be registered in the [`ItemIdMap`] and the item must
/// still exist in the scene.
fn resolve_item<'a>(
    ctx: &RequestContext<'_>,
    scene: &'a Scene,
    data: Option<&Value>,
) -> Result<&'a SceneItem, RequestStatus> {
    let number = req_u64(data, "sceneItemId")?;
    let item_id = ctx.item_ids.resolve(scene.id, number).ok_or_else(|| {
        not_found(format!(
            "sceneItemId {number} is not known in scene '{}'",
            scene.name
        ))
    })?;
    scene.item(item_id).ok_or_else(|| {
        not_found(format!(
            "sceneItemId {number} no longer exists in scene '{}'",
            scene.name
        ))
    })
}

// --- GetVersion ---

/// OBS Studio version advertised to clients: the minimum obws (>= 30.2)
/// accepts. Compatibility constant, not the Prismcast version (ADR-0021).
pub(crate) const OBS_VERSION_COMPAT: &str = "30.2.0";

fn get_version() -> Handler {
    Ok(Some(json!({
        "obsVersion": OBS_VERSION_COMPAT,
        "obsWebSocketVersion": proto::OBS_WEBSOCKET_VERSION,
        "rpcVersion": proto::RPC_VERSION,
        "availableRequests": AVAILABLE_REQUESTS,
        // Screenshots are deferred (OBSWS-002+); no formats are served.
        "supportedImageFormats": [],
        "platform": "linux",
        "platformDescription": "Linux (Prismcast obs-websocket adapter)",
    })))
}

// --- Sleep (batch-oriented; also accepted standalone, see module docs) ---

async fn execute_sleep(data: Option<&Value>) -> Handler {
    let sleep_millis = opt_u64(data, "sleepMillis")?;
    let sleep_frames = opt_u64(data, "sleepFrames")?;
    match (sleep_millis, sleep_frames) {
        (Some(ms), _) if ms > MAX_SLEEP_MILLIS => Err(RequestStatus::error(
            proto::status::REQUEST_FIELD_OUT_OF_RANGE,
            format!("sleepMillis {ms} exceeds the maximum of {MAX_SLEEP_MILLIS}"),
        )),
        (Some(ms), _) => {
            tokio::time::sleep(Duration::from_millis(ms)).await;
            Ok(None)
        }
        (None, Some(_)) => Err(RequestStatus::error(
            proto::status::INVALID_REQUEST_FIELD,
            "sleepFrames requires SerialFrame execution, which this server does not support",
        )),
        (None, None) => Err(missing("sleepMillis")),
    }
}

// --- scenes ---

fn scene_entry(index: usize, scene: &Scene) -> Value {
    json!({
        "sceneName": scene.name,
        "sceneUuid": scene.id.as_uuid().to_string(),
        "sceneIndex": index,
    })
}

fn get_scene_list(ctx: &RequestContext<'_>) -> Handler {
    let snapshot = snapshot(ctx)?;
    let state = snapshot.state();
    let current = state.current_scene.and_then(|id| state.scene(id));
    let preview = state
        .studio_mode
        .as_ref()
        .and_then(|studio| state.scene(studio.preview));
    let mut data = json!({
        "currentProgramSceneName": current.map(|scene| scene.name.as_str()),
        "currentProgramSceneUuid": current.map(|scene| scene.id.as_uuid().to_string()),
        "scenes": state.scenes.values().enumerate().map(|(i, scene)| scene_entry(i, scene)).collect::<Vec<_>>(),
    });
    if let Some(preview) = preview {
        data["currentPreviewSceneName"] = json!(preview.name);
        data["currentPreviewSceneUuid"] = json!(preview.id.as_uuid().to_string());
    }
    Ok(Some(data))
}

fn get_current_program_scene(ctx: &RequestContext<'_>) -> Handler {
    let snapshot = snapshot(ctx)?;
    let state = snapshot.state();
    let scene = state
        .current_scene
        .and_then(|id| state.scene(id))
        .ok_or_else(|| not_found("no current program scene"))?;
    Ok(Some(json!({
        "currentProgramSceneName": scene.name,
        "currentProgramSceneUuid": scene.id.as_uuid().to_string(),
    })))
}

async fn set_current_program_scene(ctx: &RequestContext<'_>, data: Option<&Value>) -> Handler {
    let snapshot = snapshot(ctx)?;
    let scene = resolve_scene(snapshot.state(), data)?;
    dispatch(
        ctx,
        RequestKind::SetCurrentScene {
            scene_id: *scene.id.as_uuid(),
        },
    )
    .await?;
    Ok(None)
}

fn get_current_preview_scene(ctx: &RequestContext<'_>) -> Handler {
    let snapshot = snapshot(ctx)?;
    let state = snapshot.state();
    let studio = state.studio_mode.as_ref().ok_or_else(|| {
        RequestStatus::error(
            proto::status::STUDIO_MODE_NOT_ACTIVE,
            "studio mode is not active",
        )
    })?;
    let scene = state
        .scene(studio.preview)
        .ok_or_else(|| not_found("no current preview scene"))?;
    Ok(Some(json!({
        "currentPreviewSceneName": scene.name,
        "currentPreviewSceneUuid": scene.id.as_uuid().to_string(),
    })))
}

async fn set_current_preview_scene(ctx: &RequestContext<'_>, data: Option<&Value>) -> Handler {
    let snapshot = snapshot(ctx)?;
    let state = snapshot.state();
    if state.studio_mode.is_none() {
        return Err(RequestStatus::error(
            proto::status::STUDIO_MODE_NOT_ACTIVE,
            "studio mode is not active",
        ));
    }
    let scene = resolve_scene(state, data)?;
    dispatch(
        ctx,
        RequestKind::SetPreviewScene {
            scene_id: *scene.id.as_uuid(),
        },
    )
    .await?;
    Ok(None)
}

async fn create_scene(ctx: &RequestContext<'_>, data: Option<&Value>) -> Handler {
    let snapshot = snapshot(ctx)?;
    let name = req_str(data, "sceneName")?.to_string();
    if names::scene_by_name(snapshot.state(), &name).is_some() {
        return Err(RequestStatus::error(
            proto::status::RESOURCE_ALREADY_EXISTS,
            format!("scene '{name}' already exists"),
        ));
    }
    let events = dispatch(ctx, RequestKind::AddScene { name }).await?;
    let scene_id = events.iter().find_map(|event| match event {
        Event::Scene(SceneEvent::Added { scene_id, .. }) => Some(*scene_id),
        _ => None,
    });
    Ok(Some(json!({
        "sceneUuid": scene_id.map(|id| id.as_uuid().to_string()),
    })))
}

async fn remove_scene(ctx: &RequestContext<'_>, data: Option<&Value>) -> Handler {
    let snapshot = snapshot(ctx)?;
    let scene = resolve_scene(snapshot.state(), data)?;
    let scene_id = scene.id;
    dispatch(
        ctx,
        RequestKind::RemoveScene {
            scene_id: *scene_id.as_uuid(),
        },
    )
    .await?;
    // Eager eviction; the fan-out listener covers removals by other clients.
    ctx.item_ids.evict_scene(scene_id);
    Ok(None)
}

async fn set_scene_name(ctx: &RequestContext<'_>, data: Option<&Value>) -> Handler {
    let snapshot = snapshot(ctx)?;
    let state = snapshot.state();
    let scene = resolve_scene(state, data)?;
    let new_name = req_str(data, "newSceneName")?.to_string();
    if state
        .scenes
        .values()
        .any(|other| other.id != scene.id && other.name == new_name)
    {
        return Err(RequestStatus::error(
            proto::status::RESOURCE_ALREADY_EXISTS,
            format!("scene '{new_name}' already exists"),
        ));
    }
    dispatch(
        ctx,
        RequestKind::RenameScene {
            scene_id: *scene.id.as_uuid(),
            name: new_name,
        },
    )
    .await?;
    Ok(None)
}

// --- scene items ---

fn get_scene_item_list(ctx: &RequestContext<'_>, data: Option<&Value>) -> Handler {
    let snapshot = snapshot(ctx)?;
    let scene = resolve_scene(snapshot.state(), data)?.clone();
    // Mint in list order so first enumeration numbers bottom-to-top.
    ctx.item_ids
        .mint_all(scene.id, scene.items.iter().map(|item| item.id));
    let items = scene
        .items
        .iter()
        .enumerate()
        .map(|(index, item)| scene_item_json(&snapshot, ctx.item_ids, &scene, item, index))
        .collect::<Vec<_>>();
    Ok(Some(json!({ "sceneItems": items })))
}

fn get_scene_item_id(ctx: &RequestContext<'_>, data: Option<&Value>) -> Handler {
    let snapshot = snapshot(ctx)?;
    let state = snapshot.state();
    let scene = resolve_scene(state, data)?;
    let source_name = req_str(data, "sourceName")?;
    let offset = opt_u64(data, "searchOffset")?.unwrap_or(0) as usize;
    let item = scene
        .items
        .iter()
        .filter(|item| {
            state
                .source(item.source_id)
                .is_some_and(|source| source.name == source_name)
        })
        .nth(offset)
        .ok_or_else(|| {
            not_found(format!(
                "no scene item of source '{source_name}' at searchOffset {offset} in scene '{}'",
                scene.name
            ))
        })?;
    let number = ctx.item_ids.mint(scene.id, item.id);
    Ok(Some(json!({ "sceneItemId": number })))
}

async fn create_scene_item(ctx: &RequestContext<'_>, data: Option<&Value>) -> Handler {
    let snapshot = snapshot(ctx)?;
    let state = snapshot.state();
    let scene = resolve_scene(state, data)?;
    let scene_id = scene.id;
    let source_name = req_str(data, "sourceName")?;
    let source_id = names::source_by_name(state, source_name)
        .map(|source| source.id)
        .ok_or_else(|| not_found(format!("source '{source_name}'")))?;
    let enabled = opt_bool(data, "sceneItemEnabled")?;
    let events = dispatch(
        ctx,
        RequestKind::AddSceneItem {
            scene_id: *scene_id.as_uuid(),
            source_id: *source_id.as_uuid(),
        },
    )
    .await?;
    let item_id = events.iter().find_map(|event| match event {
        Event::Scene(SceneEvent::ItemAdded { item, .. }) => Some(item.id),
        _ => None,
    });
    let Some(item_id) = item_id else {
        return Err(RequestStatus::error(
            proto::status::RESOURCE_CREATION_FAILED,
            "scene item creation committed no ItemAdded event",
        ));
    };
    if enabled == Some(false) {
        dispatch(
            ctx,
            RequestKind::SetSceneItemVisible {
                scene_id: *scene_id.as_uuid(),
                item_id: *item_id.as_uuid(),
                visible: false,
            },
        )
        .await?;
    }
    let number = ctx.item_ids.mint(scene_id, item_id);
    Ok(Some(json!({ "sceneItemId": number })))
}

async fn remove_scene_item(ctx: &RequestContext<'_>, data: Option<&Value>) -> Handler {
    let snapshot = snapshot(ctx)?;
    let scene = resolve_scene(snapshot.state(), data)?.clone();
    let item = resolve_item(ctx, &scene, data)?;
    let item_id = item.id;
    dispatch(
        ctx,
        RequestKind::RemoveSceneItem {
            scene_id: *scene.id.as_uuid(),
            item_id: *item_id.as_uuid(),
        },
    )
    .await?;
    // Eager eviction; the fan-out listener covers removals by other clients.
    ctx.item_ids.evict_item(scene.id, item_id);
    Ok(None)
}

async fn set_scene_item_enabled(ctx: &RequestContext<'_>, data: Option<&Value>) -> Handler {
    let snapshot = snapshot(ctx)?;
    let scene = resolve_scene(snapshot.state(), data)?.clone();
    let item = resolve_item(ctx, &scene, data)?;
    let enabled = req_bool(data, "sceneItemEnabled")?;
    dispatch(
        ctx,
        RequestKind::SetSceneItemVisible {
            scene_id: *scene.id.as_uuid(),
            item_id: *item.id.as_uuid(),
            visible: enabled,
        },
    )
    .await?;
    Ok(None)
}

fn get_scene_item_transform(ctx: &RequestContext<'_>, data: Option<&Value>) -> Handler {
    let snapshot = snapshot(ctx)?;
    let scene = resolve_scene(snapshot.state(), data)?.clone();
    let item = resolve_item(ctx, &scene, data)?;
    Ok(Some(json!({
        "sceneItemTransform": transform_json(&snapshot, item),
    })))
}

async fn set_scene_item_transform(ctx: &RequestContext<'_>, data: Option<&Value>) -> Handler {
    let snapshot = snapshot(ctx)?;
    let scene = resolve_scene(snapshot.state(), data)?.clone();
    let item = resolve_item(ctx, &scene, data)?;
    let patch = req_object(data, "sceneItemTransform")?;

    let mut transform = item.transform;
    let mut crop = item.crop;
    let mut bounds = item.bounds;
    let mut transform_touched = false;
    let mut crop_touched = false;
    let mut bounds_touched = false;

    let set_f32 =
        |field: &str, target: &mut f32, touched: &mut bool| -> Result<(), RequestStatus> {
            if let Some(value) = opt_f64(Some(patch), field)? {
                *target = value as f32;
                *touched = true;
            }
            Ok(())
        };
    let mut set_crop = |field: &str, target: &mut u32| -> Result<(), RequestStatus> {
        if let Some(value) = opt_u64(Some(patch), field)? {
            *target = u32::try_from(value).map_err(|_| {
                RequestStatus::error(
                    proto::status::REQUEST_FIELD_OUT_OF_RANGE,
                    format!("request field `{field}` exceeds the u32 range"),
                )
            })?;
            crop_touched = true;
        }
        Ok(())
    };

    set_f32(
        "positionX",
        &mut transform.position.x,
        &mut transform_touched,
    )?;
    set_f32(
        "positionY",
        &mut transform.position.y,
        &mut transform_touched,
    )?;
    set_f32("rotation", &mut transform.rotation, &mut transform_touched)?;
    set_f32("scaleX", &mut transform.scale.x, &mut transform_touched)?;
    set_f32("scaleY", &mut transform.scale.y, &mut transform_touched)?;
    if let Some(value) = opt_u64(Some(patch), "alignment")? {
        transform.anchor = anchor_from_obs(value)?;
        transform_touched = true;
    }
    set_crop("cropLeft", &mut crop.left)?;
    set_crop("cropTop", &mut crop.top)?;
    set_crop("cropRight", &mut crop.right)?;
    set_crop("cropBottom", &mut crop.bottom)?;
    if let Some(bounds_type) = opt_str(Some(patch), "boundsType")? {
        bounds.kind = bounds_kind_from_obs(bounds_type)?;
        bounds_touched = true;
    }
    if let Some(value) = opt_u64(Some(patch), "boundsAlignment")? {
        bounds.alignment = anchor_from_obs(value)?;
        bounds_touched = true;
    }
    set_f32("boundsWidth", &mut bounds.size.x, &mut bounds_touched)?;
    set_f32("boundsHeight", &mut bounds.size.y, &mut bounds_touched)?;

    let mut commands = Vec::new();
    if transform_touched {
        commands.push(RequestKind::SetSceneItemTransform {
            scene_id: *scene.id.as_uuid(),
            item_id: *item.id.as_uuid(),
            transform: transform_to_data(transform),
        });
    }
    if crop_touched {
        commands.push(RequestKind::SetSceneItemCrop {
            scene_id: *scene.id.as_uuid(),
            item_id: *item.id.as_uuid(),
            crop: data::Crop {
                left: crop.left,
                top: crop.top,
                right: crop.right,
                bottom: crop.bottom,
            },
        });
    }
    if bounds_touched {
        commands.push(RequestKind::SetSceneItemBounds {
            scene_id: *scene.id.as_uuid(),
            item_id: *item.id.as_uuid(),
            bounds: bounds_to_data(bounds),
        });
    }
    // An empty patch is a successful no-op, like upstream.
    if !commands.is_empty() {
        dispatch(ctx, RequestKind::Transaction { commands }).await?;
    }
    Ok(None)
}

// --- studio mode ---

fn get_studio_mode_enabled(ctx: &RequestContext<'_>) -> Handler {
    let snapshot = snapshot(ctx)?;
    Ok(Some(json!({
        "studioModeEnabled": snapshot.state().studio_mode.is_some(),
    })))
}

async fn set_studio_mode_enabled(ctx: &RequestContext<'_>, data: Option<&Value>) -> Handler {
    let enabled = req_bool(data, "studioModeEnabled")?;
    dispatch(ctx, RequestKind::SetStudioModeEnabled { enabled }).await?;
    Ok(None)
}

async fn trigger_studio_mode_transition(ctx: &RequestContext<'_>) -> Handler {
    let snapshot = snapshot(ctx)?;
    if snapshot.state().studio_mode.is_none() {
        return Err(RequestStatus::error(
            proto::status::STUDIO_MODE_NOT_ACTIVE,
            "studio mode is not active",
        ));
    }
    dispatch(ctx, RequestKind::TransitionToProgram).await?;
    Ok(None)
}

// --- inputs ---

/// The adapter's `inputKind` identifier for a source kind (there is no OBS
/// plugin registry behind these; `unversionedInputKind` mirrors them).
fn input_kind_str(kind: SourceKind) -> &'static str {
    match kind {
        SourceKind::PipeWireDisplay => "pipewire_display_capture",
        SourceKind::PipeWireWindow => "pipewire_window_capture",
        SourceKind::V4l2Camera => "v4l2_input",
        SourceKind::PipeWireAudioInput => "pipewire_audio_input_capture",
        SourceKind::PipeWireAppAudio => "pipewire_app_audio_capture",
        SourceKind::MediaFile => "ffmpeg_source",
        SourceKind::Image => "image_source",
        SourceKind::ImageSlideshow => "slideshow",
        SourceKind::Color => "color_source",
        SourceKind::Text => "text_source",
        SourceKind::Browser => "browser_source",
        SourceKind::Scene(_) => "scene",
        SourceKind::TestPattern => "test_pattern",
        SourceKind::NetworkStream => "network_stream",
    }
}

fn get_input_list(ctx: &RequestContext<'_>, data: Option<&Value>) -> Handler {
    let snapshot = snapshot(ctx)?;
    let filter = opt_str(data, "inputKind")?;
    let inputs = snapshot
        .state()
        .sources
        .values()
        // Scenes-as-sources are scenes, not inputs (upstream parity).
        .filter(|source| !matches!(source.kind, SourceKind::Scene(_)))
        .filter(|source| filter.is_none_or(|kind| input_kind_str(source.kind) == kind))
        .map(|source| {
            json!({
                "inputName": source.name,
                "inputUuid": source.id.as_uuid().to_string(),
                "inputKind": input_kind_str(source.kind),
                "unversionedInputKind": input_kind_str(source.kind),
            })
        })
        .collect::<Vec<_>>();
    Ok(Some(json!({ "inputs": inputs })))
}

fn get_input_mute(ctx: &RequestContext<'_>, data: Option<&Value>) -> Handler {
    let snapshot = snapshot(ctx)?;
    let source_id = resolve_source(snapshot.state(), data)?;
    let muted = snapshot.state().audio.mixer_state(source_id).muted;
    Ok(Some(json!({ "inputMuted": muted })))
}

async fn set_input_mute(ctx: &RequestContext<'_>, data: Option<&Value>) -> Handler {
    let snapshot = snapshot(ctx)?;
    let source_id = resolve_source(snapshot.state(), data)?;
    let muted = req_bool(data, "inputMuted")?;
    dispatch(
        ctx,
        RequestKind::SetSourceMuted {
            source_id: *source_id.as_uuid(),
            muted,
        },
    )
    .await?;
    Ok(None)
}

async fn toggle_input_mute(ctx: &RequestContext<'_>, data: Option<&Value>) -> Handler {
    let snapshot = snapshot(ctx)?;
    let source_id = resolve_source(snapshot.state(), data)?;
    let muted = !snapshot.state().audio.mixer_state(source_id).muted;
    dispatch(
        ctx,
        RequestKind::SetSourceMuted {
            source_id: *source_id.as_uuid(),
            muted,
        },
    )
    .await?;
    Ok(Some(json!({ "inputMuted": muted })))
}

/// obs dB → multiplier (`0` below the −100 dB floor; the core requires
/// finite gains, so −∞ is not representable).
fn db_to_mul(db: f32) -> f64 {
    if db <= -100.0 {
        0.0
    } else {
        10_f64.powf(f64::from(db) / 20.0)
    }
}

/// obs multiplier → dB (`0` maps to the −100 dB floor, not −∞).
fn mul_to_db(mul: f64) -> f32 {
    if mul <= 0.0 {
        -100.0
    } else {
        (20.0 * mul.log10()) as f32
    }
}

fn get_input_volume(ctx: &RequestContext<'_>, data: Option<&Value>) -> Handler {
    let snapshot = snapshot(ctx)?;
    let source_id = resolve_source(snapshot.state(), data)?;
    let volume_db = snapshot.state().audio.mixer_state(source_id).volume_db;
    Ok(Some(json!({
        "inputVolumeMul": db_to_mul(volume_db),
        "inputVolumeDb": volume_db,
    })))
}

async fn set_input_volume(ctx: &RequestContext<'_>, data: Option<&Value>) -> Handler {
    let snapshot = snapshot(ctx)?;
    let source_id = resolve_source(snapshot.state(), data)?;
    let volume_mul = opt_f64(data, "inputVolumeMul")?;
    let volume_db = opt_f64(data, "inputVolumeDb")?;
    // Upstream prefers inputVolumeMul when both are provided.
    let volume_db = match (volume_mul, volume_db) {
        (Some(mul), _) => mul_to_db(mul),
        (None, Some(db)) => db as f32,
        (None, None) => return Err(missing("inputVolumeMul or inputVolumeDb")),
    };
    dispatch(
        ctx,
        RequestKind::SetSourceVolume {
            source_id: *source_id.as_uuid(),
            volume_db,
        },
    )
    .await?;
    Ok(None)
}

async fn set_input_name(ctx: &RequestContext<'_>, data: Option<&Value>) -> Handler {
    let snapshot = snapshot(ctx)?;
    let state = snapshot.state();
    let source_id = resolve_source(state, data)?;
    let new_name = req_str(data, "newInputName")?.to_string();
    if state
        .sources
        .values()
        .any(|other| other.id != source_id && other.name == new_name)
    {
        return Err(RequestStatus::error(
            proto::status::RESOURCE_ALREADY_EXISTS,
            format!("input '{new_name}' already exists"),
        ));
    }
    dispatch(
        ctx,
        RequestKind::RenameSource {
            source_id: *source_id.as_uuid(),
            name: new_name,
        },
    )
    .await?;
    Ok(None)
}

// --- transitions ---

/// The obs `transitionKind` identifier for a core transition kind.
fn transition_kind_str(kind: TransitionKind) -> &'static str {
    match kind {
        TransitionKind::Cut => "cut_transition",
        TransitionKind::Fade => "fade_transition",
        TransitionKind::Swipe => "swipe_transition",
        TransitionKind::Slide => "slide_transition",
        TransitionKind::Stinger => "stinger_transition",
    }
}

/// The display name a client uses in `SetCurrentSceneTransition`.
fn transition_display_name(kind: TransitionKind) -> &'static str {
    match kind {
        TransitionKind::Cut => "Cut",
        TransitionKind::Fade => "Fade",
        TransitionKind::Swipe => "Swipe",
        TransitionKind::Slide => "Slide",
        TransitionKind::Stinger => "Stinger",
    }
}

/// Resolves a transition by display name or obs kind identifier.
fn transition_kind_by_name(name: &str) -> Option<TransitionKind> {
    [
        TransitionKind::Cut,
        TransitionKind::Fade,
        TransitionKind::Swipe,
        TransitionKind::Slide,
        TransitionKind::Stinger,
    ]
    .into_iter()
    .find(|kind| transition_display_name(*kind) == name || transition_kind_str(*kind) == name)
}

fn transition_json(transition: &prismcast_core::transition::Transition) -> Value {
    json!({
        "transitionName": transition_display_name(transition.kind),
        "transitionKind": transition_kind_str(transition.kind),
        "transitionFixed": false,
        "transitionDuration": transition.duration_ms,
        "transitionConfigurable": !transition.settings.is_null(),
        "transitionSettings": transition.settings,
    })
}

fn get_current_scene_transition(ctx: &RequestContext<'_>) -> Handler {
    let snapshot = snapshot(ctx)?;
    Ok(Some(transition_json(&snapshot.state().transition)))
}

async fn set_current_scene_transition(ctx: &RequestContext<'_>, data: Option<&Value>) -> Handler {
    let snapshot = snapshot(ctx)?;
    let name = req_str(data, "transitionName")?;
    let kind =
        transition_kind_by_name(name).ok_or_else(|| not_found(format!("transition '{name}'")))?;
    // Only the kind is switchable; duration and settings are preserved.
    let current = &snapshot.state().transition;
    dispatch(
        ctx,
        RequestKind::SetTransition {
            transition: data::Transition {
                kind: match kind {
                    TransitionKind::Cut => data::TransitionKind::Cut,
                    TransitionKind::Fade => data::TransitionKind::Fade,
                    TransitionKind::Swipe => data::TransitionKind::Swipe,
                    TransitionKind::Slide => data::TransitionKind::Slide,
                    TransitionKind::Stinger => data::TransitionKind::Stinger,
                },
                duration_ms: current.duration_ms,
                settings: current.settings.clone(),
            },
        },
    )
    .await?;
    Ok(None)
}

// --- outputs ---

/// The adapter's `outputKind` identifier (Prismcast's own strings; there is
/// no OBS output registry behind them).
fn output_kind_str(kind: OutputKind) -> &'static str {
    match kind {
        OutputKind::Recording => "recording_output",
        OutputKind::Rtmp => "rtmp_output",
        OutputKind::Srt => "srt_output",
        OutputKind::Whip => "whip_output",
        OutputKind::VirtualCamera => "virtualcam_output",
    }
}

/// obs's "output is active" notion: producing or trying to produce.
fn output_is_active(state: OutputState) -> bool {
    matches!(
        state,
        OutputState::Starting
            | OutputState::Running
            | OutputState::Degraded
            | OutputState::Reconnecting { .. }
    )
}

/// The active profile's video config (the output canvas), defaulting to
/// 1080p60 when no profile is active.
fn video_config(state: &AppState) -> VideoConfig {
    state
        .active_profile
        .and_then(|id| state.profiles.get(&id))
        .map(|profile| profile.video)
        .unwrap_or_default()
}

fn output_json(output: &Output, video: VideoConfig) -> Value {
    json!({
        "outputName": output.name,
        "outputUuid": output.id.as_uuid().to_string(),
        "outputKind": output_kind_str(output.kind),
        "outputWidth": video.width,
        "outputHeight": video.height,
        "outputFlags": {
            "OBS_OUTPUT_VIDEO": true,
            "OBS_OUTPUT_AUDIO": true,
            "OBS_OUTPUT_ENCODED": true,
            "OBS_OUTPUT_MULTI_TRACK": false,
            "OBS_OUTPUT_SERVICE": matches!(output.kind, OutputKind::Rtmp | OutputKind::Srt | OutputKind::Whip),
        },
        "outputActive": output_is_active(output.state),
        "outputReconnecting": matches!(output.state, OutputState::Reconnecting { .. }),
        "outputCongestion": 0.0,
    })
}

/// Runtime metrics the OutputGraph does not track yet read as zero
/// (documented divergence; stats are OBSWS-002+).
fn output_status_json(output: &Output) -> Value {
    json!({
        "outputActive": output_is_active(output.state),
        "outputReconnecting": matches!(output.state, OutputState::Reconnecting { .. }),
        "outputTimecode": "00:00:00.000",
        "outputDuration": 0,
        "outputCongestion": 0.0,
        "outputBytes": 0,
        "outputSkippedFrames": 0,
        "outputTotalFrames": 0,
    })
}

fn get_output_list(ctx: &RequestContext<'_>) -> Handler {
    let snapshot = snapshot(ctx)?;
    let state = snapshot.state();
    let video = video_config(state);
    let outputs = state
        .outputs
        .values()
        .map(|output| output_json(output, video))
        .collect::<Vec<_>>();
    Ok(Some(json!({ "outputs": outputs })))
}

fn get_output_status(ctx: &RequestContext<'_>, data: Option<&Value>) -> Handler {
    let snapshot = snapshot(ctx)?;
    let output_id = resolve_output(snapshot.state(), data)?;
    let output = snapshot
        .output(output_id)
        .ok_or_else(|| not_found(format!("output {output_id}")))?;
    Ok(Some(output_status_json(output)))
}

/// Starts `output_id`, pre-checking the state so an active output gets 500
/// even before the core's own conflict error.
async fn start_output_by_id(
    ctx: &RequestContext<'_>,
    state: &AppState,
    output_id: OutputId,
) -> Handler {
    let output = state
        .output(output_id)
        .ok_or_else(|| not_found(format!("output {output_id}")))?;
    if output_is_active(output.state) {
        return Err(RequestStatus::error(
            proto::status::OUTPUT_RUNNING,
            format!("output '{}' is already running", output.name),
        ));
    }
    dispatch(
        ctx,
        RequestKind::StartOutput {
            output_id: *output_id.as_uuid(),
        },
    )
    .await?;
    Ok(None)
}

/// Stops `output_id`, pre-checking the state so an inactive output gets 501.
async fn stop_output_by_id(
    ctx: &RequestContext<'_>,
    state: &AppState,
    output_id: OutputId,
) -> Handler {
    let output = state
        .output(output_id)
        .ok_or_else(|| not_found(format!("output {output_id}")))?;
    if !output_is_active(output.state) {
        return Err(RequestStatus::error(
            proto::status::OUTPUT_NOT_RUNNING,
            format!("output '{}' is not running", output.name),
        ));
    }
    dispatch(
        ctx,
        RequestKind::StopOutput {
            output_id: *output_id.as_uuid(),
        },
    )
    .await?;
    Ok(None)
}

/// Toggles `output_id`; returns the new active flag.
async fn toggle_output_by_id(
    ctx: &RequestContext<'_>,
    state: &AppState,
    output_id: OutputId,
) -> Result<bool, RequestStatus> {
    let output = state
        .output(output_id)
        .ok_or_else(|| not_found(format!("output {output_id}")))?;
    let active = output_is_active(output.state);
    if active {
        stop_output_by_id(ctx, state, output_id).await?;
    } else {
        start_output_by_id(ctx, state, output_id).await?;
    }
    Ok(!active)
}

async fn start_output(ctx: &RequestContext<'_>, data: Option<&Value>) -> Handler {
    let snapshot = snapshot(ctx)?;
    let output_id = resolve_output(snapshot.state(), data)?;
    start_output_by_id(ctx, snapshot.state(), output_id).await
}

async fn stop_output(ctx: &RequestContext<'_>, data: Option<&Value>) -> Handler {
    let snapshot = snapshot(ctx)?;
    let output_id = resolve_output(snapshot.state(), data)?;
    stop_output_by_id(ctx, snapshot.state(), output_id).await
}

async fn toggle_output(ctx: &RequestContext<'_>, data: Option<&Value>) -> Handler {
    let snapshot = snapshot(ctx)?;
    let output_id = resolve_output(snapshot.state(), data)?;
    let active = toggle_output_by_id(ctx, snapshot.state(), output_id).await?;
    Ok(Some(json!({ "outputActive": active })))
}

// --- stream/record singletons (designated primaries, ADR-0020 §e) ---

/// Which designated primary a singleton request addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Singleton {
    /// First `Rtmp` output, falling back to `Srt`, then `Whip`.
    Stream,
    /// First `Recording` output.
    Record,
}

impl Singleton {
    fn label(self) -> &'static str {
        match self {
            Self::Stream => "stream",
            Self::Record => "record",
        }
    }

    fn primary(self, state: &AppState) -> Option<&Output> {
        match self {
            Self::Stream => names::primary_stream_output(state),
            Self::Record => names::primary_record_output(state),
        }
    }
}

fn singleton_status(ctx: &RequestContext<'_>, singleton: Singleton) -> Handler {
    let snapshot = snapshot(ctx)?;
    // Absent primary reads as "not running" (501), per the task contract.
    let output = singleton.primary(snapshot.state()).ok_or_else(|| {
        RequestStatus::error(
            proto::status::OUTPUT_NOT_RUNNING,
            format!("no designated primary {} output exists", singleton.label()),
        )
    })?;
    let mut data = output_status_json(output);
    if singleton == Singleton::Record {
        // Record adds outputPaused; pause is not modeled yet (always false).
        data["outputPaused"] = json!(false);
    }
    Ok(Some(data))
}

async fn singleton_start(ctx: &RequestContext<'_>, singleton: Singleton) -> Handler {
    let snapshot = snapshot(ctx)?;
    let output_id = singleton
        .primary(snapshot.state())
        .map(|output| output.id)
        .ok_or_else(|| {
            not_found(format!(
                "no designated primary {} output exists",
                singleton.label()
            ))
        })?;
    start_output_by_id(ctx, snapshot.state(), output_id).await
}

async fn singleton_stop(ctx: &RequestContext<'_>, singleton: Singleton) -> Handler {
    let snapshot = snapshot(ctx)?;
    let output = singleton.primary(snapshot.state()).ok_or_else(|| {
        RequestStatus::error(
            proto::status::OUTPUT_NOT_RUNNING,
            format!("no designated primary {} output exists", singleton.label()),
        )
    })?;
    stop_output_by_id(ctx, snapshot.state(), output.id).await
}

async fn singleton_toggle(ctx: &RequestContext<'_>, singleton: Singleton) -> Handler {
    let snapshot = snapshot(ctx)?;
    // Toggle is start-capable, so an absent primary is a not-found (600).
    let output_id = singleton
        .primary(snapshot.state())
        .map(|output| output.id)
        .ok_or_else(|| {
            not_found(format!(
                "no designated primary {} output exists",
                singleton.label()
            ))
        })?;
    let active = toggle_output_by_id(ctx, snapshot.state(), output_id).await?;
    Ok(Some(json!({ "outputActive": active })))
}

// --- obs scene-item shape helpers ---

/// obs `alignment` bitfield (center 0, left 1, right 2, top 4, bottom 8).
fn alignment_to_obs(anchor: Anchor) -> u32 {
    match anchor {
        Anchor::Center => 0,
        Anchor::Left => 1,
        Anchor::Right => 2,
        Anchor::Top => 4,
        Anchor::TopLeft => 5,
        Anchor::TopRight => 6,
        Anchor::Bottom => 8,
        Anchor::BottomLeft => 9,
        Anchor::BottomRight => 10,
    }
}

fn anchor_from_obs(alignment: u64) -> Result<Anchor, RequestStatus> {
    match alignment {
        0 => Ok(Anchor::Center),
        1 => Ok(Anchor::Left),
        2 => Ok(Anchor::Right),
        4 => Ok(Anchor::Top),
        5 => Ok(Anchor::TopLeft),
        6 => Ok(Anchor::TopRight),
        8 => Ok(Anchor::Bottom),
        9 => Ok(Anchor::BottomLeft),
        10 => Ok(Anchor::BottomRight),
        other => Err(invalid_value(
            "alignment",
            format!("{other} is not a valid obs alignment"),
        )),
    }
}

fn bounds_type_str(kind: BoundsKind) -> &'static str {
    match kind {
        BoundsKind::None => "OBS_BOUNDS_NONE",
        BoundsKind::Stretch => "OBS_BOUNDS_STRETCH",
        BoundsKind::FitInner => "OBS_BOUNDS_SCALE_INNER",
        BoundsKind::FitOuter => "OBS_BOUNDS_SCALE_OUTER",
    }
}

fn bounds_kind_from_obs(bounds_type: &str) -> Result<BoundsKind, RequestStatus> {
    match bounds_type {
        "OBS_BOUNDS_NONE" => Ok(BoundsKind::None),
        "OBS_BOUNDS_STRETCH" => Ok(BoundsKind::Stretch),
        "OBS_BOUNDS_SCALE_INNER" => Ok(BoundsKind::FitInner),
        "OBS_BOUNDS_SCALE_OUTER" => Ok(BoundsKind::FitOuter),
        other => Err(invalid_value(
            "boundsType",
            format!("'{other}' is not supported (supported: OBS_BOUNDS_NONE, OBS_BOUNDS_STRETCH, OBS_BOUNDS_SCALE_INNER, OBS_BOUNDS_SCALE_OUTER)"),
        )),
    }
}

fn blend_mode_str(mode: prismcast_core::scene::BlendMode) -> &'static str {
    use prismcast_core::scene::BlendMode as B;
    match mode {
        B::Normal => "OBS_BLEND_NORMAL",
        B::Additive => "OBS_BLEND_ADDITIVE",
        B::Multiply => "OBS_BLEND_MULTIPLY",
        B::Screen => "OBS_BLEND_SCREEN",
    }
}

/// The obs `sceneItemTransform` object for an item. `sourceWidth`/
/// `sourceHeight` are the captured dimensions when the source has reported
/// them, else 0 (no capture runtime yet — documented divergence).
fn transform_json(snapshot: &AppSnapshot, item: &SceneItem) -> Value {
    let dimensions = snapshot
        .source_runtime(item.source_id)
        .and_then(|runtime| runtime.dimensions);
    let source_width = dimensions.map_or(0.0, |d| f64::from(d.width));
    let source_height = dimensions.map_or(0.0, |d| f64::from(d.height));
    let cropped_width =
        (source_width - f64::from(item.crop.left) - f64::from(item.crop.right)).max(0.0);
    let cropped_height =
        (source_height - f64::from(item.crop.top) - f64::from(item.crop.bottom)).max(0.0);
    let (width, height) = if item.bounds.kind == BoundsKind::None {
        (
            cropped_width * f64::from(item.transform.scale.x),
            cropped_height * f64::from(item.transform.scale.y),
        )
    } else {
        (f64::from(item.bounds.size.x), f64::from(item.bounds.size.y))
    };
    json!({
        "positionX": item.transform.position.x,
        "positionY": item.transform.position.y,
        "rotation": item.transform.rotation,
        "scaleX": item.transform.scale.x,
        "scaleY": item.transform.scale.y,
        "sourceWidth": source_width,
        "sourceHeight": source_height,
        "width": width,
        "height": height,
        "alignment": alignment_to_obs(item.transform.anchor),
        "boundsType": bounds_type_str(item.bounds.kind),
        "boundsAlignment": alignment_to_obs(item.bounds.alignment),
        "boundsWidth": item.bounds.size.x,
        "boundsHeight": item.bounds.size.y,
        "cropLeft": item.crop.left,
        "cropTop": item.crop.top,
        "cropRight": item.crop.right,
        "cropBottom": item.crop.bottom,
    })
}

/// One entry of `GetSceneItemList`'s `sceneItems` array.
fn scene_item_json(
    snapshot: &AppSnapshot,
    item_ids: &ItemIdMap,
    scene: &Scene,
    item: &SceneItem,
    index: usize,
) -> Value {
    let source = snapshot.source(item.source_id);
    let is_scene = matches!(source.map(|s| s.kind), Some(SourceKind::Scene(_)));
    json!({
        "sceneItemId": item_ids.mint(scene.id, item.id),
        "sceneItemIndex": index,
        "sourceName": source.map(|s| s.name.as_str()),
        "sourceUuid": source.map(|s| s.id.as_uuid().to_string()),
        "sourceType": if is_scene { "OBS_SOURCE_TYPE_SCENE" } else { "OBS_SOURCE_TYPE_INPUT" },
        // obs reports null inputKind for scene sources.
        "inputKind": if is_scene { Value::Null } else { source.map(|s| json!(input_kind_str(s.kind))).unwrap_or(Value::Null) },
        "isGroup": false,
        "sceneItemEnabled": item.visible,
        "sceneItemLocked": item.locked,
        "sceneItemBlendMode": blend_mode_str(item.blend_mode),
        "sceneItemTransform": transform_json(snapshot, item),
    })
}

// --- core → wire data payloads (for the RequestKind pivot) ---

fn anchor_to_data(anchor: Anchor) -> data::Anchor {
    match anchor {
        Anchor::TopLeft => data::Anchor::TopLeft,
        Anchor::Top => data::Anchor::Top,
        Anchor::TopRight => data::Anchor::TopRight,
        Anchor::Left => data::Anchor::Left,
        Anchor::Center => data::Anchor::Center,
        Anchor::Right => data::Anchor::Right,
        Anchor::BottomLeft => data::Anchor::BottomLeft,
        Anchor::Bottom => data::Anchor::Bottom,
        Anchor::BottomRight => data::Anchor::BottomRight,
    }
}

fn vec2_to_data(v: Vec2) -> data::Vec2 {
    data::Vec2 { x: v.x, y: v.y }
}

fn transform_to_data(transform: Transform) -> data::Transform {
    data::Transform {
        position: vec2_to_data(transform.position),
        scale: vec2_to_data(transform.scale),
        rotation: transform.rotation,
        anchor: anchor_to_data(transform.anchor),
    }
}

fn bounds_to_data(bounds: Bounds) -> data::Bounds {
    data::Bounds {
        kind: match bounds.kind {
            BoundsKind::None => data::BoundsKind::None,
            BoundsKind::Stretch => data::BoundsKind::Stretch,
            BoundsKind::FitInner => data::BoundsKind::FitInner,
            BoundsKind::FitOuter => data::BoundsKind::FitOuter,
        },
        size: vec2_to_data(bounds.size),
        alignment: anchor_to_data(bounds.alignment),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn available_requests_is_sorted_and_unique() {
        let mut sorted = AVAILABLE_REQUESTS.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted, AVAILABLE_REQUESTS, "sorted and deduplicated");
    }

    #[test]
    fn alignment_roundtrips_through_obs_bitfield() {
        for anchor in [
            Anchor::Center,
            Anchor::Left,
            Anchor::Right,
            Anchor::Top,
            Anchor::TopLeft,
            Anchor::TopRight,
            Anchor::Bottom,
            Anchor::BottomLeft,
            Anchor::BottomRight,
        ] {
            assert_eq!(
                anchor_from_obs(u64::from(alignment_to_obs(anchor))).expect("valid"),
                anchor
            );
        }
        // Contradictory combinations are rejected.
        assert_eq!(anchor_from_obs(3).expect_err("left|right").code, 400);
        assert_eq!(anchor_from_obs(12).expect_err("top|bottom").code, 400);
        assert_eq!(anchor_from_obs(7).expect_err("triple").code, 400);
    }

    #[test]
    fn bounds_type_roundtrips_supported_kinds() {
        for kind in [
            BoundsKind::None,
            BoundsKind::Stretch,
            BoundsKind::FitInner,
            BoundsKind::FitOuter,
        ] {
            assert_eq!(
                bounds_kind_from_obs(bounds_type_str(kind)).expect("valid"),
                kind
            );
        }
        assert_eq!(
            bounds_kind_from_obs("OBS_BOUNDS_SCALE_TO_WIDTH")
                .expect_err("unsupported")
                .code,
            400
        );
    }

    #[test]
    fn volume_mul_db_conversions() {
        assert!((db_to_mul(0.0) - 1.0).abs() < 1e-9);
        assert!((db_to_mul(-6.0) - 0.501_187).abs() < 1e-4);
        assert_eq!(db_to_mul(-100.0), 0.0, "at/below the floor reads as 0");
        assert_eq!(db_to_mul(-200.0), 0.0);
        assert!((mul_to_db(1.0) - 0.0).abs() < 1e-6);
        assert!((mul_to_db(0.5) - -6.0206).abs() < 1e-3);
        assert_eq!(mul_to_db(0.0), -100.0, "zero maps to the finite floor");
        assert_eq!(mul_to_db(-1.0), -100.0, "negative maps to the floor");
    }

    #[test]
    fn transition_names_match_display_and_kind_ids() {
        assert_eq!(transition_kind_by_name("Cut"), Some(TransitionKind::Cut));
        assert_eq!(
            transition_kind_by_name("fade_transition"),
            Some(TransitionKind::Fade)
        );
        assert_eq!(transition_kind_by_name("luma_wipe"), None);
    }

    #[test]
    fn field_extractors_map_to_300_401_402() {
        let data = json!({"name": "x", "count": 3, "neg": -1, "flag": true});
        assert_eq!(req_str(Some(&data), "name").expect("ok"), "x");
        assert_eq!(req_str(Some(&data), "missing").expect_err("300").code, 300);
        assert_eq!(req_str(None, "name").expect_err("300").code, 300);
        assert_eq!(req_str(Some(&data), "count").expect_err("401").code, 401);
        assert_eq!(req_u64(Some(&data), "count").expect("ok"), 3);
        assert_eq!(req_u64(Some(&data), "neg").expect_err("402").code, 402);
        assert_eq!(req_u64(Some(&data), "name").expect_err("401").code, 401);
        assert!(req_bool(Some(&data), "flag").expect("ok"));
        assert_eq!(opt_f64(Some(&data), "missing").expect("ok"), None);
        assert_eq!(opt_u64(Some(&data), "flag").expect_err("401").code, 401);
    }

    #[test]
    fn core_error_status_mapping() {
        assert_eq!(
            status_for_core_error(&Error::NotFound("x".into())).code,
            600
        );
        assert_eq!(
            status_for_core_error(&Error::Unauthorized("x".into())).code,
            703
        );
        assert_eq!(
            status_for_core_error(&Error::InvalidInput(
                "cannot start output in state Running".into()
            ))
            .code,
            500
        );
        assert_eq!(
            status_for_core_error(&Error::InvalidInput(
                "cannot stop output in state Stopped".into()
            ))
            .code,
            501
        );
        assert_eq!(
            status_for_core_error(&Error::InvalidInput("studio mode is disabled".into())).code,
            506
        );
        assert_eq!(
            status_for_core_error(&Error::InvalidInput("other".into())).code,
            400
        );
        assert_eq!(status_for_core_error(&Error::Media("x".into())).code, 701);
    }
}
