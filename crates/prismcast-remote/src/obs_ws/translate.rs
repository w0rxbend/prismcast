//! Domain event → obs `Event` (op 5) translation.
//!
//! **Stub for the OBSWS-001 foundation slice**: the session engine's gating
//! (subscription bitmask → native category set) is real, but no domain event
//! is translated to an obs event yet — `event_to_obs` returns `None` for
//! everything, so identified sessions receive no events. The translation
//! slice replaces this stub; implementations must set `event_intent` with
//! the gating bit (see [`super::bitmask::intent_for_category`], using the
//! finer per-event bits like `Filters`/`SceneItems` where the obs event
//! taxonomy distinguishes them).

use prismcast_core::event::Event;

use super::proto;

/// Translates one domain event into an obs `Event` message. Returns `None`
/// for events with no obs-websocket 5.x representation (and, this slice, for
/// all events).
pub(crate) fn event_to_obs(_event: &Event) -> Option<proto::Event> {
    None
}
