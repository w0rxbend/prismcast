//! Audio domain: buses, routes, and per-source mixer state (PLAN.md §9).
//!
//! ## Bus model decision (RES-002)
//!
//! OBS fixes audio at **6 global mixes** (`MAX_AUDIO_MIXES`) and 8 channels;
//! RES-002 (`docs/research/obs-architecture.md` §11, "Redesign" point 3)
//! explicitly rejects copying that. Prismcast instead uses an **unbounded set
//! of named [`AudioBus`]es**. Sources are routed to buses with [`AudioRoute`]s;
//! each route carries a [`TrackMask`] (bitset over `u32`) describing which of
//! the bus's output tracks the source feeds. Tracks are a muxer-level mapping
//! concept, not a global mix bitmask, so recording can carry many audio tracks
//! (PLAN §13: "not artificially limited to six") and each streaming output can
//! pick its own track set.
//!
//! Per-source mixing parameters (volume, mute, solo, monitoring, balance, sync
//! offset) live in [`AudioMixerState`], keyed by `SourceId` in
//! [`AudioMixerConfig`].

use serde::{Deserialize, Serialize};

use crate::id::{AudioBusId, SourceId};

/// Default master bus name.
pub const MASTER_BUS_NAME: &str = "Master";

/// A named mix bus. Buses are created on demand; there is no fixed count.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioBus {
    /// Unique bus ID.
    pub id: AudioBusId,
    /// User-facing name.
    pub name: String,
}

impl AudioBus {
    /// Creates a bus with a fresh ID.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            id: AudioBusId::new(),
            name: name.into(),
        }
    }
}

/// Bitset over a `u32` selecting which output tracks of a bus a route feeds.
///
/// Bit `n` set = source contributes to track `n` (0-based). Track indices
/// beyond 31 are not representable; practical multitrack recording stays far
/// below that.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TrackMask(u32);

impl TrackMask {
    /// Mask with no tracks selected.
    pub const NONE: Self = Self(0);
    /// Mask with all 32 tracks selected.
    pub const ALL: Self = Self(u32::MAX);

    /// Builds a mask from a track index (0-based). Indices ≥ 32 yield
    /// [`TrackMask::NONE`].
    pub fn from_track(track: u32) -> Self {
        match track {
            t if t < 32 => Self(1 << t),
            _ => Self::NONE,
        }
    }

    /// Stereo default: tracks 0 and 1.
    pub fn stereo_pair() -> Self {
        Self(0b11)
    }

    /// Returns the raw bitmask.
    pub fn bits(self) -> u32 {
        self.0
    }

    /// Returns whether the given track index is selected.
    pub fn contains(self, track: u32) -> bool {
        track < 32 && (self.0 & (1 << track)) != 0
    }

    /// Returns a mask with the given track additionally selected.
    pub fn with(self, track: u32) -> Self {
        if track < 32 {
            Self(self.0 | (1 << track))
        } else {
            self
        }
    }

    /// Returns a mask with the given track deselected.
    pub fn without(self, track: u32) -> Self {
        if track < 32 {
            Self(self.0 & !(1 << track))
        } else {
            self
        }
    }

    /// Returns the union of two masks.
    pub fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Returns whether no tracks are selected.
    pub fn is_empty(self) -> bool {
        self.0 == 0
    }
}

impl Default for TrackMask {
    /// Defaults to the stereo pair (tracks 0+1), the common case.
    fn default() -> Self {
        Self::stereo_pair()
    }
}

/// Routes one source's audio into one bus on a set of output tracks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioRoute {
    /// The audio-producing source.
    pub source_id: SourceId,
    /// The destination bus.
    pub bus_id: AudioBusId,
    /// Output tracks of the bus this route feeds.
    pub tracks: TrackMask,
}

/// Monitoring destination for a source (PLAN.md §9).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MonitorMode {
    /// No monitoring.
    #[default]
    Off,
    /// Monitor only; the source does not reach output buses.
    MonitorOnly,
    /// Monitor and send to output buses.
    MonitorAndOutput,
}

/// Per-source mixer parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioMixerState {
    /// Gain in decibels (`0.0` = unity).
    pub volume_db: f32,
    /// Muted sources contribute silence to their buses.
    pub muted: bool,
    /// Soloed sources mute all non-soloed sources on the same buses.
    pub solo: bool,
    /// Monitoring destination.
    pub monitor: MonitorMode,
    /// Stereo balance in `[-1.0, 1.0]` (`0.0` = centered).
    pub balance: f32,
    /// Sync offset in milliseconds; positive delays the audio.
    pub sync_offset_ms: i32,
}

impl Default for AudioMixerState {
    fn default() -> Self {
        Self {
            volume_db: 0.0,
            muted: false,
            solo: false,
            monitor: MonitorMode::default(),
            balance: 0.0,
            sync_offset_ms: 0,
        }
    }
}

/// The audio configuration of a scene collection: buses, routes, and mixer
/// state. Persisted as part of `SceneCollection` (PLAN.md §19).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioMixerConfig {
    /// Named mix buses (unbounded; at least the master bus by convention).
    pub buses: Vec<AudioBus>,
    /// Source → bus routing with track assignment.
    pub routes: Vec<AudioRoute>,
    /// Mixer parameters per source. Sources without an entry use defaults.
    pub mixer: indexmap::IndexMap<SourceId, AudioMixerState>,
}

impl AudioMixerConfig {
    /// Creates a configuration with a single master bus and no routes.
    pub fn with_master_bus() -> Self {
        Self {
            buses: vec![AudioBus::new(MASTER_BUS_NAME)],
            routes: Vec::new(),
            mixer: indexmap::IndexMap::new(),
        }
    }

    /// Returns the mixer state for a source, or defaults if unset.
    pub fn mixer_state(&self, source_id: SourceId) -> AudioMixerState {
        self.mixer.get(&source_id).cloned().unwrap_or_default()
    }

    /// Returns the routes feeding a given bus.
    pub fn routes_for_bus(&self, bus_id: AudioBusId) -> impl Iterator<Item = &AudioRoute> {
        self.routes.iter().filter(move |r| r.bus_id == bus_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn track_mask_bit_ops() {
        let mut mask = TrackMask::from_track(0);
        assert!(mask.contains(0));
        assert!(!mask.contains(1));
        mask = mask.with(3).with(31);
        assert!(mask.contains(3));
        assert!(mask.contains(31));
        mask = mask.without(0);
        assert!(!mask.contains(0));
        assert!(!TrackMask::from_track(32).contains(0));
        assert_eq!(TrackMask::from_track(32), TrackMask::NONE);
        assert!(TrackMask::NONE.is_empty());
        assert!(!TrackMask::default().is_empty());
        assert_eq!(TrackMask::stereo_pair().bits(), 0b11);
        assert_eq!(
            TrackMask::from_track(2)
                .union(TrackMask::from_track(0))
                .bits(),
            0b101
        );
    }

    #[test]
    fn audio_types_serde_roundtrip() {
        let bus = AudioBus::new("Stream Mix");
        let json = serde_json::to_string(&bus).unwrap();
        assert_eq!(bus, serde_json::from_str::<AudioBus>(&json).unwrap());

        let route = AudioRoute {
            source_id: SourceId::new(),
            bus_id: bus.id,
            tracks: TrackMask::from_track(0).with(2),
        };
        let json = serde_json::to_string(&route).unwrap();
        assert_eq!(route, serde_json::from_str::<AudioRoute>(&json).unwrap());

        let mixer = AudioMixerState {
            volume_db: -6.5,
            muted: true,
            solo: false,
            monitor: MonitorMode::MonitorAndOutput,
            balance: 0.25,
            sync_offset_ms: 120,
        };
        let json = serde_json::to_string(&mixer).unwrap();
        assert_eq!(
            mixer,
            serde_json::from_str::<AudioMixerState>(&json).unwrap()
        );

        let json = serde_json::to_string(&route.tracks).unwrap();
        assert_eq!(
            route.tracks,
            serde_json::from_str::<TrackMask>(&json).unwrap()
        );

        for mode in [
            MonitorMode::Off,
            MonitorMode::MonitorOnly,
            MonitorMode::MonitorAndOutput,
        ] {
            let json = serde_json::to_string(&mode).unwrap();
            assert_eq!(mode, serde_json::from_str(&json).unwrap());
        }
    }

    #[test]
    fn mixer_config_master_bus_and_defaults() {
        let config = AudioMixerConfig::with_master_bus();
        assert_eq!(config.buses.len(), 1);
        assert_eq!(config.buses[0].name, MASTER_BUS_NAME);
        let unknown = SourceId::new();
        assert_eq!(config.mixer_state(unknown), AudioMixerState::default());

        let json = serde_json::to_string(&config).unwrap();
        let back: AudioMixerConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(config, back);
    }

    #[test]
    fn routes_for_bus_filters() {
        let mut config = AudioMixerConfig::with_master_bus();
        let other = AudioBus::new("VOD");
        let src = SourceId::new();
        config.routes.push(AudioRoute {
            source_id: src,
            bus_id: config.buses[0].id,
            tracks: TrackMask::default(),
        });
        config.routes.push(AudioRoute {
            source_id: src,
            bus_id: other.id,
            tracks: TrackMask::ALL,
        });
        assert_eq!(config.routes_for_bus(config.buses[0].id).count(), 1);
        assert_eq!(config.routes_for_bus(other.id).count(), 1);
        assert_eq!(config.routes_for_bus(AudioBusId::new()).count(), 0);
    }
}
