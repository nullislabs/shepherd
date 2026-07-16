//! Task lifecycle and graceful shutdown: every runtime task is spawned
//! through a [`TaskExecutor`] minted by a [`TaskManager`], which owns the
//! shutdown signal and the bounded drain.
//!
//! This crate is the only place a raw `tokio` spawn appears; consumers
//! route every task through the executor so shutdown reaches all of them.

mod manager;
mod shutdown;
mod task;

pub use manager::{TaskExecutor, TaskManager};
pub use shutdown::{
    DrainOutcome, GracefulShutdown, GracefulShutdownGuard, Shutdown, ShutdownTrigger,
};
pub use task::{TaskExit, TaskHandle, TaskSet};
