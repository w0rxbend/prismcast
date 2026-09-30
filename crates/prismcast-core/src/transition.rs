//! Scene transitions (PLAN.md §17).
//!
//! A [`Transition`] describes how the program output moves between scenes.
//! Per-scene overrides, transition matrices, and quick transitions are later
//! work; the domain model keeps `settings` open-ended for them.

use serde::{Deserialize, Serialize};

/// A scene transition configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Transition {
    /// Transition effect.
    pub kind: TransitionKind,
    /// Duration in milliseconds (ignored by `Cut`).
    pub duration_ms: u32,
    /// Kind-specific settings (direction, stinger media path, ...).
    pub settings: serde_json::Value,
}

impl Transition {
    /// The default transition: a 300 ms fade (PLAN.md §78 MVP default).
    pub fn default_fade() -> Self {
        Self {
            kind: TransitionKind::Fade,
            duration_ms: 300,
            settings: serde_json::Value::Null,
        }
    }
}

impl Default for Transition {
    fn default() -> Self {
        Self::default_fade()
    }
}

/// Transition effect kinds (PLAN.md §17 MVP list).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransitionKind {
    /// Instant switch.
    Cut,
    /// Crossfade.
    Fade,
    /// New scene swipes the old one away.
    Swipe,
    /// Both scenes slide.
    Slide,
    /// Video overlay with a cut point (stinger).
    Stinger,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transition_serde_roundtrip() {
        let transition = Transition {
            kind: TransitionKind::Stinger,
            duration_ms: 1_500,
            settings: serde_json::json!({"path": "/stingers/wipe.webm", "cut_ms": 700}),
        };
        let json = serde_json::to_string(&transition).unwrap();
        let back: Transition = serde_json::from_str(&json).unwrap();
        assert_eq!(transition, back);
    }

    #[test]
    fn transition_kind_serde_roundtrip_all_variants() {
        for kind in [
            TransitionKind::Cut,
            TransitionKind::Fade,
            TransitionKind::Swipe,
            TransitionKind::Slide,
            TransitionKind::Stinger,
        ] {
            let json = serde_json::to_string(&kind).unwrap();
            assert_eq!(kind, serde_json::from_str(&json).unwrap());
        }
    }

    #[test]
    fn default_is_fade_300ms() {
        let t = Transition::default();
        assert_eq!(t.kind, TransitionKind::Fade);
        assert_eq!(t.duration_ms, 300);
    }
}
