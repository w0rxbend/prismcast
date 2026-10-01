# ADR-0012: Coalesced snapshot wakeups for desktop rendering

## Context

Core events are broadcast before snapshot publication. Using those events to
trigger a snapshot read can leave the final render stale. Relm4 uses unbounded
channels, so forwarding every event and every panel snapshot also permits
unbounded rendering backlog.

## Decision

Render from `AppHandle::subscribe_snapshots()` after committed publication.
Each consumer owns a latest-snapshot reader and an atomic pending bit. Only the
first producer notification queues a payload-free reader token. Consumers clear
the pending bit before reading the latest snapshot: concurrent publications are
either included in that read or can queue another wakeup. Root and all three
panels have independent budgets, each with at most one queued refresh. No event
payloads or historical snapshots are retained by rendering queues. User intents
and command replies retain their existing semantics.

The root aborts its owned snapshot pump when destroyed. Core startup runs inside
the background runtime's `block_on` context. Transition rendering suppresses
GTK notifications inside the signal callback before they enter Relm4's queue.

## Validation

Headless tests cover ordinary-thread startup/dispatch/shutdown, notification
bursts, failed delivery, acknowledgement interleavings, latest committed state,
and receiver destruction. GTK signal behavior additionally requires display
smoke validation.

## Status

Accepted for BRIDGE-001.
