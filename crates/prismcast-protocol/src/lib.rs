//! # prismcast-protocol
//!
//! Versioned wire protocol types for the IPC and WebSocket interfaces.
//!
//! Per PLAN.md §75 these protocol structs are *not* domain structs and must
//! never be reused as such; mapping happens at the interface boundary.
//!
//! **Layer: Interfaces.**
