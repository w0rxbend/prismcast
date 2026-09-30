//! Trait-level tests: every backend trait is exercised through its mock,
//! as a `Box<dyn Trait>` where the trait is meant to be object-safe.

use prismcast_core::{
    CanvasId, EncoderId, EncoderSettings, Error, FilterId, OutputId, OutputKind, Scene, SceneItem,
    SecretString, Service, ServiceId, SourceId, SourceKind, Transition, TransitionKind,
    VideoConfig,
};
use prismcast_media::mock::{
    MockAudioFilterBackend, MockCompositorBackend, MockEncoderBackend, MockEncoderRegistry,
    MockFilterBackend, MockOutputBackend, MockSourceBackend, MockStreamingServiceBackend,
    MockVideoFilterBackend,
};
use prismcast_media::{
    AudioFilterBackend, AudioLevels, BackendComponent, BackendEvent, ComponentState,
    CompositorBackend, EncoderBackend, EncoderCapability, EncoderRegistry, FilterBackend,
    FilterDescriptor, FilterMedia, HardwareAccel, IngestEndpoint, OutputBackend,
    OutputCapabilities, OutputStats, ServiceProbe, SourceBackend, StreamingServiceBackend,
    VideoFilterBackend,
};

fn service() -> Service {
    Service {
        id: ServiceId::new(),
        name: "Twitch".to_string(),
        url: "rtmps://live.twitch.tv/app".to_string(),
        key: SecretString::new("live_test_key"),
        settings: serde_json::Value::Null,
    }
}

#[test]
fn traits_are_object_safe() {
    let source: Box<dyn SourceBackend> = Box::new(MockSourceBackend::new(
        SourceId::new(),
        SourceKind::TestPattern,
    ));
    let video_filter: Box<dyn VideoFilterBackend> =
        Box::new(MockVideoFilterBackend(MockFilterBackend::new(
            FilterId::new(),
            FilterDescriptor {
                kind: "chroma_key".to_string(),
                display_name: "Chroma Key".to_string(),
                media: FilterMedia::Video,
            },
        )));
    let audio_filter: Box<dyn AudioFilterBackend> =
        Box::new(MockAudioFilterBackend(MockFilterBackend::new(
            FilterId::new(),
            FilterDescriptor {
                kind: "compressor".to_string(),
                display_name: "Compressor".to_string(),
                media: FilterMedia::Audio,
            },
        )));
    let compositor: Box<dyn CompositorBackend> = Box::new(MockCompositorBackend::default());
    let encoder: Box<dyn EncoderBackend> = Box::new(MockEncoderBackend::new(EncoderSettings {
        id: EncoderId::new(),
        codec: "h264".to_string(),
        bitrate_kbps: 6_000,
        keyframe_interval: Some(120),
        settings: serde_json::Value::Null,
    }));
    let output: Box<dyn OutputBackend> = Box::new(MockOutputBackend::new(
        OutputId::new(),
        OutputKind::Recording,
    ));
    let service_backend: Box<dyn StreamingServiceBackend> =
        Box::new(MockStreamingServiceBackend::new(OutputKind::Rtmp));
    let registry: Box<dyn EncoderRegistry> = Box::new(MockEncoderRegistry::default());

    // Any live component can be handled through the common supertrait too.
    let components: Vec<&dyn BackendComponent> = vec![
        &*source,
        &*video_filter,
        &*audio_filter,
        &*compositor,
        &*encoder,
        &*output,
    ];
    assert!(components
        .iter()
        .all(|c| c.state() == ComponentState::Stopped));
    drop(service_backend);
    drop(registry);
}

#[test]
fn source_lifecycle_and_events() {
    let id = SourceId::new();
    let mut source: Box<dyn SourceBackend> = Box::new(
        MockSourceBackend::new(id, SourceKind::V4l2Camera).with_schema(
            serde_json::json!({"type": "object", "properties": {"device": {"type": "string"}}}),
        ),
    );

    assert_eq!(source.source_id(), id);
    assert_eq!(source.kind(), SourceKind::V4l2Camera);
    assert_eq!(source.state(), ComponentState::Stopped);
    assert!(source.settings_schema().is_object());

    source.start().unwrap();
    assert_eq!(source.state(), ComponentState::Running);
    assert_eq!(
        source.drain_events(),
        vec![BackendEvent::StateChanged {
            state: ComponentState::Running
        }]
    );
    // Draining is destructive.
    assert!(source.drain_events().is_empty());

    source
        .update_settings(serde_json::json!({"device": "/dev/video0"}))
        .unwrap();
    source.stop().unwrap();
    assert_eq!(source.state(), ComponentState::Stopped);
}

#[test]
fn source_failure_injection_and_levels() {
    let mut source = MockSourceBackend::new(SourceId::new(), SourceKind::PipeWireAudioInput)
        .with_levels(AudioLevels {
            peak_db: vec![-3.0, -3.5],
            rms_db: vec![-12.0, -13.0],
        });

    // Injected failure: the next fallible call fails once, state untouched.
    source
        .core
        .inject_failure(Error::Media("node gone".to_string()));
    let err = source.start().unwrap_err();
    assert_eq!(err.to_string(), "media error: node gone");
    assert_eq!(source.state(), ComponentState::Stopped);
    source.start().unwrap();

    // Async happenings are queued by the engine and drained by the actor.
    source.core.push_event(BackendEvent::DeviceLost {
        reason: "PipeWire restarted".to_string(),
    });
    source.core.set_state(ComponentState::Recovering);
    assert_eq!(source.state(), ComponentState::Recovering);
    assert_eq!(
        source.drain_events(),
        vec![
            BackendEvent::StateChanged {
                state: ComponentState::Running
            },
            BackendEvent::DeviceLost {
                reason: "PipeWire restarted".to_string()
            },
        ]
    );

    let levels = source.audio_levels().unwrap();
    assert_eq!(levels.peak_db.len(), 2);

    let silent = MockSourceBackend::new(SourceId::new(), SourceKind::Color);
    assert!(silent.audio_levels().is_none());
}

#[test]
fn filter_backends_record_updates() {
    let descriptor = FilterDescriptor {
        kind: "color_correction".to_string(),
        display_name: "Color Correction".to_string(),
        media: FilterMedia::Video,
    };
    let mut filter = MockVideoFilterBackend(MockFilterBackend::new(FilterId::new(), descriptor));

    assert_eq!(filter.descriptor().kind, "color_correction");
    assert_eq!(filter.descriptor().media, FilterMedia::Video);
    assert!(filter.0.enabled);

    filter
        .update_settings(serde_json::json!({"gamma": 0.5}))
        .unwrap();
    filter.set_enabled(false).unwrap();
    assert!(!filter.0.enabled);
    assert_eq!(filter.0.settings_updates.len(), 1);

    filter
        .0
        .core
        .inject_failure(Error::InvalidInput("bad gamma".to_string()));
    let err = filter
        .update_settings(serde_json::json!({"gamma": "high"}))
        .unwrap_err();
    assert!(matches!(err, Error::InvalidInput(_)));
}

#[test]
fn compositor_records_scene_operations() {
    let mut compositor = MockCompositorBackend::default();

    let canvas = CanvasId::new();
    compositor
        .configure_canvas(canvas, VideoConfig::hd_1080p60())
        .unwrap();
    assert_eq!(compositor.canvas_configs.len(), 1);
    assert_eq!(compositor.canvas_configs[0].1.width, 1920);

    let mut scene = Scene::new("Main");
    let item = SceneItem::new(SourceId::new(), 0);
    let item_id = item.id;
    scene.add_item(item.clone());
    compositor.sync_scene(&scene).unwrap();
    compositor.upsert_item(scene.id, &item).unwrap();
    compositor.remove_item(scene.id, item_id).unwrap();
    compositor.set_program_scene(scene.id).unwrap();
    compositor
        .start_transition(&Transition {
            kind: TransitionKind::Fade,
            duration_ms: 300,
            settings: serde_json::Value::Null,
        })
        .unwrap();

    assert_eq!(compositor.synced_scenes, vec![scene.id]);
    assert_eq!(compositor.upserted_items.len(), 1);
    assert_eq!(compositor.removed_items, vec![(scene.id, item_id)]);
    assert_eq!(compositor.program_scene, Some(scene.id));
    assert_eq!(compositor.transitions.len(), 1);

    // Compositor failures surface as events (e.g. GPU device lost).
    compositor.core.push_event(BackendEvent::Error {
        message: "GPU device disappeared".to_string(),
    });
    compositor.core.set_state(ComponentState::Failed);
    assert_eq!(compositor.state(), ComponentState::Failed);
    assert!(matches!(
        compositor.drain_events().as_slice(),
        [BackendEvent::Error { .. }]
    ));
}

#[test]
fn encoder_registry_probes_and_creates() {
    let registry = MockEncoderRegistry::with_capabilities(vec![
        EncoderCapability {
            codec: "h264".to_string(),
            display_name: "vah264enc".to_string(),
            hardware: HardwareAccel::VaApi,
            rate_controls: vec!["cbr".to_string(), "vbr".to_string()],
            supports_force_keyframe: true,
        },
        EncoderCapability {
            codec: "h264".to_string(),
            display_name: "x264enc".to_string(),
            hardware: HardwareAccel::Software,
            rate_controls: vec!["crf".to_string()],
            supports_force_keyframe: false,
        },
    ]);

    let probed = registry.probe();
    assert_eq!(probed.len(), 2);
    assert!(probed.iter().any(|c| c.hardware == HardwareAccel::VaApi));

    let settings = EncoderSettings {
        id: EncoderId::new(),
        codec: "h264".to_string(),
        bitrate_kbps: 6_000,
        keyframe_interval: Some(120),
        settings: serde_json::json!({"rate_control": "cbr"}),
    };
    let mut encoder = registry.create(settings.clone()).unwrap();
    assert_eq!(encoder.encoder_id(), settings.id);
    assert_eq!(encoder.settings(), &settings);
    assert_eq!(registry.created.lock().unwrap().len(), 1);

    encoder.force_keyframe().unwrap();
    let new_settings = EncoderSettings {
        bitrate_kbps: 4_500,
        ..settings.clone()
    };
    encoder.update_settings(&new_settings).unwrap();

    let missing = EncoderSettings {
        codec: "av1".to_string(),
        ..settings
    };
    let result = registry.create(missing);
    assert!(matches!(result, Err(Error::Media(_))));
}

#[test]
fn encoder_without_force_keyframe_support_rejects() {
    let mut encoder = MockEncoderBackend::new(EncoderSettings {
        id: EncoderId::new(),
        codec: "h264".to_string(),
        bitrate_kbps: 6_000,
        keyframe_interval: None,
        settings: serde_json::Value::Null,
    });
    encoder.force_keyframe_supported = false;
    let err = encoder.force_keyframe().unwrap_err();
    assert!(matches!(err, Error::InvalidInput(_)));
}

#[test]
fn output_lifecycle_capabilities_and_stats() {
    let id = OutputId::new();
    let mut output =
        MockOutputBackend::new(id, OutputKind::Recording).with_capabilities(OutputCapabilities {
            pause_resume: true,
            manual_split: true,
            statistics: true,
        });
    output.stats = OutputStats {
        active_duration_ms: 5_000,
        bytes_total: 3_750_000,
        frames_rendered: 300,
        frames_dropped: 0,
        current_bitrate_kbps: Some(6_000),
        extra: serde_json::Value::Null,
    };

    assert_eq!(output.output_id(), id);
    assert_eq!(output.kind(), OutputKind::Recording);
    assert!(output.capabilities().pause_resume);

    output.start().unwrap();
    assert_eq!(output.state(), ComponentState::Running);
    output.pause().unwrap();
    assert!(output.paused);
    output.resume().unwrap();
    assert!(!output.paused);
    output.split().unwrap();
    assert_eq!(output.splits, 1);

    let stats = output.statistics();
    assert_eq!(stats.frames_rendered, 300);
    assert_eq!(stats.current_bitrate_kbps, Some(6_000));

    output.stop().unwrap();
    assert_eq!(output.state(), ComponentState::Stopped);
    assert_eq!(
        output.drain_events(),
        vec![
            BackendEvent::StateChanged {
                state: ComponentState::Running
            },
            BackendEvent::StateChanged {
                state: ComponentState::Stopped
            },
        ]
    );
}

#[test]
fn output_rejects_unsupported_operations() {
    // RTMP streaming cannot pause or split.
    let mut output = MockOutputBackend::new(OutputId::new(), OutputKind::Rtmp);
    assert!(!output.capabilities().pause_resume);
    assert!(matches!(
        output.pause().unwrap_err(),
        Error::InvalidInput(_)
    ));
    assert!(matches!(
        output.split().unwrap_err(),
        Error::InvalidInput(_)
    ));

    // One broken output never affects another (PLAN §11): independent mocks.
    let mut other = MockOutputBackend::new(OutputId::new(), OutputKind::Srt);
    other.start().unwrap();
    output
        .core
        .inject_failure(Error::Media("connection reset".to_string()));
    assert!(output.start().is_err());
    assert_eq!(other.state(), ComponentState::Running);
}

#[test]
fn streaming_service_backend_validates_and_probes() {
    let mut backend = MockStreamingServiceBackend::new(OutputKind::Rtmp);
    backend.canned_endpoints = vec![IngestEndpoint {
        name: "EU: Frankfurt".to_string(),
        url: "rtmps://fra.contribute.live-video.net/app".to_string(),
        recommended: true,
    }];
    backend.canned_probe = ServiceProbe {
        reachable: true,
        auth_ok: Some(true),
        latency_ms: Some(24),
        bandwidth_kbps: None,
        details: "handshake ok".to_string(),
    };

    let service = service();
    assert_eq!(backend.protocol(), OutputKind::Rtmp);
    backend.validate(&service).unwrap();
    assert_eq!(backend.validated.lock().unwrap().as_slice(), &[service.id]);

    let endpoints = backend.endpoints(&service).unwrap();
    assert_eq!(endpoints.len(), 1);
    assert!(endpoints[0].recommended);

    let probe = backend.probe(&service).unwrap();
    assert!(probe.reachable);
    assert_eq!(probe.auth_ok, Some(true));

    backend.validate_error = Some(Error::InvalidInput("missing stream key".to_string()));
    let err = backend.validate(&service).unwrap_err();
    assert!(matches!(err, Error::InvalidInput(_)));
}
