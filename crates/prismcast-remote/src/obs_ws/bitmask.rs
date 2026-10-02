//! obs `eventSubscriptions` bitmask ↔ native subscription mapping
//! (OBSWS-001 task notes; RES-007 §Event subscription model).
//!
//! obs gates events with a flat bitmask; Prismcast's native protocol uses a
//! typed [`SubscriptionSet`]. The adapter bridges them at the session
//! boundary: the bitmask a client sends in `Identify`/`Reidentify` becomes a
//! native set, and emitted events carry the obs bit as their `eventIntent`.
//!
//! Mapping table (obs bit → native categories):
//!
//! | obs bit | native category |
//! |---|---|
//! | `General` | `General` |
//! | `Config` | `System` |
//! | `Scenes` | `Scene` |
//! | `Inputs` | `Source` + `Audio` |
//! | `Transitions` | `System` |
//! | `Filters` | `Source` (filter state is source-domain in Prismcast) |
//! | `Outputs` | `Output` |
//! | `SceneItems` | `Scene` |
//! | `MediaInputs` | `Source` |
//! | `Vendors` | `General` |
//! | `Ui` | `System` |
//! | `Canvases` | — (no native equivalent; accepted, inert) |
//! | `InputVolumeMeters` | `Meter` (inert until meter producers exist) |
//! | `InputActiveStateChanged`, `InputShowStateChanged`, `SceneItemTransformChanged` | — (high-volume; accepted, inert without producers) |
//!
//! Inert bits are accepted silently (subscribing is not an error); they
//! simply admit no events until producers exist.

use prismcast_protocol::subscription::{EventCategory, Subscription, SubscriptionSet};

use super::proto::subscription as obs;

/// Converts an obs `eventSubscriptions` bitmask into the native
/// [`SubscriptionSet`] the session gates delivery with. Duplicate native
/// categories (e.g. `Scenes` and `SceneItems` both mapping to `Scene`) are
/// produced once.
pub fn subscription_set_from_bitmask(mask: u32) -> SubscriptionSet {
    let mut categories: Vec<EventCategory> = Vec::new();
    let mut push = |category: EventCategory| {
        if !categories.contains(&category) {
            categories.push(category);
        }
    };
    if mask & obs::GENERAL != 0 {
        push(EventCategory::General);
    }
    if mask & obs::CONFIG != 0 {
        push(EventCategory::System);
    }
    if mask & obs::SCENES != 0 {
        push(EventCategory::Scene);
    }
    if mask & obs::INPUTS != 0 {
        push(EventCategory::Source);
        push(EventCategory::Audio);
    }
    if mask & obs::TRANSITIONS != 0 {
        push(EventCategory::System);
    }
    if mask & obs::FILTERS != 0 {
        push(EventCategory::Source);
    }
    if mask & obs::OUTPUTS != 0 {
        push(EventCategory::Output);
    }
    if mask & obs::SCENE_ITEMS != 0 {
        push(EventCategory::Scene);
    }
    if mask & obs::MEDIA_INPUTS != 0 {
        push(EventCategory::Source);
    }
    if mask & obs::VENDORS != 0 {
        push(EventCategory::General);
    }
    if mask & obs::UI != 0 {
        push(EventCategory::System);
    }
    if mask & obs::INPUT_VOLUME_METERS != 0 {
        push(EventCategory::Meter);
    }
    SubscriptionSet {
        entries: categories.into_iter().map(Subscription::category).collect(),
    }
}

/// The obs subscription bit a native event category reports as its
/// `eventIntent` when delivered. This is the reverse of the primary mapping
/// above (a native `System` event reports `Config`, a `Source` event reports
/// `Inputs`); finer per-event intents (e.g. `Filters` vs `Inputs` inside the
/// source domain) are the translation slice's job.
pub fn intent_for_category(category: EventCategory) -> u32 {
    match category {
        EventCategory::General => obs::GENERAL,
        EventCategory::Scene => obs::SCENES,
        EventCategory::Source => obs::INPUTS,
        EventCategory::Audio => obs::INPUTS,
        EventCategory::Output => obs::OUTPUTS,
        EventCategory::System => obs::CONFIG,
        EventCategory::Meter => obs::INPUT_VOLUME_METERS,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn categories(mask: u32) -> Vec<EventCategory> {
        subscription_set_from_bitmask(mask)
            .entries
            .into_iter()
            .map(|s| s.category)
            .collect()
    }

    #[test]
    fn none_and_unknown_bits_admit_nothing() {
        assert!(categories(obs::NONE).is_empty());
        assert!(
            categories(1 << 12).is_empty(),
            "unassigned category gap bit"
        );
        assert!(categories(1 << 25).is_empty(), "unknown high bit");
    }

    #[test]
    fn each_category_bit_maps_per_the_table() {
        assert_eq!(categories(obs::GENERAL), [EventCategory::General]);
        assert_eq!(categories(obs::CONFIG), [EventCategory::System]);
        assert_eq!(categories(obs::SCENES), [EventCategory::Scene]);
        assert_eq!(
            categories(obs::INPUTS),
            [EventCategory::Source, EventCategory::Audio]
        );
        assert_eq!(categories(obs::TRANSITIONS), [EventCategory::System]);
        assert_eq!(categories(obs::FILTERS), [EventCategory::Source]);
        assert_eq!(categories(obs::OUTPUTS), [EventCategory::Output]);
        assert_eq!(categories(obs::SCENE_ITEMS), [EventCategory::Scene]);
        assert_eq!(categories(obs::MEDIA_INPUTS), [EventCategory::Source]);
        assert_eq!(categories(obs::VENDORS), [EventCategory::General]);
        assert_eq!(categories(obs::UI), [EventCategory::System]);
        assert!(
            categories(obs::CANVASES).is_empty(),
            "no native canvas category"
        );
    }

    #[test]
    fn duplicate_native_categories_are_produced_once() {
        let set = subscription_set_from_bitmask(obs::SCENES | obs::SCENE_ITEMS);
        assert_eq!(
            set.entries
                .iter()
                .filter(|s| s.category == EventCategory::Scene)
                .count(),
            1
        );
        // The produced set must always pass native validation.
        set.validate().unwrap();
        subscription_set_from_bitmask(obs::ALL | obs::INPUT_VOLUME_METERS)
            .validate()
            .unwrap();
    }

    #[test]
    fn high_volume_bits_are_accepted() {
        assert_eq!(categories(obs::INPUT_VOLUME_METERS), [EventCategory::Meter]);
        assert!(categories(obs::INPUT_ACTIVE_STATE_CHANGED).is_empty());
        assert!(categories(obs::INPUT_SHOW_STATE_CHANGED).is_empty());
        assert!(categories(obs::SCENE_ITEM_TRANSFORM_CHANGED).is_empty());
    }

    #[test]
    fn all_covers_every_native_standard_category() {
        let set = subscription_set_from_bitmask(obs::ALL);
        for category in prismcast_protocol::subscription::EventCategory::STANDARD {
            assert!(
                set.get(category).is_some(),
                "obs All must cover {category:?}"
            );
        }
        assert!(set.get(EventCategory::Meter).is_none());
    }

    #[test]
    fn intent_echoes_the_gating_bit() {
        assert_eq!(intent_for_category(EventCategory::General), obs::GENERAL);
        assert_eq!(intent_for_category(EventCategory::Scene), obs::SCENES);
        assert_eq!(intent_for_category(EventCategory::Source), obs::INPUTS);
        assert_eq!(intent_for_category(EventCategory::Audio), obs::INPUTS);
        assert_eq!(intent_for_category(EventCategory::Output), obs::OUTPUTS);
        assert_eq!(intent_for_category(EventCategory::System), obs::CONFIG);
        assert_eq!(
            intent_for_category(EventCategory::Meter),
            obs::INPUT_VOLUME_METERS
        );
        // Every intent bit must be admitted by the forward mapping:
        // subscribing to the intent's category alone admits the event.
        for category in prismcast_protocol::subscription::EventCategory::STANDARD {
            let intent = intent_for_category(category);
            let set = subscription_set_from_bitmask(intent);
            assert!(
                set.get(category).is_some(),
                "intent bit of {category:?} must admit it"
            );
        }
    }
}
