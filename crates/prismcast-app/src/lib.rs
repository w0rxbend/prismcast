//! # prismcast-app
//!
//! Application core **services** layer (PLAN.md §2, §57; ADR-0005). Unlike
//! `prismcast-core` (pure domain), this crate runs on Tokio: it owns the core
//! actor, the command dispatcher's authorization hook, the event broadcaster,
//! immutable snapshots, and the undo service.
//!
//! **Layer: Application Core services.** Dependency direction is
//! `domain (prismcast-core) <- app (here) <- remote/web/ui`; this crate must
//! not depend on `prismcast-protocol` (that mapping happens in
//! `prismcast-remote`).
//!
//! ## Architecture
//!
//! ```text
//! controllers (GTK / CLI / WS / IPC / web)      ← interchangeable (PLAN §76)
//!        │  CommandEnvelope (bounded mpsc)
//!        ▼
//!   CoreActor ── owns AppState, applies commands sequentially
//!        │      ├── authz check (dispatch::Permissions) before state access
//!        │      ├── prismcast_core::apply → Vec<Event>
//!        │      ├── UndoService records pre-state inverses (PLAN §59)
//!        ├──► EventBroadcaster: bounded per-subscriber queues,
//!        │      slow-consumer drop-oldest + Lagged notice (broadcaster docs)
//!        └──► watch-published Arc<AppSnapshot> per applied command
//!
//! reads: AppHandle::snapshot()/query()/subscribe() — never touch the queue
//! ```
//!
//! Quick start:
//!
//! ```no_run
//! use prismcast_app::{AppHandle, CoreConfig, EventFilter};
//! use prismcast_core::Command;
//!
//! # async fn example() {
//! let app = AppHandle::spawn(CoreConfig::default());
//! let mut events = app.subscribe(EventFilter::all());
//! let response = app.dispatch(Command::AddScene { name: "Main".into() }).await.unwrap();
//! let snapshot = app.snapshot();
//! assert_eq!(snapshot.revision(), 1);
//! # }
//! ```

pub mod actor;
pub mod broadcaster;
pub mod dispatch;
pub mod snapshot;
pub mod undo;

pub use actor::{
    AppHandle, CommandEnvelope, CommandResponse, CoreConfig, HandleError, DEFAULT_COMMAND_CAPACITY,
};
pub use broadcaster::{
    category_of, primary_entity, EventBroadcaster, EventCategory, EventFilter, EventStream,
    StreamEvent, DEFAULT_SUBSCRIBER_CAPACITY, MIN_SUBSCRIBER_CAPACITY,
};
pub use dispatch::{required_permission, Permission, Permissions, Query, QueryResponse};
pub use snapshot::AppSnapshot;
pub use undo::{UndoEntry, UndoService, DEFAULT_UNDO_CAPACITY};
