//! Task lifecycle and graceful shutdown: every runtime task is spawned
//! through a [`TaskExecutor`] minted by a [`TaskManager`], which owns the
//! shutdown signal and the bounded drain. The only crate a raw `tokio`
//! spawn appears in, so shutdown reaches every task.

mod manager;
mod shutdown;
mod task;

pub use manager::{TaskExecutor, TaskManager};
pub use shutdown::{
    DrainOutcome, GracefulShutdown, GracefulShutdownGuard, Shutdown, ShutdownTrigger,
};
pub use task::{TaskExit, TaskHandle, TaskSet};
