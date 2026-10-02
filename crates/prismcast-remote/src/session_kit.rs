//! Protocol-agnostic session machinery extracted from [`crate::session`]
//! (OBSWS-001): the pieces every per-connection session engine needs,
//! independent of the wire protocol spoken on the connection.
//!
//! - [`FrameReader`]/[`FrameWriter`] abstract the transport so the handshake
//!   state machine, dispatch, subscriptions, and backpressure live in one
//!   place per protocol, not per transport.
//! - [`Outbound`] is the bounded outbound queue feeding the socket-writing
//!   [`run_writer`] task; producers never write to the socket directly.
//! - [`Throttle`] coalesces high-frequency events per `(category, entity)`
//!   key (latest-wins pending slots).
//! - [`RateLimiter`] bounds inbound request rate per session.
//! - [`OverflowStrikes`] implements the slow-consumer shedding policy:
//!   after [`MAX_OVERFLOW_STRIKES`] consecutive outbound-queue overflows the
//!   session is shed.
//!
//! Extracted behavior-neutrally so the obs-websocket compatibility adapter
//! (ADR-0010, ADR-0020) can build its session engine on the same kit instead
//! of re-growing a second copy.

use std::collections::HashMap;
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::time::Instant;
use tracing::debug;
use uuid::Uuid;

use prismcast_protocol::event::WireEvent;
use prismcast_protocol::message::ServerMessage;
use prismcast_protocol::subscription::EventCategory;

use crate::codec::ClosingNotice;

/// Consecutive outbound-queue overflows tolerated before the session is shed
/// with [`CloseCode::SlowConsumer`](prismcast_protocol::handshake::CloseCode::SlowConsumer).
pub(crate) const MAX_OVERFLOW_STRIKES: u32 = 8;

/// An inbound frame could not be read or decoded. The session maps this to a
/// [`CloseCode::MessageDecodeError`](prismcast_protocol::handshake::CloseCode::MessageDecodeError) close (protocol doc §8).
#[derive(Debug)]
pub(crate) struct FrameReadError(pub String);

impl std::fmt::Display for FrameReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// An outbound frame could not be written; the connection is dead.
#[derive(Debug)]
pub(crate) struct FrameWriteError(pub String);

impl std::fmt::Display for FrameWriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The reading half of a transport connection. Implementations decode one
/// inbound frame into a generic [`serde_json::Value`] so the session can
/// classify the `type` tag before committing to a typed decode.
pub(crate) trait FrameReader: Send {
    /// Reads one frame. `Ok(None)` means the peer closed cleanly (EOF or a
    /// close frame); `Err` means the frame was unreadable or undecodable and
    /// the session closes with [`CloseCode::MessageDecodeError`](prismcast_protocol::handshake::CloseCode::MessageDecodeError).
    fn read_value(
        &mut self,
    ) -> impl std::future::Future<Output = Result<Option<serde_json::Value>, FrameReadError>> + Send;
}

/// The writing half of a transport connection. Owned by the session's writer
/// task.
pub(crate) trait FrameWriter: Send + 'static {
    /// Writes one protocol message.
    fn write_message(
        &mut self,
        message: &ServerMessage,
    ) -> impl std::future::Future<Output = Result<(), FrameWriteError>> + Send;
    /// Writes the terminal closing notice — a length-prefixed `closing`
    /// frame on IPC, a WebSocket close frame carrying the numeric code on WS
    /// (protocol doc §8) — then the writer task ends.
    fn write_close(
        &mut self,
        notice: &ClosingNotice,
    ) -> impl std::future::Future<Output = Result<(), FrameWriteError>> + Send;
}

/// One item in a session's bounded outbound queue.
pub(crate) enum Outbound {
    /// A regular protocol message.
    Message(ServerMessage),
    /// Terminal closing notice; the writer sends it and exits (IPC substitute
    /// for WebSocket close codes, protocol doc §8).
    Close(ClosingNotice),
}

/// Socket-writing half of a session: drains the bounded outbound queue; a
/// [`Outbound::Close`] item is written and terminates the task.
pub(crate) async fn run_writer<W: FrameWriter>(mut writer: W, mut rx: mpsc::Receiver<Outbound>) {
    while let Some(item) = rx.recv().await {
        let closing = matches!(item, Outbound::Close(_));
        let written = match &item {
            Outbound::Message(message) => writer.write_message(message).await,
            Outbound::Close(notice) => writer.write_close(notice).await,
        };
        if let Err(error) = written {
            debug!(%error, "transport write failed; closing writer");
            return;
        }
        if closing {
            return;
        }
    }
}

/// Slow-consumer shedding state: counts consecutive outbound-queue overflows
/// so a session is shed only when the consumer is *persistently* behind, not
/// on a transient burst (protocol doc §Backpressure).
#[derive(Debug, Default)]
pub(crate) struct OverflowStrikes {
    strikes: u32,
}

impl OverflowStrikes {
    /// Resets the counter after a successful enqueue.
    pub(crate) fn reset(&mut self) {
        self.strikes = 0;
    }

    /// Records one dropped-on-overflow message. Returns `true` once
    /// [`MAX_OVERFLOW_STRIKES`] consecutive overflows have accumulated and
    /// the session must be shed with [`CloseCode::SlowConsumer`](prismcast_protocol::handshake::CloseCode::SlowConsumer).
    pub(crate) fn strike(&mut self) -> bool {
        self.strikes += 1;
        self.strikes >= MAX_OVERFLOW_STRIKES
    }

    /// The current consecutive-overflow count (for logging).
    pub(crate) fn strikes(&self) -> u32 {
        self.strikes
    }
}

/// Coalescing throttle state: minimum delivery interval per
/// `(category, entity)` key with latest-wins pending slots.
#[derive(Default)]
pub(crate) struct Throttle {
    last_sent: HashMap<(EventCategory, Option<Uuid>), Instant>,
    pending: HashMap<(EventCategory, Option<Uuid>), PendingEvent>,
}

pub(crate) struct PendingEvent {
    pub(crate) event: WireEvent,
    pub(crate) deliver_at: Instant,
}

/// Outcome of offering an event to the throttle.
pub(crate) enum ThrottleDecision {
    /// Deliver this event immediately (first in window or window expired).
    DeliverNow(WireEvent),
    /// The event replaced the coalescing slot; flush at `next_deadline()`.
    Deferred,
}

impl Throttle {
    /// Offers an event for throttled delivery.
    pub(crate) fn offer(
        &mut self,
        key: (EventCategory, Option<Uuid>),
        interval: Duration,
        event: WireEvent,
        now: Instant,
    ) -> ThrottleDecision {
        match self.last_sent.get(&key) {
            Some(last) if now < *last + interval => {
                let deliver_at = *last + interval;
                let pending = PendingEvent { event, deliver_at };
                self.pending.insert(key, pending);
                ThrottleDecision::Deferred
            }
            _ => {
                self.last_sent.insert(key, now);
                // A stale pending slot holds an older snapshot of the same
                // entity; the newer event supersedes it.
                self.pending.remove(&key);
                ThrottleDecision::DeliverNow(event)
            }
        }
    }

    /// Drains pending events whose deadline has passed, in deadline order.
    pub(crate) fn take_expired(&mut self, now: Instant) -> Vec<PendingEvent> {
        let expired: Vec<(EventCategory, Option<Uuid>)> = self
            .pending
            .iter()
            .filter(|(_, pending)| pending.deliver_at <= now)
            .map(|(key, _)| *key)
            .collect();
        let mut drained: Vec<PendingEvent> = expired
            .into_iter()
            .filter_map(|key| {
                let pending = self.pending.remove(&key)?;
                self.last_sent.insert(key, now);
                Some(pending)
            })
            .collect();
        drained.sort_by_key(|pending| pending.deliver_at);
        drained
    }

    /// The earliest pending flush deadline.
    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        self.pending
            .values()
            .map(|pending| pending.deliver_at)
            .min()
    }

    pub(crate) fn clear(&mut self) {
        self.last_sent.clear();
        self.pending.clear();
    }

    /// Discards older measurements while preserving the delivery cadence.
    pub(crate) fn invalidate_meters(&mut self) {
        self.pending
            .retain(|(category, _), _| *category != EventCategory::Meter);
    }

    /// Retires a source's meter window when its producer lifecycle ends.
    pub(crate) fn retire_meter(&mut self, source_id: Uuid) {
        let key = (EventCategory::Meter, Some(source_id));
        self.last_sent.remove(&key);
        self.pending.remove(&key);
    }
}

/// Fixed-window inbound request rate limiter (protocol doc §1: 100 req/s,
/// burst 200; excess → `rate_limited` error responses).
pub(crate) struct RateLimiter {
    window_start: Instant,
    count: u32,
    burst: u32,
}

impl RateLimiter {
    pub(crate) fn new(burst: u32) -> Self {
        Self {
            window_start: Instant::now(),
            count: 0,
            burst,
        }
    }

    /// Whether the request may proceed.
    pub(crate) fn check(&mut self) -> bool {
        let now = Instant::now();
        if now.duration_since(self.window_start) >= Duration::from_secs(1) {
            self.window_start = now;
            self.count = 0;
        }
        self.count += 1;
        self.count <= self.burst
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use prismcast_protocol::subscription::EventCategory as Cat;

    fn wire_scene_event(name: &str) -> WireEvent {
        WireEvent::Scene(prismcast_protocol::event::SceneEvent::Added {
            scene_id: Uuid::new_v4(),
            name: name.into(),
        })
    }

    fn assert_deliver_now(decision: ThrottleDecision) {
        assert!(matches!(decision, ThrottleDecision::DeliverNow(_)));
    }

    fn assert_deferred(decision: ThrottleDecision) {
        assert!(matches!(decision, ThrottleDecision::Deferred));
    }

    #[test]
    fn throttle_delivers_first_event_immediately() {
        let mut throttle = Throttle::default();
        let now = Instant::now();
        let key = (Cat::Scene, Some(Uuid::new_v4()));
        assert_deliver_now(throttle.offer(
            key,
            Duration::from_millis(100),
            wire_scene_event("a"),
            now,
        ));
        assert!(throttle.next_deadline().is_none());
    }

    #[test]
    fn throttle_coalesces_within_window_latest_wins() {
        let mut throttle = Throttle::default();
        let now = Instant::now();
        let key = (Cat::Scene, Some(Uuid::new_v4()));
        let interval = Duration::from_millis(100);
        assert_deliver_now(throttle.offer(key, interval, wire_scene_event("first"), now));
        assert_deferred(throttle.offer(
            key,
            interval,
            wire_scene_event("second"),
            now + Duration::from_millis(10),
        ));
        assert_eq!(throttle.next_deadline(), Some(now + interval));
        assert_deferred(throttle.offer(
            key,
            interval,
            wire_scene_event("third"),
            now + Duration::from_millis(20),
        ));
        assert_eq!(
            throttle.next_deadline(),
            Some(now + interval),
            "same window, same deadline"
        );

        // Nothing expires inside the window.
        assert!(throttle
            .take_expired(now + Duration::from_millis(50))
            .is_empty());
        let expired = throttle.take_expired(now + interval);
        assert_eq!(expired.len(), 1, "coalesced to a single latest event");
        assert!(matches!(
            &expired[0].event,
            WireEvent::Scene(prismcast_protocol::event::SceneEvent::Added { name, .. }) if name == "third"
        ));
        // After flushing at the window's end, the window restarts at the
        // flush moment (fixed cadence): an event right after is deferred,
        // one past the new window delivers immediately.
        assert_deferred(throttle.offer(
            key,
            interval,
            wire_scene_event("fourth"),
            now + interval + Duration::from_millis(1),
        ));
        assert_eq!(throttle.take_expired(now + 2 * interval).len(), 1);
        // The flush at t = now+2i is itself a delivery; an event a full
        // window later delivers immediately.
        assert_deliver_now(throttle.offer(
            key,
            interval,
            wire_scene_event("fifth"),
            now + 3 * interval,
        ));
    }

    #[test]
    fn throttle_windows_are_per_entity() {
        let mut throttle = Throttle::default();
        let now = Instant::now();
        let interval = Duration::from_millis(100);
        let a = (Cat::Scene, Some(Uuid::new_v4()));
        let b = (Cat::Scene, Some(Uuid::new_v4()));
        assert_deliver_now(throttle.offer(a, interval, wire_scene_event("a1"), now));
        assert_deliver_now(throttle.offer(b, interval, wire_scene_event("b1"), now));
        assert_deferred(throttle.offer(
            a,
            interval,
            wire_scene_event("a2"),
            now + Duration::from_millis(10),
        ));
        assert_deferred(throttle.offer(
            b,
            interval,
            wire_scene_event("b2"),
            now + Duration::from_millis(10),
        ));
        assert_eq!(throttle.take_expired(now + interval).len(), 2);
    }

    #[test]
    fn entityless_events_throttle_under_their_own_key() {
        let mut throttle = Throttle::default();
        let now = Instant::now();
        let key = (Cat::System, None);
        let interval = Duration::from_millis(50);
        assert_deliver_now(throttle.offer(
            key,
            interval,
            WireEvent::System(prismcast_protocol::event::SystemEvent::StudioModeChanged {
                enabled: true,
            }),
            now,
        ));
        assert_deferred(throttle.offer(
            key,
            interval,
            WireEvent::System(prismcast_protocol::event::SystemEvent::StudioModeChanged {
                enabled: false,
            }),
            now,
        ));
    }

    #[test]
    fn configuration_change_discards_meter_slots_and_preserves_control_throttle() {
        let mut throttle = Throttle::default();
        let now = Instant::now();
        let interval = Duration::from_millis(100);
        let source_id = Uuid::new_v4();
        let meter_key = (Cat::Meter, Some(source_id));
        let meter = WireEvent::Meter(prismcast_protocol::event::MeterEvent::Levels {
            source_id,
            peak_dbfs: vec![-6.0; 2],
            rms_dbfs: vec![-9.0; 2],
        });
        throttle.offer(meter_key, interval, meter.clone(), now);
        throttle.offer(meter_key, interval, meter, now);
        let scene_key = (Cat::Scene, Some(Uuid::new_v4()));
        throttle.offer(scene_key, interval, wire_scene_event("first"), now);
        throttle.offer(scene_key, interval, wire_scene_event("pending"), now);
        throttle.invalidate_meters();
        assert!(throttle.last_sent.contains_key(&meter_key));
        assert!(!throttle.pending.contains_key(&meter_key));
        assert!(throttle.last_sent.contains_key(&scene_key));
        assert_eq!(throttle.take_expired(now + interval).len(), 1);
        // Configuration changes discard pending telemetry, but cannot bypass
        // the client's requested minimum delivery interval.
        let meter = WireEvent::Meter(prismcast_protocol::event::MeterEvent::Levels {
            source_id,
            peak_dbfs: vec![-12.0; 2],
            rms_dbfs: vec![-15.0; 2],
        });
        assert_deferred(throttle.offer(meter_key, interval, meter, now));
        throttle.retire_meter(source_id);
        assert!(!throttle.last_sent.contains_key(&meter_key));
        assert!(!throttle.pending.contains_key(&meter_key));
    }

    #[test]
    fn overflow_strikes_shed_only_after_max_consecutive_overflows() {
        let mut strikes = OverflowStrikes::default();
        for _ in 0..MAX_OVERFLOW_STRIKES - 1 {
            assert!(!strikes.strike(), "below the shedding threshold");
        }
        strikes.reset();
        assert!(!strikes.strike(), "a successful enqueue resets the count");
        for _ in 0..MAX_OVERFLOW_STRIKES - 1 {
            strikes.strike();
        }
        assert!(strikes.strike(), "the threshold sheds the session");
    }

    #[test]
    fn rate_limiter_allows_burst_then_rejects() {
        let mut limiter = RateLimiter::new(3);
        assert!(limiter.check());
        assert!(limiter.check());
        assert!(limiter.check());
        assert!(!limiter.check());
    }
}
