//! Typed handles over spawned tasks and the abort-on-drop set the event
//! loop drains at shutdown.

use tracing::debug;

/// Why a pump task returned; the sole ordinary exit is its downstream
/// receiver closing at shutdown.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum TaskExit {
    /// The receiver the task feeds was dropped, so it stopped pumping.
    ReceiverGone,
}

/// Handle to one spawned task.
#[derive(Debug)]
pub struct TaskHandle<T>(pub(crate) tokio::task::JoinHandle<T>);

impl<T> TaskHandle<T> {
    /// Request cancellation; the task stops at its next await point.
    pub fn abort(&self) {
        self.0.abort();
    }

    /// Wait for the task to finish. `None` when it was aborted or panicked.
    pub async fn join(self) -> Option<T> {
        self.0.await.ok()
    }
}

/// The pump-task handles the event loop owns for its lifetime; abortable as
/// a set so every task is observed to finish before the engine returns.
#[derive(Debug, Default)]
pub struct TaskSet {
    handles: Vec<TaskHandle<TaskExit>>,
}

impl TaskSet {
    /// An empty set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Take ownership of a freshly spawned task's handle.
    pub fn push(&mut self, handle: TaskHandle<TaskExit>) {
        self.handles.push(handle);
    }

    /// Abort every task, then await each handle so all tasks are observed
    /// to finish. A `None` join (aborted or panicked) counts against the
    /// aborted tally in the drain summary.
    pub async fn shutdown(mut self) {
        for handle in &self.handles {
            handle.abort();
        }
        let total = self.handles.len();
        let mut clean = 0usize;
        let mut aborted = 0usize;
        for handle in self.handles.drain(..) {
            match handle.join().await {
                Some(_) => clean += 1,
                None => aborted += 1,
            }
        }
        debug!(total, clean, aborted, "pump task set drained");
    }
}

impl Drop for TaskSet {
    /// Abort any handles [`shutdown`](TaskSet::shutdown) did not drain, so
    /// the tasks do not detach and outlive the engine (a bare `JoinHandle`
    /// detaches on drop).
    fn drop(&mut self) {
        for handle in &self.handles {
            handle.abort();
        }
    }
}
