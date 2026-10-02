use gstreamer::{self as gst, prelude::*};
use prismcast_capture::audio::{
    check_audio_remote, connect_authorized_audio_sources, disconnect_audio_remote,
    resolve_audio_target,
};
use prismcast_core::{
    AppState, CaptureGeneration, PipeWireAudioMode, PipeWireAudioSettings, Source, SourceId,
    SourceKind,
};
use prismcast_media_gst::GstAudioMixer;
use std::{
    thread,
    time::{Duration, Instant},
};
#[path = "common/pipewire.rs"]
mod pipewire;

#[test]
#[ignore = "requires installed PipeWire, WirePlumber policy profile, and GStreamer plugin; private synthetic daemon only"]
fn isolated_pipewire_audio() {
    pipewire::run_isolated("isolated_worker");
}
#[test]
#[ignore = "additional direct plugin closed-FD probe; upstream synchronous startup timeout approximately 30s"]
fn isolated_pipewire_audio_closed_fd_probe() {
    pipewire::run_isolated_closed_fd("isolated_worker");
}

fn source(state: &mut AppState, settings: PipeWireAudioSettings) -> SourceId {
    let kind = if settings.mode == PipeWireAudioMode::Application {
        SourceKind::PipeWireAppAudio
    } else {
        SourceKind::PipeWireAudioInput
    };
    let mut source = Source::new(kind, "fixture");
    source.settings = serde_json::to_value(settings).unwrap();
    let id = source.id;
    state.sources.insert(id, source);
    id
}
fn wait_meter(mixer: &mut GstAudioMixer, id: SourceId, expected: f32) -> f32 {
    let deadline = Instant::now() + Duration::from_secs(4);
    let mut last = None;
    loop {
        for meter in mixer.poll().unwrap() {
            if meter.source_id == id {
                let peak = meter.peak_dbfs[0];
                last = Some(peak);
                assert!(meter
                    .peak_dbfs
                    .iter()
                    .chain(&meter.rms_dbfs)
                    .all(|v| v.is_finite()));
                if (peak - expected).abs() < 0.25 {
                    return peak;
                }
            }
        }
        assert!(
            Instant::now() < deadline,
            "source {id} never reached {expected}; last={last:?}"
        );
        thread::sleep(Duration::from_millis(10));
    }
}
#[test]
#[ignore = "subprocess worker only; use isolated_pipewire_audio"]
fn isolated_worker() {
    let elapsed = Instant::now();
    let mut fixture = pipewire::Fixture::start();
    let mut state = AppState::new();
    let input = source(&mut state, pipewire::Fixture::source_settings());
    let output = source(&mut state, pipewire::Fixture::output_settings());
    let application = source(&mut state, pipewire::Fixture::application_settings());
    let mut mixer = GstAudioMixer::new().unwrap();
    mixer.reconcile(&state).unwrap();
    assert!(
        mixer.poll().unwrap().is_empty(),
        "persisted settings opened capture without authorization"
    );
    let grants = [input, output, application].map(|id| {
        resolve_audio_target(
            id,
            CaptureGeneration::new(1),
            &PipeWireAudioSettings::from_source(state.source(id).unwrap()).unwrap(),
        )
        .unwrap()
    });
    mixer.reconcile_authorized(&state, &grants).unwrap();
    eprintln!("three capture branches started at {:?}", elapsed.elapsed());
    wait_meter(&mut mixer, input, -12.0412);
    // The application tone is .25 amplitude. An unrelated .5 amplitude,997Hz
    // sentinel plays into the same private sink; monitoring that sink is louder.
    wait_meter(&mut mixer, application, -12.0412);
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if mixer
            .poll()
            .unwrap()
            .iter()
            .any(|m| m.source_id == output && m.peak_dbfs.iter().all(|v| *v > -4.0))
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "output monitor did not contain louder unrelated sentinel"
        );
        thread::sleep(Duration::from_millis(10));
    }
    state.audio.mixer.entry(application).or_default().volume_db = -6.0206;
    mixer.reconcile_authorized(&state, &grants).unwrap();
    wait_meter(&mut mixer, application, -18.0618);
    state.audio.mixer.get_mut(&application).unwrap().muted = true;
    mixer.reconcile_authorized(&state, &grants).unwrap();
    wait_meter(&mut mixer, application, -120.0);
    mixer.stop().unwrap();
    eprintln!(
        "isolation/gain/mute and shutdown complete at {:?}",
        elapsed.elapsed()
    );
    assert!(mixer.poll().unwrap().is_empty());
    // Target removal is terminal and grants cannot bind a same-name replacement.
    mixer
        .reconcile_authorized(&state, &[grants[2].clone()])
        .unwrap();
    wait_meter(&mut mixer, application, -120.0);
    fixture.stop_application();
    let deadline = Instant::now() + Duration::from_secs(4);
    while mixer.poll().is_ok() {
        assert!(
            Instant::now() < deadline,
            "removed application capture remained active"
        );
        thread::sleep(Duration::from_millis(10));
    }
    mixer.stop().unwrap();
    fixture.restart_application();
    fixture.wait_targets();
    assert!(mixer
        .reconcile_authorized(&state, &[grants[2].clone()])
        .is_err());
    eprintln!("same-name replacement rejected at {:?}", elapsed.elapsed());
    // Critical race seam: pin+verify sockets while the old daemon exists, then
    // restart before pipewiresrc connects its protocol stream. Old fd must not
    // fall back to the new server even when it recreates the same name/serial.
    let connected = connect_authorized_audio_sources(&[grants[0].clone()]).unwrap();
    fixture.restart_daemon();
    let (_, element, remote) = connected.into_iter().next().unwrap().into_parts();
    let old_serial = element.property::<String>("target-object");
    let pipeline = gst::Pipeline::new();
    let sink = gst::ElementFactory::make("fakesink")
        .property("sync", false)
        .property("async", false)
        .build()
        .unwrap();
    pipeline.add_many([&element, &sink]).unwrap();
    element.link(&sink).unwrap();
    assert!(
        check_audio_remote(&remote).is_err(),
        "old daemon socket was not detected as closed"
    );
    // Optional additional plugin proof exercises its upstream ~30s closed-FD
    // startup timeout; normal graph preflight rejects this known EOF promptly.
    if std::env::var_os("PRISMCAST_TEST_CLOSED_FD_GST").is_some() {
        let start = pipeline.set_state(gst::State::Playing);
        let failed = start.is_err()
            || pipeline
                .bus()
                .unwrap()
                .timed_pop_filtered(
                    gst::ClockTime::from_seconds(3),
                    &[gst::MessageType::Error, gst::MessageType::Eos],
                )
                .is_some();
        assert!(
            failed,
            "old daemon socket captured replacement instead of failing"
        );
    }
    disconnect_audio_remote(&remote);
    pipeline.set_state(gst::State::Null).unwrap();
    drop(remote);
    assert!(mixer
        .reconcile_authorized(&state, &[grants[0].clone()])
        .is_err());
    let fresh = resolve_audio_target(
        input,
        CaptureGeneration::new(2),
        &pipewire::Fixture::source_settings(),
    )
    .unwrap();
    let fresh_connection = connect_authorized_audio_sources(std::slice::from_ref(&fresh))
        .unwrap()
        .pop()
        .unwrap();
    let (_, fresh_element, fresh_remote) = fresh_connection.into_parts();
    assert_eq!(
        fresh_element.property::<String>("target-object"),
        old_serial,
        "fixture must exercise serial reuse across daemon restart"
    );
    disconnect_audio_remote(&fresh_remote);
    mixer.reconcile_authorized(&state, &[fresh]).unwrap();
    wait_meter(&mut mixer, input, -12.0412);
    mixer.stop().unwrap();
}
