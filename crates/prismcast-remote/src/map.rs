//! Wire ↔ domain mapping at the interface boundary (PLAN.md §75).
//!
//! `prismcast-protocol` types and `prismcast-core` types are deliberately
//! distinct; every conversion between them lives here so the wire schema and
//! the domain model can evolve independently. IDs cross the boundary as
//! `From<Uuid>` conversions into the domain's typed newtypes.
//!
//! Error mapping follows the table in `docs/protocols/native-protocol.md`
//! §Structured errors: `NotFound` → 600, `InvalidInput` → 400,
//! `Unauthorized` → 800, `Protocol` → 200, `Media`/`Io`/`Persistence` → 700.

use prismcast_app::dispatch::{Permission as AppPermission, Permissions as AppPermissions};
use prismcast_app::snapshot::AppSnapshot;
use prismcast_core::audio::{self, AudioMixerConfig, AudioMixerState, MonitorMode, TrackMask};
use prismcast_core::error::Error;
use prismcast_core::event::{AudioEvent, Event, OutputEvent, SceneEvent, SourceEvent, SystemEvent};
use prismcast_core::id::{
    AudioBusId, EncoderId, FilterId, OutputId, ProfileId, SceneCollectionId, SceneId, SceneItemId,
    ServiceId, SourceId,
};
use prismcast_core::output::{Output, OutputKind, OutputState, ReconnectPolicy};
use prismcast_core::project::{Profile, SceneCollection, StudioMode, VideoConfig};
use prismcast_core::scene::{
    Anchor, BlendMode, Bounds, BoundsKind, Crop, Scene, SceneItem, Transform, Vec2,
};
use prismcast_core::source::{Source, SourceKind};
use prismcast_core::state::AppState;
use prismcast_core::transition::{Transition, TransitionKind};
use prismcast_core::Command;
use prismcast_protocol::data;
use prismcast_protocol::error::{ErrorKind, WireError};
use prismcast_protocol::event::WireEvent;
use prismcast_protocol::handshake::Permission;
use prismcast_protocol::request::RequestKind;
use prismcast_protocol::response::ResponseData;
use prismcast_protocol::subscription::EventCategory;

/// Request type tags served at protocol v1, returned by `get_version`
/// (`available_requests` capability discovery; protocol doc §3).
///
/// Drift guard: `available_requests_are_real_tags` verifies every entry
/// against `RequestKind`'s serde tags.
pub const AVAILABLE_REQUESTS: &[&str] = &[
    "add_audio_bus",
    "add_output",
    "add_profile",
    "add_scene",
    "add_scene_collection",
    "add_scene_item",
    "add_source",
    "authorize_source_capture",
    "duplicate_scene_item",
    "get_audio_state",
    "get_output",
    "get_scene",
    "get_snapshot",
    "get_source",
    "get_subscriptions",
    "get_version",
    "list_outputs",
    "list_profiles",
    "list_scene_collections",
    "list_scenes",
    "list_sources",
    "lower_scene_item",
    "raise_scene_item",
    "remove_audio_bus",
    "remove_audio_route",
    "remove_output",
    "remove_profile",
    "remove_scene",
    "remove_scene_collection",
    "remove_scene_item",
    "remove_source",
    "rename_scene",
    "rename_source",
    "reorder_scene",
    "select_profile",
    "select_scene_collection",
    "set_audio_route",
    "set_current_scene",
    "set_output_reconnect_policy",
    "set_preview_scene",
    "set_scene_item_bounds",
    "set_scene_item_crop",
    "set_scene_item_locked",
    "set_scene_item_opacity",
    "set_scene_item_transform",
    "set_scene_item_visible",
    "set_scene_item_z_index",
    "set_source_balance",
    "set_source_enabled",
    "set_source_monitor",
    "set_source_muted",
    "set_source_settings",
    "set_source_solo",
    "set_source_sync_offset",
    "set_source_volume",
    "set_studio_mode_enabled",
    "set_transition",
    "start_output",
    "stop_output",
    "swap_preview_program",
    "transaction",
    "transition_to_program",
    "update_subscriptions",
];

/// Whether a request tag is known at this protocol version (unknown tags get
/// `unknown_request_type` 202 responses; protocol doc §2).
pub fn is_known_request_tag(tag: &str) -> bool {
    AVAILABLE_REQUESTS.contains(&tag)
}

// --- permissions ---

/// Maps a wire permission onto the application core's scope enum.
pub fn permission_to_app(permission: Permission) -> AppPermission {
    match permission {
        Permission::Read => AppPermission::Read,
        Permission::ControlScenes => AppPermission::ControlScenes,
        Permission::ControlAudio => AppPermission::ControlAudio,
        Permission::ControlOutputs => AppPermission::ControlOutputs,
        Permission::ModifyConfiguration => AppPermission::ModifyConfiguration,
        Permission::Admin => AppPermission::Admin,
    }
}

/// Maps a session's wire permissions onto the core's permission set.
pub fn permissions_to_app(permissions: &[Permission]) -> AppPermissions {
    AppPermissions::of(permissions.iter().copied().map(permission_to_app))
}

/// Maps the core's permission set back to the wire list (for `identified`).
pub fn permissions_to_wire(permissions: AppPermissions) -> Vec<Permission> {
    [
        AppPermission::Read,
        AppPermission::ControlScenes,
        AppPermission::ControlAudio,
        AppPermission::ControlOutputs,
        AppPermission::ModifyConfiguration,
        AppPermission::Admin,
    ]
    .into_iter()
    .filter(|p| permissions.contains(*p))
    .map(|p| match p {
        AppPermission::Read => Permission::Read,
        AppPermission::ControlScenes => Permission::ControlScenes,
        AppPermission::ControlAudio => Permission::ControlAudio,
        AppPermission::ControlOutputs => Permission::ControlOutputs,
        AppPermission::ModifyConfiguration => Permission::ModifyConfiguration,
        AppPermission::Admin => Permission::Admin,
    })
    .collect()
}

// --- errors ---

/// Maps a core error onto a wire error per the protocol's mapping table.
pub fn wire_error(error: &Error) -> WireError {
    match error {
        Error::NotFound(message) => WireError::new(ErrorKind::NotFound, message.clone()),
        Error::InvalidInput(message) => WireError::new(ErrorKind::InvalidField, message.clone()),
        Error::Unauthorized(message) => WireError::new(ErrorKind::Forbidden, message.clone()),
        Error::Protocol(message) => WireError::new(ErrorKind::InvalidRequest, message.clone()),
        Error::Media(message) | Error::Io(message) | Error::Persistence(message) => {
            WireError::new(ErrorKind::ProcessingFailed, message.clone())
        }
    }
}

// --- subscriptions ---

/// The application-layer category for a wire category, if the domain emits
/// events for it. `General` (session notices) and `Meter` (media telemetry,
/// not yet emitted by the domain) have no `prismcast-app` counterpart.
pub fn category_to_app(category: EventCategory) -> Option<prismcast_app::EventCategory> {
    match category {
        EventCategory::General | EventCategory::Meter => None,
        EventCategory::Scene => Some(prismcast_app::EventCategory::Scene),
        EventCategory::Source => Some(prismcast_app::EventCategory::Source),
        EventCategory::Audio => Some(prismcast_app::EventCategory::Audio),
        EventCategory::Output => Some(prismcast_app::EventCategory::Output),
        EventCategory::System => Some(prismcast_app::EventCategory::System),
    }
}

// --- requests ---

/// The role a request kind plays (protocol doc §5: commands mutate, queries
/// read, session requests manage the connection).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestClass {
    /// Maps to a [`Command`] and goes through `AppHandle::dispatch`.
    Command,
    /// Read-only; served from the latest snapshot via `AppHandle::query`.
    Query,
    /// Session management (`update_subscriptions`, `get_subscriptions`).
    Session,
}

/// Classifies a request kind.
pub fn classify(kind: &RequestKind) -> RequestClass {
    use RequestKind as R;
    match kind {
        R::GetVersion
        | R::GetSnapshot
        | R::ListScenes
        | R::GetScene { .. }
        | R::ListSources
        | R::GetSource { .. }
        | R::ListOutputs
        | R::GetOutput { .. }
        | R::GetAudioState
        | R::ListProfiles
        | R::ListSceneCollections => RequestClass::Query,
        R::UpdateSubscriptions { .. } | R::GetSubscriptions => RequestClass::Session,
        _ => RequestClass::Command,
    }
}

/// Maps a wire request onto a core command. Rejects non-command kinds and
/// transaction members that are queries or nested transactions (protocol
/// doc §5).
pub fn command_from_wire(kind: RequestKind) -> Result<Command, WireError> {
    use RequestKind as R;
    let not_a_command = || {
        WireError::new(
            ErrorKind::InvalidRequest,
            format!("'{}' is not a command", kind.tag()),
        )
    };
    match kind {
        R::AddScene { name } => Ok(Command::AddScene { name }),
        R::RemoveScene { scene_id } => Ok(Command::RemoveScene {
            scene_id: SceneId::from(scene_id),
        }),
        R::RenameScene { scene_id, name } => Ok(Command::RenameScene {
            scene_id: SceneId::from(scene_id),
            name,
        }),
        R::ReorderScene {
            scene_id,
            new_index,
        } => Ok(Command::ReorderScene {
            scene_id: SceneId::from(scene_id),
            new_index,
        }),
        R::SetCurrentScene { scene_id } => Ok(Command::SetCurrentScene {
            scene_id: SceneId::from(scene_id),
        }),
        R::AddSceneItem {
            scene_id,
            source_id,
        } => Ok(Command::AddSceneItem {
            scene_id: SceneId::from(scene_id),
            source_id: SourceId::from(source_id),
        }),
        R::RemoveSceneItem { scene_id, item_id } => Ok(Command::RemoveSceneItem {
            scene_id: SceneId::from(scene_id),
            item_id: SceneItemId::from(item_id),
        }),
        R::DuplicateSceneItem { scene_id, item_id } => Ok(Command::DuplicateSceneItem {
            scene_id: SceneId::from(scene_id),
            item_id: SceneItemId::from(item_id),
        }),
        R::SetSceneItemTransform {
            scene_id,
            item_id,
            transform,
        } => Ok(Command::SetSceneItemTransform {
            scene_id: SceneId::from(scene_id),
            item_id: SceneItemId::from(item_id),
            transform: transform_from_wire(&transform),
        }),
        R::SetSceneItemCrop {
            scene_id,
            item_id,
            crop,
        } => Ok(Command::SetSceneItemCrop {
            scene_id: SceneId::from(scene_id),
            item_id: SceneItemId::from(item_id),
            crop: crop_from_wire(crop),
        }),
        R::SetSceneItemVisible {
            scene_id,
            item_id,
            visible,
        } => Ok(Command::SetSceneItemVisible {
            scene_id: SceneId::from(scene_id),
            item_id: SceneItemId::from(item_id),
            visible,
        }),
        R::SetSceneItemLocked {
            scene_id,
            item_id,
            locked,
        } => Ok(Command::SetSceneItemLocked {
            scene_id: SceneId::from(scene_id),
            item_id: SceneItemId::from(item_id),
            locked,
        }),
        R::SetSceneItemZIndex {
            scene_id,
            item_id,
            z_index,
        } => Ok(Command::SetSceneItemZIndex {
            scene_id: SceneId::from(scene_id),
            item_id: SceneItemId::from(item_id),
            z_index,
        }),
        R::RaiseSceneItem { scene_id, item_id } => Ok(Command::RaiseSceneItem {
            scene_id: SceneId::from(scene_id),
            item_id: SceneItemId::from(item_id),
        }),
        R::LowerSceneItem { scene_id, item_id } => Ok(Command::LowerSceneItem {
            scene_id: SceneId::from(scene_id),
            item_id: SceneItemId::from(item_id),
        }),
        R::SetSceneItemOpacity {
            scene_id,
            item_id,
            opacity,
        } => Ok(Command::SetSceneItemOpacity {
            scene_id: SceneId::from(scene_id),
            item_id: SceneItemId::from(item_id),
            opacity,
        }),
        R::SetSceneItemBounds {
            scene_id,
            item_id,
            bounds,
        } => Ok(Command::SetSceneItemBounds {
            scene_id: SceneId::from(scene_id),
            item_id: SceneItemId::from(item_id),
            bounds: bounds_from_wire(&bounds),
        }),
        R::AddSource { kind, name } => Ok(Command::AddSource {
            kind: source_kind_from_wire(kind),
            name,
        }),
        R::RemoveSource { source_id } => Ok(Command::RemoveSource {
            source_id: SourceId::from(source_id),
        }),
        R::RenameSource { source_id, name } => Ok(Command::RenameSource {
            source_id: SourceId::from(source_id),
            name,
        }),
        R::SetSourceSettings {
            source_id,
            settings,
        } => Ok(Command::SetSourceSettings {
            source_id: SourceId::from(source_id),
            settings,
        }),
        R::AuthorizeSourceCapture { source_id } => Ok(Command::AuthorizeSourceCapture {
            source_id: SourceId::from(source_id),
        }),
        R::SetSourceEnabled { source_id, enabled } => Ok(Command::SetSourceEnabled {
            source_id: SourceId::from(source_id),
            enabled,
        }),
        R::SetSourceVolume {
            source_id,
            volume_db,
        } => Ok(Command::SetSourceVolume {
            source_id: SourceId::from(source_id),
            volume_db,
        }),
        R::SetSourceMuted { source_id, muted } => Ok(Command::SetSourceMuted {
            source_id: SourceId::from(source_id),
            muted,
        }),
        R::SetSourceSolo { source_id, solo } => Ok(Command::SetSourceSolo {
            source_id: SourceId::from(source_id),
            solo,
        }),
        R::SetSourceMonitor { source_id, monitor } => Ok(Command::SetSourceMonitor {
            source_id: SourceId::from(source_id),
            monitor: monitor_mode_from_wire(monitor),
        }),
        R::SetSourceBalance { source_id, balance } => Ok(Command::SetSourceBalance {
            source_id: SourceId::from(source_id),
            balance,
        }),
        R::SetSourceSyncOffset {
            source_id,
            sync_offset_ms,
        } => Ok(Command::SetSourceSyncOffset {
            source_id: SourceId::from(source_id),
            sync_offset_ms,
        }),
        R::AddAudioBus { name } => Ok(Command::AddAudioBus { name }),
        R::RemoveAudioBus { bus_id } => Ok(Command::RemoveAudioBus {
            bus_id: AudioBusId::from(bus_id),
        }),
        R::SetAudioRoute {
            source_id,
            bus_id,
            tracks,
        } => Ok(Command::SetAudioRoute {
            source_id: SourceId::from(source_id),
            bus_id: AudioBusId::from(bus_id),
            tracks: track_mask_from_wire(tracks),
        }),
        R::RemoveAudioRoute { source_id, bus_id } => Ok(Command::RemoveAudioRoute {
            source_id: SourceId::from(source_id),
            bus_id: AudioBusId::from(bus_id),
        }),
        R::AddOutput { output } => Ok(Command::AddOutput {
            output: output_from_wire(&output),
        }),
        R::RemoveOutput { output_id } => Ok(Command::RemoveOutput {
            output_id: OutputId::from(output_id),
        }),
        R::StartOutput { output_id } => Ok(Command::StartOutput {
            output_id: OutputId::from(output_id),
        }),
        R::StopOutput { output_id } => Ok(Command::StopOutput {
            output_id: OutputId::from(output_id),
        }),
        R::SetOutputReconnectPolicy { output_id, policy } => {
            Ok(Command::SetOutputReconnectPolicy {
                output_id: OutputId::from(output_id),
                policy: reconnect_policy_from_wire(policy),
            })
        }
        R::SetStudioModeEnabled { enabled } => Ok(Command::SetStudioModeEnabled { enabled }),
        R::SetPreviewScene { scene_id } => Ok(Command::SetPreviewScene {
            scene_id: SceneId::from(scene_id),
        }),
        R::TransitionToProgram => Ok(Command::TransitionToProgram),
        R::SwapPreviewProgram => Ok(Command::SwapPreviewProgram),
        R::SetTransition { transition } => Ok(Command::SetTransition {
            transition: transition_from_wire(&transition),
        }),
        R::AddProfile { profile } => Ok(Command::AddProfile {
            profile: profile_from_wire(&profile),
        }),
        R::RemoveProfile { profile_id } => Ok(Command::RemoveProfile {
            profile_id: ProfileId::from(profile_id),
        }),
        R::SelectProfile { profile_id } => Ok(Command::SelectProfile {
            profile_id: ProfileId::from(profile_id),
        }),
        R::AddSceneCollection { collection } => Ok(Command::AddSceneCollection {
            collection: collection_from_wire(&collection),
        }),
        R::RemoveSceneCollection { collection_id } => Ok(Command::RemoveSceneCollection {
            collection_id: SceneCollectionId::from(collection_id),
        }),
        R::SelectSceneCollection { collection_id } => Ok(Command::SelectSceneCollection {
            collection_id: SceneCollectionId::from(collection_id),
        }),
        R::Transaction { commands } => {
            let mut mapped = Vec::with_capacity(commands.len());
            for member in commands {
                if matches!(member, R::Transaction { .. }) {
                    return Err(WireError::new(
                        ErrorKind::InvalidRequest,
                        "transaction cannot nest inside transaction",
                    ));
                }
                if classify(&member) != RequestClass::Command {
                    return Err(WireError::new(
                        ErrorKind::InvalidRequest,
                        format!("transaction member '{}' is not a command", member.tag()),
                    ));
                }
                mapped.push(command_from_wire(member)?);
            }
            Ok(Command::Transaction { commands: mapped })
        }
        _ => Err(not_a_command()),
    }
}

/// Builds the success payload for a command from the events it committed:
/// creation commands answer with the server-assigned ID of the entity they
/// created; everything else answers `empty`.
pub fn response_data_for(request_tag: &str, events: &[Event]) -> ResponseData {
    match request_tag {
        "add_scene" => events
            .iter()
            .find_map(|e| match e {
                Event::Scene(SceneEvent::Added { scene_id, .. }) => {
                    Some(ResponseData::SceneCreated {
                        scene_id: *scene_id.as_uuid(),
                    })
                }
                _ => None,
            })
            .unwrap_or(ResponseData::Empty),
        "add_scene_item" | "duplicate_scene_item" => events
            .iter()
            .find_map(|e| match e {
                Event::Scene(SceneEvent::ItemAdded { item, .. }) => {
                    Some(ResponseData::SceneItemCreated {
                        item_id: *item.id.as_uuid(),
                    })
                }
                _ => None,
            })
            .unwrap_or(ResponseData::Empty),
        "add_source" => events
            .iter()
            .find_map(|e| match e {
                Event::Source(SourceEvent::Added { source }) => Some(ResponseData::SourceCreated {
                    source_id: *source.id.as_uuid(),
                }),
                _ => None,
            })
            .unwrap_or(ResponseData::Empty),
        "add_audio_bus" => events
            .iter()
            .find_map(|e| match e {
                Event::Audio(AudioEvent::BusAdded { bus_id, .. }) => {
                    Some(ResponseData::AudioBusCreated {
                        bus_id: *bus_id.as_uuid(),
                    })
                }
                _ => None,
            })
            .unwrap_or(ResponseData::Empty),
        "add_output" => events
            .iter()
            .find_map(|e| match e {
                Event::Output(OutputEvent::Added { output_id, .. }) => {
                    Some(ResponseData::OutputCreated {
                        output_id: *output_id.as_uuid(),
                    })
                }
                _ => None,
            })
            .unwrap_or(ResponseData::Empty),
        "add_profile" => events
            .iter()
            .find_map(|e| match e {
                Event::System(SystemEvent::ProfileAdded { profile_id }) => {
                    Some(ResponseData::ProfileCreated {
                        profile_id: *profile_id.as_uuid(),
                    })
                }
                _ => None,
            })
            .unwrap_or(ResponseData::Empty),
        "add_scene_collection" => events
            .iter()
            .find_map(|e| match e {
                Event::System(SystemEvent::CollectionAdded { collection_id }) => {
                    Some(ResponseData::CollectionCreated {
                        collection_id: *collection_id.as_uuid(),
                    })
                }
                _ => None,
            })
            .unwrap_or(ResponseData::Empty),
        _ => ResponseData::Empty,
    }
}

// --- events ---

/// Maps a committed domain event onto its wire mirror.
pub fn event_to_wire(event: &Event) -> WireEvent {
    match event {
        Event::Scene(event) => WireEvent::Scene(match event {
            SceneEvent::Added { scene_id, name } => data_scene_event_added(*scene_id, name),
            SceneEvent::Removed { scene_id } => prismcast_protocol::event::SceneEvent::Removed {
                scene_id: *scene_id.as_uuid(),
            },
            SceneEvent::Renamed { scene_id, name } => {
                prismcast_protocol::event::SceneEvent::Renamed {
                    scene_id: *scene_id.as_uuid(),
                    name: name.clone(),
                }
            }
            SceneEvent::Reordered => prismcast_protocol::event::SceneEvent::Reordered,
            SceneEvent::CurrentChanged { scene_id } => {
                prismcast_protocol::event::SceneEvent::CurrentChanged {
                    scene_id: *scene_id.as_uuid(),
                }
            }
            SceneEvent::ItemAdded { scene_id, item } => {
                prismcast_protocol::event::SceneEvent::ItemAdded {
                    scene_id: *scene_id.as_uuid(),
                    item: Box::new(scene_item_to_wire(item)),
                }
            }
            SceneEvent::ItemRemoved {
                scene_id,
                item_id,
                source_id,
            } => prismcast_protocol::event::SceneEvent::ItemRemoved {
                scene_id: *scene_id.as_uuid(),
                item_id: *item_id.as_uuid(),
                source_id: *source_id.as_uuid(),
            },
            SceneEvent::ItemUpdated { scene_id, item } => {
                prismcast_protocol::event::SceneEvent::ItemUpdated {
                    scene_id: *scene_id.as_uuid(),
                    item: Box::new(scene_item_to_wire(item)),
                }
            }
        }),
        Event::Source(event) => WireEvent::Source(match event {
            SourceEvent::CaptureAuthorizationRequested { source_id } => {
                prismcast_protocol::event::SourceEvent::CaptureAuthorizationRequested {
                    source_id: *source_id.as_uuid(),
                }
            }
            SourceEvent::RuntimeChanged { source_id, runtime } => {
                prismcast_protocol::event::SourceEvent::RuntimeChanged {
                    source_id: *source_id.as_uuid(),
                    runtime: runtime.as_ref().map(runtime_to_wire),
                }
            }
            SourceEvent::Added { source } => prismcast_protocol::event::SourceEvent::Added {
                source: Box::new(source_to_wire(source)),
            },
            SourceEvent::Removed { source_id } => prismcast_protocol::event::SourceEvent::Removed {
                source_id: *source_id.as_uuid(),
            },
            SourceEvent::Renamed { source_id, name } => {
                prismcast_protocol::event::SourceEvent::Renamed {
                    source_id: *source_id.as_uuid(),
                    name: name.clone(),
                }
            }
            SourceEvent::SettingsChanged { source_id } => {
                prismcast_protocol::event::SourceEvent::SettingsChanged {
                    source_id: *source_id.as_uuid(),
                }
            }
            SourceEvent::EnabledChanged { source_id, enabled } => {
                prismcast_protocol::event::SourceEvent::EnabledChanged {
                    source_id: *source_id.as_uuid(),
                    enabled: *enabled,
                }
            }
        }),
        Event::Audio(event) => WireEvent::Audio(match event {
            AudioEvent::MixerChanged { source_id, state } => {
                prismcast_protocol::event::AudioEvent::MixerChanged {
                    source_id: *source_id.as_uuid(),
                    state: mixer_state_to_wire(state),
                }
            }
            AudioEvent::BusAdded { bus_id, name } => {
                prismcast_protocol::event::AudioEvent::BusAdded {
                    bus_id: *bus_id.as_uuid(),
                    name: name.clone(),
                }
            }
            AudioEvent::BusRemoved { bus_id } => {
                prismcast_protocol::event::AudioEvent::BusRemoved {
                    bus_id: *bus_id.as_uuid(),
                }
            }
            AudioEvent::RouteChanged {
                source_id,
                bus_id,
                tracks,
            } => prismcast_protocol::event::AudioEvent::RouteChanged {
                source_id: *source_id.as_uuid(),
                bus_id: *bus_id.as_uuid(),
                tracks: track_mask_to_wire(*tracks),
            },
            AudioEvent::RouteRemoved { source_id, bus_id } => {
                prismcast_protocol::event::AudioEvent::RouteRemoved {
                    source_id: *source_id.as_uuid(),
                    bus_id: *bus_id.as_uuid(),
                }
            }
        }),
        Event::Output(event) => WireEvent::Output(match event {
            OutputEvent::Added { output_id, name } => {
                prismcast_protocol::event::OutputEvent::Added {
                    output_id: *output_id.as_uuid(),
                    name: name.clone(),
                }
            }
            OutputEvent::Removed { output_id } => prismcast_protocol::event::OutputEvent::Removed {
                output_id: *output_id.as_uuid(),
            },
            OutputEvent::StateChanged { output_id, state } => {
                prismcast_protocol::event::OutputEvent::StateChanged {
                    output_id: *output_id.as_uuid(),
                    state: output_state_to_wire(*state),
                }
            }
            OutputEvent::ReconnectPolicyChanged { output_id } => {
                prismcast_protocol::event::OutputEvent::ReconnectPolicyChanged {
                    output_id: *output_id.as_uuid(),
                }
            }
        }),
        Event::System(event) => WireEvent::System(match event {
            SystemEvent::StudioModeChanged { enabled } => {
                prismcast_protocol::event::SystemEvent::StudioModeChanged { enabled: *enabled }
            }
            SystemEvent::PreviewSceneChanged { scene_id } => {
                prismcast_protocol::event::SystemEvent::PreviewSceneChanged {
                    scene_id: *scene_id.as_uuid(),
                }
            }
            SystemEvent::TransitionChanged { transition } => {
                prismcast_protocol::event::SystemEvent::TransitionChanged {
                    transition: transition_to_wire(transition),
                }
            }
            SystemEvent::TransitionStarted { kind, duration_ms } => {
                prismcast_protocol::event::SystemEvent::TransitionStarted {
                    kind: transition_kind_to_wire(*kind),
                    duration_ms: *duration_ms,
                }
            }
            SystemEvent::ProfileAdded { profile_id } => {
                prismcast_protocol::event::SystemEvent::ProfileAdded {
                    profile_id: *profile_id.as_uuid(),
                }
            }
            SystemEvent::ProfileRemoved { profile_id } => {
                prismcast_protocol::event::SystemEvent::ProfileRemoved {
                    profile_id: *profile_id.as_uuid(),
                }
            }
            SystemEvent::ProfileSelected { profile_id } => {
                prismcast_protocol::event::SystemEvent::ProfileSelected {
                    profile_id: *profile_id.as_uuid(),
                }
            }
            SystemEvent::CollectionAdded { collection_id } => {
                prismcast_protocol::event::SystemEvent::CollectionAdded {
                    collection_id: *collection_id.as_uuid(),
                }
            }
            SystemEvent::CollectionRemoved { collection_id } => {
                prismcast_protocol::event::SystemEvent::CollectionRemoved {
                    collection_id: *collection_id.as_uuid(),
                }
            }
            SystemEvent::CollectionSelected { collection_id } => {
                prismcast_protocol::event::SystemEvent::CollectionSelected {
                    collection_id: *collection_id.as_uuid(),
                }
            }
        }),
    }
}

fn data_scene_event_added(scene_id: SceneId, name: &str) -> prismcast_protocol::event::SceneEvent {
    prismcast_protocol::event::SceneEvent::Added {
        scene_id: *scene_id.as_uuid(),
        name: name.to_string(),
    }
}

// --- data types ---

/// Maps the full domain state onto the wire snapshot (`get_snapshot`).
pub fn state_to_wire(state: &AppState) -> data::StateSnapshot {
    data::StateSnapshot {
        profiles: state.profiles.values().map(profile_to_wire).collect(),
        active_profile: state.active_profile.map(|id| *id.as_uuid()),
        collections: state.collections.values().map(collection_to_wire).collect(),
        active_collection: state.active_collection.map(|id| *id.as_uuid()),
        scenes: state.scenes.values().map(scene_to_wire).collect(),
        sources: state.sources.values().map(source_to_wire).collect(),
        source_runtime: Vec::new(),
        transition: transition_to_wire(&state.transition),
        audio: audio_config_to_wire(&state.audio),
        outputs: state.outputs.values().map(output_to_wire).collect(),
        current_scene: state.current_scene.map(|id| *id.as_uuid()),
        studio_mode: state.studio_mode.as_ref().map(studio_mode_to_wire),
    }
}

/// Convenience wrapper for snapshots.
pub fn snapshot_to_wire(snapshot: &AppSnapshot) -> data::StateSnapshot {
    let mut wire = state_to_wire(snapshot.state());
    wire.source_runtime = snapshot
        .source_runtimes()
        .map(|(source_id, runtime)| data::SourceRuntimeEntry {
            source_id: *source_id.as_uuid(),
            runtime: runtime_to_wire(runtime),
        })
        .collect();
    wire.source_runtime.sort_by_key(|entry| entry.source_id);
    wire
}

fn runtime_to_wire(runtime: &prismcast_core::SourceRuntime) -> data::SourceRuntime {
    use prismcast_core::CaptureStatus as C;
    data::SourceRuntime {
        generation: runtime.generation.value(),
        status: match runtime.status {
            C::Authorizing => data::CaptureStatus::Authorizing,
            C::Active => data::CaptureStatus::Active,
            C::Cancelled => data::CaptureStatus::Cancelled,
            C::Denied => data::CaptureStatus::Denied,
            C::Revoked => data::CaptureStatus::Revoked,
            C::Failed => data::CaptureStatus::Failed,
        },
        dimensions: runtime.dimensions.map(|d| data::SourceDimensions {
            width: d.width,
            height: d.height,
        }),
        message: runtime.message.clone(),
    }
}

fn vec2_to_wire(v: Vec2) -> data::Vec2 {
    data::Vec2 { x: v.x, y: v.y }
}

fn vec2_from_wire(v: data::Vec2) -> Vec2 {
    Vec2::new(v.x, v.y)
}

fn anchor_to_wire(anchor: Anchor) -> data::Anchor {
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

fn anchor_from_wire(anchor: data::Anchor) -> Anchor {
    match anchor {
        data::Anchor::TopLeft => Anchor::TopLeft,
        data::Anchor::Top => Anchor::Top,
        data::Anchor::TopRight => Anchor::TopRight,
        data::Anchor::Left => Anchor::Left,
        data::Anchor::Center => Anchor::Center,
        data::Anchor::Right => Anchor::Right,
        data::Anchor::BottomLeft => Anchor::BottomLeft,
        data::Anchor::Bottom => Anchor::Bottom,
        data::Anchor::BottomRight => Anchor::BottomRight,
    }
}

fn blend_mode_to_wire(mode: BlendMode) -> data::BlendMode {
    match mode {
        BlendMode::Normal => data::BlendMode::Normal,
        BlendMode::Additive => data::BlendMode::Additive,
        BlendMode::Multiply => data::BlendMode::Multiply,
        BlendMode::Screen => data::BlendMode::Screen,
    }
}

fn blend_mode_from_wire(mode: data::BlendMode) -> BlendMode {
    match mode {
        data::BlendMode::Normal => BlendMode::Normal,
        data::BlendMode::Additive => BlendMode::Additive,
        data::BlendMode::Multiply => BlendMode::Multiply,
        data::BlendMode::Screen => BlendMode::Screen,
    }
}

fn bounds_kind_to_wire(kind: BoundsKind) -> data::BoundsKind {
    match kind {
        BoundsKind::None => data::BoundsKind::None,
        BoundsKind::Stretch => data::BoundsKind::Stretch,
        BoundsKind::FitInner => data::BoundsKind::FitInner,
        BoundsKind::FitOuter => data::BoundsKind::FitOuter,
    }
}

fn bounds_kind_from_wire(kind: data::BoundsKind) -> BoundsKind {
    match kind {
        data::BoundsKind::None => BoundsKind::None,
        data::BoundsKind::Stretch => BoundsKind::Stretch,
        data::BoundsKind::FitInner => BoundsKind::FitInner,
        data::BoundsKind::FitOuter => BoundsKind::FitOuter,
    }
}

fn bounds_to_wire(bounds: &Bounds) -> data::Bounds {
    data::Bounds {
        kind: bounds_kind_to_wire(bounds.kind),
        size: vec2_to_wire(bounds.size),
        alignment: anchor_to_wire(bounds.alignment),
    }
}

fn bounds_from_wire(bounds: &data::Bounds) -> Bounds {
    Bounds {
        kind: bounds_kind_from_wire(bounds.kind),
        size: vec2_from_wire(bounds.size),
        alignment: anchor_from_wire(bounds.alignment),
    }
}

fn crop_to_wire(crop: Crop) -> data::Crop {
    data::Crop {
        left: crop.left,
        top: crop.top,
        right: crop.right,
        bottom: crop.bottom,
    }
}

fn crop_from_wire(crop: data::Crop) -> Crop {
    Crop {
        left: crop.left,
        top: crop.top,
        right: crop.right,
        bottom: crop.bottom,
    }
}

fn transform_to_wire(transform: &Transform) -> data::Transform {
    data::Transform {
        position: vec2_to_wire(transform.position),
        scale: vec2_to_wire(transform.scale),
        rotation: transform.rotation,
        anchor: anchor_to_wire(transform.anchor),
    }
}

fn transform_from_wire(transform: &data::Transform) -> Transform {
    Transform {
        position: vec2_from_wire(transform.position),
        scale: vec2_from_wire(transform.scale),
        rotation: transform.rotation,
        anchor: anchor_from_wire(transform.anchor),
    }
}

fn scene_item_to_wire(item: &SceneItem) -> data::SceneItem {
    data::SceneItem {
        id: *item.id.as_uuid(),
        source_id: *item.source_id.as_uuid(),
        transform: transform_to_wire(&item.transform),
        crop: crop_to_wire(item.crop),
        opacity: item.opacity,
        visible: item.visible,
        locked: item.locked,
        blend_mode: blend_mode_to_wire(item.blend_mode),
        bounds: bounds_to_wire(&item.bounds),
        z_index: item.z_index,
    }
}

fn scene_item_from_wire(item: &data::SceneItem) -> SceneItem {
    SceneItem {
        id: SceneItemId::from(item.id),
        source_id: SourceId::from(item.source_id),
        transform: transform_from_wire(&item.transform),
        crop: crop_from_wire(item.crop),
        opacity: item.opacity,
        visible: item.visible,
        locked: item.locked,
        blend_mode: blend_mode_from_wire(item.blend_mode),
        bounds: bounds_from_wire(&item.bounds),
        z_index: item.z_index,
    }
}

/// Maps a domain scene onto the wire type.
pub fn scene_to_wire(scene: &Scene) -> data::Scene {
    data::Scene {
        id: *scene.id.as_uuid(),
        name: scene.name.clone(),
        items: scene.items.iter().map(scene_item_to_wire).collect(),
    }
}

fn scene_from_wire(scene: &data::Scene) -> Scene {
    Scene {
        id: SceneId::from(scene.id),
        name: scene.name.clone(),
        items: scene.items.iter().map(scene_item_from_wire).collect(),
    }
}

fn source_kind_to_wire(kind: SourceKind) -> data::SourceKind {
    match kind {
        SourceKind::PipeWireDisplay => data::SourceKind::PipeWireDisplay,
        SourceKind::PipeWireWindow => data::SourceKind::PipeWireWindow,
        SourceKind::V4l2Camera => data::SourceKind::V4l2Camera,
        SourceKind::PipeWireAudioInput => data::SourceKind::PipeWireAudioInput,
        SourceKind::PipeWireAppAudio => data::SourceKind::PipeWireAppAudio,
        SourceKind::MediaFile => data::SourceKind::MediaFile,
        SourceKind::Image => data::SourceKind::Image,
        SourceKind::ImageSlideshow => data::SourceKind::ImageSlideshow,
        SourceKind::Color => data::SourceKind::Color,
        SourceKind::Text => data::SourceKind::Text,
        SourceKind::Browser => data::SourceKind::Browser,
        SourceKind::Scene(scene_id) => data::SourceKind::Scene(*scene_id.as_uuid()),
        SourceKind::TestPattern => data::SourceKind::TestPattern,
        SourceKind::NetworkStream => data::SourceKind::NetworkStream,
    }
}

fn source_kind_from_wire(kind: data::SourceKind) -> SourceKind {
    match kind {
        data::SourceKind::PipeWireDisplay => SourceKind::PipeWireDisplay,
        data::SourceKind::PipeWireWindow => SourceKind::PipeWireWindow,
        data::SourceKind::V4l2Camera => SourceKind::V4l2Camera,
        data::SourceKind::PipeWireAudioInput => SourceKind::PipeWireAudioInput,
        data::SourceKind::PipeWireAppAudio => SourceKind::PipeWireAppAudio,
        data::SourceKind::MediaFile => SourceKind::MediaFile,
        data::SourceKind::Image => SourceKind::Image,
        data::SourceKind::ImageSlideshow => SourceKind::ImageSlideshow,
        data::SourceKind::Color => SourceKind::Color,
        data::SourceKind::Text => SourceKind::Text,
        data::SourceKind::Browser => SourceKind::Browser,
        data::SourceKind::Scene(scene_id) => SourceKind::Scene(SceneId::from(scene_id)),
        data::SourceKind::TestPattern => SourceKind::TestPattern,
        data::SourceKind::NetworkStream => SourceKind::NetworkStream,
    }
}

/// Maps a domain source onto the wire type.
pub fn source_to_wire(source: &Source) -> data::Source {
    data::Source {
        id: *source.id.as_uuid(),
        kind: source_kind_to_wire(source.kind),
        name: source.name.clone(),
        enabled: source.enabled,
        settings: source.settings.clone(),
        filters: source.filters.iter().map(|f| *f.as_uuid()).collect(),
    }
}

fn source_from_wire(source: &data::Source) -> Source {
    Source {
        id: SourceId::from(source.id),
        kind: source_kind_from_wire(source.kind),
        name: source.name.clone(),
        enabled: source.enabled,
        settings: source.settings.clone(),
        filters: source.filters.iter().copied().map(FilterId::from).collect(),
    }
}

fn monitor_mode_to_wire(mode: MonitorMode) -> data::MonitorMode {
    match mode {
        MonitorMode::Off => data::MonitorMode::Off,
        MonitorMode::MonitorOnly => data::MonitorMode::MonitorOnly,
        MonitorMode::MonitorAndOutput => data::MonitorMode::MonitorAndOutput,
    }
}

fn monitor_mode_from_wire(mode: data::MonitorMode) -> MonitorMode {
    match mode {
        data::MonitorMode::Off => MonitorMode::Off,
        data::MonitorMode::MonitorOnly => MonitorMode::MonitorOnly,
        data::MonitorMode::MonitorAndOutput => MonitorMode::MonitorAndOutput,
    }
}

fn track_mask_to_wire(mask: TrackMask) -> data::TrackMask {
    data::TrackMask::from_bits(mask.bits())
}

/// Rebuilds a core track mask from wire bits (`prismcast-core` exposes no
/// `from_bits` constructor; composed from the public `with` builder).
fn track_mask_from_wire(mask: data::TrackMask) -> TrackMask {
    let bits = mask.bits();
    let mut result = TrackMask::NONE;
    for track in 0..32 {
        if bits & (1 << track) != 0 {
            result = result.with(track);
        }
    }
    result
}

fn mixer_state_to_wire(state: &AudioMixerState) -> data::AudioMixerState {
    data::AudioMixerState {
        volume_db: state.volume_db,
        muted: state.muted,
        solo: state.solo,
        monitor: monitor_mode_to_wire(state.monitor),
        balance: state.balance,
        sync_offset_ms: state.sync_offset_ms,
    }
}

fn mixer_state_from_wire(state: &data::AudioMixerState) -> AudioMixerState {
    AudioMixerState {
        volume_db: state.volume_db,
        muted: state.muted,
        solo: state.solo,
        monitor: monitor_mode_from_wire(state.monitor),
        balance: state.balance,
        sync_offset_ms: state.sync_offset_ms,
    }
}

/// Maps the domain audio configuration onto the wire type (mixer map becomes
/// a deterministic entry list).
pub fn audio_config_to_wire(config: &AudioMixerConfig) -> data::AudioMixerConfig {
    data::AudioMixerConfig {
        buses: config
            .buses
            .iter()
            .map(|bus| data::AudioBus {
                id: *bus.id.as_uuid(),
                name: bus.name.clone(),
            })
            .collect(),
        routes: config
            .routes
            .iter()
            .map(|route| data::AudioRoute {
                source_id: *route.source_id.as_uuid(),
                bus_id: *route.bus_id.as_uuid(),
                tracks: track_mask_to_wire(route.tracks),
            })
            .collect(),
        mixer: config
            .mixer
            .iter()
            .map(|(source_id, state)| data::MixerEntry {
                source_id: *source_id.as_uuid(),
                state: mixer_state_to_wire(state),
            })
            .collect(),
    }
}

fn audio_config_from_wire(config: &data::AudioMixerConfig) -> AudioMixerConfig {
    AudioMixerConfig {
        buses: config
            .buses
            .iter()
            .map(|bus| audio::AudioBus {
                id: AudioBusId::from(bus.id),
                name: bus.name.clone(),
            })
            .collect(),
        routes: config
            .routes
            .iter()
            .map(|route| audio::AudioRoute {
                source_id: SourceId::from(route.source_id),
                bus_id: AudioBusId::from(route.bus_id),
                tracks: track_mask_from_wire(route.tracks),
            })
            .collect(),
        mixer: config
            .mixer
            .iter()
            .map(|entry| {
                (
                    SourceId::from(entry.source_id),
                    mixer_state_from_wire(&entry.state),
                )
            })
            .collect(),
    }
}

fn transition_kind_to_wire(kind: TransitionKind) -> data::TransitionKind {
    match kind {
        TransitionKind::Cut => data::TransitionKind::Cut,
        TransitionKind::Fade => data::TransitionKind::Fade,
        TransitionKind::Swipe => data::TransitionKind::Swipe,
        TransitionKind::Slide => data::TransitionKind::Slide,
        TransitionKind::Stinger => data::TransitionKind::Stinger,
    }
}

fn transition_kind_from_wire(kind: data::TransitionKind) -> TransitionKind {
    match kind {
        data::TransitionKind::Cut => TransitionKind::Cut,
        data::TransitionKind::Fade => TransitionKind::Fade,
        data::TransitionKind::Swipe => TransitionKind::Swipe,
        data::TransitionKind::Slide => TransitionKind::Slide,
        data::TransitionKind::Stinger => TransitionKind::Stinger,
    }
}

/// Maps a domain transition onto the wire type.
pub fn transition_to_wire(transition: &Transition) -> data::Transition {
    data::Transition {
        kind: transition_kind_to_wire(transition.kind),
        duration_ms: transition.duration_ms,
        settings: transition.settings.clone(),
    }
}

fn transition_from_wire(transition: &data::Transition) -> Transition {
    Transition {
        kind: transition_kind_from_wire(transition.kind),
        duration_ms: transition.duration_ms,
        settings: transition.settings.clone(),
    }
}

fn output_kind_to_wire(kind: OutputKind) -> data::OutputKind {
    match kind {
        OutputKind::Recording => data::OutputKind::Recording,
        OutputKind::Rtmp => data::OutputKind::Rtmp,
        OutputKind::Srt => data::OutputKind::Srt,
        OutputKind::Whip => data::OutputKind::Whip,
        OutputKind::VirtualCamera => data::OutputKind::VirtualCamera,
    }
}

fn output_kind_from_wire(kind: data::OutputKind) -> OutputKind {
    match kind {
        data::OutputKind::Recording => OutputKind::Recording,
        data::OutputKind::Rtmp => OutputKind::Rtmp,
        data::OutputKind::Srt => OutputKind::Srt,
        data::OutputKind::Whip => OutputKind::Whip,
        data::OutputKind::VirtualCamera => OutputKind::VirtualCamera,
    }
}

fn output_state_to_wire(state: OutputState) -> data::OutputState {
    match state {
        OutputState::Stopped => data::OutputState::Stopped,
        OutputState::Starting => data::OutputState::Starting,
        OutputState::Running => data::OutputState::Running,
        OutputState::Reconnecting { attempt } => data::OutputState::Reconnecting { attempt },
        OutputState::Degraded => data::OutputState::Degraded,
        OutputState::Failed => data::OutputState::Failed,
        OutputState::Stopping => data::OutputState::Stopping,
    }
}

fn reconnect_policy_to_wire(policy: ReconnectPolicy) -> data::ReconnectPolicy {
    data::ReconnectPolicy {
        max_retries: policy.max_retries,
        initial_backoff_ms: policy.initial_backoff_ms,
        max_backoff_ms: policy.max_backoff_ms,
    }
}

fn reconnect_policy_from_wire(policy: data::ReconnectPolicy) -> ReconnectPolicy {
    ReconnectPolicy {
        max_retries: policy.max_retries,
        initial_backoff_ms: policy.initial_backoff_ms,
        max_backoff_ms: policy.max_backoff_ms,
    }
}

/// Maps a domain output onto the wire type.
pub fn output_to_wire(output: &Output) -> data::Output {
    data::Output {
        id: *output.id.as_uuid(),
        kind: output_kind_to_wire(output.kind),
        name: output.name.clone(),
        video_encoder: *output.video_encoder.as_uuid(),
        audio_encoders: output.audio_encoders.iter().map(|e| *e.as_uuid()).collect(),
        service: output.service.map(|s| *s.as_uuid()),
        reconnect_policy: reconnect_policy_to_wire(output.reconnect_policy),
        state: output_state_to_wire(output.state),
    }
}

/// Maps a wire output onto a domain output for `add_output`. The `id` and
/// `state` fields are server-assigned: client-supplied values are ignored
/// (protocol doc §5).
fn output_from_wire(output: &data::Output) -> Output {
    Output {
        id: OutputId::new(),
        kind: output_kind_from_wire(output.kind),
        name: output.name.clone(),
        video_encoder: EncoderId::from(output.video_encoder),
        audio_encoders: output
            .audio_encoders
            .iter()
            .copied()
            .map(EncoderId::from)
            .collect(),
        service: output.service.map(ServiceId::from),
        reconnect_policy: reconnect_policy_from_wire(output.reconnect_policy),
        state: OutputState::Stopped,
    }
}

fn video_config_to_wire(video: VideoConfig) -> data::VideoConfig {
    data::VideoConfig {
        width: video.width,
        height: video.height,
        fps_num: video.fps_num,
        fps_den: video.fps_den,
    }
}

fn video_config_from_wire(video: data::VideoConfig) -> VideoConfig {
    VideoConfig {
        width: video.width,
        height: video.height,
        fps_num: video.fps_num,
        fps_den: video.fps_den,
    }
}

/// Maps a domain profile onto the wire type.
pub fn profile_to_wire(profile: &Profile) -> data::Profile {
    data::Profile {
        id: *profile.id.as_uuid(),
        name: profile.name.clone(),
        video: video_config_to_wire(profile.video),
        settings: profile.settings.clone(),
    }
}

/// Maps a wire profile onto a domain profile for `add_profile`. The `id`
/// field is server-assigned (protocol doc §5).
fn profile_from_wire(profile: &data::Profile) -> Profile {
    Profile {
        id: ProfileId::new(),
        name: profile.name.clone(),
        video: video_config_from_wire(profile.video),
        settings: profile.settings.clone(),
    }
}

/// Maps a domain scene collection onto the wire type.
pub fn collection_to_wire(collection: &SceneCollection) -> data::SceneCollection {
    data::SceneCollection {
        id: *collection.id.as_uuid(),
        name: collection.name.clone(),
        scenes: collection.scenes.iter().map(scene_to_wire).collect(),
        sources: collection.sources.iter().map(source_to_wire).collect(),
        transition: transition_to_wire(&collection.transition),
        audio: audio_config_to_wire(&collection.audio),
    }
}

/// Maps a wire collection onto a domain collection for
/// `add_scene_collection`. The `id` field is server-assigned.
fn collection_from_wire(collection: &data::SceneCollection) -> SceneCollection {
    SceneCollection {
        id: SceneCollectionId::new(),
        name: collection.name.clone(),
        scenes: collection.scenes.iter().map(scene_from_wire).collect(),
        sources: collection.sources.iter().map(source_from_wire).collect(),
        transition: transition_from_wire(&collection.transition),
        audio: audio_config_from_wire(&collection.audio),
    }
}

fn studio_mode_to_wire(studio_mode: &StudioMode) -> data::StudioMode {
    data::StudioMode {
        enabled: studio_mode.enabled,
        program: *studio_mode.program.as_uuid(),
        preview: *studio_mode.preview.as_uuid(),
    }
}

/// Extracts a UUID field from a decoded frame for error reporting.
pub(crate) fn value_str<'a>(value: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(|v| v.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use prismcast_protocol::subscription::{Subscription, SubscriptionSet};
    use uuid::Uuid;

    #[tokio::test]
    async fn capture_command_runtime_snapshot_and_events_are_grant_free_wire_types() {
        use prismcast_core::{CaptureStatus, SourceDimensions};
        let mut state = AppState::new();
        let source = Source::new(SourceKind::PipeWireDisplay, "screen");
        let source_id = source.id;
        state.sources.insert(source_id, source);
        let app =
            prismcast_app::AppHandle::spawn_with_state(state, prismcast_app::CoreConfig::default());
        let mut owner = app.attach_capture_owner().await.unwrap();
        let wire_request: RequestKind = serde_json::from_value(serde_json::json!({"request":"authorize_source_capture", "source_id":source_id.as_uuid()})).unwrap();
        assert_eq!(
            command_from_wire(wire_request).unwrap(),
            Command::AuthorizeSourceCapture { source_id }
        );
        let response = app
            .authorize_source_capture(source_id, Some("x11:abc123".into()))
            .await
            .unwrap();
        let request = owner.requests.recv().await.unwrap();
        owner
            .runtime
            .report(
                source_id,
                request.generation,
                CaptureStatus::Active,
                Some(SourceDimensions {
                    width: 6144,
                    height: 3456,
                }),
                None,
            )
            .await
            .unwrap();
        let snapshot = snapshot_to_wire(&app.snapshot());
        assert_eq!(snapshot.source_runtime.len(), 1);
        let observation = &snapshot.source_runtime[0];
        assert_eq!(observation.source_id, *source_id.as_uuid());
        assert_eq!(observation.runtime.generation, request.generation.value());
        assert_eq!(observation.runtime.status, data::CaptureStatus::Active);
        assert_eq!(
            observation.runtime.dimensions,
            Some(data::SourceDimensions {
                width: 6144,
                height: 3456
            })
        );
        let json = serde_json::to_value(&snapshot).unwrap();
        assert!(!json.to_string().contains("abc123"));
        assert!(json["sources"][0].get("runtime").is_none());
        assert!(json["sources"][0]["settings"].is_null());
        let roundtrip: data::StateSnapshot = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(roundtrip, snapshot);
        // Older wire snapshots omit the additive observation map.
        let mut older = json;
        older.as_object_mut().unwrap().remove("source_runtime");
        assert!(serde_json::from_value::<data::StateSnapshot>(older)
            .unwrap()
            .source_runtime
            .is_empty());
        for event in response.events {
            let wire = event_to_wire(&event);
            assert_eq!(wire.primary_entity(), Some(*source_id.as_uuid()));
            let encoded = serde_json::to_value(&wire).unwrap();
            assert!(!encoded.to_string().contains("abc123"));
            assert_eq!(serde_json::from_value::<WireEvent>(encoded).unwrap(), wire);
        }
        app.shutdown().await;
    }

    #[test]
    fn available_requests_are_real_tags() {
        // Drift guard: every listed tag must be a known `RequestKind` serde
        // tag. Known-but-fieldless variants deserialize; known variants with
        // required fields fail with "missing field"; unknown tags fail with
        // "unknown variant".
        for tag in AVAILABLE_REQUESTS {
            let probe = serde_json::json!({"request": tag});
            match serde_json::from_value::<RequestKind>(probe) {
                Ok(_) => {}
                Err(error) => assert!(
                    !error.to_string().contains("unknown variant"),
                    "AVAILABLE_REQUESTS lists unknown tag `{tag}`: {error}"
                ),
            }
        }
        // The list must be sorted and deduplicated (it is presented as a set).
        let mut sorted = AVAILABLE_REQUESTS.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted, AVAILABLE_REQUESTS, "list must be sorted, unique");
        assert_eq!(sorted.len(), 63, "protocol v1 has 63 request kinds");
    }

    #[test]
    fn unknown_tags_are_detected() {
        assert!(is_known_request_tag("add_scene"));
        assert!(!is_known_request_tag("teleport"));
    }

    #[test]
    fn permission_mapping_roundtrips() {
        let wire = vec![Permission::Read, Permission::ControlScenes];
        let app = permissions_to_app(&wire);
        assert!(app.grants(AppPermission::Read));
        assert!(app.grants(AppPermission::ControlScenes));
        assert!(!app.grants(AppPermission::ControlAudio));
        assert_eq!(permissions_to_wire(app), wire);

        let admin = permissions_to_app(&[Permission::Admin]);
        assert!(admin.grants(AppPermission::ControlOutputs));
        assert_eq!(permissions_to_wire(admin), vec![Permission::Admin]);
    }

    #[test]
    fn core_errors_map_per_protocol_table() {
        let cases = [
            (Error::NotFound("x".into()), ErrorKind::NotFound, 600),
            (
                Error::InvalidInput("x".into()),
                ErrorKind::InvalidField,
                400,
            ),
            (Error::Unauthorized("x".into()), ErrorKind::Forbidden, 800),
            (Error::Protocol("x".into()), ErrorKind::InvalidRequest, 200),
            (Error::Media("x".into()), ErrorKind::ProcessingFailed, 700),
            (Error::Io("x".into()), ErrorKind::ProcessingFailed, 700),
            (
                Error::Persistence("x".into()),
                ErrorKind::ProcessingFailed,
                700,
            ),
        ];
        for (error, kind, code) in cases {
            let wire = wire_error(&error);
            assert_eq!((wire.kind, wire.code), (kind, code));
        }
    }

    #[test]
    fn command_roundtrip_through_wire() {
        let scene = Uuid::new_v4();
        let source = Uuid::new_v4();
        let kinds = vec![
            RequestKind::AddScene {
                name: "Main".into(),
            },
            RequestKind::SetCurrentScene { scene_id: scene },
            RequestKind::AddSceneItem {
                scene_id: scene,
                source_id: source,
            },
            RequestKind::SetSourceMuted {
                source_id: source,
                muted: true,
            },
            RequestKind::Transaction {
                commands: vec![
                    RequestKind::AddScene { name: "a".into() },
                    RequestKind::SetCurrentScene { scene_id: scene },
                ],
            },
        ];
        for kind in kinds {
            assert_eq!(classify(&kind), RequestClass::Command);
            let command = command_from_wire(kind.clone()).expect("map");
            // The core command serializes with the same tag as the wire kind.
            let value = serde_json::to_value(&command).expect("ser");
            assert_eq!(value["command"], kind.tag(), "tag parity for {kind:?}");
        }
    }

    #[test]
    fn transaction_rejects_queries_and_nesting() {
        let with_query = RequestKind::Transaction {
            commands: vec![RequestKind::GetVersion],
        };
        let err = command_from_wire(with_query).expect_err("query in transaction");
        assert_eq!(err.kind, ErrorKind::InvalidRequest);

        let nested = RequestKind::Transaction {
            commands: vec![RequestKind::Transaction { commands: vec![] }],
        };
        assert!(command_from_wire(nested).is_err());
    }

    #[test]
    fn queries_and_session_requests_are_not_commands() {
        assert_eq!(classify(&RequestKind::GetSnapshot), RequestClass::Query);
        assert_eq!(
            classify(&RequestKind::UpdateSubscriptions {
                subscriptions: SubscriptionSet::none()
            }),
            RequestClass::Session
        );
        let err = command_from_wire(RequestKind::GetVersion).expect_err("not a command");
        assert_eq!(err.kind, ErrorKind::InvalidRequest);
    }

    #[test]
    fn creation_responses_carry_server_assigned_ids() {
        let scene_id = SceneId::new();
        let events = vec![
            Event::Scene(SceneEvent::Added {
                scene_id,
                name: "Main".into(),
            }),
            Event::Scene(SceneEvent::CurrentChanged { scene_id }),
        ];
        assert_eq!(
            response_data_for("add_scene", &events),
            ResponseData::SceneCreated {
                scene_id: *scene_id.as_uuid()
            }
        );
        assert_eq!(
            response_data_for("set_current_scene", &events),
            ResponseData::Empty
        );
        // Defensive: missing creation event falls back to Empty.
        assert_eq!(response_data_for("add_scene", &[]), ResponseData::Empty);
    }

    #[test]
    fn event_mapping_preserves_category_and_entity() {
        let scene_id = SceneId::new();
        let event = Event::Scene(SceneEvent::CurrentChanged { scene_id });
        let wire = event_to_wire(&event);
        assert_eq!(wire.category(), EventCategory::Scene);
        assert_eq!(wire.primary_entity(), Some(*scene_id.as_uuid()));
        // Wire events survive a JSON roundtrip unchanged.
        let json = serde_json::to_string(&wire).expect("ser");
        assert_eq!(wire, serde_json::from_str(&json).expect("de"));
    }

    #[test]
    fn state_snapshot_roundtrips_through_wire() {
        let mut state = AppState::new();
        state
            .apply(&Command::AddScene {
                name: "Main".into(),
            })
            .expect("apply");
        let wire = state_to_wire(&state);
        assert_eq!(wire.scenes.len(), 1);
        assert_eq!(wire.scenes[0].name, "Main");
        assert_eq!(wire.current_scene, Some(wire.scenes[0].id));
        assert_eq!(wire.profiles.len(), 1);
        let json = serde_json::to_string(&wire).expect("ser");
        let back: data::StateSnapshot = serde_json::from_str(&json).expect("de");
        assert_eq!(wire, back);
    }

    #[test]
    fn add_output_and_profile_ignore_client_ids() {
        let wire_output = data::Output {
            id: Uuid::new_v4(),
            kind: data::OutputKind::Recording,
            name: "rec".into(),
            video_encoder: Uuid::new_v4(),
            audio_encoders: Vec::new(),
            service: None,
            reconnect_policy: data::ReconnectPolicy {
                max_retries: 1,
                initial_backoff_ms: 2,
                max_backoff_ms: 3,
            },
            state: data::OutputState::Running,
        };
        let command = command_from_wire(RequestKind::AddOutput {
            output: wire_output.clone(),
        })
        .expect("map");
        match command {
            Command::AddOutput { output } => {
                assert_ne!(
                    *output.id.as_uuid(),
                    wire_output.id,
                    "server assigns the id"
                );
                assert_eq!(output.state, OutputState::Stopped, "server assigns state");
                assert_eq!(output.name, "rec");
            }
            other => panic!("unexpected {other:?}"),
        }

        let wire_profile = data::Profile {
            id: Uuid::new_v4(),
            name: "p".into(),
            video: data::VideoConfig {
                width: 1280,
                height: 720,
                fps_num: 30,
                fps_den: 1,
            },
            settings: serde_json::Value::Null,
        };
        let command = command_from_wire(RequestKind::AddProfile {
            profile: wire_profile.clone(),
        })
        .expect("map");
        match command {
            Command::AddProfile { profile } => {
                assert_ne!(*profile.id.as_uuid(), wire_profile.id);
                assert_eq!(profile.video.width, 1280);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn subscription_category_mapping() {
        assert_eq!(
            category_to_app(EventCategory::Scene),
            Some(prismcast_app::EventCategory::Scene)
        );
        assert_eq!(category_to_app(EventCategory::Meter), None);
        assert_eq!(category_to_app(EventCategory::General), None);
        let set = SubscriptionSet {
            entries: vec![Subscription::category(EventCategory::Scene)],
        };
        assert!(set.validate().is_ok());
    }
}
