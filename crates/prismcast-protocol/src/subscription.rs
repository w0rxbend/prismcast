//! Event subscription model (RES-007 §Event subscription model, conclusion
//! 3).
//!
//! Instead of obs-websocket's flat category bitmask with magic high-volume
//! bits, the native protocol uses a **typed subscription set**: each entry
//! names a [`EventCategory`], optionally restricts delivery to specific
//! entities (`entity_ids`), and optionally sets a throttle interval
//! (`throttle_ms`) so rate is explicit per subscription rather than implied
//! by a magic bit.
//!
//! Semantics:
//! - An event is delivered to a session iff its category is subscribed and
//!   (`entity_ids` is empty *or* the event's primary entity is listed).
//! - Events whose primary entity is not determinable (e.g.
//!   `scene_reordered`) are delivered when the category is subscribed with
//!   an empty filter; a non-empty filter suppresses them.
//! - `throttle_ms` sets the minimum interval between deliveries per
//!   subscribed entity; within a window the server coalesces to the latest
//!   state. It is mandatory-effective for [`EventCategory::Meter`] (default
//!   [`DEFAULT_METER_INTERVAL_MS`]) and optional elsewhere.
//! - The whole set can be replaced at runtime with the
//!   `update_subscriptions` request; there is no incremental add/remove in
//!   protocol v1 (keep the wire small; replace is idempotent and simple).

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Default meter delivery interval in milliseconds (matches obs-websocket's
/// 50 ms meter cadence).
pub const DEFAULT_METER_INTERVAL_MS: u32 = 50;

/// Smallest throttle interval the server will honor. Smaller requests are
/// rejected at validation time, not silently clamped.
pub const MIN_THROTTLE_MS: u32 = 10;

/// Largest throttle interval accepted (10 minutes); larger values indicate
/// client bugs more often than intent.
pub const MAX_THROTTLE_MS: u32 = 600_000;

/// Event categories a session can subscribe to.
///
/// The first six map one-to-one onto the core `Event` domains
/// (`prismcast_core::Event`); [`EventCategory::Meter`] is high-volume,
/// media-layer-originated, and never included in default subscriptions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventCategory {
    /// Session/server lifecycle notices (resync hints, shutdown warnings).
    General,
    /// Scene and scene-item changes.
    Scene,
    /// Source changes.
    Source,
    /// Audio mixer/routing changes.
    Audio,
    /// Output graph changes.
    Output,
    /// Studio mode, transitions, profiles, collections.
    System,
    /// High-volume audio level meters; requires explicit opt-in.
    Meter,
}

impl EventCategory {
    /// Standard (non-high-volume) categories, in stable order.
    pub const STANDARD: [Self; 6] = [
        Self::General,
        Self::Scene,
        Self::Source,
        Self::Audio,
        Self::Output,
        Self::System,
    ];

    /// Whether this category is high-volume and excluded from defaults.
    pub fn is_high_volume(self) -> bool {
        matches!(self, Self::Meter)
    }
}

/// One category subscription with optional per-entity filter and throttle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subscription {
    /// The subscribed category.
    pub category: EventCategory,
    /// Restrict delivery to these entity IDs (scene/source/output/bus IDs,
    /// by category). Empty = all entities in the category.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entity_ids: Vec<Uuid>,
    /// Minimum interval between deliveries per entity, in milliseconds.
    /// For [`EventCategory::Meter`] the effective interval defaults to
    /// [`DEFAULT_METER_INTERVAL_MS`] when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub throttle_ms: Option<u32>,
}

impl Subscription {
    /// Subscribes a whole category with no entity filter or throttle.
    pub fn category(category: EventCategory) -> Self {
        Self {
            category,
            entity_ids: Vec::new(),
            throttle_ms: None,
        }
    }

    /// Subscribes the meter category for specific sources.
    pub fn meters(entity_ids: Vec<Uuid>, interval_ms: Option<u32>) -> Self {
        Self {
            category: EventCategory::Meter,
            entity_ids,
            throttle_ms: interval_ms,
        }
    }

    /// The delivery interval this subscription effectively requests for
    /// high-volume categories.
    pub fn effective_interval_ms(&self) -> Option<u32> {
        match self.category {
            EventCategory::Meter => Some(self.throttle_ms.unwrap_or(DEFAULT_METER_INTERVAL_MS)),
            _ => self.throttle_ms,
        }
    }
}

/// The full subscription set of a session. At most one entry per category
/// (use one entry with an `entity_ids` list rather than several entries).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SubscriptionSet {
    /// The subscribed categories; empty = receive no events.
    pub entries: Vec<Subscription>,
}

impl SubscriptionSet {
    /// The default set: all standard categories, no high-volume categories
    /// (mirrors obs-websocket's `All` minus high-volume semantics).
    pub fn default_all() -> Self {
        Self {
            entries: EventCategory::STANDARD
                .into_iter()
                .map(Subscription::category)
                .collect(),
        }
    }

    /// The empty set: no events.
    pub fn none() -> Self {
        Self::default()
    }

    /// Returns the entry for a category, if subscribed.
    pub fn get(&self, category: EventCategory) -> Option<&Subscription> {
        self.entries.iter().find(|s| s.category == category)
    }

    /// Validates the set per the rules above.
    pub fn validate(&self) -> Result<(), SubscriptionError> {
        for (index, entry) in self.entries.iter().enumerate() {
            if self.entries[..index]
                .iter()
                .any(|prev| prev.category == entry.category)
            {
                return Err(SubscriptionError::DuplicateCategory(entry.category));
            }
            if let Some(ms) = entry.throttle_ms {
                if !(MIN_THROTTLE_MS..=MAX_THROTTLE_MS).contains(&ms) {
                    return Err(SubscriptionError::ThrottleOutOfRange {
                        category: entry.category,
                        throttle_ms: ms,
                    });
                }
            }
            if entry.entity_ids.len() > MAX_ENTITY_FILTER {
                return Err(SubscriptionError::TooManyEntityFilters(entry.category));
            }
        }
        Ok(())
    }
}

/// Maximum entity IDs in a single subscription's filter.
pub const MAX_ENTITY_FILTER: usize = 256;

/// Subscription validation failures (surfaced as
/// [`crate::error::ErrorKind::InvalidField`] responses).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SubscriptionError {
    /// The same category appears twice in one set.
    #[error("duplicate subscription for category {0:?}")]
    DuplicateCategory(EventCategory),
    /// A throttle interval outside `[MIN_THROTTLE_MS, MAX_THROTTLE_MS]`.
    #[error("throttle {throttle_ms} ms for {category:?} is outside {MIN_THROTTLE_MS}..={MAX_THROTTLE_MS}")]
    ThrottleOutOfRange {
        /// The offending category.
        category: EventCategory,
        /// The offending interval.
        throttle_ms: u32,
    },
    /// More than [`MAX_ENTITY_FILTER`] entity IDs in one filter.
    #[error("too many entity filters for {0:?} (max {MAX_ENTITY_FILTER})")]
    TooManyEntityFilters(EventCategory),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_all_covers_standard_categories_but_not_meter() {
        let set = SubscriptionSet::default_all();
        assert_eq!(set.entries.len(), EventCategory::STANDARD.len());
        assert!(set.get(EventCategory::Meter).is_none());
        assert!(set.get(EventCategory::Scene).is_some());
    }

    #[test]
    fn serde_roundtrip() {
        let set = SubscriptionSet {
            entries: vec![
                Subscription::category(EventCategory::Scene),
                Subscription::meters(vec![Uuid::new_v4()], Some(100)),
            ],
        };
        let json = serde_json::to_string(&set).unwrap();
        assert_eq!(set, serde_json::from_str(&json).unwrap());
    }

    #[test]
    fn empty_optional_fields_are_omitted() {
        let sub = Subscription::category(EventCategory::Output);
        let json = serde_json::to_string(&sub).unwrap();
        assert_eq!(json, r#"{"category":"output"}"#);
    }

    #[test]
    fn meter_interval_defaults_and_overrides() {
        assert_eq!(
            Subscription::meters(Vec::new(), None).effective_interval_ms(),
            Some(DEFAULT_METER_INTERVAL_MS)
        );
        assert_eq!(
            Subscription::meters(Vec::new(), Some(200)).effective_interval_ms(),
            Some(200)
        );
        assert_eq!(
            Subscription::category(EventCategory::Scene).effective_interval_ms(),
            None
        );
    }

    #[test]
    fn validation_rejects_duplicates_and_bad_throttles() {
        let dup = SubscriptionSet {
            entries: vec![
                Subscription::category(EventCategory::Scene),
                Subscription::category(EventCategory::Scene),
            ],
        };
        assert_eq!(
            dup.validate(),
            Err(SubscriptionError::DuplicateCategory(EventCategory::Scene))
        );

        let fast = SubscriptionSet {
            entries: vec![Subscription::meters(Vec::new(), Some(1))],
        };
        assert!(matches!(
            fast.validate(),
            Err(SubscriptionError::ThrottleOutOfRange { .. })
        ));

        let too_many = SubscriptionSet {
            entries: vec![Subscription {
                category: EventCategory::Source,
                entity_ids: vec![Uuid::nil(); MAX_ENTITY_FILTER + 1],
                throttle_ms: None,
            }],
        };
        assert_eq!(
            too_many.validate(),
            Err(SubscriptionError::TooManyEntityFilters(
                EventCategory::Source
            ))
        );

        assert!(SubscriptionSet::default_all().validate().is_ok());
    }
}
