//! Background core runtime and bounded latest-snapshot notifications.
//!
//! Snapshot watch publication happens after the command commits. Each UI
//! consumer retains at most one refresh notification, regardless of producer
//! speed. GTK objects never cross threads.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc as std_mpsc, Arc};
use std::thread::JoinHandle;

use prismcast_app::{AppHandle, AppSnapshot, CoreConfig};
use tracing::{error, info};

/// A latest-snapshot reader with one outstanding notification per consumer.
/// Clones share the pending bit; distinct consumers need distinct readers.
#[derive(Clone)]
pub struct SnapshotRefresh {
    handle: AppHandle,
    pending: Arc<AtomicBool>,
}

impl std::fmt::Debug for SnapshotRefresh {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SnapshotRefresh").finish_non_exhaustive()
    }
}

impl SnapshotRefresh {
    /// Creates an independent notification budget for one consumer.
    pub fn new(handle: AppHandle) -> Self {
        Self {
            handle,
            pending: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Queues a wakeup only when this consumer has none pending.
    /// Returns false if delivery failed, so producers can stop.
    pub fn notify(&self, send: impl FnOnce(Self) -> bool) -> bool {
        if self.pending.swap(true, Ordering::AcqRel) {
            return true;
        }
        if send(self.clone()) {
            true
        } else {
            self.pending.store(false, Ordering::Release);
            false
        }
    }

    /// Acknowledges the wakeup before reading the latest committed snapshot.
    /// Clearing before reading prevents a concurrent publication from being
    /// lost: it is either included in this read or queues another wakeup.
    pub fn read(&self) -> Arc<AppSnapshot> {
        self.acknowledge();
        self.handle.snapshot()
    }

    fn acknowledge(&self) {
        self.pending.store(false, Ordering::Release);
    }
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
        let (tx, rx) = std_mpsc::sync_channel::<(AppHandle, tokio::runtime::Handle)>(1);
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
                runtime.block_on(async {
                    let handle = AppHandle::spawn(CoreConfig::default());
                    if tx
                        .send((handle.clone(), tokio::runtime::Handle::current()))
                        .is_err()
                    {
                        error!("UI side disappeared before the core finished booting");
                        handle.shutdown().await;
                        return;
                    }
                    info!("core actor running on background Tokio runtime");
                    handle.closed().await;
                });
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

    /// Watches committed snapshots and sends coalesced wakeups to the UI.
    /// The returned task must be aborted when its component is destroyed.
    pub fn spawn_snapshot_pump<M>(
        &self,
        sender: relm4::Sender<M>,
        map: impl Fn(SnapshotRefresh) -> M + Send + 'static,
    ) -> tokio::task::JoinHandle<()>
    where
        M: Send + 'static,
    {
        let mut snapshots = self.handle.subscribe_snapshots();
        let refresh = SnapshotRefresh::new(self.handle.clone());
        self.runtime.spawn(async move {
            while snapshots.changed().await.is_ok() {
                snapshots.borrow_and_update();
                if !refresh.notify(|wake| sender.send(map(wake)).is_ok()) {
                    return;
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use prismcast_core::Command;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn ordinary_thread_boot_dispatch_shutdown() {
        let (bridge, thread) = CoreBridge::spawn_background().unwrap();
        bridge.runtime.block_on(async {
            bridge
                .handle
                .dispatch(Command::AddScene {
                    name: "Main".into(),
                })
                .await
                .unwrap();
            assert_eq!(bridge.handle.snapshot().revision(), 1);
            bridge.handle.shutdown().await;
        });
        thread.join().unwrap();
    }

    #[test]
    fn notification_budget_and_failed_delivery() {
        let (bridge, thread) = CoreBridge::spawn_background().unwrap();
        let refresh = SnapshotRefresh::new(bridge.handle.clone());
        let sends = AtomicUsize::new(0);
        for _ in 0..10_000 {
            assert!(refresh.notify(|_| {
                sends.fetch_add(1, Ordering::Relaxed);
                true
            }));
        }
        assert_eq!(sends.load(Ordering::Relaxed), 1);
        refresh.read();
        assert!(!refresh.notify(|_| false));
        assert!(refresh.notify(|_| {
            sends.fetch_add(1, Ordering::Relaxed);
            true
        }));
        assert_eq!(sends.load(Ordering::Relaxed), 2);
        bridge.runtime.block_on(bridge.handle.shutdown());
        thread.join().unwrap();
    }

    #[test]
    fn publications_at_each_acknowledgement_boundary_are_not_lost() {
        let (bridge, thread) = CoreBridge::spawn_background().unwrap();
        let refresh = SnapshotRefresh::new(bridge.handle.clone());
        let publish = |name: &str| {
            bridge
                .runtime
                .block_on(
                    bridge
                        .handle
                        .dispatch(Command::AddScene { name: name.into() }),
                )
                .unwrap()
        };
        // Before acknowledgement: existing wakeup reads the latest value.
        refresh.notify(|_| true);
        publish("before");
        assert_eq!(refresh.read().revision(), 1);
        // Between acknowledgement and read: the publication may enqueue a
        // second wakeup, but the first read also observes its committed value.
        refresh.notify(|_| true);
        refresh.acknowledge();
        publish("between");
        let mut sends = 0;
        refresh.notify(|_| {
            sends += 1;
            true
        });
        assert_eq!(bridge.handle.snapshot().revision(), 2);
        assert_eq!(sends, 1);
        assert_eq!(refresh.read().revision(), 2);
        // After the read: a fresh wakeup must be deliverable.
        publish("after");
        refresh.notify(|_| {
            sends += 1;
            true
        });
        assert_eq!(sends, 2);
        assert_eq!(refresh.read().revision(), 3);
        bridge.runtime.block_on(bridge.handle.shutdown());
        thread.join().unwrap();
    }

    #[test]
    fn independent_panels_do_not_share_notification_budgets() {
        let (bridge, thread) = CoreBridge::spawn_background().unwrap();
        let panels: Vec<_> = (0..3)
            .map(|_| SnapshotRefresh::new(bridge.handle.clone()))
            .collect();
        let mut sends = [0; 3];
        for _ in 0..100 {
            for (index, panel) in panels.iter().enumerate() {
                panel.notify(|_| {
                    sends[index] += 1;
                    true
                });
            }
        }
        assert_eq!(sends, [1, 1, 1]);
        panels[1].read();
        for (index, panel) in panels.iter().enumerate() {
            panel.notify(|_| {
                sends[index] += 1;
                true
            });
        }
        assert_eq!(sends, [1, 2, 1]);
        bridge.runtime.block_on(bridge.handle.shutdown());
        thread.join().unwrap();
    }

    #[test]
    fn snapshot_pump_reads_final_commit_and_stops_on_closed_receiver() {
        let (bridge, thread) = CoreBridge::spawn_background().unwrap();
        let (sender, receiver) = relm4::channel();
        let pump = bridge.spawn_snapshot_pump(sender, |wake| wake);
        bridge.runtime.block_on(async {
            for index in 0..100 {
                bridge
                    .handle
                    .dispatch(Command::AddScene {
                        name: format!("Scene {index}"),
                    })
                    .await
                    .unwrap();
            }
            let wake = tokio::time::timeout(std::time::Duration::from_secs(5), receiver.recv())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(wake.read().revision(), 100);
            drop(receiver);
            bridge
                .handle
                .dispatch(Command::AddScene {
                    name: "Receiver gone".into(),
                })
                .await
                .unwrap();
            tokio::time::timeout(std::time::Duration::from_secs(5), pump)
                .await
                .unwrap()
                .unwrap();
            bridge.handle.shutdown().await;
        });
        thread.join().unwrap();
    }
}
