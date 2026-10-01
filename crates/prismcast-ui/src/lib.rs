//! # prismcast-ui
//!
//! GTK4/Relm4/libadwaita desktop application (binary: `prismcast`).
//!
//! **Layer: Interfaces (UI).** Relm4 components contain no media logic;
//! the UI only sends Commands to the application core and renders
//! Events/Snapshots.
//!
//! ## Wiring (PLAN.md §57, §76)
//!
//! ```text
//! GTK main thread                          background thread "prismcast-core"
//! ┌──────────────────────────────┐         ┌──────────────────────────────┐
//! │ AppModel (Relm4 root)        │         │ Tokio runtime                │
//! │  ├─ scenes / sources /       │ Command │  └─ CoreActor (owns AppState)│
//! │  │  outputs panels           ├────────►│        │                     │
//! │  └─ header / preview /       │ oneshot │        ▼                     │
//! │     transition bar           │ command │  Snapshot watch            │
//! │           ▲                  │◄────────┤        │                     │
//! │           │ AppMsg::Pump     │ Sender  │  snapshot pump (tokio task)     │
//! └──────────────────────────────┘         └──────────────────────────────┘
//! ```

pub mod app;
pub mod bridge;
pub mod components;
pub mod presentation;

mod capture_parent;
pub mod preview_editor;
