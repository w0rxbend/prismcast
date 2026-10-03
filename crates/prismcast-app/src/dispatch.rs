//! Command authorization and read-only queries (CORE-002; PLAN.md §20, §24).
//!
//! Every command passes an authorization checkpoint before it touches state
//! (ADR-0005: "Every command is an authorization checkpoint").
//! [`required_permission`] maps each [`Command`] variant to the [`Permission`]
//! scope it needs; the core actor rejects commands whose caller's
//! [`Permissions`] do not [`grant`](Permissions::grants) it with
//! [`Error::Unauthorized`], before any state access.
//!
//! Read-only [`Query`]s are served by [`crate::actor::AppHandle`] directly
//! from the latest immutable [`crate::snapshot::AppSnapshot`] — they never
//! enter the actor's command queue, so reads cannot block or be reordered
//! behind writes.
//!
//! ## Permission mapping (documented choice)
//!
//! PLAN.md §24 defines five scopes; each command domain maps onto exactly one:
//!
//! - Scenes, scene items, sources, studio mode, transitions → `ControlScenes`
//!   (sources exist to be composited in scenes; studio mode and transitions
//!   control what reaches program).
//! - Mixer parameters, audio routes, audio buses → `ControlAudio`.
//! - Output lifecycle and reconnect policy → `ControlOutputs`.
//! - Profiles and scene collections → `ModifyConfiguration`.
//! - Undo/Redo require any mutation scope initially, then every replay scope.
//! - `Transaction` requires the union of its members' permissions (empty =
//!   `Read`, a harmless no-op).
//!
//! `Admin` supersedes all scopes. `Read` alone never authorizes a mutation.

use std::fmt;

use serde::{Deserialize, Serialize};

use prismcast_core::error::{Error, Result};
use prismcast_core::id::{OutputId, SceneId, SourceId};
use prismcast_core::output::Output;
use prismcast_core::scene::Scene;
use prismcast_core::source::Source;
use prismcast_core::Command;

use crate::snapshot::AppSnapshot;
use std::sync::Arc;

/// An authorization scope (PLAN.md §24).
///
/// Mirrors `prismcast_protocol::Permission` without depending on the protocol
/// crate (dependency direction is `app <- remote`); the remote layer maps its
/// wire enum onto this one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Permission {
    /// Read state, receive subscribed events.
    Read,
    /// Scene, scene-item, source, studio-mode, and transition commands.
    ControlScenes,
    /// Audio mixer/routing commands.
    ControlAudio,
    /// Output lifecycle commands (start/stop/reconfigure outputs).
    ControlOutputs,
    /// Profiles, scene collections, and global configuration.
    ModifyConfiguration,
    /// Full control; supersedes the other scopes.
    Admin,
}

impl Permission {
    /// Bit used in the [`Permissions`] set.
    const fn bit(self) -> u8 {
        match self {
            Self::Read => 1 << 0,
            Self::ControlScenes => 1 << 1,
            Self::ControlAudio => 1 << 2,
            Self::ControlOutputs => 1 << 3,
            Self::ModifyConfiguration => 1 << 4,
            Self::Admin => 1 << 5,
        }
    }

    const ALL: [Self; 6] = [
        Self::Read,
        Self::ControlScenes,
        Self::ControlAudio,
        Self::ControlOutputs,
        Self::ModifyConfiguration,
        Self::Admin,
    ];
}

/// A set of permissions granted to a caller (session/token/controller).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Permissions {
    bits: u8,
}

impl Permissions {
    /// No permissions.
    pub fn none() -> Self {
        Self::default()
    }

    /// Only [`Permission::Read`] (e.g. a read-only dashboard).
    pub fn read_only() -> Self {
        Self::from_iter([Permission::Read])
    }

    /// [`Permission::Admin`]: full control. Local trusted controllers (GTK UI,
    /// CLI on the same machine) use this.
    pub fn admin() -> Self {
        Self::from_iter([Permission::Admin])
    }

    /// Builds a set from individual permissions.
    pub fn of(permissions: impl IntoIterator<Item = Permission>) -> Self {
        permissions.into_iter().collect()
    }

    /// Whether `permission` is in the set.
    pub fn contains(&self, permission: Permission) -> bool {
        self.bits & permission.bit() != 0
    }

    /// Whether this set authorizes an operation requiring `required`.
    ///
    /// [`Permission::Admin`] grants everything.
    pub fn grants(&self, required: Permission) -> bool {
        self.contains(Permission::Admin) || self.contains(required)
    }

    /// Whether the set can mutate anything (anything beyond read-only).
    /// History additionally authorizes every replayed operation in the actor.
    pub fn can_control(&self) -> bool {
        Permission::ALL
            .iter()
            .any(|p| *p != Permission::Read && self.contains(*p))
    }

    /// Checks the caller is authorized for `command`, returning
    /// [`Error::Unauthorized`] otherwise.
    ///
    /// Transactions are checked member by member: the caller must hold every
    /// scope the group touches (a folded union scope would let a caller with
    /// only one of the scopes execute the whole group).
    pub fn check(&self, command: &Command) -> Result<()> {
        if matches!(command, Command::Undo | Command::Redo) {
            return if self.can_control() {
                Ok(())
            } else {
                Err(Error::Unauthorized(format!(
                    "{} requires at least one control permission",
                    command.label()
                )))
            };
        }
        if let Command::Transaction { commands } = command {
            for member in commands {
                self.check(member)?;
            }
            return Ok(());
        }
        let required = required_permission(command);
        if self.grants(required) {
            Ok(())
        } else {
            Err(Error::Unauthorized(format!(
                "command '{}' requires {:?}",
                command.label(),
                required
            )))
        }
    }
}

impl FromIterator<Permission> for Permissions {
    fn from_iter<T: IntoIterator<Item = Permission>>(iter: T) -> Self {
        let mut set = Self::none();
        for permission in iter {
            set.bits |= permission.bit();
        }
        set
    }
}

impl fmt::Display for Permissions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names: Vec<&'static str> = Permission::ALL
            .iter()
            .filter(|p| self.contains(**p))
            .map(|p| match p {
                Permission::Read => "read",
                Permission::ControlScenes => "control_scenes",
                Permission::ControlAudio => "control_audio",
                Permission::ControlOutputs => "control_outputs",
                Permission::ModifyConfiguration => "modify_configuration",
                Permission::Admin => "admin",
            })
            .collect();
        write!(f, "[{}]", names.join(", "))
    }
}

impl Serialize for Permissions {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        let granted: Vec<Permission> = Permission::ALL
            .into_iter()
            .filter(|p| self.contains(*p))
            .collect();
        granted.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Permissions {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let permissions = Vec::<Permission>::deserialize(deserializer)?;
        Ok(Self::from_iter(permissions))
    }
}

/// The permission a command requires (PLAN.md §24; see module docs for the
/// mapping rationale). History returns the conservative Admin sentinel because
/// its actual scopes depend on actor-owned history; use Permissions::check
/// for the initial gate and actor dispatch for replay authorization.
pub fn required_permission(command: &Command) -> Permission {
    // AuthorizeSourceCapture preserves its existing ControlScenes policy for
    // both source families; controller authorization is independent of the
    // local video/audio owner capability (ADR-0024).
    use Command as C;
    match command {
        // A single static scope cannot describe history. Permissions::check
        // handles its initial gate; the actor checks the actual replay scopes.
        C::Undo | C::Redo => Permission::Admin,
        C::AddScene { .. }
        | C::RemoveScene { .. }
        | C::RenameScene { .. }
        | C::ReorderScene { .. }
        | C::SetCurrentScene { .. }
        | C::AddSceneItem { .. }
        | C::RemoveSceneItem { .. }
        | C::DuplicateSceneItem { .. }
        | C::SetSceneItemTransform { .. }
        | C::SetSceneItemCrop { .. }
        | C::SetSceneItemVisible { .. }
        | C::SetSceneItemLocked { .. }
        | C::SetSceneItemZIndex { .. }
        | C::RaiseSceneItem { .. }
        | C::LowerSceneItem { .. }
        | C::SetSceneItemOpacity { .. }
        | C::SetSceneItemBounds { .. }
        | C::AddSource { .. }
        | C::RemoveSource { .. }
        | C::RenameSource { .. }
        | C::SetSourceSettings { .. }
        | C::AuthorizeSourceCapture { .. }
        | C::SetSourceEnabled { .. }
        | C::SetStudioModeEnabled { .. }
        | C::SetPreviewScene { .. }
        | C::TransitionToProgram
        | C::SwapPreviewProgram
        | C::SetTransition { .. } => Permission::ControlScenes,

        C::SetSourceVolume { .. }
        | C::SetSourceMuted { .. }
        | C::SetSourceSolo { .. }
        | C::SetSourceMonitor { .. }
        | C::SetSourceBalance { .. }
        | C::SetSourceSyncOffset { .. }
        | C::AddAudioBus { .. }
        | C::RemoveAudioBus { .. }
        | C::SetAudioRoute { .. }
        | C::RemoveAudioRoute { .. } => Permission::ControlAudio,

        C::AddOutput { .. }
        | C::RemoveOutput { .. }
        | C::StartOutput { .. }
        | C::StopOutput { .. }
        | C::SetOutputReconnectPolicy { .. } => Permission::ControlOutputs,

        C::AddProfile { .. }
        | C::RemoveProfile { .. }
        | C::SelectProfile { .. }
        | C::AddSceneCollection { .. }
        | C::RemoveSceneCollection { .. }
        | C::SelectSceneCollection { .. } => Permission::ModifyConfiguration,

        C::Transaction { commands } => commands
            .iter()
            .map(required_permission)
            .fold(Permission::Read, permission_union),
    }
}

/// The least scope granting both `a` and `b` (used to aggregate transaction
/// members). Ordering is the declaration order of [`Permission`]; `Admin`
/// also conservatively classifies dynamic history operations.
fn permission_union(a: Permission, b: Permission) -> Permission {
    fn rank(p: Permission) -> u8 {
        match p {
            Permission::Read => 0,
            Permission::ControlScenes | Permission::ControlAudio | Permission::ControlOutputs => 1,
            Permission::ModifyConfiguration => 2,
            Permission::Admin => 3,
        }
    }
    if rank(a) >= rank(b) {
        a
    } else {
        b
    }
}

/// A read-only query served from the latest [`AppSnapshot`] — never queued to
/// the actor (PLAN.md §20 `Query`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Query {
    /// Full immutable snapshot (controllers render from this + events,
    /// PLAN.md §23).
    GetSnapshot,
    /// One scene by ID.
    GetScene {
        /// Scene to fetch.
        scene_id: SceneId,
    },
    /// All scenes in UI list order.
    ListScenes,
    /// One source by ID.
    GetSource {
        /// Source to fetch.
        source_id: SourceId,
    },
    /// All shared sources.
    ListSources,
    /// One output by ID.
    GetOutput {
        /// Output to fetch.
        output_id: OutputId,
    },
    /// All configured outputs.
    ListOutputs,
    /// The current (program) scene ID, if any.
    GetCurrentScene,
}

/// The answer to a [`Query`]. Entities are cloned out of the snapshot so the
/// caller does not hold the `Arc` open.
#[derive(Debug, Clone)]
pub enum QueryResponse {
    /// `GetSnapshot` answer.
    Snapshot(Arc<AppSnapshot>),
    /// `GetScene` answer (`None` = unknown ID).
    Scene(Option<Scene>),
    /// `ListScenes` answer.
    Scenes(Vec<Scene>),
    /// `GetSource` answer (`None` = unknown ID).
    Source(Option<Source>),
    /// `ListSources` answer.
    Sources(Vec<Source>),
    /// `GetOutput` answer (`None` = unknown ID).
    Output(Option<Output>),
    /// `ListOutputs` answer.
    Outputs(Vec<Output>),
    /// `GetCurrentScene` answer.
    CurrentScene(Option<SceneId>),
}

impl Query {
    /// Runs the query against a snapshot.
    pub fn resolve(&self, snapshot: &AppSnapshot) -> QueryResponse {
        match self {
            Self::GetSnapshot => QueryResponse::Snapshot(Arc::new(snapshot.clone())),
            Self::GetScene { scene_id } => QueryResponse::Scene(snapshot.scene(*scene_id).cloned()),
            Self::ListScenes => QueryResponse::Scenes(snapshot.scenes().cloned().collect()),
            Self::GetSource { source_id } => {
                QueryResponse::Source(snapshot.source(*source_id).cloned())
            }
            Self::ListSources => QueryResponse::Sources(snapshot.sources().cloned().collect()),
            Self::GetOutput { output_id } => {
                QueryResponse::Output(snapshot.output(*output_id).cloned())
            }
            Self::ListOutputs => QueryResponse::Outputs(snapshot.outputs().cloned().collect()),
            Self::GetCurrentScene => QueryResponse::CurrentScene(snapshot.current_scene()),
        }
    }

    /// Queries require [`Permission::Read`].
    pub fn required_permission(&self) -> Permission {
        Permission::Read
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use prismcast_core::audio::{MonitorMode, TrackMask};
    use prismcast_core::id::{AudioBusId, ProfileId, SceneCollectionId, SceneItemId};
    use prismcast_core::output::{Output, OutputKind, ReconnectPolicy};
    use prismcast_core::project::{Profile, SceneCollection, VideoConfig};
    use prismcast_core::scene::{Bounds, Crop, Transform};
    use prismcast_core::source::SourceKind;
    use prismcast_core::transition::Transition;
    use prismcast_core::EncoderId;

    fn sample_commands() -> Vec<(Command, Permission)> {
        let scene = SceneId::new();
        let item = SceneItemId::new();
        let source = SourceId::new();
        let bus = AudioBusId::new();
        let output = OutputId::new();
        vec![
            (Command::Undo, Permission::Admin),
            (Command::Redo, Permission::Admin),
            (
                Command::AddScene { name: "s".into() },
                Permission::ControlScenes,
            ),
            (
                Command::RemoveScene { scene_id: scene },
                Permission::ControlScenes,
            ),
            (
                Command::RenameScene {
                    scene_id: scene,
                    name: "r".into(),
                },
                Permission::ControlScenes,
            ),
            (
                Command::ReorderScene {
                    scene_id: scene,
                    new_index: 0,
                },
                Permission::ControlScenes,
            ),
            (
                Command::SetCurrentScene { scene_id: scene },
                Permission::ControlScenes,
            ),
            (
                Command::AddSceneItem {
                    scene_id: scene,
                    source_id: source,
                },
                Permission::ControlScenes,
            ),
            (
                Command::RemoveSceneItem {
                    scene_id: scene,
                    item_id: item,
                },
                Permission::ControlScenes,
            ),
            (
                Command::DuplicateSceneItem {
                    scene_id: scene,
                    item_id: item,
                },
                Permission::ControlScenes,
            ),
            (
                Command::SetSceneItemTransform {
                    scene_id: scene,
                    item_id: item,
                    transform: Transform::default(),
                },
                Permission::ControlScenes,
            ),
            (
                Command::SetSceneItemCrop {
                    scene_id: scene,
                    item_id: item,
                    crop: Crop::default(),
                },
                Permission::ControlScenes,
            ),
            (
                Command::SetSceneItemVisible {
                    scene_id: scene,
                    item_id: item,
                    visible: true,
                },
                Permission::ControlScenes,
            ),
            (
                Command::SetSceneItemLocked {
                    scene_id: scene,
                    item_id: item,
                    locked: true,
                },
                Permission::ControlScenes,
            ),
            (
                Command::SetSceneItemZIndex {
                    scene_id: scene,
                    item_id: item,
                    z_index: 1,
                },
                Permission::ControlScenes,
            ),
            (
                Command::RaiseSceneItem {
                    scene_id: scene,
                    item_id: item,
                },
                Permission::ControlScenes,
            ),
            (
                Command::LowerSceneItem {
                    scene_id: scene,
                    item_id: item,
                },
                Permission::ControlScenes,
            ),
            (
                Command::SetSceneItemOpacity {
                    scene_id: scene,
                    item_id: item,
                    opacity: 0.5,
                },
                Permission::ControlScenes,
            ),
            (
                Command::SetSceneItemBounds {
                    scene_id: scene,
                    item_id: item,
                    bounds: Bounds::default(),
                },
                Permission::ControlScenes,
            ),
            (
                Command::AddSource {
                    kind: SourceKind::Color,
                    name: "c".into(),
                },
                Permission::ControlScenes,
            ),
            (
                Command::RemoveSource { source_id: source },
                Permission::ControlScenes,
            ),
            (
                Command::RenameSource {
                    source_id: source,
                    name: "x".into(),
                },
                Permission::ControlScenes,
            ),
            (
                Command::SetSourceSettings {
                    source_id: source,
                    settings: serde_json::json!({}),
                },
                Permission::ControlScenes,
            ),
            (
                Command::AuthorizeSourceCapture { source_id: source },
                Permission::ControlScenes,
            ),
            (
                Command::SetSourceEnabled {
                    source_id: source,
                    enabled: true,
                },
                Permission::ControlScenes,
            ),
            (
                Command::SetSourceVolume {
                    source_id: source,
                    volume_db: -1.0,
                },
                Permission::ControlAudio,
            ),
            (
                Command::SetSourceMuted {
                    source_id: source,
                    muted: true,
                },
                Permission::ControlAudio,
            ),
            (
                Command::SetSourceSolo {
                    source_id: source,
                    solo: true,
                },
                Permission::ControlAudio,
            ),
            (
                Command::SetSourceMonitor {
                    source_id: source,
                    monitor: MonitorMode::Off,
                },
                Permission::ControlAudio,
            ),
            (
                Command::SetSourceBalance {
                    source_id: source,
                    balance: 0.0,
                },
                Permission::ControlAudio,
            ),
            (
                Command::SetSourceSyncOffset {
                    source_id: source,
                    sync_offset_ms: 0,
                },
                Permission::ControlAudio,
            ),
            (
                Command::AddAudioBus { name: "b".into() },
                Permission::ControlAudio,
            ),
            (
                Command::RemoveAudioBus { bus_id: bus },
                Permission::ControlAudio,
            ),
            (
                Command::SetAudioRoute {
                    source_id: source,
                    bus_id: bus,
                    tracks: TrackMask::stereo_pair(),
                },
                Permission::ControlAudio,
            ),
            (
                Command::RemoveAudioRoute {
                    source_id: source,
                    bus_id: bus,
                },
                Permission::ControlAudio,
            ),
            (
                Command::AddOutput {
                    output: Output::new(OutputKind::Recording, "rec", EncoderId::new()),
                },
                Permission::ControlOutputs,
            ),
            (
                Command::RemoveOutput { output_id: output },
                Permission::ControlOutputs,
            ),
            (
                Command::StartOutput { output_id: output },
                Permission::ControlOutputs,
            ),
            (
                Command::StopOutput { output_id: output },
                Permission::ControlOutputs,
            ),
            (
                Command::SetOutputReconnectPolicy {
                    output_id: output,
                    policy: ReconnectPolicy::default(),
                },
                Permission::ControlOutputs,
            ),
            (
                Command::SetStudioModeEnabled { enabled: true },
                Permission::ControlScenes,
            ),
            (
                Command::SetPreviewScene { scene_id: scene },
                Permission::ControlScenes,
            ),
            (Command::TransitionToProgram, Permission::ControlScenes),
            (Command::SwapPreviewProgram, Permission::ControlScenes),
            (
                Command::SetTransition {
                    transition: Transition::default(),
                },
                Permission::ControlScenes,
            ),
            (
                Command::AddProfile {
                    profile: Profile::new("p", VideoConfig::default()),
                },
                Permission::ModifyConfiguration,
            ),
            (
                Command::RemoveProfile {
                    profile_id: ProfileId::new(),
                },
                Permission::ModifyConfiguration,
            ),
            (
                Command::SelectProfile {
                    profile_id: ProfileId::new(),
                },
                Permission::ModifyConfiguration,
            ),
            (
                Command::AddSceneCollection {
                    collection: SceneCollection::new("c"),
                },
                Permission::ModifyConfiguration,
            ),
            (
                Command::RemoveSceneCollection {
                    collection_id: SceneCollectionId::new(),
                },
                Permission::ModifyConfiguration,
            ),
            (
                Command::SelectSceneCollection {
                    collection_id: SceneCollectionId::new(),
                },
                Permission::ModifyConfiguration,
            ),
        ]
    }

    #[test]
    fn every_variant_maps_to_expected_permission() {
        for (command, expected) in sample_commands() {
            assert_eq!(
                required_permission(&command),
                expected,
                "mapping for {}",
                command.label()
            );
        }
    }

    #[test]
    fn transaction_requires_union_of_members() {
        let mixed = Command::Transaction {
            commands: vec![
                Command::SetSourceMuted {
                    source_id: SourceId::new(),
                    muted: true,
                },
                Command::StartOutput {
                    output_id: OutputId::new(),
                },
            ],
        };
        // The aggregate scope is one of the member scopes (same rank); the
        // real enforcement is per-member in `Permissions::check`.
        assert!(matches!(
            required_permission(&mixed),
            Permission::ControlAudio | Permission::ControlOutputs
        ));
        // Holding only one member scope must not authorize the whole group.
        assert!(Permissions::from_iter([Permission::ControlAudio])
            .check(&mixed)
            .is_err());
        assert!(
            Permissions::from_iter([Permission::ControlAudio, Permission::ControlOutputs])
                .check(&mixed)
                .is_ok()
        );

        let empty = Command::Transaction { commands: vec![] };
        assert_eq!(required_permission(&empty), Permission::Read);
    }

    #[test]
    fn admin_grants_everything_read_grants_nothing_mutating() {
        let admin = Permissions::admin();
        let read = Permissions::read_only();
        for (command, required) in sample_commands() {
            assert!(
                admin.grants(required),
                "admin should grant {}",
                command.label()
            );
            assert!(
                !read.grants(required),
                "read-only must not grant {}",
                command.label()
            );
        }
        assert!(admin.grants(Permission::Read));
        assert!(read.grants(Permission::Read));
        assert!(!Permissions::none().grants(Permission::Read));
    }

    #[test]
    fn check_returns_unauthorized_with_context() {
        let err = Permissions::read_only()
            .check(&Command::SetCurrentScene {
                scene_id: SceneId::new(),
            })
            .expect_err("must be rejected");
        match err {
            Error::Unauthorized(message) => {
                assert!(message.contains("set current scene"), "{message}");
                assert!(message.contains("ControlScenes"), "{message}");
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn can_control() {
        assert!(!Permissions::none().can_control());
        assert!(!Permissions::read_only().can_control());
        assert!(Permissions::from_iter([Permission::ControlAudio]).can_control());
        assert!(Permissions::admin().can_control());
    }

    #[test]
    fn permissions_serde_roundtrip_as_list() {
        let perms = Permissions::from_iter([Permission::Read, Permission::ControlScenes]);
        let json = serde_json::to_string(&perms).expect("serialize");
        assert_eq!(json, r#"["read","control_scenes"]"#);
        let back: Permissions = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(perms, back);
    }

    #[test]
    fn query_resolves_against_snapshot() {
        let mut state = prismcast_core::AppState::new();
        state
            .apply(&Command::AddScene {
                name: "Main".into(),
            })
            .expect("apply");
        let snapshot = AppSnapshot::new(7, state);
        let scene_id = snapshot.scenes().next().expect("one scene").id;

        match (Query::GetScene { scene_id }).resolve(&snapshot) {
            QueryResponse::Scene(Some(scene)) => assert_eq!(scene.name, "Main"),
            other => panic!("unexpected {other:?}"),
        }
        match Query::ListScenes.resolve(&snapshot) {
            QueryResponse::Scenes(scenes) => assert_eq!(scenes.len(), 1),
            other => panic!("unexpected {other:?}"),
        }
        match Query::GetCurrentScene.resolve(&snapshot) {
            // First scene becomes current on creation.
            QueryResponse::CurrentScene(Some(id)) => assert_eq!(id, scene_id),
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(Query::GetSnapshot.required_permission(), Permission::Read);
    }
}
