//! The task manager: mints [`TaskExecutor`]s, owns the shutdown signal, the
//! drain-guard counter, and the critical-task failure channel.

use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use futures::FutureExt;
use tokio::sync::{mpsc, watch};
use tracing::{error, info};

use crate::shutdown::{
    DrainOutcome, GracefulShutdown, GracefulShutdownGuard, Shutdown, ShutdownTrigger,
};
use crate::task::TaskHandle;

/// One queued wake-up is enough to force a drain recheck.
const DRAIN_WAKE_BUF: usize = 1;

/// Owns the task lifecycle: hands out [`TaskExecutor`]s, observes critical
/// failures, and drives the bounded drain. Dropping it fires shutdown.
#[derive(Debug)]
pub struct TaskManager {
    executor: TaskExecutor,
    drained_rx: mpsc::Receiver<()>,
    critical_rx: mpsc::UnboundedReceiver<String>,
}

impl TaskManager {
    /// New manager spawning on the ambient tokio runtime; call within one.
    pub fn new() -> Self {
        let (signal_tx, _signal_rx) = watch::channel(false);
        let (drained_tx, drained_rx) = mpsc::channel(DRAIN_WAKE_BUF);
        let (critical_tx, critical_rx) = mpsc::unbounded_channel();
        Self {
            executor: TaskExecutor {
                handle: tokio::runtime::Handle::current(),
                signal_tx,
                drained_tx,
                outstanding: Arc::new(AtomicUsize::new(0)),
                critical_tx,
            },
            drained_rx,
            critical_rx,
        }
    }

    /// A clonable executor spawning onto this manager.
    pub fn executor(&self) -> TaskExecutor {
        self.executor.clone()
    }

    /// A shutdown signal a task awaits via [`Shutdown::recv`].
    pub fn subscribe(&self) -> Shutdown {
        Shutdown {
            rx: self.executor.signal_tx.subscribe(),
        }
    }

    /// A trigger that fires the shutdown signal.
    pub fn trigger(&self) -> ShutdownTrigger {
        ShutdownTrigger {
            signal_tx: self.executor.signal_tx.clone(),
        }
    }

    /// Resolves with the task name when a critical task ends or panics;
    /// shutdown is already signalled by then.
    pub async fn on_critical_failure(&mut self) -> String {
        match self.critical_rx.recv().await {
            Some(name) => name,
            // No live executor, so no critical task can ever end.
            None => std::future::pending().await,
        }
    }

    /// Fire the shutdown signal, then wait until every graceful guard has
    /// released or `timeout` elapses. Firing is idempotent.
    pub async fn graceful_shutdown_with_timeout(mut self, timeout: Duration) -> DrainOutcome {
        self.executor.signal_tx.send_replace(true);
        let deadline = tokio::time::sleep(timeout);
        tokio::pin!(deadline);
        loop {
            if self.executor.outstanding.load(Ordering::SeqCst) == 0 {
                return DrainOutcome::Drained;
            }
            tokio::select! {
                () = &mut deadline => {
                    return DrainOutcome::TimedOut {
                        outstanding: self.executor.outstanding.load(Ordering::SeqCst),
                    };
                }
                // Wake-ups coalesce; the count above is authoritative.
                _ = self.drained_rx.recv() => {}
            }
        }
    }
}

impl Default for TaskManager {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for TaskManager {
    /// A drop without a drain still stops the runtime.
    fn drop(&mut self) {
        self.executor.signal_tx.send_replace(true);
    }
}

/// Clonable spawn surface bound to one [`TaskManager`].
#[derive(Debug, Clone)]
pub struct TaskExecutor {
    handle: tokio::runtime::Handle,
    signal_tx: watch::Sender<bool>,
    drained_tx: mpsc::Sender<()>,
    outstanding: Arc<AtomicUsize>,
    critical_tx: mpsc::UnboundedSender<String>,
}

impl TaskExecutor {
    /// Spawn a regular task; it is not awaited on shutdown.
    pub fn spawn<F>(&self, fut: F) -> TaskHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        TaskHandle(self.handle.spawn(fut))
    }

    /// Spawn a task whose end, by return or panic, shuts the runtime down.
    pub fn spawn_critical<F>(&self, name: &str, fut: F) -> TaskHandle<()>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let name = name.to_owned();
        let signal_tx = self.signal_tx.clone();
        let critical_tx = self.critical_tx.clone();
        TaskHandle(self.handle.spawn(async move {
            match AssertUnwindSafe(fut).catch_unwind().await {
                Ok(()) => info!(task = %name, "critical task ended, shutting down"),
                Err(_) => error!(task = %name, "critical task panicked, shutting down"),
            }
            signal_tx.send_replace(true);
            let _ = critical_tx.send(name);
        }))
    }

    /// Spawn a task that flushes at shutdown: `f` receives a
    /// [`GracefulShutdown`] whose guard blocks the drain until dropped.
    pub fn spawn_graceful<F, Fut>(&self, f: F) -> TaskHandle<Fut::Output>
    where
        F: FnOnce(GracefulShutdown) -> Fut,
        Fut: Future + Send + 'static,
        Fut::Output: Send + 'static,
    {
        let graceful = GracefulShutdown::new(
            Shutdown {
                rx: self.signal_tx.subscribe(),
            },
            GracefulShutdownGuard::new(self.outstanding.clone(), self.drained_tx.clone()),
        );
        TaskHandle(self.handle.spawn(f(graceful)))
    }

    /// Run a blocking closure on the blocking pool.
    pub fn spawn_blocking<F, T>(&self, f: F) -> TaskHandle<T>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        TaskHandle(self.handle.spawn_blocking(f))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;

    use super::*;
    use crate::task::TaskExit;

    #[tokio::test]
    async fn spawn_runs_the_future_to_its_result() {
        let manager = TaskManager::new();
        let handle = manager.executor().spawn(async { TaskExit::ReceiverGone });
        assert_eq!(handle.join().await, Some(TaskExit::ReceiverGone));
    }

    #[tokio::test]
    async fn spawn_critical_panic_triggers_shutdown() {
        let mut manager = TaskManager::new();
        let mut signal = manager.subscribe();
        manager
            .executor()
            .spawn_critical("boom", async { panic!("boom") });
        signal.recv().await;
        assert_eq!(manager.on_critical_failure().await, "boom");
    }

    #[tokio::test]
    async fn spawn_critical_return_triggers_shutdown() {
        let mut manager = TaskManager::new();
        let mut signal = manager.subscribe();
        manager.executor().spawn_critical("done", async {});
        signal.recv().await;
        assert_eq!(manager.on_critical_failure().await, "done");
    }

    #[tokio::test]
    async fn graceful_guard_blocks_the_drain_until_dropped() {
        let manager = TaskManager::new();
        let flushed = Arc::new(AtomicBool::new(false));
        let seen = flushed.clone();
        manager
            .executor()
            .spawn_graceful(move |graceful| async move {
                let guard = graceful.await;
                tokio::time::sleep(Duration::from_millis(20)).await;
                seen.store(true, Ordering::SeqCst);
                drop(guard);
            });
        assert_eq!(
            manager
                .graceful_shutdown_with_timeout(Duration::from_secs(5))
                .await,
            DrainOutcome::Drained
        );
        assert!(
            flushed.load(Ordering::SeqCst),
            "flush ran before the drain returned"
        );
    }

    #[tokio::test]
    async fn drain_times_out_with_the_outstanding_count() {
        let manager = TaskManager::new();
        manager.executor().spawn_graceful(|graceful| async move {
            let _guard = graceful.await;
            tokio::time::sleep(Duration::from_secs(60)).await;
        });
        assert_eq!(
            manager
                .graceful_shutdown_with_timeout(Duration::from_millis(50))
                .await,
            DrainOutcome::TimedOut { outstanding: 1 }
        );
    }

    #[tokio::test]
    async fn signal_fired_before_subscribe_is_not_lost() {
        let manager = TaskManager::new();
        manager.trigger().fire();
        let mut signal = manager.subscribe();
        signal.recv().await;
    }

    #[tokio::test]
    async fn dropping_the_manager_fires_shutdown() {
        let manager = TaskManager::new();
        let mut signal = manager.subscribe();
        drop(manager);
        signal.recv().await;
    }

    #[tokio::test]
    async fn spawn_blocking_yields_the_closure_result() {
        let manager = TaskManager::new();
        let handle = manager.executor().spawn_blocking(|| 7u32);
        assert_eq!(handle.join().await, Some(7));
    }

    /// A graceful task counts against the drain from spawn, so a drain that
    /// starts before the task first polls still waits for it.
    #[tokio::test]
    async fn drain_waits_for_a_graceful_task_that_has_not_polled() {
        let manager = TaskManager::new();
        manager.executor().spawn_graceful(|graceful| async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            drop(graceful.await);
        });
        assert_eq!(
            manager
                .graceful_shutdown_with_timeout(Duration::from_secs(5))
                .await,
            DrainOutcome::Drained
        );
    }
}
