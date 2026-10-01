//! Golden-file tests (PLAN §63, persistence-model §9): V1 `collection.json`
//! and `profile.toml` fixtures are byte-compared against serialization output
//! (serialization stability) and loaded to assert structure (deserialization
//! stability).
//!
//! Regenerate after an intentional format change with:
//! `BLESS_GOLDEN=1 cargo test -p prismcast-app --test golden`

use prismcast_app::persistence::envelope::{CollectionFileV1, CollectionSnapshot, SessionState};
use prismcast_app::persistence::profile::{ProfileDocument, ProfileSnapshot};
use prismcast_core::audio::{AudioMixerState, AudioRoute, TrackMask};
use prismcast_core::id::{EncoderId, SceneId, ServiceId, SourceId};
use prismcast_core::output::{
    EncoderSettings, Output, OutputKind, OutputState, SecretString, Service,
};
use prismcast_core::project::{Profile, SceneCollection, VideoConfig};
use prismcast_core::scene::{Scene, SceneItem};
use prismcast_core::source::{Source, SourceKind};
use std::str::FromStr;

const GOLDEN_COLLECTION: &str = "tests/golden/collection-v1.json";
const GOLDEN_PROFILE: &str = "tests/golden/profile-v1.toml";

fn fixed_ids() -> (SceneCollection, Scene, Source) {
    let source_id = SourceId::from(uuid::Uuid::from_u128(
        0x1111_1111_1111_1111_1111_1111_1111_1111,
    ));
    let scene_id = SceneId::from(uuid::Uuid::from_u128(
        0x2222_2222_2222_2222_2222_2222_2222_2222,
    ));
    let item_id = prismcast_core::id::SceneItemId::from(uuid::Uuid::from_u128(
        0x3333_3333_3333_3333_3333_3333_3333_3333,
    ));
    let collection_id = prismcast_core::id::SceneCollectionId::from(uuid::Uuid::from_u128(
        0x4444_4444_4444_4444_4444_4444_4444_4444,
    ));
    let bus_id = prismcast_core::id::AudioBusId::from(uuid::Uuid::from_u128(
        0x5555_5555_5555_5555_5555_5555_5555_5555,
    ));

    let mut source = Source::new(SourceKind::V4l2Camera, "Camera");
    source.id = source_id;
    source.settings = serde_json::json!({"device": "/dev/video0"});

    let mut item = SceneItem::new(source_id, 0);
    item.id = item_id;
    let mut scene = Scene::new("Main");
    scene.id = scene_id;
    scene.add_item(item);

    let mut collection = SceneCollection::new("dev-stream");
    collection.id = collection_id;
    collection.audio.buses[0].id = bus_id;
    collection.audio.routes.push(AudioRoute {
        source_id,
        bus_id,
        tracks: TrackMask::stereo_pair(),
    });
    collection.audio.mixer.insert(
        source_id,
        AudioMixerState {
            volume_db: -6.0,
            ..AudioMixerState::default()
        },
    );
    collection.sources.push(source.clone());
    collection.scenes.push(scene.clone());
    (collection, scene, source)
}

fn golden_collection_snapshot() -> CollectionSnapshot {
    let (collection, scene, _) = fixed_ids();
    CollectionSnapshot {
        collection,
        session: SessionState {
            current_scene: Some(scene.id),
            studio_mode: None,
        },
    }
}

fn golden_profile_snapshot() -> ProfileSnapshot {
    let profile_id = prismcast_core::id::ProfileId::from(uuid::Uuid::from_u128(
        0x6666_6666_6666_6666_6666_6666_6666_6666,
    ));
    let encoder_id = EncoderId::from(uuid::Uuid::from_u128(
        0x7777_7777_7777_7777_7777_7777_7777_7777,
    ));
    let service_id = ServiceId::from(uuid::Uuid::from_u128(
        0x8888_8888_8888_8888_8888_8888_8888_8888,
    ));
    let output_id = prismcast_core::id::OutputId::from(uuid::Uuid::from_u128(
        0x9999_9999_9999_9999_9999_9999_9999_9999,
    ));

    let mut profile = Profile::new("twitch-1080p", VideoConfig::hd_1080p60());
    profile.id = profile_id;
    profile.settings =
        serde_json::json!({"recording": {"format": "mkv", "path": "~/Videos/prismcast"}});

    let mut output = Output::new(OutputKind::Rtmp, "Twitch main", encoder_id);
    output.id = output_id;
    output.service = Some(service_id);
    output.state = OutputState::Running; // must not persist

    ProfileSnapshot {
        profile,
        encoders: vec![EncoderSettings {
            id: encoder_id,
            codec: "h264".into(),
            bitrate_kbps: 6000,
            keyframe_interval: Some(120),
            settings: serde_json::json!({"preset": "veryfast", "rate_control": "cbr"}),
        }],
        services: vec![Service {
            id: service_id,
            name: "Twitch".into(),
            url: "rtmps://live.twitch.tv/app".into(),
            key: SecretString::new("live_example_key"),
            settings: serde_json::Value::Null,
        }],
        outputs: vec![output],
    }
}

fn bless_or_compare(path: &str, expected: &[u8]) {
    if std::env::var_os("BLESS_GOLDEN").is_some() {
        std::fs::write(path, expected).expect("bless golden");
        return;
    }
    let golden = std::fs::read(path).unwrap_or_else(|e| panic!("read {path}: {e}"));
    assert_eq!(
        golden, expected,
        "{path} drifted from serialization output; if intentional, bless with BLESS_GOLDEN=1"
    );
}

#[test]
fn golden_collection_byte_stable_and_loads() {
    let snapshot = golden_collection_snapshot();
    let envelope = CollectionFileV1::from_snapshot(&snapshot, None);
    let bytes = envelope.to_json_bytes().unwrap();
    bless_or_compare(GOLDEN_COLLECTION, &bytes);

    let golden = std::fs::read(GOLDEN_COLLECTION).unwrap();
    let (loaded, migrated) =
        CollectionFileV1::from_json_bytes(&golden, std::path::Path::new(GOLDEN_COLLECTION))
            .unwrap();
    assert!(!migrated);
    // Deserialization stability: structure matches the domain snapshot.
    assert_eq!(loaded.to_snapshot(), snapshot);
    // Serialization stability: load → save is byte-identical.
    assert_eq!(loaded.to_json_bytes().unwrap(), golden);
}

#[test]
fn golden_profile_stable_and_loads() {
    let snapshot = golden_profile_snapshot();
    let bytes = ProfileDocument::to_bytes(&snapshot, None).unwrap();
    bless_or_compare(GOLDEN_PROFILE, &bytes);

    let golden = std::fs::read(GOLDEN_PROFILE).unwrap();
    let doc = ProfileDocument::parse(&golden, std::path::Path::new(GOLDEN_PROFILE)).unwrap();
    assert!(!doc.migrated());
    let loaded = doc.snapshot();
    let mut expected = snapshot.clone();
    for output in &mut expected.outputs {
        output.state = OutputState::Stopped;
    }
    assert_eq!(loaded, expected);
    // Load → save through the retained document is byte-identical for a
    // file containing only known keys in canonical order.
    let resaved = ProfileDocument::to_bytes(&loaded, Some(&doc)).unwrap();
    assert_eq!(
        String::from_utf8_lossy(&resaved),
        String::from_utf8_lossy(&golden),
        "profile load→save must be byte-identical"
    );
}

#[test]
fn uuid_fromstr_used_in_golden_fixtures() {
    // Keeps FromStr in scope for readers extending fixtures.
    assert!(SourceId::from_str("11111111-1111-1111-1111-111111111111").is_ok());
}
