//! Owns the background Tokio runtime running the core actor, and pumps core
//! events into the GTK main loop as Relm4 messages.
//!
//! Threading model (PLAN.md §57): the core actor lives on a dedicated
//! background thread's Tokio runtime and never touches GTK; the GTK main
//! thread never blocks on Tokio (only lock-free `tokio::sync` primitives are
//! awaited, and only from futures spawned on the GLib main context). The
//! single crossing point is [`CoreBridge::spawn_event_pump`], which forwards
//! [`StreamEvent`]s into a [`relm4::Sender`] — a thread-safe (`flume`-based)
//! channel whose receiver is polled by the component runtime on the GTK main
//! loop. No polling, no shared mutable state.

use std::io;
use std::sync::mpsc as std_mpsc;
use std::thread::JoinHandle;

use prismcast_app::{AppHandle, CoreConfig, EventFilter, StreamEvent};
use prismcast_core::Event;
use tracing::{debug, error, info};

/// One item crossing from the core's event stream into the UI.
///
/// The UI maps these onto its own input message type via the closure passed
/// to [`CoreBridge::spawn_event_pump`], keeping this module free of any
/// component knowledge.
#[derive(Debug)]
pub enum PumpEvent {
    /// A committed domain event with its global sequence number.
    Event {
        /// Global event sequence number.
        seq: u64,
        /// The committed event.
        event: Event,
    },
    /// The UI subscription fell behind; the UI must re-read the snapshot.
    Lagged {
        /// How many events were dropped.
        dropped: u64,
    },
    /// The core shut down and the stream is drained.
    Closed,
}

/// Owns the core actor and the Tokio runtime it runs on.
///
/// Cheap to move into the root component; the actor itself is shared through
/// the cloneable [`AppHandle`].
pub struct CoreBridge {
    handle: AppHandle,
    runtime: tokio::runtime::Handle,
}

impl CoreBridge {
    /// Spawns the background thread hosting the Tokio runtime and the core
    /// actor, blocking until the actor is up. Returns the bridge and the
    /// thread's join handle; the thread exits once the actor has shut down
    /// (see [`AppHandle::shutdown`]), so joining it after the GTK app quits
    /// gives a clean process teardown.
    pub fn spawn_background() -> io::Result<(Self, JoinHandle<()>)> {
        let (tx, rx) = std_mpsc::channel::<(AppHandle, tokio::runtime::Handle)>();
        let join = std::thread::Builder::new()
            .name("prismcast-core".to_string())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        error!(%error, "failed to build Tokio runtime");
                        return;
                    }
                };
                // Must happen inside the runtime: spawning the actor calls
                // tokio::spawn.
                let handle = AppHandle::spawn(CoreConfig::default());
                if tx.send((handle.clone(), runtime.handle().clone())).is_err() {
                    error!("UI side disappeared before the core finished booting");
                    return;
                }
                info!("core actor running on background Tokio runtime");
                // Park the thread until the actor exits; dropping the runtime
                // afterwards shuts everything down.
                runtime.block_on(handle.closed());
                info!("core runtime stopped");
            })?;
        match rx.recv() {
            Ok((handle, runtime)) => Ok((Self { handle, runtime }, join)),
            Err(_) => {
                // The thread failed before handing over the actor; join it so
                // its error is logged, then report the boot failure.
                let _ = join.join();
                Err(io::Error::other("core runtime failed to start"))
            }
        }
    }

    /// The core actor handle. Clone freely; reads never touch the command
    /// queue (see [`AppHandle::snapshot`]).
    pub fn handle(&self) -> &AppHandle {
        &self.handle
    }

    /// Spawns the event pump on the background runtime: every core event is
    /// mapped into a Relm4 input message and sent to the component. The pump
    /// stops on its own when the receiver is dropped or the core closes the
    /// stream (a final `map(PumpEvent::Closed)` is sent in the latter case).
    pub fn spawn_event_pump<M>(
        &self,
        sender: relm4::Sender<M>,
        map: impl Fn(PumpEvent) -> M + Send + 'static,
    ) where
        M: Send + 'static,
    {
        let mut stream = self.handle.subscribe(EventFilter::all());
        self.runtime.spawn(async move {
            while let Some(item) = stream.recv().await {
                let event = match item {
                    StreamEvent::Event { seq, event } => PumpEvent::Event { seq, event },
                    StreamEvent::Lagged { dropped } => PumpEvent::Lagged { dropped },
                };
                if sender.send(map(event)).is_err() {
                    debug!("UI receiver gone; stopping event pump");
                    return;
                }
            }
            let _ = sender.send(map(PumpEvent::Closed));
        });
    }
}
