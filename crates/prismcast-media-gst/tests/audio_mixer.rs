use prismcast_audio::{BusMeter, SourceMeter, SILENCE_DBFS};
use prismcast_core::{
    audio::{AudioBus, AudioMixerState, AudioRoute, TrackMask},
    AppState, AudioBusId, Error, Source, SourceId, SourceKind,
};
use prismcast_media_gst::GstAudioMixer;
use std::{
    collections::HashMap,
    thread,
    time::{Duration, Instant},
};

fn tone(state: &mut AppState, name: &str, route: Option<AudioBusId>) -> SourceId {
    let mut source = Source::new(SourceKind::TestPattern, name);
    source.settings = serde_json::json!({"audio_test":true});
    let id = source.id;
    state.sources.insert(id, source);
    if let Some(bus_id) = route {
        state.audio.routes.push(AudioRoute {
            source_id: id,
            bus_id,
            tracks: TrackMask::ALL,
        });
    }
    id
}
fn gain(state: &mut AppState, id: SourceId, volume_db: f32) {
    state.audio.mixer.entry(id).or_default().volume_db = volume_db;
}
fn wait_source(mixer: &mut GstAudioMixer, id: SourceId, expected: f32) -> SourceMeter {
    let deadline = Instant::now() + Duration::from_secs(4);
    let mut last = None;
    while Instant::now() < deadline {
        for value in mixer.poll().unwrap() {
            if value.source_id == id {
                assert_eq!(value.peak_dbfs.len(), 2);
                assert_eq!(value.rms_dbfs.len(), 2);
                assert!(value
                    .peak_dbfs
                    .iter()
                    .chain(&value.rms_dbfs)
                    .all(|v| v.is_finite()));
                if value.peak_dbfs.iter().all(|v| (*v - expected).abs() < 0.2) {
                    return value;
                }
                last = Some(value);
            }
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("source {id} never reached {expected} dBFS: {last:?}");
}
fn wait_bus(mixer: &mut GstAudioMixer, id: AudioBusId, expected: f32) -> BusMeter {
    let deadline = Instant::now() + Duration::from_secs(4);
    let mut last = None;
    while Instant::now() < deadline {
        mixer.poll().unwrap();
        for value in mixer.take_bus_meters() {
            if value.bus_id == id {
                if value.peak_dbfs.iter().all(|v| (*v - expected).abs() < 0.3) {
                    return value;
                }
                last = Some(value);
            }
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("bus {id} never reached {expected} dBFS: {last:?}");
}

#[test]
fn real_stereo_sine_gain_mute_and_repeated_shutdown() {
    let mut state = AppState::new();
    let bus = state.audio.buses[0].id;
    let id = tone(&mut state, "tone", Some(bus));
    let mut mixer = GstAudioMixer::new().unwrap();
    mixer.reconcile(&state).unwrap();
    let value = wait_source(&mut mixer, id, -6.0206);
    assert!(value.rms_dbfs.iter().all(|v| (*v + 9.0309).abs() < 0.2));
    gain(&mut state, id, -6.0206);
    mixer.reconcile(&state).unwrap();
    wait_source(&mut mixer, id, -12.0412);
    wait_bus(&mut mixer, bus, -12.0412);
    state.audio.mixer.get_mut(&id).unwrap().muted = true;
    mixer.reconcile(&state).unwrap();
    wait_source(&mut mixer, id, SILENCE_DBFS);
    wait_bus(&mut mixer, bus, SILENCE_DBFS);
    for _ in 0..3 {
        mixer.stop().unwrap();
        assert!(mixer.poll().unwrap().is_empty());
        mixer.reconcile(&state).unwrap();
        wait_source(&mut mixer, id, SILENCE_DBFS);
    }
    mixer.stop().unwrap();
    mixer.stop().unwrap();
}

#[test]
fn real_solo_is_independent_for_each_bus_and_explicit_mute_wins() {
    let mut state = AppState::new();
    let bus = state.audio.buses[0].id;
    let other = AudioBus::new("other");
    state.audio.buses.push(other.clone());
    let a = tone(&mut state, "a", Some(bus));
    let b = tone(&mut state, "b", Some(bus));
    state.audio.routes.push(AudioRoute {
        source_id: b,
        bus_id: other.id,
        tracks: TrackMask::ALL,
    });
    gain(&mut state, a, -18.0618); // amplitude .0625 -> -24.0824 dBFS
    state.audio.mixer.get_mut(&a).unwrap().solo = true;
    let mut mixer = GstAudioMixer::new().unwrap();
    mixer.reconcile(&state).unwrap();
    wait_bus(&mut mixer, bus, -24.0824);
    wait_bus(&mut mixer, other.id, -6.0206);
    // Source meter is pre-bus-solo and remains audible on the other bus.
    wait_source(&mut mixer, b, -6.0206);
    state.audio.mixer.get_mut(&a).unwrap().muted = true;
    mixer.reconcile(&state).unwrap();
    wait_bus(&mut mixer, bus, SILENCE_DBFS);
    wait_bus(&mut mixer, other.id, -6.0206);
}

#[test]
fn unrouted_source_is_metered_without_creating_output_mix() {
    let mut state = AppState::new();
    let id = tone(&mut state, "unrouted", None);
    let mut mixer = GstAudioMixer::new().unwrap();
    mixer.reconcile(&state).unwrap();
    wait_source(&mut mixer, id, -6.0206);
    assert!(mixer.take_bus_meters().is_empty());
    state.audio.routes.push(AudioRoute {
        source_id: id,
        bus_id: state.audio.buses[0].id,
        tracks: TrackMask::NONE,
    });
    mixer.reconcile(&state).unwrap();
    wait_source(&mut mixer, id, -6.0206);
    assert!(mixer.take_bus_meters().is_empty());
}

#[test]
fn thirty_two_sources_slow_consumer_removal_and_disable_stay_bounded() {
    let mut state = AppState::new();
    let bus = state.audio.buses[0].id;
    let ids: Vec<_> = (0..32)
        .map(|i| tone(&mut state, &format!("tone {i}"), Some(bus)))
        .collect();
    let mut mixer = GstAudioMixer::new().unwrap();
    mixer.reconcile(&state).unwrap();
    thread::sleep(Duration::from_millis(250)); // consumer intentionally absent
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut observed = HashMap::new();
    while observed.len() < 32 && Instant::now() < deadline {
        let values = mixer.poll().unwrap();
        assert!(values.len() <= 32);
        for value in values {
            assert_eq!(value.peak_dbfs.len(), 2);
            observed.insert(value.source_id, value);
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(observed.len(), 32);
    for value in observed.values() {
        assert!(value.peak_dbfs.iter().all(|v| (*v + 6.0206).abs() < 0.2));
    }
    state.audio.routes.retain(|route| route.source_id != ids[0]);
    state.sources.shift_remove(&ids[0]);
    state.sources.get_mut(&ids[1]).unwrap().enabled = false;
    mixer.reconcile(&state).unwrap();
    wait_source(&mut mixer, ids[2], -6.0206);
    for _ in 0..10 {
        let values = mixer.poll().unwrap();
        assert!(values.len() <= 30);
        assert!(values
            .iter()
            .all(|value| ![ids[0], ids[1]].contains(&value.source_id)));
        thread::sleep(Duration::from_millis(10));
    }
    mixer.stop().unwrap();
    assert!(mixer.poll().unwrap().is_empty());
}

#[test]
fn invalid_gain_processing_and_runtime_budgets_are_typed_errors() {
    let mut state = AppState::new();
    let id = tone(&mut state, "tone", None);
    let mut mixer = GstAudioMixer::new().unwrap();
    for bad in [
        AudioMixerState {
            volume_db: f32::MAX,
            ..Default::default()
        },
        AudioMixerState {
            // Finite as an f64 gain, but overflows the negotiated F32 audio.
            volume_db: 800.0,
            ..Default::default()
        },
        AudioMixerState {
            // One source is representable, but a coherent 32-source sum is not.
            volume_db: 760.0,
            ..Default::default()
        },
        AudioMixerState {
            balance: 0.5,
            ..Default::default()
        },
        AudioMixerState {
            sync_offset_ms: 1,
            ..Default::default()
        },
        AudioMixerState {
            monitor: prismcast_core::audio::MonitorMode::MonitorOnly,
            ..Default::default()
        },
    ] {
        state.audio.mixer.insert(id, bad);
        assert!(matches!(
            mixer.reconcile(&state),
            Err(Error::InvalidInput(_))
        ));
    }
    state.audio.mixer.clear();
    state
        .sources
        .get_mut(&id)
        .unwrap()
        .filters
        .push(prismcast_core::FilterId::new());
    assert!(matches!(
        mixer.reconcile(&state),
        Err(Error::InvalidInput(_))
    ));
    state.sources.get_mut(&id).unwrap().filters.clear();
    for i in 1..33 {
        tone(&mut state, &format!("extra {i}"), None);
    }
    assert!(matches!(
        mixer.reconcile(&state),
        Err(Error::InvalidInput(_))
    ));
    state.sources.clear();
    for i in 1..9 {
        state.audio.buses.push(AudioBus::new(format!("bus {i}")));
    }
    assert!(matches!(
        mixer.reconcile(&state),
        Err(Error::InvalidInput(_))
    ));
}

#[test]
fn opt_in_is_explicit_and_settings_parser_agrees_with_video() {
    let mut state = AppState::new();
    let mut source = Source::new(SourceKind::TestPattern, "silent video");
    source.settings = serde_json::json!({});
    state.sources.insert(source.id, source);
    let mut mixer = GstAudioMixer::new().unwrap();
    mixer.reconcile(&state).unwrap();
    assert!(mixer.poll().unwrap().is_empty());
    assert!(
        !prismcast_media_gst::TestPatternSettings::from_json(serde_json::json!({}))
            .unwrap()
            .audio_test
    );
    assert!(
        prismcast_media_gst::TestPatternSettings::from_json(serde_json::json!({"audio_test":true}))
            .unwrap()
            .audio_test
    );
    state.sources.values_mut().next().unwrap().settings = serde_json::json!({"audio_test":"yes"});
    assert!(matches!(
        mixer.reconcile(&state),
        Err(Error::InvalidInput(_))
    ));
}
