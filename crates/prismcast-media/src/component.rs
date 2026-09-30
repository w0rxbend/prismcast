//! Shared backend lifecycle: component state and events (PLAN.md §61).
//!
//! Every backend instance (source, filter, encoder, output) reports a
//! [`ComponentState`] and produces [`BackendEvent`]s that the media control
//! actor drains and translates into core [`prismcast_core::Event`]s. The
//! mapping is intentionally not 1:1 with domain states — e.g. the actor turns
//! `Recovering` + reconnect policy bookkeeping into
//! [`prismcast_core::OutputState::Reconnecting`].

use serde::{Deserialize, Serialize};

/// Runtime health of a backend component (PLAN.md §61 failure model).
///
/// Unlike the persisted domain states (e.g. [`prismcast_core::OutputState`]),
/// this is the *observed* state of a live media component; it is never
/// persisted and never crosses the wire.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComponentState {
    /// Not producing media.
    #[default]
    Stopped,
    /// Producing media as configured.
    Running,
    /// Producing, but with problems (dropped frames, clock drift, ...).
    Degraded,
    /// A failure was detected and the backend is attempting recovery
    /// (device re-probe, reconnect, renegotiation).
    Recovering,
    /// Unrecoverable failure; a new start is required.
    Failed,
}

/// An asynchronous occurrence reported by a backend component.
///
/// Backends never push into core channels directly (that would couple them to
/// a runtime); instead the media control actor polls [`BackendComponent::
/// drain_events`](crate::BackendComponent::drain_events) on its own thread.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum BackendEvent {
    /// The component's [`ComponentState`] changed.
    StateChanged {
        /// New state.
        state: ComponentState,
    },
    /// A non-fatal problem occurred (the component may be `Degraded`).
    Warning {
        /// Human-readable description; never contains secrets.
        message: String,
    },
    /// A fatal error occurred; the component is now `Failed`.
    Error {
        /// Human-readable description; never contains secrets.
        message: String,
    },
    /// A finite source reached its end (media file, slideshow, ...).
    EndOfStream,
    /// The underlying device or stream disappeared (camera unplugged,
    /// PipeWire restart, portal session expired — PLAN.md §61).
    DeviceLost {
        /// Human-readable reason; never contains secrets.
        reason: String,
    },
    /// The input format changed and the downstream graph must renegotiate
    /// (RES-003 §4: PipeWire stream resolution/format changes).
    RenegotiationRequired,
}

/// Common lifecycle seam for all live backend instances.
///
/// `Send` is required because instances are owned by the media control actor
/// and are transferred across threads during graph (re)construction
/// (PLAN.md §57).
pub trait BackendComponent: Send {
    /// The last observed component state.
    fn state(&self) -> ComponentState;

    /// Drains pending events, oldest first.
    fn drain_events(&mut self) -> Vec<BackendEvent>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn component_state_serde_roundtrip() {
        for state in [
            ComponentState::Stopped,
            ComponentState::Running,
            ComponentState::Degraded,
            ComponentState::Recovering,
            ComponentState::Failed,
        ] {
            let json = serde_json::to_string(&state).unwrap();
            assert_eq!(state, serde_json::from_str(&json).unwrap());
        }
    }

    #[test]
    fn backend_event_serde_roundtrip() {
        let events = [
            BackendEvent::StateChanged {
                state: ComponentState::Recovering,
            },
            BackendEvent::Warning {
                message: "clock drift".to_string(),
            },
            BackendEvent::Error {
                message: "encoder crashed".to_string(),
            },
            BackendEvent::EndOfStream,
            BackendEvent::DeviceLost {
                reason: "camera unplugged".to_string(),
            },
            BackendEvent::RenegotiationRequired,
        ];
        for event in events {
            let json = serde_json::to_string(&event).unwrap();
            assert_eq!(event, serde_json::from_str(&json).unwrap());
        }
    }
}
