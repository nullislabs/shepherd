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

mod stdio;
mod store;

use std::sync::Arc;
use std::time::SystemTime;

use strum::IntoStaticStr;

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

/// Severity carried on a record, mirroring the WIT logging level so the
/// pipeline stays independent of the generated bindings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
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
    pub level: LogLevel,
    /// The line text.
    pub message: String,
}

impl LogRecord {
    /// Build a record stamped at the current wall-clock instant.
    pub fn now(run: RunId, source: LogSource, level: LogLevel, message: String) -> Self {
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
        self.message.len()
    }
}

/// Fans every captured record to a host `tracing` event and the
/// retention store.
pub struct LogRouter {
    store: Arc<dyn RunLogStore>,
}

impl LogRouter {
    /// Router writing into `store`.
    pub fn new(store: Arc<dyn RunLogStore>) -> Self {
        Self { store }
    }

    /// Emit the tracing event, then retain the record.
    pub fn record(&self, record: LogRecord) {
        emit_tracing(&record);
        self.store.append(record);
    }

    fn store(&self) -> &Arc<dyn RunLogStore> {
        &self.store
    }
}

/// Emit one record as a host tracing event at its own level, carrying
/// the module, run sequence, and source. Level must be static per macro
/// site, hence the match.
fn emit_tracing(record: &LogRecord) {
    let module = &*record.run.module;
    let run = record.run.seq;
    let source: &'static str = record.source.into();
    let message = record.message.as_str();
    match record.level {
        LogLevel::Trace => tracing::trace!(module, run, source, "{message}"),
        LogLevel::Debug => tracing::debug!(module, run, source, "{message}"),
        LogLevel::Info => tracing::info!(module, run, source, "{message}"),
        LogLevel::Warn => tracing::warn!(module, run, source, "{message}"),
        LogLevel::Error => tracing::error!(module, run, source, "{message}"),
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
    use std::sync::Mutex;

    use super::*;

    /// Store that both counts appends and forwards to an inner ring, so
    /// the fan-out test can prove the retention consumer saw the record.
    struct CountingStore {
        appended: Mutex<Vec<LogRecord>>,
    }

    impl RunLogStore for CountingStore {
        fn append(&self, record: LogRecord) {
            self.appended.lock().unwrap().push(record);
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
            LogLevel::Info,
            "hello".to_owned(),
        ));
        let appended = store.appended.lock().unwrap();
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
            LogLevel::Info,
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
