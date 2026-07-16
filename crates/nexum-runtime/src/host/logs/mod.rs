//! Typed module-log pipeline.
//!
//! Three capture points construct [`LogRecord`]s and hand them to one
//! [`LogRouter`]: the `nexum:host/logging` glue (`HostInterface`), the
//! per-store stdout/stderr pipes ([`StdioStream`], `Stdout`/`Stderr`),
//! and the supervisor's death path (`Panic`). The router fans each
//! record to exactly two consumers: a host `tracing` event (so the
//! operator console and OTLP stacks stay live) and the retention store.
//! `tracing` is a consumer of the pipeline, not its transport.
//!
//! [`LogPipeline`] is the shared handle: it rides the host state so
//! every capture point reaches the router, and it exposes the store's
//! read side to the embedding surface for run listing and log paging.
//!
//! One guest panic deliberately yields three records, distinguishable
//! by source: the guest panic hook writes to stderr (`Stderr`, warn)
//! and then reports over the host logging call (`HostInterface`,
//! error), and the supervisor synthesizes a death record (`Panic`,
//! error) once the trap surfaces. The redundancy is kept because the
//! channels survive different failure modes: stderr capture works even
//! if the sink's host call traps, and the supervisor record covers a
//! guest with no hook installed at all.

mod stdio;
mod store;

use std::sync::Arc;
use std::time::SystemTime;

use strum::IntoStaticStr;
use tracing_core::Level;

pub use stdio::StdioStream;
pub use store::{InMemoryRunLogStore, LogPage, RunLogStore, RunMeta};

/// Identity of one module run. Minted at every instantiation; a restart
/// increments `seq`, so each run is a distinct retention key.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RunId {
    /// Module namespace this run belongs to.
    pub module: Arc<str>,
    /// Monotonic run counter within the module; 0 is the first boot.
    pub seq: u64,
    /// Wall-clock instant the run was instantiated.
    pub started_at: SystemTime,
}

impl RunId {
    /// Mint a run for `module` at sequence `seq`, stamping the current
    /// wall-clock instant.
    pub fn new(module: impl Into<Arc<str>>, seq: u64) -> Self {
        Self {
            module: module.into(),
            seq,
            started_at: SystemTime::now(),
        }
    }
}

/// Which capture point produced a record. The snake_case name is the
/// `source` field on the host tracing event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum LogSource {
    /// The `nexum:host/logging` glue: an explicit guest `log` call.
    HostInterface,
    /// A line captured from the guest's stdout pipe.
    Stdout,
    /// A line captured from the guest's stderr pipe.
    Stderr,
    /// Synthesized by the supervisor when a run dies via trap or exit.
    Panic,
}

/// One captured log line from any capture point.
#[derive(Debug, Clone)]
pub struct LogRecord {
    /// Run the line belongs to.
    pub run: RunId,
    /// Wall-clock capture time.
    pub ts: SystemTime,
    /// Capture point of origin.
    pub source: LogSource,
    /// Line severity.
    pub level: Level,
    /// The line text.
    pub message: String,
}

impl LogRecord {
    /// Build a record stamped at the current wall-clock instant.
    pub fn now(run: RunId, source: LogSource, level: Level, message: String) -> Self {
        Self {
            run,
            ts: SystemTime::now(),
            source,
            level,
            message,
        }
    }

    /// Byte cost charged against the per-run retention budget.
    fn cost(&self) -> usize {
        RECORD_OVERHEAD + self.message.len()
    }
}

/// Fixed per-record charge on top of the message bytes, approximating
/// the host memory a retained record occupies (run key, timestamps,
/// `String` header, ring slot). Without it a flood of empty messages
/// would grow the ring far past the `[limits.logs]` byte budget.
const RECORD_OVERHEAD: usize = 128;

/// Fans every captured record to a host `tracing` event and the
/// retention store.
pub struct LogRouter {
    store: Arc<dyn RunLogStore>,
    /// Woken after each append so a consumer can await new output instead
    /// of polling. `notify_waiters` wakes only armed waiters, so a reader
    /// must arm before it reads (see [`LogPipeline::appended`]).
    appended: Arc<tokio::sync::Notify>,
}

impl LogRouter {
    /// Router writing into `store`.
    pub fn new(store: Arc<dyn RunLogStore>) -> Self {
        Self {
            store,
            appended: Arc::new(tokio::sync::Notify::new()),
        }
    }

    /// Emit the tracing event, retain the record, then wake append waiters.
    pub fn record(&self, record: LogRecord) {
        emit_tracing(&record);
        self.store.append(record);
        self.appended.notify_waiters();
    }

    fn store(&self) -> &Arc<dyn RunLogStore> {
        &self.store
    }
}

/// Emit one record as a host tracing event at its own level, carrying
/// the module, run sequence, and source. `tracing`'s macros require a
/// static level per call site, and `Level` is a set of associated
/// consts rather than a matchable enum, so dispatch through an equality
/// ladder over the five tiers.
fn emit_tracing(record: &LogRecord) {
    let module = &*record.run.module;
    let run = record.run.seq;
    let source: &'static str = record.source.into();
    let message = record.message.as_str();
    if record.level == Level::TRACE {
        tracing::trace!(module, run, source, "{message}");
    } else if record.level == Level::DEBUG {
        tracing::debug!(module, run, source, "{message}");
    } else if record.level == Level::INFO {
        tracing::info!(module, run, source, "{message}");
    } else if record.level == Level::WARN {
        tracing::warn!(module, run, source, "{message}");
    } else {
        tracing::error!(module, run, source, "{message}");
    }
}

/// Shared log pipeline threaded into every module store. Cheap to clone
/// (one `Arc`); the write side is [`router`](Self::router) and the read
/// side is [`list_runs`](Self::list_runs) / [`read`](Self::read).
#[derive(Clone)]
pub struct LogPipeline {
    router: Arc<LogRouter>,
}

impl LogPipeline {
    /// Pipeline over an arbitrary retention backend.
    pub fn new(store: Arc<dyn RunLogStore>) -> Self {
        Self {
            router: Arc::new(LogRouter::new(store)),
        }
    }

    /// Pipeline over the default byte-bounded in-memory backend, sized by
    /// the resolved `[limits.logs]` knobs.
    pub fn in_memory(limits: crate::engine_config::LogRetentionLimits) -> Self {
        Self::new(Arc::new(InMemoryRunLogStore::new(limits)))
    }

    /// The write handle the capture points route through.
    pub fn router(&self) -> Arc<LogRouter> {
        self.router.clone()
    }

    /// A notify woken after each append, for awaiting new output without
    /// polling. Arm a `notified()` future (and `enable()` it) before reading
    /// so an append between the read and the await is not lost.
    pub fn appended(&self) -> Arc<tokio::sync::Notify> {
        self.router.appended.clone()
    }

    /// Runs recorded for `module`, oldest retained first.
    pub fn list_runs(&self, module: &str) -> Vec<RunMeta> {
        self.router.store().list_runs(module)
    }

    /// Page a run's retained records from `cursor` (0 for the start).
    pub fn read(&self, run: &RunId, cursor: u64) -> LogPage {
        self.router.store().read(run, cursor)
    }
}

#[cfg(test)]
mod tests {
    use parking_lot::Mutex;

    use super::*;

    /// Store that both counts appends and forwards to an inner ring, so
    /// the fan-out test can prove the retention consumer saw the record.
    struct CountingStore {
        appended: Mutex<Vec<LogRecord>>,
    }

    impl RunLogStore for CountingStore {
        fn append(&self, record: LogRecord) {
            self.appended.lock().push(record);
        }
        fn list_runs(&self, _module: &str) -> Vec<RunMeta> {
            Vec::new()
        }
        fn read(&self, _run: &RunId, _cursor: u64) -> LogPage {
            LogPage::default()
        }
    }

    #[test]
    fn router_fans_out_to_the_retention_store() {
        let store = Arc::new(CountingStore {
            appended: Mutex::new(Vec::new()),
        });
        let router = LogRouter::new(store.clone());
        router.record(LogRecord::now(
            RunId::new("m", 0),
            LogSource::HostInterface,
            Level::INFO,
            "hello".to_owned(),
        ));
        let appended = store.appended.lock();
        assert_eq!(appended.len(), 1, "retention consumer saw the record");
        assert_eq!(appended[0].message, "hello");
        assert_eq!(appended[0].source, LogSource::HostInterface);
    }

    #[test]
    fn pipeline_read_side_reaches_the_backend() {
        let limits = crate::engine_config::LogRetentionLimits {
            bytes_per_run: 1024,
            runs_retained: 4,
        };
        let pipeline = LogPipeline::in_memory(limits);
        let run = RunId::new("m", 0);
        pipeline.router().record(LogRecord::now(
            run.clone(),
            LogSource::Stdout,
            Level::INFO,
            "line".to_owned(),
        ));
        let runs = pipeline.list_runs("m");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].run.seq, 0);
        let page = pipeline.read(&run, 0);
        assert_eq!(page.records[0].message, "line");
    }

    #[test]
    fn source_names_are_snake_case_for_the_tracing_field() {
        let s: &'static str = LogSource::HostInterface.into();
        assert_eq!(s, "host_interface");
        let s: &'static str = LogSource::Panic.into();
        assert_eq!(s, "panic");
    }
}
