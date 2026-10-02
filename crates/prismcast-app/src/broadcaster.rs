//! Multi-subscriber event fan-out with bounded per-subscriber queues
//! (CORE-003; PLAN.md §57/§58).
//!
//! The core actor publishes every committed [`Event`] to the
//! [`EventBroadcaster`]; each subscriber owns a **bounded** queue (PLAN.md
//! §75: "no unbounded channels for media/control data") and receives events
//! through an [`EventStream`], filtered by its [`EventFilter`].
//!
//! ## Slow-consumer policy (documented choice)
//!
//! When a subscriber's queue is full, the broadcaster **drops the oldest
//! queued events** to make room for newer ones and pins a
//! [`StreamEvent::Lagged`] notice at the front of the queue recording how many
//! events were dropped (consecutive drops coalesce into one notice). The
//! publisher is never blocked and other subscribers are never affected. This
//! mirrors the protocol-level contract where per-session sequence numbers let
//! clients detect gaps and re-sync from a snapshot (`docs/protocols/
//! native-protocol.md` §Backpressure): a controller that observes `Lagged`
//! must treat its incremental state as stale and re-read
//! [`crate::snapshot::AppSnapshot`].
//!
//! Per-category/per-entity filtering follows the semantics of
//! `prismcast-protocol`'s subscription model, but the types are defined here
//! (`prismcast-app` must not depend on `prismcast-protocol`; the dependency
//! direction is `app <- remote`, so the remote crate maps its wire
//! `EventCategory` onto [`EventCategory`]).
//!
//! Meter events share this bounded fan-out and source filtering. Interface
//! adapters apply delivery throttling; the app's meter watch is latest-only.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use serde::{Deserialize, Serialize};
use tokio::sync::Notify;
use tracing::warn;
use uuid::Uuid;

use prismcast_core::event::{
    AudioEvent, Event, MeterEvent, OutputEvent, SceneEvent, SourceEvent, SystemEvent,
};

/// Default per-subscriber queue capacity.
pub const DEFAULT_SUBSCRIBER_CAPACITY: usize = 256;

/// Minimum queue capacity; smaller requests are clamped. A capacity of at
/// least 2 is required so a [`StreamEvent::Lagged`] notice plus the newest
/// event always fit.
pub const MIN_SUBSCRIBER_CAPACITY: usize = 2;

/// Event domain categories, mirroring the top-level [`Event`] variants.
///
/// Matches the domain groups of `prismcast_protocol::EventCategory`; the remote crate maps between the
/// two without this crate depending on the protocol crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventCategory {
    /// Scene and scene-item changes.
    Scene,
    /// Source changes.
    Source,
    /// Audio mixer/routing changes.
    Audio,
    /// High-volume transient source audio observations.
    Meter,
    /// Output graph changes.
    Output,
    /// Studio mode, transitions, profiles, collections.
    System,
}

/// Returns the category gating an event.
pub fn category_of(event: &Event) -> EventCategory {
    match event {
        Event::Scene(_) => EventCategory::Scene,
        Event::Source(_) => EventCategory::Source,
        Event::Audio(_) => EventCategory::Audio,
        Event::Meter(_) => EventCategory::Meter,
        Event::Output(_) => EventCategory::Output,
        Event::System(_) => EventCategory::System,
    }
}

/// Returns the primary entity an event is about, for per-entity filtering.
///
/// `None` for events without a filterable entity (e.g. `SceneEvent::Reordered`,
/// `SystemEvent::StudioModeChanged`); those pass only filters with no entity
/// restriction, matching the protocol subscription semantics.
pub fn primary_entity(event: &Event) -> Option<Uuid> {
    let uuid = match event {
        Event::Scene(scene) => match scene {
            SceneEvent::Added { scene_id, .. }
            | SceneEvent::Removed { scene_id }
            | SceneEvent::Renamed { scene_id, .. }
            | SceneEvent::CurrentChanged { scene_id }
            | SceneEvent::ItemAdded { scene_id, .. }
            | SceneEvent::ItemRemoved { scene_id, .. }
            | SceneEvent::ItemUpdated { scene_id, .. } => scene_id.as_uuid(),
            SceneEvent::Reordered => return None,
        },
        Event::Source(source) => match source {
            SourceEvent::Added { source } => source.id.as_uuid(),
            SourceEvent::Removed { source_id }
            | SourceEvent::Renamed { source_id, .. }
            | SourceEvent::SettingsChanged { source_id }
            | SourceEvent::CaptureAuthorizationRequested { source_id }
            | SourceEvent::RuntimeChanged { source_id, .. }
            | SourceEvent::EnabledChanged { source_id, .. } => source_id.as_uuid(),
        },
        Event::Meter(MeterEvent::Levels { source_id, .. }) => source_id.as_uuid(),
        Event::Audio(audio) => match audio {
            AudioEvent::MixerChanged { source_id, .. }
            | AudioEvent::RouteChanged { source_id, .. }
            | AudioEvent::RouteRemoved { source_id, .. } => source_id.as_uuid(),
            AudioEvent::BusAdded { bus_id, .. } | AudioEvent::BusRemoved { bus_id } => {
                bus_id.as_uuid()
            }
        },
        Event::Output(output) => match output {
            OutputEvent::Added { output_id, .. }
            | OutputEvent::Removed { output_id }
            | OutputEvent::StateChanged { output_id, .. }
            | OutputEvent::ReconnectPolicyChanged { output_id } => output_id.as_uuid(),
        },
        Event::System(system) => match system {
            SystemEvent::PreviewSceneChanged { scene_id } => scene_id.as_uuid(),
            SystemEvent::ProfileAdded { profile_id }
            | SystemEvent::ProfileRemoved { profile_id }
            | SystemEvent::ProfileSelected { profile_id } => profile_id.as_uuid(),
            SystemEvent::CollectionAdded { collection_id }
            | SystemEvent::CollectionRemoved { collection_id }
            | SystemEvent::CollectionSelected { collection_id } => collection_id.as_uuid(),
            SystemEvent::StudioModeChanged { .. }
            | SystemEvent::TransitionChanged { .. }
            | SystemEvent::TransitionStarted { .. } => return None,
        },
    };
    Some(*uuid)
}

/// Which events a subscriber receives.
///
/// - `categories`: `None` = all categories; `Some` = only listed categories.
/// - `entities`: `None` = all entities; `Some` = only events whose
///   [`primary_entity`] is listed. Events with no primary entity are
///   suppressed by an entity-restricted filter (same rule as the wire
///   subscription model).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EventFilter {
    categories: Option<BTreeSet<EventCategory>>,
    entities: Option<BTreeSet<Uuid>>,
}

impl EventFilter {
    /// Receives every event.
    pub fn all() -> Self {
        Self::default()
    }

    /// Receives only the given categories.
    pub fn categories(categories: impl IntoIterator<Item = EventCategory>) -> Self {
        Self {
            categories: Some(categories.into_iter().collect()),
            entities: None,
        }
    }

    /// Restricts delivery to events about the given entities.
    pub fn entities(mut self, entities: impl IntoIterator<Item = Uuid>) -> Self {
        self.entities = Some(entities.into_iter().collect());
        self
    }

    /// Whether an event passes this filter.
    pub fn matches(&self, event: &Event) -> bool {
        if let Some(categories) = &self.categories {
            if !categories.contains(&category_of(event)) {
                return false;
            }
        }
        if let Some(entities) = &self.entities {
            match primary_entity(event) {
                Some(entity) if entities.contains(&entity) => {}
                _ => return false,
            }
        }
        true
    }
}

/// One item delivered to a subscriber.
#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    /// A committed domain event with its global, monotonically increasing
    /// sequence number (assigned by the core actor in apply order).
    Event {
        /// Global event sequence number.
        seq: u64,
        /// The committed event.
        event: Event,
    },
    /// The subscriber fell behind: `dropped` oldest queued events were
    /// discarded under the slow-consumer policy (see module docs). The
    /// receiver should re-sync from a snapshot.
    Lagged {
        /// How many events were discarded since the last delivered item.
        dropped: u64,
    },
}

/// A per-subscriber bounded queue with drop-oldest overflow.
struct SubscriberQueue {
    inner: Mutex<QueueInner>,
    notify: Notify,
    capacity: usize,
}

#[derive(Default)]
struct QueueInner {
    queue: VecDeque<StreamEvent>,
    closed: bool,
}

impl SubscriberQueue {
    fn new(capacity: usize) -> Self {
        Self {
            inner: Mutex::new(QueueInner::default()),
            notify: Notify::new(),
            capacity: capacity.max(MIN_SUBSCRIBER_CAPACITY),
        }
    }

    fn lock(&self) -> MutexGuard<'_, QueueInner> {
        // Poisoning only happens if a thread panicked mid-push; the queue is
        // still structurally sound, so recover rather than panic in the actor.
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Enqueues an event, dropping oldest entries (and recording the drop in a
    /// pinned front `Lagged` notice) when at capacity. Never blocks.
    fn push(&self, item: StreamEvent) {
        let dropped = {
            let mut inner = self.lock();
            if inner.closed {
                return;
            }
            let mut dropped = 0_u64;
            // Make room for the incoming item. A `Lagged` notice at the front
            // is pinned: the oldest *real* event behind it is dropped instead,
            // and the drop folds into the notice.
            while inner.queue.len() >= self.capacity {
                if matches!(inner.queue.front(), Some(StreamEvent::Lagged { .. })) {
                    inner.queue.remove(1);
                } else {
                    inner.queue.pop_front();
                }
                dropped += 1;
            }
            if dropped > 0 {
                match inner.queue.front_mut() {
                    Some(StreamEvent::Lagged { dropped: total }) => *total += dropped,
                    _ => {
                        inner.queue.push_front(StreamEvent::Lagged { dropped });
                        if inner.queue.len() >= self.capacity {
                            // The notice itself needs a slot: drop the oldest
                            // real event behind it and fold that in too.
                            inner.queue.remove(1);
                            if let Some(StreamEvent::Lagged { dropped: total }) =
                                inner.queue.front_mut()
                            {
                                *total += 1;
                            }
                        }
                    }
                }
            }
            inner.queue.push_back(item);
            dropped
        };
        if dropped > 0 {
            warn!(dropped, "slow event subscriber: dropped oldest events");
        }
        self.notify.notify_one();
    }

    fn pop(&self) -> Option<StreamEvent> {
        self.lock().queue.pop_front()
    }

    fn is_closed_and_empty(&self) -> bool {
        let inner = self.lock();
        inner.closed && inner.queue.is_empty()
    }

    fn close(&self) {
        self.lock().closed = true;
        // Wake any current waiter so it can observe closure.
        self.notify.notify_one();
    }
}

/// Receiving end of one subscription.
///
/// `recv` yields queued items in publish order, then `None` once the
/// broadcaster is closed **and** the queue is drained — events committed
/// before shutdown are not lost.
pub struct EventStream {
    queue: Arc<SubscriberQueue>,
}

impl EventStream {
    /// Waits for the next item. Returns `None` after broadcaster shutdown once
    /// the queue is drained.
    pub async fn recv(&mut self) -> Option<StreamEvent> {
        loop {
            if let item @ Some(_) = self.queue.pop() {
                return item;
            }
            if self.queue.is_closed_and_empty() {
                return None;
            }
            // `Notify` buffers one permit, so a push between the check above
            // and this await is not missed.
            self.queue.notify.notified().await;
        }
    }
}

struct Subscriber {
    filter: EventFilter,
    queue: Arc<SubscriberQueue>,
}

struct BroadcasterInner {
    subscribers: Mutex<HashMap<u64, Subscriber>>,
    next_id: AtomicU64,
    default_capacity: usize,
}

/// Multi-subscriber event fan-out. Cloneable; all clones share the registry.
///
/// `publish` is synchronous and never blocks: delivery to each subscriber is
/// a bounded-queue push under a short mutex (no `.await` while held).
#[derive(Clone)]
pub struct EventBroadcaster {
    inner: Arc<BroadcasterInner>,
}

impl EventBroadcaster {
    /// Creates a broadcaster with the given default per-subscriber capacity.
    pub fn new(default_capacity: usize) -> Self {
        Self {
            inner: Arc::new(BroadcasterInner {
                subscribers: Mutex::new(HashMap::new()),
                next_id: AtomicU64::new(0),
                default_capacity: default_capacity.max(MIN_SUBSCRIBER_CAPACITY),
            }),
        }
    }

    fn lock_subscribers(&self) -> MutexGuard<'_, HashMap<u64, Subscriber>> {
        self.inner
            .subscribers
            .lock()
            .unwrap_or_else(|p| p.into_inner())
    }

    /// Subscribes with the default queue capacity.
    pub fn subscribe(&self, filter: EventFilter) -> EventStream {
        self.subscribe_with_capacity(filter, self.inner.default_capacity)
    }

    /// Subscribes with an explicit queue capacity (clamped to
    /// [`MIN_SUBSCRIBER_CAPACITY`]). Intended for tests and controllers with
    /// known consumption rates.
    pub fn subscribe_with_capacity(&self, filter: EventFilter, capacity: usize) -> EventStream {
        let queue = Arc::new(SubscriberQueue::new(capacity));
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        self.lock_subscribers().insert(
            id,
            Subscriber {
                filter,
                queue: Arc::clone(&queue),
            },
        );
        EventStream { queue }
    }

    /// Publishes one event to all matching subscribers, in subscription
    /// registry order. Applies the slow-consumer policy per subscriber.
    pub fn publish(&self, seq: u64, event: &Event) {
        let subscribers = self.lock_subscribers();
        for subscriber in subscribers.values() {
            if subscriber.filter.matches(event) {
                subscriber.queue.push(StreamEvent::Event {
                    seq,
                    event: event.clone(),
                });
            }
        }
    }

    /// Closes all subscriber queues. Streams drain remaining items, then
    /// `recv` returns `None`. New subscriptions are still accepted but will
    /// only observe future publishes (used at actor shutdown).
    pub fn close_all(&self) {
        for subscriber in self.lock_subscribers().values() {
            subscriber.queue.close();
        }
    }

    /// Number of live subscriptions (metrics/tests).
    pub fn subscriber_count(&self) -> usize {
        self.lock_subscribers().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use prismcast_core::id::SceneId;
    use prismcast_core::source::SourceKind;

    fn scene_added(name: &str) -> Event {
        Event::Scene(SceneEvent::Added {
            scene_id: SceneId::new(),
            name: name.into(),
        })
    }

    async fn collect(stream: &mut EventStream, n: usize) -> Vec<StreamEvent> {
        let mut items = Vec::new();
        for _ in 0..n {
            if let Some(item) = stream.recv().await {
                items.push(item);
            }
        }
        items
    }

    #[tokio::test]
    async fn two_subscribers_receive_in_publish_order() {
        let broadcaster = EventBroadcaster::new(8);
        let mut a = broadcaster.subscribe(EventFilter::all());
        let mut b = broadcaster.subscribe(EventFilter::all());
        assert_eq!(broadcaster.subscriber_count(), 2);

        let events = [scene_added("one"), scene_added("two"), scene_added("three")];
        for (seq, event) in events.iter().enumerate() {
            broadcaster.publish(seq as u64, event);
        }

        for stream in [&mut a, &mut b] {
            let got = collect(stream, 3).await;
            let got_events: Vec<&Event> = got
                .iter()
                .map(|item| match item {
                    StreamEvent::Event { event, .. } => event,
                    other => panic!("unexpected {other:?}"),
                })
                .collect();
            assert_eq!(got_events, events.iter().collect::<Vec<_>>());
            let seqs: Vec<u64> = got
                .iter()
                .map(|item| match item {
                    StreamEvent::Event { seq, .. } => *seq,
                    other => panic!("unexpected {other:?}"),
                })
                .collect();
            assert_eq!(seqs, vec![0, 1, 2]);
        }
    }

    #[tokio::test]
    async fn category_filter_gates_delivery() {
        let broadcaster = EventBroadcaster::new(8);
        let mut scenes_only =
            broadcaster.subscribe(EventFilter::categories([EventCategory::Scene]));
        let mut all = broadcaster.subscribe(EventFilter::all());

        let scene = scene_added("s");
        let source = Event::Source(SourceEvent::Added {
            source: Box::new(prismcast_core::source::Source::new(SourceKind::Color, "c")),
        });
        broadcaster.publish(0, &scene);
        broadcaster.publish(1, &source);

        let got = collect(&mut scenes_only, 1).await;
        assert_eq!(got.len(), 1);
        assert!(matches!(&got[0], StreamEvent::Event { seq: 0, .. }));
        assert!(collect(&mut all, 2).await.len() == 2);
    }

    #[tokio::test]
    async fn entity_filter_suppresses_other_entities_and_entityless_events() {
        let broadcaster = EventBroadcaster::new(8);
        let wanted = SceneId::new();
        let mut filtered = broadcaster.subscribe(
            EventFilter::categories([EventCategory::Scene]).entities([*wanted.as_uuid()]),
        );

        let hit = Event::Scene(SceneEvent::CurrentChanged { scene_id: wanted });
        let miss = scene_added("other scene");
        let entityless = Event::Scene(SceneEvent::Reordered);
        broadcaster.publish(0, &hit);
        broadcaster.publish(1, &miss);
        broadcaster.publish(2, &entityless);

        let got = collect(&mut filtered, 1).await;
        assert_eq!(got.len(), 1);
        assert!(matches!(&got[0], StreamEvent::Event { seq: 0, event } if *event == hit));
    }

    #[tokio::test]
    async fn slow_consumer_gets_drop_oldest_plus_coalesced_lagged_notice() {
        let broadcaster = EventBroadcaster::new(4);
        let mut slow = broadcaster.subscribe_with_capacity(EventFilter::all(), 4);

        for seq in 0..7 {
            broadcaster.publish(seq, &scene_added("x"));
        }

        // Capacity 4, 7 published: the queue holds a coalesced Lagged notice
        // plus the three newest events (4 dropped in total).
        let got = collect(&mut slow, 4).await;
        assert_eq!(got.len(), 4);
        assert!(matches!(got[0], StreamEvent::Lagged { dropped: 4 }));
        let seqs: Vec<u64> = got[1..]
            .iter()
            .map(|item| match item {
                StreamEvent::Event { seq, .. } => *seq,
                other => panic!("unexpected {other:?}"),
            })
            .collect();
        assert_eq!(seqs, vec![4, 5, 6]);
    }

    #[tokio::test]
    async fn consecutive_overflows_coalesce_into_one_notice() {
        let broadcaster = EventBroadcaster::new(2);
        let mut slow = broadcaster.subscribe_with_capacity(EventFilter::all(), 2);

        for seq in 0..3 {
            broadcaster.publish(seq, &scene_added("x"));
        }
        let got = collect(&mut slow, 2).await;
        assert!(matches!(got[0], StreamEvent::Lagged { dropped: 2 }));
        assert!(matches!(got[1], StreamEvent::Event { seq: 2, .. }));
    }

    #[tokio::test]
    async fn fast_subscriber_unaffected_by_slow_one() {
        let broadcaster = EventBroadcaster::new(4);
        let mut slow = broadcaster.subscribe_with_capacity(EventFilter::all(), 2);
        let mut fast = broadcaster.subscribe(EventFilter::all());

        for seq in 0..6 {
            broadcaster.publish(seq, &scene_added("x"));
            // Fast consumer keeps up after each publish.
            assert!(matches!(fast.recv().await, Some(StreamEvent::Event { .. })));
        }

        let got = collect(&mut slow, 2).await;
        assert!(matches!(got[0], StreamEvent::Lagged { dropped: 5 }));
        assert!(matches!(got[1], StreamEvent::Event { seq: 5, .. }));
    }

    #[tokio::test]
    async fn close_all_drains_then_ends_streams() {
        let broadcaster = EventBroadcaster::new(8);
        let mut stream = broadcaster.subscribe(EventFilter::all());
        broadcaster.publish(0, &scene_added("queued"));
        broadcaster.close_all();

        assert!(matches!(
            stream.recv().await,
            Some(StreamEvent::Event { seq: 0, .. })
        ));
        assert_eq!(stream.recv().await, None);
    }

    #[test]
    fn category_and_entity_extraction() {
        let scene_id = SceneId::new();
        let event = Event::Scene(SceneEvent::CurrentChanged { scene_id });
        assert_eq!(category_of(&event), EventCategory::Scene);
        assert_eq!(primary_entity(&event), Some(*scene_id.as_uuid()));

        let reordered = Event::Scene(SceneEvent::Reordered);
        assert_eq!(category_of(&reordered), EventCategory::Scene);
        assert_eq!(primary_entity(&reordered), None);

        let studio = Event::System(SystemEvent::StudioModeChanged { enabled: true });
        assert_eq!(category_of(&studio), EventCategory::System);
        assert_eq!(primary_entity(&studio), None);
    }
}
