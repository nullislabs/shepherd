//! Injectable task executor for the long-lived reconnect tasks.
//!
//! The event loop spawns one reconnect task per chain block subscription
//! and per chain-log subscription. Rather than reach for a concrete
//! `tokio::task::JoinSet` at the spawn site, the openers take a
//! [`TaskExecutor`] and hand back a typed [`TaskHandle`] per task. The
//! default [`TokioExecutor`] spawns onto the ambient tokio runtime; tests
//! and alternative launchers can supply their own.

use std::future::Future;
use std::pin::Pin;

/// Boxed future a reconnect task runs to completion. Boxed so the
/// executor stays object-safe behind `&dyn TaskExecutor`.
pub type TaskFuture = Pin<Box<dyn Future<Output = TaskExit> + Send + 'static>>;

/// Why a reconnect task returned. The tasks loop forever over their
/// subscription; the sole ordinary exit is the downstream receiver
/// closing, which happens when the event loop tears down on shutdown.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum TaskExit {
    /// The mpsc receiver the task feeds was dropped, so the task
    /// stopped pumping and returned. Expected on graceful shutdown.
    ReceiverGone,
}

/// Spawns reconnect tasks and hands back a [`TaskHandle`] for each.
///
/// Object-safe: consumers thread `&dyn TaskExecutor` so the concrete
/// runtime is chosen once at the launch root, not baked into the openers.
pub trait TaskExecutor: Send + Sync {
    /// Spawn `fut` and return a handle owning the running task.
    fn spawn(&self, fut: TaskFuture) -> TaskHandle;
}

/// Spawns onto the ambient tokio runtime. The default executor for the
/// binary launch path.
#[derive(Debug, Clone, Copy, Default)]
pub struct TokioExecutor;

impl TaskExecutor for TokioExecutor {
    fn spawn(&self, fut: TaskFuture) -> TaskHandle {
        TaskHandle(tokio::task::spawn(fut))
    }
}

/// Typed handle to one spawned reconnect task. Aborting is best-effort;
/// the tasks also exit on their own once their receiver is dropped.
#[derive(Debug)]
pub struct TaskHandle(tokio::task::JoinHandle<TaskExit>);

impl TaskHandle {
    /// Request cancellation. The task stops at its next await point.
    pub fn abort(&self) {
        self.0.abort();
    }

    /// Wait for the task to finish, yielding its [`TaskExit`] reason.
    /// `None` when the task was aborted or panicked.
    pub async fn join(self) -> Option<TaskExit> {
        self.0.await.ok()
    }
}

/// The set of reconnect-task handles the event loop owns for its
/// lifetime. Threaded through the openers at boot and drained on
/// shutdown so every task is aborted and observed to finish before the
/// engine returns.
#[derive(Debug, Default)]
pub struct TaskSet {
    handles: Vec<TaskHandle>,
}

impl TaskSet {
    /// An empty set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Take ownership of a freshly spawned task's handle.
    pub fn push(&mut self, handle: TaskHandle) {
        self.handles.push(handle);
    }

    /// Abort every task, then wait for each to unwind. Mirrors
    /// `JoinSet::shutdown`: the engine sees the tasks genuinely finish
    /// before it returns.
    pub async fn shutdown(self) {
        for handle in &self.handles {
            handle.abort();
        }
        for handle in self.handles {
            let _ = handle.join().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn tokio_executor_runs_the_future_to_its_exit_reason() {
        let handle = TokioExecutor.spawn(Box::pin(async { TaskExit::ReceiverGone }));
        assert_eq!(handle.join().await, Some(TaskExit::ReceiverGone));
    }

    #[tokio::test]
    async fn abort_stops_a_never_ending_task() {
        let handle = TokioExecutor.spawn(Box::pin(async {
            std::future::pending::<()>().await;
            TaskExit::ReceiverGone
        }));
        handle.abort();
        // Aborted task yields no exit reason.
        assert_eq!(handle.join().await, None);
    }

    #[tokio::test]
    async fn shutdown_drains_a_pending_task_set() {
        let mut set = TaskSet::new();
        set.push(TokioExecutor.spawn(Box::pin(async {
            std::future::pending::<()>().await;
            TaskExit::ReceiverGone
        })));
        // Returns rather than hanging: shutdown aborts before joining.
        set.shutdown().await;
    }
}
