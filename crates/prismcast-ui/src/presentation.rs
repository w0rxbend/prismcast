//! Pure presentation helpers: map domain state onto labels and styles.
//!
//! Everything here is free of GTK types so it stays unit-testable without a
//! display (PLAN.md §75: presentation state only, no media logic).

use prismcast_core::output::OutputState;
use prismcast_core::source::SourceKind;
use prismcast_core::transition::{Transition, TransitionKind};

/// Aggregate stream status shown in the header bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamStatus {
    /// No output is running or starting.
    Offline,
    /// At least one output is in a transitional state (and none is live).
    Transitioning,
    /// At least one output is running (possibly degraded).
    Live,
}

impl StreamStatus {
    /// Derives the status from the lifecycle states of all configured outputs.
    pub fn from_states<'a>(states: impl IntoIterator<Item = &'a OutputState>) -> Self {
        let mut transitioning = false;
        for state in states {
            match state {
                OutputState::Running | OutputState::Degraded => return Self::Live,
                OutputState::Starting
                | OutputState::Reconnecting { .. }
                | OutputState::Stopping => {
                    transitioning = true;
                }
                OutputState::Stopped | OutputState::Failed => {}
            }
        }
        if transitioning {
            Self::Transitioning
        } else {
            Self::Offline
        }
    }

    /// User-facing label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Offline => "Offline",
            Self::Transitioning => "Working…",
            Self::Live => "LIVE",
        }
    }

    /// libadwaita accent style class for the status label.
    pub fn css_class(self) -> &'static str {
        match self {
            Self::Offline => "dim-label",
            Self::Transitioning => "warning",
            Self::Live => "error",
        }
    }
}

/// Transition kinds offered by the transition selector, in dropdown order.
pub const TRANSITION_CHOICES: [TransitionKind; 4] = [
    TransitionKind::Cut,
    TransitionKind::Fade,
    TransitionKind::Swipe,
    TransitionKind::Slide,
];

/// Maps a dropdown index to a transition configuration (`None` when out of
/// range). Non-cut transitions use the 300 ms MVP default (PLAN.md §78).
pub fn transition_for_choice(index: u32) -> Option<Transition> {
    let kind = TRANSITION_CHOICES.get(index as usize).copied()?;
    let duration_ms = if kind == TransitionKind::Cut { 0 } else { 300 };
    Some(Transition {
        kind,
        duration_ms,
        settings: serde_json::Value::Null,
    })
}

/// Maps a transition kind back to a dropdown index (`None` for kinds not
/// offered by the selector, e.g. stinger).
pub fn choice_for_transition(kind: TransitionKind) -> Option<u32> {
    TRANSITION_CHOICES
        .iter()
        .position(|choice| *choice == kind)
        .map(|position| position as u32)
}

/// Short user-facing label for a source kind (source list subtitles and the
/// kind picker).
pub fn source_kind_label(kind: &SourceKind) -> &'static str {
    match kind {
        SourceKind::PipeWireDisplay => "Display Capture",
        SourceKind::PipeWireWindow => "Window Capture",
        SourceKind::V4l2Camera => "Camera",
        SourceKind::PipeWireAudioInput => "Audio Input",
        SourceKind::PipeWireAppAudio => "Application Audio",
        SourceKind::MediaFile => "Media File",
        SourceKind::Image => "Image",
        SourceKind::ImageSlideshow => "Image Slideshow",
        SourceKind::Color => "Solid Color",
        SourceKind::Text => "Text",
        SourceKind::Browser => "Browser",
        SourceKind::Scene(_) => "Scene",
        SourceKind::TestPattern => "Test Pattern",
        SourceKind::NetworkStream => "Network Stream",
    }
}

/// Runtime permission/capture state; absence never triggers a permission picker.
pub fn capture_status_label(runtime: Option<&prismcast_core::SourceRuntime>) -> &'static str {
    use prismcast_core::CaptureStatus;
    match runtime.map(|runtime| runtime.status) {
        None => "Authorization required",
        Some(CaptureStatus::Authorizing) => "Choose a monitor or window…",
        Some(CaptureStatus::Active) => "Capturing",
        Some(CaptureStatus::Cancelled) => "Selection cancelled",
        Some(CaptureStatus::Denied) => "Permission denied",
        Some(CaptureStatus::Revoked) => "Capture permission revoked",
        Some(CaptureStatus::Failed) => "Capture unavailable",
    }
}

/// User-facing label for an output's lifecycle state.
pub fn output_state_label(state: &OutputState) -> String {
    match state {
        OutputState::Stopped => "Stopped".to_string(),
        OutputState::Starting => "Starting…".to_string(),
        OutputState::Running => "Running".to_string(),
        OutputState::Reconnecting { attempt } => format!("Reconnecting (attempt {attempt})"),
        OutputState::Degraded => "Degraded".to_string(),
        OutputState::Failed => "Failed".to_string(),
        OutputState::Stopping => "Stopping…".to_string(),
    }
}

/// Whether an output can legally be started (`Stopped`/`Failed` only, per
/// `prismcast_core::state`).
pub fn output_can_start(state: &OutputState) -> bool {
    matches!(state, OutputState::Stopped | OutputState::Failed)
}

/// Whether an output can legally be stopped.
pub fn output_can_stop(state: &OutputState) -> bool {
    matches!(
        state,
        OutputState::Starting
            | OutputState::Running
            | OutputState::Degraded
            | OutputState::Reconnecting { .. }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn capture_statuses_show_explicit_authorization_and_recovery() {
        use prismcast_core::{CaptureGeneration, CaptureStatus, SourceRuntime};
        assert_eq!(capture_status_label(None), "Authorization required");
        for (status, expected) in [
            (CaptureStatus::Authorizing, "Choose a monitor or window…"),
            (CaptureStatus::Active, "Capturing"),
            (CaptureStatus::Cancelled, "Selection cancelled"),
            (CaptureStatus::Denied, "Permission denied"),
            (CaptureStatus::Revoked, "Capture permission revoked"),
            (CaptureStatus::Failed, "Capture unavailable"),
        ] {
            let runtime = SourceRuntime {
                generation: CaptureGeneration::new(1),
                status,
                dimensions: None,
                message: None,
            };
            assert_eq!(capture_status_label(Some(&runtime)), expected);
        }
    }

    #[test]
    fn stream_status_prefers_live_over_transitioning() {
        let states = [OutputState::Starting, OutputState::Running];
        assert_eq!(StreamStatus::from_states(&states), StreamStatus::Live);
    }

    #[test]
    fn stream_status_transitioning_and_offline() {
        let states = [
            OutputState::Stopped,
            OutputState::Reconnecting { attempt: 3 },
        ];
        assert_eq!(
            StreamStatus::from_states(&states),
            StreamStatus::Transitioning
        );
        let stopped = [OutputState::Stopped, OutputState::Failed];
        assert_eq!(StreamStatus::from_states(&stopped), StreamStatus::Offline);
        assert_eq!(StreamStatus::from_states(&[]), StreamStatus::Offline);
    }

    #[test]
    fn transition_choice_roundtrip() {
        for (index, kind) in TRANSITION_CHOICES.iter().enumerate() {
            let transition = transition_for_choice(index as u32).expect("in range");
            assert_eq!(transition.kind, *kind);
            assert_eq!(choice_for_transition(*kind), Some(index as u32));
        }
        assert!(transition_for_choice(TRANSITION_CHOICES.len() as u32).is_none());
        assert_eq!(choice_for_transition(TransitionKind::Stinger), None);
        assert_eq!(
            transition_for_choice(0).expect("cut").duration_ms,
            0,
            "cut is instant"
        );
    }

    #[test]
    fn output_start_stop_legality_matches_state_machine_docs() {
        assert!(output_can_start(&OutputState::Stopped));
        assert!(output_can_start(&OutputState::Failed));
        assert!(!output_can_start(&OutputState::Running));
        assert!(output_can_stop(&OutputState::Running));
        assert!(output_can_stop(&OutputState::Reconnecting { attempt: 1 }));
        assert!(!output_can_stop(&OutputState::Stopped));
        assert!(!output_can_stop(&OutputState::Stopping));
    }

    #[test]
    fn every_source_kind_has_a_label() {
        let kinds = [
            SourceKind::PipeWireDisplay,
            SourceKind::PipeWireWindow,
            SourceKind::V4l2Camera,
            SourceKind::PipeWireAudioInput,
            SourceKind::PipeWireAppAudio,
            SourceKind::MediaFile,
            SourceKind::Image,
            SourceKind::ImageSlideshow,
            SourceKind::Color,
            SourceKind::Text,
            SourceKind::Browser,
            SourceKind::Scene(prismcast_core::id::SceneId::new()),
            SourceKind::TestPattern,
            SourceKind::NetworkStream,
        ];
        for kind in &kinds {
            assert!(!source_kind_label(kind).is_empty());
        }
    }
}
