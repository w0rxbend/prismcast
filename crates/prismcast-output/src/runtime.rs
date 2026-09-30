//! Per-output runtime: independent state machine, reconnect backoff, stats.
//!
//! Every output owns exactly one [`OutputRuntime`] (ADR-0007 §3: "each
//! destination owns its state machine, network queue, reconnect/backoff
//! policy, statistics"). Runtimes share **no** mutable state: a failure driven
//! into one runtime cannot observe, let alone mutate, a sibling — that is the
//! structural basis of the failure-isolation guarantee (PLAN.md §11, §50).
//!
//! The state machine mirrors the PLAN §61 failure model documented on
//! `prismcast_core::OutputState`:
//!
//! ```text
//! Stopped ─┐
//!          ├→ Starting ─→ Running ─→ Degraded ─┐
//! Failed ──┘        │         │                ├→ Running
//!                   ↓         ↓                ├→ Failed
//!                Failed    Reconnecting ───────┴→ Stopping → Stopped
//! ```

use std::time::Duration;

use serde::{Deserialize, Serialize};

use prismcast_core::{OutputId, OutputState, ReconnectPolicy};

use crate::error::{OutputGraphError, Result};

/// Computes the backoff before reconnect `attempt` (1-based), or `None` when
/// the policy does not allow that attempt.
///
/// Exponential growth from `initial_backoff_ms`, doubling per attempt, capped
/// at `max_backoff_ms`; all arithmetic is saturating. `None` is returned when
/// `max_retries == 0`, `attempt == 0`, or `attempt > max_retries` — the caller
/// treats `None` as "give up and go `Failed`".
///
/// The result is deliberately jitter-free so scheduling is reproducible and
/// testable; the media layer may apply jitter when it arms the actual timer.
pub fn backoff_ms(policy: &ReconnectPolicy, attempt: u32) -> Option<u64> {
    if policy.max_retries == 0 || attempt == 0 || attempt > policy.max_retries {
        return None;
    }
    let multiplier = 1u64.checked_shl(attempt - 1).unwrap_or(u64::MAX);
    let grown = u64::from(policy.initial_backoff_ms).saturating_mul(multiplier);
    Some(grown.min(u64::from(policy.max_backoff_ms)))
}

/// The outcome of a connection loss, decided from the reconnect policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconnectStep {
    /// Retry after `backoff`; the output is now `Reconnecting { attempt }`.
    Retry {
        /// 1-based reconnect attempt number.
        attempt: u32,
        /// How long to wait before dialing again.
        backoff: Duration,
    },
    /// The retry budget is exhausted; the output is now `Failed`.
    Exhausted,
}

/// Per-output statistics, reported to UI/remote uniformly (ADR-0007 §6).
///
/// Counters are monotonic for the lifetime of the runtime; they persist across
/// stop/start cycles so controllers can render cumulative history.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputStats {
    /// Total payload bytes handed to the sink.
    pub bytes_sent: u64,
    /// Total encoded packets handed to the sink.
    pub packets_sent: u64,
    /// Frames dropped because the per-output (bounded) queue was full.
    pub dropped_frames: u64,
    /// Total reconnect attempts performed.
    pub reconnect_attempts: u32,
    /// Human-readable description of the most recent failure, if any.
    pub last_error: Option<String>,
}

/// The independent runtime of a single output.
#[derive(Debug, Clone)]
pub struct OutputRuntime {
    output_id: OutputId,
    state: OutputState,
    policy: ReconnectPolicy,
    stats: OutputStats,
}

impl OutputRuntime {
    /// Creates a runtime for a fresh output: `Stopped`, default counters.
    pub fn new(output_id: OutputId, policy: ReconnectPolicy) -> Self {
        Self {
            output_id,
            state: OutputState::Stopped,
            policy,
            stats: OutputStats::default(),
        }
    }

    /// Rebuilds a runtime from persisted domain state (e.g. restoring a
    /// project whose outputs were mid-lifecycle when saved).
    pub fn restore(output_id: OutputId, policy: ReconnectPolicy, state: OutputState) -> Self {
        Self {
            output_id,
            state,
            policy,
            stats: OutputStats::default(),
        }
    }

    /// The output this runtime belongs to.
    pub fn output_id(&self) -> OutputId {
        self.output_id
    }

    /// Current lifecycle state.
    pub fn state(&self) -> OutputState {
        self.state
    }

    /// Current reconnect policy.
    pub fn policy(&self) -> ReconnectPolicy {
        self.policy
    }

    /// Current statistics snapshot.
    pub fn stats(&self) -> &OutputStats {
        &self.stats
    }

    /// Mutable statistics handle for the media layer (counters only; state
    /// transitions go through [`Self::transition`]).
    pub fn stats_mut(&mut self) -> &mut OutputStats {
        &mut self.stats
    }

    /// Replaces the reconnect policy (takes effect on the next failure).
    pub fn set_policy(&mut self, policy: ReconnectPolicy) {
        self.policy = policy;
    }

    /// Whether the transition `from → to` is legal under the PLAN §61 model.
    ///
    /// Two edges extend the minimal table documented on
    /// `prismcast_core::OutputState`:
    /// - `Running → Failed` — a hard failure (encoder crash, sink error with
    ///   no retry budget) must not have to detour through `Degraded`.
    /// - `Reconnecting → Reconnecting` — the next retry attempt while the
    ///   connection is still down.
    pub fn is_legal_transition(from: OutputState, to: OutputState) -> bool {
        use OutputState as S;
        match from {
            S::Stopped | S::Failed => matches!(to, S::Starting),
            S::Starting => matches!(to, S::Running | S::Failed),
            S::Running => {
                matches!(
                    to,
                    S::Degraded | S::Reconnecting { .. } | S::Stopping | S::Failed
                )
            }
            S::Degraded => matches!(to, S::Running | S::Failed | S::Stopping),
            S::Reconnecting { .. } => {
                matches!(
                    to,
                    S::Running | S::Failed | S::Stopping | S::Reconnecting { .. }
                )
            }
            S::Stopping => matches!(to, S::Stopped),
        }
    }

    /// Applies a lifecycle transition, validating it against the PLAN §61
    /// model.
    ///
    /// # Errors
    ///
    /// [`OutputGraphError::IllegalTransition`] if the transition is forbidden.
    pub fn transition(&mut self, to: OutputState) -> Result<()> {
        if !Self::is_legal_transition(self.state, to) {
            return Err(OutputGraphError::IllegalTransition {
                output_id: self.output_id,
                from: self.state,
                to,
            });
        }
        tracing::debug!(
            output_id = %self.output_id,
            from = ?self.state,
            to = ?to,
            "output state transition"
        );
        self.state = to;
        Ok(())
    }

    /// Records a connection loss and decides the next step from the reconnect
    /// policy: either `Reconnecting { attempt }` with a backoff, or `Failed`
    /// when the retry budget is exhausted.
    ///
    /// # Errors
    ///
    /// [`OutputGraphError::NotReconnectable`] if the current state cannot lose
    /// a connection (anything other than `Running`, `Degraded`, or
    /// `Reconnecting`).
    pub fn connection_lost(&mut self, reason: impl Into<String>) -> Result<ReconnectStep> {
        let attempt = match self.state {
            OutputState::Running | OutputState::Degraded => 1,
            OutputState::Reconnecting { attempt } => attempt + 1,
            _ => return Err(OutputGraphError::NotReconnectable(self.output_id)),
        };
        self.stats.last_error = Some(reason.into());

        let Some(ms) = backoff_ms(&self.policy, attempt) else {
            self.stats.reconnect_attempts = attempt.saturating_sub(1);
            self.transition(OutputState::Failed)?;
            return Ok(ReconnectStep::Exhausted);
        };

        self.stats.reconnect_attempts = attempt;
        self.transition(OutputState::Reconnecting { attempt })?;
        Ok(ReconnectStep::Retry {
            attempt,
            backoff: Duration::from_millis(ms),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runtime(policy: ReconnectPolicy) -> OutputRuntime {
        OutputRuntime::new(OutputId::new(), policy)
    }

    fn running(policy: ReconnectPolicy) -> OutputRuntime {
        let mut rt = runtime(policy);
        rt.transition(OutputState::Starting).unwrap();
        rt.transition(OutputState::Running).unwrap();
        rt
    }

    #[test]
    fn backoff_grows_exponentially_and_caps() {
        let policy = ReconnectPolicy::default(); // 1s initial, 30s cap, 10 retries
        let expected = [
            1_000, 2_000, 4_000, 8_000, 16_000, 30_000, 30_000, 30_000, 30_000, 30_000,
        ];
        for (attempt, &ms) in (1..=10u32).zip(expected.iter()) {
            assert_eq!(backoff_ms(&policy, attempt), Some(ms), "attempt {attempt}");
        }
    }

    #[test]
    fn backoff_rejects_disallowed_attempts() {
        let policy = ReconnectPolicy::default();
        assert_eq!(backoff_ms(&policy, 0), None);
        assert_eq!(backoff_ms(&policy, 11), None, "beyond max_retries");

        let no_retries = ReconnectPolicy {
            max_retries: 0,
            ..ReconnectPolicy::default()
        };
        assert_eq!(backoff_ms(&no_retries, 1), None);
    }

    #[test]
    fn backoff_saturates_instead_of_overflowing() {
        let policy = ReconnectPolicy {
            max_retries: 100,
            initial_backoff_ms: u32::MAX,
            max_backoff_ms: 60_000,
        };
        // 2^63 shift would overflow without saturation; must hit the cap.
        assert_eq!(backoff_ms(&policy, 64), Some(60_000));
        assert_eq!(backoff_ms(&policy, 100), Some(60_000));
    }

    #[test]
    fn happy_path_transitions() {
        let mut rt = runtime(ReconnectPolicy::default());
        rt.transition(OutputState::Starting).unwrap();
        rt.transition(OutputState::Running).unwrap();
        rt.transition(OutputState::Degraded).unwrap();
        rt.transition(OutputState::Running).unwrap();
        rt.transition(OutputState::Stopping).unwrap();
        rt.transition(OutputState::Stopped).unwrap();
        assert_eq!(rt.state(), OutputState::Stopped);
    }

    #[test]
    fn illegal_transitions_are_rejected() {
        let mut rt = runtime(ReconnectPolicy::default());
        for to in [
            OutputState::Running,
            OutputState::Degraded,
            OutputState::Reconnecting { attempt: 1 },
            OutputState::Stopping,
        ] {
            let err = rt.transition(to).unwrap_err();
            assert!(
                matches!(err, OutputGraphError::IllegalTransition { .. }),
                "Stopped -> {to:?} must fail"
            );
            assert_eq!(rt.state(), OutputState::Stopped, "state unchanged");
        }
    }

    #[test]
    fn connection_loss_drives_reconnect_then_exhaustion() {
        let policy = ReconnectPolicy {
            max_retries: 2,
            initial_backoff_ms: 100,
            max_backoff_ms: 1_000,
        };
        let mut rt = running(policy);

        let step = rt.connection_lost("tcp reset").unwrap();
        assert_eq!(
            step,
            ReconnectStep::Retry {
                attempt: 1,
                backoff: Duration::from_millis(100)
            }
        );
        assert_eq!(rt.state(), OutputState::Reconnecting { attempt: 1 });

        let step = rt.connection_lost("tcp reset").unwrap();
        assert_eq!(
            step,
            ReconnectStep::Retry {
                attempt: 2,
                backoff: Duration::from_millis(200)
            }
        );

        let step = rt.connection_lost("tcp reset").unwrap();
        assert_eq!(step, ReconnectStep::Exhausted);
        assert_eq!(rt.state(), OutputState::Failed);
        assert_eq!(rt.stats().reconnect_attempts, 2);
        assert_eq!(rt.stats().last_error.as_deref(), Some("tcp reset"));
    }

    #[test]
    fn failed_output_may_restart() {
        let mut rt = running(ReconnectPolicy {
            max_retries: 0,
            ..ReconnectPolicy::default()
        });
        assert_eq!(
            rt.connection_lost("dead").unwrap(),
            ReconnectStep::Exhausted
        );
        assert_eq!(rt.state(), OutputState::Failed);
        rt.transition(OutputState::Starting).unwrap();
        rt.transition(OutputState::Running).unwrap();
        assert_eq!(rt.state(), OutputState::Running);
    }

    #[test]
    fn connection_loss_from_stopped_is_an_error() {
        let mut rt = runtime(ReconnectPolicy::default());
        let err = rt.connection_lost("dead").unwrap_err();
        assert!(matches!(err, OutputGraphError::NotReconnectable(_)));
    }

    #[test]
    fn stats_serde_roundtrip() {
        let stats = OutputStats {
            bytes_sent: 1_024,
            packets_sent: 42,
            dropped_frames: 3,
            reconnect_attempts: 1,
            last_error: Some("dns failure".to_string()),
        };
        let json = serde_json::to_string(&stats).unwrap();
        assert_eq!(stats, serde_json::from_str(&json).unwrap());
    }
}
