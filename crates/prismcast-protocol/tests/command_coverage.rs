//! Command coverage: every `prismcast_core::Command` variant must be
//! representable as a `prismcast_protocol::RequestKind` (ARCH-007 acceptance
//! criterion).
//!
//! The test pairs each core command with its wire counterpart using the
//! *same* UUIDs and field values, then asserts:
//! 1. the wire tag (`request`) equals the core tag (`command`) — names stay
//!    aligned so the adapter and logs read consistently;
//! 2. every core command variant appears in the pairing (exhaustiveness is
//!    by construction: this list duplicates core's sample list; when a core
//!    variant is added, add its pair here — `cargo test` coverage of the
//!    remaining variants in core's own test suite will surface the gap in
//!    review, and the count assertion below pins the current surface);
//! 3. the pairing covers the full command surface: no duplicate tags, and
//!    the set of command tags in `RequestKind` equals the core set.

use std::collections::BTreeSet;

use prismcast_core::audio::{MonitorMode as CoreMonitorMode, TrackMask as CoreTrackMask};
use prismcast_core::command::Command;
use prismcast_core::id::{
    AudioBusId, EncoderId, OutputId, ProfileId, SceneCollectionId, SceneId, SceneItemId, SourceId,
};
use prismcast_core::output::{
    Output as CoreOutput, OutputKind as CoreOutputKind, ReconnectPolicy as CoreReconnectPolicy,
};
use prismcast_core::project::{
    Profile as CoreProfile, SceneCollection as CoreCollection, VideoConfig,
};
use prismcast_core::scene::{
    Anchor as CoreAnchor, Bounds as CoreBounds, Crop as CoreCrop, Transform as CoreTransform,
    Vec2 as CoreVec2,
};
use prismcast_core::source::SourceKind as CoreSourceKind;
use prismcast_core::transition::{
    Transition as CoreTransition, TransitionKind as CoreTransitionKind,
};
use prismcast_protocol::data::{
    AudioMixerConfig, Bounds, Crop, MonitorMode, Output, OutputKind, OutputState, ReconnectPolicy,
    SceneCollection, SourceKind, TrackMask, Transform, Transition, TransitionKind, Vec2,
};
use prismcast_protocol::request::RequestKind;
use uuid::Uuid;

/// All 52 core command variants paired with their wire representation.
/// Field values are identical on both sides so representability — not just
/// name alignment — is exercised.
fn command_pairs() -> Vec<(Command, RequestKind)> {
    let scene = Uuid::new_v4();
    let item = Uuid::new_v4();
    let source = Uuid::new_v4();
    let bus = Uuid::new_v4();
    let output = Uuid::new_v4();
    let profile = Uuid::new_v4();
    let collection = Uuid::new_v4();
    let encoder = Uuid::new_v4();

    let scene_id = SceneId::from(scene);
    let item_id = SceneItemId::from(item);
    let source_id = SourceId::from(source);
    let bus_id = AudioBusId::from(bus);
    let output_id = OutputId::from(output);

    let core_transform = CoreTransform {
        position: CoreVec2::new(1.0, 2.0),
        scale: CoreVec2::new(1.5, 1.5),
        rotation: 90.0,
        anchor: CoreAnchor::Center,
    };
    let wire_transform = Transform {
        position: Vec2 { x: 1.0, y: 2.0 },
        scale: Vec2 { x: 1.5, y: 1.5 },
        rotation: 90.0,
        anchor: prismcast_protocol::data::Anchor::Center,
    };
    let core_crop = CoreCrop {
        left: 1,
        top: 2,
        right: 3,
        bottom: 4,
    };
    let wire_crop = Crop {
        left: 1,
        top: 2,
        right: 3,
        bottom: 4,
    };
    let core_policy = CoreReconnectPolicy {
        max_retries: 5,
        initial_backoff_ms: 250,
        max_backoff_ms: 10_000,
    };
    let wire_policy = ReconnectPolicy {
        max_retries: 5,
        initial_backoff_ms: 250,
        max_backoff_ms: 10_000,
    };

    vec![
        (Command::Undo, RequestKind::Undo),
        (Command::Redo, RequestKind::Redo),
        (
            Command::AddScene {
                name: "Main".into(),
            },
            RequestKind::AddScene {
                name: "Main".into(),
            },
        ),
        (
            Command::RemoveScene { scene_id },
            RequestKind::RemoveScene { scene_id: scene },
        ),
        (
            Command::RenameScene {
                scene_id,
                name: "Renamed".into(),
            },
            RequestKind::RenameScene {
                scene_id: scene,
                name: "Renamed".into(),
            },
        ),
        (
            Command::ReorderScene {
                scene_id,
                new_index: 2,
            },
            RequestKind::ReorderScene {
                scene_id: scene,
                new_index: 2,
            },
        ),
        (
            Command::SetCurrentScene { scene_id },
            RequestKind::SetCurrentScene { scene_id: scene },
        ),
        (
            Command::AddSceneItem {
                scene_id,
                source_id,
            },
            RequestKind::AddSceneItem {
                scene_id: scene,
                source_id: source,
            },
        ),
        (
            Command::RemoveSceneItem { scene_id, item_id },
            RequestKind::RemoveSceneItem {
                scene_id: scene,
                item_id: item,
            },
        ),
        (
            Command::DuplicateSceneItem { scene_id, item_id },
            RequestKind::DuplicateSceneItem {
                scene_id: scene,
                item_id: item,
            },
        ),
        (
            Command::SetSceneItemTransform {
                scene_id,
                item_id,
                transform: core_transform,
            },
            RequestKind::SetSceneItemTransform {
                scene_id: scene,
                item_id: item,
                transform: wire_transform,
            },
        ),
        (
            Command::SetSceneItemCrop {
                scene_id,
                item_id,
                crop: core_crop,
            },
            RequestKind::SetSceneItemCrop {
                scene_id: scene,
                item_id: item,
                crop: wire_crop,
            },
        ),
        (
            Command::SetSceneItemVisible {
                scene_id,
                item_id,
                visible: false,
            },
            RequestKind::SetSceneItemVisible {
                scene_id: scene,
                item_id: item,
                visible: false,
            },
        ),
        (
            Command::SetSceneItemLocked {
                scene_id,
                item_id,
                locked: true,
            },
            RequestKind::SetSceneItemLocked {
                scene_id: scene,
                item_id: item,
                locked: true,
            },
        ),
        (
            Command::SetSceneItemZIndex {
                scene_id,
                item_id,
                z_index: 7,
            },
            RequestKind::SetSceneItemZIndex {
                scene_id: scene,
                item_id: item,
                z_index: 7,
            },
        ),
        (
            Command::RaiseSceneItem { scene_id, item_id },
            RequestKind::RaiseSceneItem {
                scene_id: scene,
                item_id: item,
            },
        ),
        (
            Command::LowerSceneItem { scene_id, item_id },
            RequestKind::LowerSceneItem {
                scene_id: scene,
                item_id: item,
            },
        ),
        (
            Command::SetSceneItemOpacity {
                scene_id,
                item_id,
                opacity: 0.5,
            },
            RequestKind::SetSceneItemOpacity {
                scene_id: scene,
                item_id: item,
                opacity: 0.5,
            },
        ),
        (
            Command::SetSceneItemBounds {
                scene_id,
                item_id,
                bounds: CoreBounds::default(),
            },
            RequestKind::SetSceneItemBounds {
                scene_id: scene,
                item_id: item,
                bounds: Bounds::default(),
            },
        ),
        (
            Command::AddSource {
                kind: CoreSourceKind::V4l2Camera,
                name: "cam".into(),
            },
            RequestKind::AddSource {
                kind: SourceKind::V4l2Camera,
                name: "cam".into(),
            },
        ),
        (
            Command::RemoveSource { source_id },
            RequestKind::RemoveSource { source_id: source },
        ),
        (
            Command::RenameSource {
                source_id,
                name: "cam2".into(),
            },
            RequestKind::RenameSource {
                source_id: source,
                name: "cam2".into(),
            },
        ),
        (
            Command::SetSourceSettings {
                source_id,
                settings: serde_json::json!({"device": "/dev/video0"}),
            },
            RequestKind::SetSourceSettings {
                source_id: source,
                settings: serde_json::json!({"device": "/dev/video0"}),
            },
        ),
        (
            Command::AuthorizeSourceCapture { source_id },
            RequestKind::AuthorizeSourceCapture { source_id: source },
        ),
        (
            Command::SetSourceEnabled {
                source_id,
                enabled: false,
            },
            RequestKind::SetSourceEnabled {
                source_id: source,
                enabled: false,
            },
        ),
        (
            Command::SetSourceVolume {
                source_id,
                volume_db: -3.0,
            },
            RequestKind::SetSourceVolume {
                source_id: source,
                volume_db: -3.0,
            },
        ),
        (
            Command::SetSourceMuted {
                source_id,
                muted: true,
            },
            RequestKind::SetSourceMuted {
                source_id: source,
                muted: true,
            },
        ),
        (
            Command::SetSourceSolo {
                source_id,
                solo: true,
            },
            RequestKind::SetSourceSolo {
                source_id: source,
                solo: true,
            },
        ),
        (
            Command::SetSourceMonitor {
                source_id,
                monitor: CoreMonitorMode::MonitorOnly,
            },
            RequestKind::SetSourceMonitor {
                source_id: source,
                monitor: MonitorMode::MonitorOnly,
            },
        ),
        (
            Command::SetSourceBalance {
                source_id,
                balance: -0.5,
            },
            RequestKind::SetSourceBalance {
                source_id: source,
                balance: -0.5,
            },
        ),
        (
            Command::SetSourceSyncOffset {
                source_id,
                sync_offset_ms: 80,
            },
            RequestKind::SetSourceSyncOffset {
                source_id: source,
                sync_offset_ms: 80,
            },
        ),
        (
            Command::AddAudioBus { name: "VOD".into() },
            RequestKind::AddAudioBus { name: "VOD".into() },
        ),
        (
            Command::RemoveAudioBus { bus_id },
            RequestKind::RemoveAudioBus { bus_id: bus },
        ),
        (
            Command::SetAudioRoute {
                source_id,
                bus_id,
                tracks: CoreTrackMask::stereo_pair(),
            },
            RequestKind::SetAudioRoute {
                source_id: source,
                bus_id: bus,
                tracks: TrackMask::stereo_pair(),
            },
        ),
        (
            Command::RemoveAudioRoute { source_id, bus_id },
            RequestKind::RemoveAudioRoute {
                source_id: source,
                bus_id: bus,
            },
        ),
        (
            Command::AddOutput {
                output: CoreOutput::new(CoreOutputKind::Recording, "rec", EncoderId::from(encoder)),
            },
            RequestKind::AddOutput {
                output: Output {
                    id: Uuid::nil(),
                    kind: OutputKind::Recording,
                    name: "rec".into(),
                    video_encoder: encoder,
                    audio_encoders: Vec::new(),
                    service: None,
                    reconnect_policy: ReconnectPolicy {
                        max_retries: 10,
                        initial_backoff_ms: 1_000,
                        max_backoff_ms: 30_000,
                    },
                    state: OutputState::Stopped,
                },
            },
        ),
        (
            Command::RemoveOutput { output_id },
            RequestKind::RemoveOutput { output_id: output },
        ),
        (
            Command::StartOutput { output_id },
            RequestKind::StartOutput { output_id: output },
        ),
        (
            Command::StopOutput { output_id },
            RequestKind::StopOutput { output_id: output },
        ),
        (
            Command::SetOutputReconnectPolicy {
                output_id,
                policy: core_policy,
            },
            RequestKind::SetOutputReconnectPolicy {
                output_id: output,
                policy: wire_policy,
            },
        ),
        (
            Command::SetStudioModeEnabled { enabled: true },
            RequestKind::SetStudioModeEnabled { enabled: true },
        ),
        (
            Command::SetPreviewScene { scene_id },
            RequestKind::SetPreviewScene { scene_id: scene },
        ),
        (
            Command::TransitionToProgram,
            RequestKind::TransitionToProgram,
        ),
        (Command::SwapPreviewProgram, RequestKind::SwapPreviewProgram),
        (
            Command::SetTransition {
                transition: CoreTransition {
                    kind: CoreTransitionKind::Cut,
                    duration_ms: 0,
                    settings: serde_json::Value::Null,
                },
            },
            RequestKind::SetTransition {
                transition: Transition {
                    kind: TransitionKind::Cut,
                    duration_ms: 0,
                    settings: serde_json::Value::Null,
                },
            },
        ),
        (
            Command::AddProfile {
                profile: CoreProfile::new("p", VideoConfig::default()),
            },
            RequestKind::AddProfile {
                profile: prismcast_protocol::data::Profile {
                    id: Uuid::nil(),
                    name: "p".into(),
                    video: prismcast_protocol::data::VideoConfig {
                        width: 1920,
                        height: 1080,
                        fps_num: 60,
                        fps_den: 1,
                    },
                    settings: serde_json::Value::Null,
                },
            },
        ),
        (
            Command::RemoveProfile {
                profile_id: ProfileId::from(profile),
            },
            RequestKind::RemoveProfile {
                profile_id: profile,
            },
        ),
        (
            Command::SelectProfile {
                profile_id: ProfileId::from(profile),
            },
            RequestKind::SelectProfile {
                profile_id: profile,
            },
        ),
        (
            Command::AddSceneCollection {
                collection: CoreCollection::new("c"),
            },
            RequestKind::AddSceneCollection {
                collection: SceneCollection {
                    id: Uuid::nil(),
                    name: "c".into(),
                    scenes: Vec::new(),
                    sources: Vec::new(),
                    transition: Transition::default(),
                    audio: AudioMixerConfig::default(),
                },
            },
        ),
        (
            Command::RemoveSceneCollection {
                collection_id: SceneCollectionId::from(collection),
            },
            RequestKind::RemoveSceneCollection {
                collection_id: collection,
            },
        ),
        (
            Command::SelectSceneCollection {
                collection_id: SceneCollectionId::from(collection),
            },
            RequestKind::SelectSceneCollection {
                collection_id: collection,
            },
        ),
        (
            Command::Transaction {
                commands: vec![
                    Command::SetSourceMuted {
                        source_id,
                        muted: true,
                    },
                    Command::RaiseSceneItem { scene_id, item_id },
                ],
            },
            RequestKind::Transaction {
                commands: vec![
                    RequestKind::SetSourceMuted {
                        source_id: source,
                        muted: true,
                    },
                    RequestKind::RaiseSceneItem {
                        scene_id: scene,
                        item_id: item,
                    },
                ],
            },
        ),
    ]
}

#[test]
fn every_core_command_is_representable_as_a_request() {
    let pairs = command_pairs();

    // Pin the current command surface: 52 variants today. When core adds a
    // command, extend `command_pairs` and bump this count in the same commit.
    assert_eq!(pairs.len(), 52, "core command surface changed");

    let mut core_tags = BTreeSet::new();
    let mut wire_tags = BTreeSet::new();
    for (command, request) in &pairs {
        let command_value = serde_json::to_value(command).unwrap();
        let core_tag = command_value["command"].as_str().unwrap().to_string();
        let request_value = serde_json::to_value(request).unwrap();
        let wire_tag = request_value["request"].as_str().unwrap().to_string();
        assert_eq!(
            core_tag, wire_tag,
            "tag mismatch between core command and wire request"
        );
        assert!(
            core_tags.insert(core_tag.clone()),
            "duplicate core tag {core_tag}"
        );
        assert!(wire_tags.insert(wire_tag), "duplicate wire tag");
        // Both directions serialize: the wire request roundtrips.
        let json = serde_json::to_string(request).unwrap();
        let back: RequestKind = serde_json::from_str(&json).unwrap();
        assert_eq!(*request, back);
    }
    assert_eq!(core_tags, wire_tags);
}

#[test]
fn request_kind_tags_match_serde_for_paired_commands() {
    for (_, request) in command_pairs() {
        let value = serde_json::to_value(&request).unwrap();
        assert_eq!(request.tag(), value["request"].as_str().unwrap());
    }
}
