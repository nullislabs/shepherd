//! Graceful-drain tier: one shutdown signal fanned to actors, plus a guard the
//! top level blocks on until every durable-flush task releases or a deadline
//! forces exit.
//!
//! A durable-flush actor holds a [`DrainGuard`] across its commit so
//! [`ShutdownController::drain`] cannot exit mid-write; abort-only reconnect
//! pumps only [`subscribe`](ShutdownController::subscribe) and never delay the
//! drain.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tokio::sync::{mpsc, watch};

/// Outcome of a [`ShutdownController::drain`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrainOutcome {
    /// Every guard released before the deadline.
    Drained,
    /// The deadline fired with `outstanding` guards still held; force exit.
    TimedOut {
        /// Guards still held when the deadline fired.
        outstanding: usize,
    },
}

/// Top-level shutdown coordinator. Hands [`Shutdown`] receivers and
/// [`DrainGuard`]s to actors and [`drain`](Self::drain)s from the Ctrl-C path.
pub struct ShutdownController {
    signal_tx: watch::Sender<bool>,
    // Retained guard sender; `drain` drops it so `guard_rx` closes once the
    // last issued guard releases. Guards only ever drop, never send.
    guard_tx: mpsc::Sender<()>,
    guard_rx: mpsc::Receiver<()>,
    outstanding: Arc<AtomicUsize>,
}

impl ShutdownController {
    /// Fresh controller with no subscribers and no guards.
    pub fn new() -> Self {
        let (signal_tx, _signal_rx) = watch::channel(false);
        let (guard_tx, guard_rx) = mpsc::channel(1);
        Self {
            signal_tx,
            guard_tx,
            guard_rx,
            outstanding: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// A shutdown signal an actor awaits via [`Shutdown::recv`].
    pub fn subscribe(&self) -> Shutdown {
        Shutdown {
            rx: self.signal_tx.subscribe(),
        }
    }

    /// A trigger that fires the shutdown signal; leaves the controller free to
    /// [`drain`](Self::drain).
    pub fn trigger(&self) -> ShutdownTrigger {
        ShutdownTrigger {
            signal_tx: self.signal_tx.clone(),
        }
    }

    /// A drain guard for a task that must flush durable state before exit.
    pub fn guard(&self) -> DrainGuard {
        self.outstanding.fetch_add(1, Ordering::SeqCst);
        DrainGuard {
            _tx: self.guard_tx.clone(),
            outstanding: self.outstanding.clone(),
        }
    }

    /// Fire the signal, then block until every guard releases or `timeout`
    /// elapses. Firing is idempotent, so this composes with a Ctrl-C listener
    /// that already fired via [`ShutdownTrigger`].
    pub async fn drain(self, timeout: Duration) -> DrainOutcome {
        // `send_replace` latches even with no receiver, so a signal fired
        // before the first `subscribe` is not lost. Plain `send` would err and
        // leave the stored value unchanged.
        self.signal_tx.send_replace(true);
        let ShutdownController {
            guard_tx,
            mut guard_rx,
            outstanding,
            ..
        } = self;
        drop(guard_tx);
        match tokio::time::timeout(timeout, guard_rx.recv()).await {
            // `recv` yields `None` once every guard sender drops; guards never
            // send, so this is the only success shape.
            Ok(_) => DrainOutcome::Drained,
            Err(_) => DrainOutcome::TimedOut {
                outstanding: outstanding.load(Ordering::SeqCst),
            },
        }
    }
}

impl Default for ShutdownController {
    fn default() -> Self {
        Self::new()
    }
}

/// Cheap, clonable handle that fires the shutdown signal.
#[derive(Clone)]
pub struct ShutdownTrigger {
    signal_tx: watch::Sender<bool>,
}

impl ShutdownTrigger {
    /// Fire the shutdown signal. Idempotent.
    pub fn fire(&self) {
        // `send_replace`: latch even with no receiver yet, so a Ctrl-C before
        // the first `subscribe` is not swallowed.
        self.signal_tx.send_replace(true);
    }
}

/// A shutdown signal an actor awaits. Resolves once the controller fires, has
/// already fired, or is dropped.
#[derive(Clone)]
pub struct Shutdown {
    rx: watch::Receiver<bool>,
}

impl Shutdown {
    /// Await the shutdown signal. Resolves immediately if already fired; safe
    /// to await more than once.
    pub async fn recv(&mut self) {
        if *self.rx.borrow_and_update() {
            return;
        }
        // A `changed` error means the controller was dropped: treat as
        // shutdown so the actor winds down rather than hanging.
        while self.rx.changed().await.is_ok() {
            if *self.rx.borrow_and_update() {
                return;
            }
        }
    }
}

/// Held by a durable-flush task; dropping it tells the controller this task has
/// drained.
pub struct DrainGuard {
    _tx: mpsc::Sender<()>,
    outstanding: Arc<AtomicUsize>,
}

impl Drop for DrainGuard {
    fn drop(&mut self) {
        self.outstanding.fetch_sub(1, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn drain_returns_when_no_guards() {
        let controller = ShutdownController::new();
        assert_eq!(
            controller.drain(Duration::from_secs(5)).await,
            DrainOutcome::Drained
        );
    }

    #[tokio::test]
    async fn drain_waits_for_guard_release() {
        let controller = ShutdownController::new();
        let mut signal = controller.subscribe();
        let guard = controller.guard();
        tokio::spawn(async move {
            signal.recv().await;
            tokio::time::sleep(Duration::from_millis(20)).await;
            drop(guard);
        });
        assert_eq!(
            controller.drain(Duration::from_secs(5)).await,
            DrainOutcome::Drained
        );
    }

    // Required failure path: a guard held past the deadline forces a timeout,
    // and the outstanding count feeds the forced-exit log.
    #[tokio::test]
    async fn drain_times_out_when_guard_outlives_deadline() {
        let controller = ShutdownController::new();
        let mut signal = controller.subscribe();
        let guard = controller.guard();
        tokio::spawn(async move {
            signal.recv().await;
            tokio::time::sleep(Duration::from_secs(60)).await;
            drop(guard);
        });
        assert_eq!(
            controller.drain(Duration::from_millis(50)).await,
            DrainOutcome::TimedOut { outstanding: 1 }
        );
    }

    #[tokio::test]
    async fn drain_waits_for_all_guards() {
        let controller = ShutdownController::new();
        for _ in 0..3 {
            let mut signal = controller.subscribe();
            let guard = controller.guard();
            tokio::spawn(async move {
                signal.recv().await;
                drop(guard);
            });
        }
        assert_eq!(
            controller.drain(Duration::from_secs(5)).await,
            DrainOutcome::Drained
        );
    }

    #[tokio::test]
    async fn trigger_wakes_subscriber() {
        let controller = ShutdownController::new();
        let trigger = controller.trigger();
        let mut signal = controller.subscribe();
        let handle = tokio::spawn(async move {
            signal.recv().await;
        });
        trigger.fire();
        handle.await.unwrap();
    }

    // The `send_replace` red-team fix: a signal fired before any subscriber
    // must still resolve a later `subscribe`.
    #[tokio::test]
    async fn already_fired_signal_resolves_immediately() {
        let controller = ShutdownController::new();
        controller.trigger().fire();
        let mut signal = controller.subscribe();
        signal.recv().await;
    }

    #[tokio::test]
    async fn dropped_controller_wakes_subscriber() {
        let controller = ShutdownController::new();
        let mut signal = controller.subscribe();
        drop(controller);
        signal.recv().await;
    }

    // A reconnect-style pump that never takes a guard does not delay the drain.
    #[tokio::test]
    async fn guardless_pump_does_not_block_drain() {
        let controller = ShutdownController::new();
        let mut signal = controller.subscribe();
        tokio::spawn(async move {
            signal.recv().await;
        });
        assert_eq!(
            controller.drain(Duration::from_secs(5)).await,
            DrainOutcome::Drained
        );
    }
}
