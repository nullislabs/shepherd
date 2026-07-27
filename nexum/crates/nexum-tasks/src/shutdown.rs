//! Shutdown signalling: one latched watch signal fanned to tasks, plus the
//! drain guards the manager blocks on before exit.

use std::future::{Future, IntoFuture};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::sync::{mpsc, watch};

/// Outcome of a bounded drain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrainOutcome {
    /// Every graceful guard released before the deadline.
    Drained,
    /// The deadline fired with `outstanding` guards still held; force exit.
    TimedOut {
        /// Guards still held when the deadline fired.
        outstanding: usize,
    },
}

/// Cheap, clonable handle that fires the shutdown signal. Idempotent.
#[derive(Debug, Clone)]
pub struct ShutdownTrigger {
    pub(crate) signal_tx: watch::Sender<bool>,
}

impl ShutdownTrigger {
    /// Fire the shutdown signal. Latched, so a fire before the first
    /// subscribe is not lost.
    pub fn fire(&self) {
        self.signal_tx.send_replace(true);
    }
}

/// A shutdown signal a task awaits. Resolves once fired, already fired, or
/// the manager is dropped.
#[derive(Debug, Clone)]
pub struct Shutdown {
    pub(crate) rx: watch::Receiver<bool>,
}

impl Shutdown {
    /// Await the shutdown signal; resolves immediately if already fired and
    /// is safe to await more than once.
    pub async fn recv(&mut self) {
        if *self.rx.borrow_and_update() {
            return;
        }
        // A `changed` error means the manager was dropped: treat as shutdown
        // so the task winds down rather than hanging.
        while self.rx.changed().await.is_ok() {
            if *self.rx.borrow_and_update() {
                return;
            }
        }
    }
}

/// Resolves when shutdown is signalled, yielding the guard the task holds
/// while it flushes. The guard is counted from spawn, so a drain that starts
/// before the task polls still waits for it.
#[derive(Debug)]
pub struct GracefulShutdown {
    signal: Shutdown,
    guard: GracefulShutdownGuard,
}

impl GracefulShutdown {
    pub(crate) fn new(signal: Shutdown, guard: GracefulShutdownGuard) -> Self {
        Self { signal, guard }
    }
}

impl IntoFuture for GracefulShutdown {
    type Output = GracefulShutdownGuard;
    type IntoFuture = Pin<Box<dyn Future<Output = GracefulShutdownGuard> + Send>>;

    fn into_future(self) -> Self::IntoFuture {
        let Self { mut signal, guard } = self;
        Box::pin(async move {
            signal.recv().await;
            guard
        })
    }
}

/// Held by a task that must flush before exit; dropping it releases the
/// drain.
#[derive(Debug)]
pub struct GracefulShutdownGuard {
    outstanding: Arc<AtomicUsize>,
    drained_tx: mpsc::Sender<()>,
}

impl GracefulShutdownGuard {
    /// Counts itself into `outstanding` on creation.
    pub(crate) fn new(outstanding: Arc<AtomicUsize>, drained_tx: mpsc::Sender<()>) -> Self {
        outstanding.fetch_add(1, Ordering::SeqCst);
        Self {
            outstanding,
            drained_tx,
        }
    }
}

impl Drop for GracefulShutdownGuard {
    fn drop(&mut self) {
        self.outstanding.fetch_sub(1, Ordering::SeqCst);
        // Queue a drain wake-up; a full queue already holds one.
        let _ = self.drained_tx.try_send(());
    }
}
