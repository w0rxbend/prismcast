//! Bounded audio planning and finite ephemeral meter data (ADR-0023).
//!
//! This crate has no runtime or native media dependency. Track masks remain
//! output assignments; this foundation renders each named bus as stereo.
use prismcast_core::{audio::MonitorMode, AppState, AudioBusId, Error, Result, SourceId};
use std::collections::HashSet;

/// Maximum enabled diagnostic sources owned by the audio graph.
pub const MAX_AUDIO_SOURCES: usize = 32;
/// Maximum configured buses in this graph implementation.
pub const MAX_AUDIO_BUSES: usize = 8;
/// Finite JSON-safe silence floor, in dBFS.
pub const SILENCE_DBFS: f32 = -120.0;

/// Latest post-gain, post-mute source measurement; never persisted.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceMeter {
    pub source_id: SourceId,
    pub peak_dbfs: Vec<f32>,
    pub rms_dbfs: Vec<f32>,
}

/// Latest mixed-bus measurement, before any output encoding.
#[derive(Debug, Clone, PartialEq)]
pub struct BusMeter {
    pub bus_id: AudioBusId,
    pub peak_dbfs: Vec<f32>,
    pub rms_dbfs: Vec<f32>,
}

/// One active route and its independent bus solo gate.
#[derive(Debug, Clone, PartialEq)]
pub struct PlannedRoute {
    pub bus_id: AudioBusId,
    pub solo_muted: bool,
}

/// Validated source signal parameters.
#[derive(Debug, Clone, PartialEq)]
pub struct PlannedSource {
    pub source_id: SourceId,
    pub gain: f64,
    pub muted: bool,
    pub routes: Vec<PlannedRoute>,
}

/// A finite normalized graph plan, suitable for no-op reconciliation.
#[derive(Debug, Clone, PartialEq)]
pub struct MixerPlan {
    pub sources: Vec<PlannedSource>,
    pub buses: Vec<AudioBusId>,
}
impl MixerPlan {
    /// Plan explicit audio-producing sources. Source kind/settings validation
    /// belongs to the backend; this method checks domain processing semantics.
    pub fn from_state(state: &AppState, active: &[SourceId]) -> Result<Self> {
        if active.len() > MAX_AUDIO_SOURCES || state.audio.buses.len() > MAX_AUDIO_BUSES {
            return Err(Error::InvalidInput(
                "audio graph supports at most 32 sources and 8 buses".into(),
            ));
        }
        let active_set: HashSet<_> = active.iter().copied().collect();
        if active_set.len() != active.len() {
            return Err(Error::InvalidInput("duplicate active audio source".into()));
        }
        let buses: Vec<_> = state.audio.buses.iter().map(|bus| bus.id).collect();
        let bus_set: HashSet<_> = buses.iter().copied().collect();
        if buses.len() != bus_set.len() {
            return Err(Error::InvalidInput("duplicate audio bus".into()));
        }
        let mut route_keys = HashSet::new();
        for route in &state.audio.routes {
            if !bus_set.contains(&route.bus_id) || !state.sources.contains_key(&route.source_id) {
                return Err(Error::InvalidInput(
                    "audio route references an absent source or bus".into(),
                ));
            }
            if !route_keys.insert((route.source_id, route.bus_id)) {
                return Err(Error::InvalidInput("duplicate audio route".into()));
            }
        }
        let solo_buses: HashSet<_> = state
            .audio
            .routes
            .iter()
            .filter(|route| {
                active_set.contains(&route.source_id)
                    && !route.tracks.is_empty()
                    && state.audio.mixer_state(route.source_id).solo
            })
            .map(|route| route.bus_id)
            .collect();
        let mut sources = Vec::with_capacity(active.len());
        for source_id in active {
            let source = state
                .source(*source_id)
                .ok_or_else(|| Error::NotFound(format!("audio source {source_id}")))?;
            if !source.enabled {
                return Err(Error::InvalidInput(
                    "disabled source in active audio plan".into(),
                ));
            }
            if !source.filters.is_empty() {
                return Err(Error::InvalidInput(
                    "source filters are not implemented on audio test tones".into(),
                ));
            }
            let mixer = state.audio.mixer_state(*source_id);
            if mixer.balance != 0.0
                || mixer.monitor != MonitorMode::Off
                || mixer.sync_offset_ms != 0
            {
                return Err(Error::InvalidInput(
                    "audio balance, monitoring and sync delay are not implemented".into(),
                ));
            }
            let gain = 10.0_f64.powf(f64::from(mixer.volume_db) / 20.0);
            // Reserve headroom for all 32 same-phase sources on an F32 bus,
            // rather than only checking the f64 property or one source.
            let max_gain = f64::from(f32::MAX) / MAX_AUDIO_SOURCES as f64;
            if !mixer.volume_db.is_finite() || !gain.is_finite() || gain > max_gain {
                return Err(Error::InvalidInput(
                    "audio gain is not representable".into(),
                ));
            }
            let routes = state
                .audio
                .routes
                .iter()
                .filter(|route| route.source_id == *source_id && !route.tracks.is_empty())
                .map(|route| PlannedRoute {
                    bus_id: route.bus_id,
                    solo_muted: solo_buses.contains(&route.bus_id) && !mixer.solo,
                })
                .collect();
            sources.push(PlannedSource {
                source_id: *source_id,
                gain,
                muted: mixer.muted,
                routes,
            });
        }
        Ok(Self { sources, buses })
    }
}

/// Normalize a native dB value without producing NaN/infinity in controllers.
/// NaN and positive infinity indicate invalid observations and are rejected.
pub fn finite_dbfs(value: f64) -> Option<f32> {
    if value.is_nan() || value == f64::INFINITY || value > f64::from(f32::MAX) {
        None
    } else {
        Some(value.max(f64::from(SILENCE_DBFS)) as f32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use prismcast_core::{
        audio::{AudioMixerState, AudioRoute, TrackMask},
        Source, SourceKind,
    };
    #[test]
    fn solo_is_per_bus_and_explicit_mute_survives_solo() {
        let mut state = AppState::new();
        let a = Source::new(SourceKind::TestPattern, "a");
        let b = Source::new(SourceKind::TestPattern, "b");
        let ids = [a.id, b.id];
        state.sources.insert(a.id, a);
        state.sources.insert(b.id, b);
        let second = prismcast_core::audio::AudioBus::new("second");
        let first = state.audio.buses[0].id;
        state.audio.buses.push(second.clone());
        for (source_id, bus_id) in [(ids[0], first), (ids[1], first), (ids[1], second.id)] {
            state.audio.routes.push(AudioRoute {
                source_id,
                bus_id,
                tracks: TrackMask::ALL,
            });
        }
        state.audio.mixer.insert(
            ids[0],
            AudioMixerState {
                solo: true,
                muted: true,
                ..Default::default()
            },
        );
        let plan = MixerPlan::from_state(&state, &ids).unwrap();
        assert!(plan.sources[0].muted);
        assert!(plan.sources[1].routes[0].solo_muted);
        assert!(!plan.sources[1].routes[1].solo_muted);
    }
    #[test]
    fn gain_and_meter_values_are_representable() {
        assert_eq!(finite_dbfs(f64::NEG_INFINITY), Some(SILENCE_DBFS));
        assert_eq!(finite_dbfs(f64::NAN), None);
        assert_eq!(finite_dbfs(f64::INFINITY), None);
        let mut state = AppState::new();
        let source = Source::new(SourceKind::TestPattern, "tone");
        let id = source.id;
        state.sources.insert(id, source);
        state.audio.mixer.insert(
            id,
            AudioMixerState {
                volume_db: f32::MAX,
                ..Default::default()
            },
        );
        assert!(MixerPlan::from_state(&state, &[id]).is_err());
        state.audio.mixer.get_mut(&id).unwrap().volume_db = -6.0206;
        assert!(
            (MixerPlan::from_state(&state, &[id]).unwrap().sources[0].gain - 0.5).abs() < 0.00001
        );
    }
}
