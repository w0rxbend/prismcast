//! The panels of the main window. Each is a [`SimpleComponent`] holding
//! presentation state only: inputs carry immutable [`AppSnapshot`]s, outputs
//! carry user intents up to the root component, which alone talks to the
//! core (AGENTS.md: no media/domain logic in Relm4 components).

pub mod outputs;
pub mod scenes;
pub mod sources;
