//! Retention store for captured log records: the trait the pipeline
//! writes through and reads back, plus the default byte-bounded
//! in-memory backend (one ring per run, a retained-runs cap per module).

use std::collections::HashMap;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::SystemTime;

use super::{LogRecord, RunId};
use crate::engine_config::LogRetentionLimits;

/// A page of a run's retained records plus the cursor to resume from.
#[derive(Debug, Clone, Default)]
pub struct LogPage {
    /// Records with sequence at or after the requested cursor, oldest
    /// first. May be shorter than the caller expects when older records
    /// have been evicted from the ring.
    pub records: Vec<LogRecord>,
    /// Cursor to pass on the next [`RunLogStore::read`] to continue after
    /// the last returned record.
    pub next_cursor: u64,
}

/// Summary of one run held by the store.
#[derive(Debug, Clone)]
pub struct RunMeta {
    /// The run this metadata describes.
    pub run: RunId,
    /// Total records appended for this run, including any since evicted.
    pub appended: u64,
    /// Records currently retained in the ring.
    pub retained: usize,
    /// Bytes currently charged against the per-run budget.
    pub retained_bytes: usize,
    /// Timestamp of the oldest retained record, if any.
    pub first_ts: Option<SystemTime>,
    /// Timestamp of the newest retained record, if any.
    pub last_ts: Option<SystemTime>,
}

/// Retention backend the [`LogRouter`](super::LogRouter) appends to and
/// the embedding surface reads from. Appends are best-effort and
/// infallible: a full ring evicts rather than erroring.
pub trait RunLogStore: Send + Sync {
    /// Retain one record, registering its run on first sighting.
    fn append(&self, record: LogRecord);
    /// Runs recorded for `module`, oldest retained first.
    fn list_runs(&self, module: &str) -> Vec<RunMeta>;
    /// Page a run's retained records from `cursor` (0 for the start).
    fn read(&self, run: &RunId, cursor: u64) -> LogPage;
}

/// Byte-bounded ring of the records for a single run.
struct Ring {
    /// `(sequence, record)` pairs, oldest at the front.
    records: VecDeque<(u64, LogRecord)>,
    /// Sum of the retained records' byte costs.
    bytes: usize,
    /// Total records ever appended; also the next sequence to assign.
    appended: u64,
    /// Per-run byte budget.
    cap: usize,
}

impl Ring {
    fn new(cap: usize) -> Self {
        Self {
            records: VecDeque::new(),
            bytes: 0,
            appended: 0,
            cap,
        }
    }

    fn push(&mut self, record: LogRecord) {
        let seq = self.appended;
        self.appended += 1;
        self.bytes += record.cost();
        self.records.push_back((seq, record));
        // Evict oldest until within budget, but never drop the sole
        // record: a single line larger than the whole budget is still
        // the newest context a reader wants at a crash.
        while self.bytes > self.cap && self.records.len() > 1 {
            if let Some((_, evicted)) = self.records.pop_front() {
                self.bytes -= evicted.cost();
            }
        }
    }

    fn meta(&self, run: RunId) -> RunMeta {
        RunMeta {
            run,
            appended: self.appended,
            retained: self.records.len(),
            retained_bytes: self.bytes,
            first_ts: self.records.front().map(|(_, r)| r.ts),
            last_ts: self.records.back().map(|(_, r)| r.ts),
        }
    }

    fn page(&self, cursor: u64) -> LogPage {
        let records: Vec<LogRecord> = self
            .records
            .iter()
            .filter(|(seq, _)| *seq >= cursor)
            .map(|(_, r)| r.clone())
            .collect();
        LogPage {
            records,
            next_cursor: self.appended,
        }
    }
}

/// Runs held for one module, capped at `runs_retained` with the oldest
/// evicted first.
struct ModuleRuns {
    /// Insertion order, front = oldest; mirrors the ascending run seq.
    order: VecDeque<RunId>,
    rings: HashMap<RunId, Ring>,
}

impl ModuleRuns {
    fn new() -> Self {
        Self {
            order: VecDeque::new(),
            rings: HashMap::new(),
        }
    }
}

struct Inner {
    modules: HashMap<Arc<str>, ModuleRuns>,
    limits: LogRetentionLimits,
}

/// Default retention backend: an in-memory ring per run, byte-bounded to
/// `bytes_per_run`, with at most `runs_retained` runs kept per module.
pub struct InMemoryRunLogStore {
    inner: Mutex<Inner>,
}

impl InMemoryRunLogStore {
    /// Backend sized by the resolved `[limits.logs]` knobs.
    pub fn new(limits: LogRetentionLimits) -> Self {
        Self {
            inner: Mutex::new(Inner {
                modules: HashMap::new(),
                limits,
            }),
        }
    }
}

impl RunLogStore for InMemoryRunLogStore {
    fn append(&self, record: LogRecord) {
        let mut inner = self.inner.lock().expect("log store mutex poisoned");
        let limits = inner.limits;
        let module = record.run.module.clone();
        let entry = inner.modules.entry(module).or_insert_with(ModuleRuns::new);
        if !entry.rings.contains_key(&record.run) {
            entry
                .rings
                .insert(record.run.clone(), Ring::new(limits.bytes_per_run));
            entry.order.push_back(record.run.clone());
            // Evict whole runs beyond the retained cap, oldest first.
            while entry.order.len() > limits.runs_retained {
                if let Some(old) = entry.order.pop_front() {
                    entry.rings.remove(&old);
                }
            }
        }
        // Defensive: the new run is pushed to the tail and eviction only
        // pops the front, so it always survives, but guard the lookup
        // rather than unwrap.
        if let Some(ring) = entry.rings.get_mut(&record.run) {
            ring.push(record);
        }
    }

    fn list_runs(&self, module: &str) -> Vec<RunMeta> {
        let inner = self.inner.lock().expect("log store mutex poisoned");
        let Some(entry) = inner.modules.get(module) else {
            return Vec::new();
        };
        entry
            .order
            .iter()
            .filter_map(|run| entry.rings.get(run).map(|ring| ring.meta(run.clone())))
            .collect()
    }

    fn read(&self, run: &RunId, cursor: u64) -> LogPage {
        let inner = self.inner.lock().expect("log store mutex poisoned");
        inner
            .modules
            .get(&*run.module)
            .and_then(|entry| entry.rings.get(run))
            .map(|ring| ring.page(cursor))
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::logs::{LogLevel, LogSource};

    fn limits(bytes_per_run: usize, runs_retained: usize) -> LogRetentionLimits {
        LogRetentionLimits {
            bytes_per_run,
            runs_retained,
        }
    }

    fn run(module: &str, seq: u64) -> RunId {
        RunId::new(module, seq)
    }

    fn record(run: &RunId, message: &str) -> LogRecord {
        LogRecord::now(
            run.clone(),
            LogSource::Stdout,
            LogLevel::Info,
            message.to_owned(),
        )
    }

    #[test]
    fn append_then_read_returns_records_in_order() {
        let store = InMemoryRunLogStore::new(limits(1024, 4));
        let r = run("m", 0);
        store.append(record(&r, "one"));
        store.append(record(&r, "two"));
        let page = store.read(&r, 0);
        let msgs: Vec<&str> = page.records.iter().map(|r| r.message.as_str()).collect();
        assert_eq!(msgs, ["one", "two"]);
        assert_eq!(page.next_cursor, 2);
    }

    #[test]
    fn read_from_cursor_returns_only_newer_records() {
        let store = InMemoryRunLogStore::new(limits(1024, 4));
        let r = run("m", 0);
        store.append(record(&r, "a"));
        store.append(record(&r, "b"));
        let first = store.read(&r, 0);
        assert_eq!(first.next_cursor, 2);
        store.append(record(&r, "c"));
        let next = store.read(&r, first.next_cursor);
        let msgs: Vec<&str> = next.records.iter().map(|r| r.message.as_str()).collect();
        assert_eq!(msgs, ["c"]);
        assert_eq!(next.next_cursor, 3);
    }

    #[test]
    fn ring_retains_exactly_at_the_byte_cap() {
        // Three 4-byte messages under a 12-byte cap: exact fit, nothing
        // evicted.
        let store = InMemoryRunLogStore::new(limits(12, 4));
        let r = run("m", 0);
        for m in ["aaaa", "bbbb", "cccc"] {
            store.append(record(&r, m));
        }
        let meta = &store.list_runs("m")[0];
        assert_eq!(meta.retained, 3);
        assert_eq!(meta.retained_bytes, 12);
    }

    #[test]
    fn ring_evicts_oldest_past_the_byte_cap() {
        // A fourth 4-byte message past the 12-byte cap evicts the oldest.
        let store = InMemoryRunLogStore::new(limits(12, 4));
        let r = run("m", 0);
        for m in ["aaaa", "bbbb", "cccc", "dddd"] {
            store.append(record(&r, m));
        }
        let page = store.read(&r, 0);
        let msgs: Vec<&str> = page.records.iter().map(|r| r.message.as_str()).collect();
        assert_eq!(msgs, ["bbbb", "cccc", "dddd"]);
        assert_eq!(page.next_cursor, 4, "cursor counts every append");
        let meta = &store.list_runs("m")[0];
        assert_eq!(meta.appended, 4);
        assert_eq!(meta.retained, 3);
        assert_eq!(meta.retained_bytes, 12);
    }

    #[test]
    fn oversized_single_record_is_retained_alone() {
        // A single record larger than the whole cap is kept: it is the
        // newest context a reader wants, so it is never evicted to nothing.
        let store = InMemoryRunLogStore::new(limits(4, 4));
        let r = run("m", 0);
        store.append(record(&r, "aaaa"));
        store.append(record(&r, "this-is-way-over-the-cap"));
        let page = store.read(&r, 0);
        let msgs: Vec<&str> = page.records.iter().map(|r| r.message.as_str()).collect();
        assert_eq!(msgs, ["this-is-way-over-the-cap"]);
    }

    #[test]
    fn runs_retained_evicts_the_oldest_run() {
        let store = InMemoryRunLogStore::new(limits(1024, 2));
        let (r0, r1, r2) = (run("m", 0), run("m", 1), run("m", 2));
        store.append(record(&r0, "zero"));
        store.append(record(&r1, "one"));
        // Two runs retained so far.
        assert_eq!(store.list_runs("m").len(), 2);
        // A third run evicts the oldest (seq 0).
        store.append(record(&r2, "two"));
        let runs = store.list_runs("m");
        let seqs: Vec<u64> = runs.iter().map(|meta| meta.run.seq).collect();
        assert_eq!(seqs, [1, 2]);
        assert!(
            store.read(&r0, 0).records.is_empty(),
            "evicted run is empty"
        );
    }

    #[test]
    fn old_run_stays_readable_until_evicted() {
        let store = InMemoryRunLogStore::new(limits(1024, 2));
        let (r0, r1) = (run("m", 0), run("m", 1));
        store.append(record(&r0, "zero"));
        store.append(record(&r1, "one"));
        // Both runs still under the cap: the old run is readable.
        let page = store.read(&r0, 0);
        assert_eq!(page.records.len(), 1);
        assert_eq!(page.records[0].message, "zero");
    }

    #[test]
    fn runs_are_keyed_independently_across_restart() {
        // Same module, two runs: the sequences do not collide and each
        // ring accounts its own bytes.
        let store = InMemoryRunLogStore::new(limits(1024, 4));
        let (r0, r1) = (run("m", 0), run("m", 1));
        store.append(record(&r0, "boot-0"));
        store.append(record(&r1, "boot-1"));
        assert_eq!(store.read(&r0, 0).records[0].message, "boot-0");
        assert_eq!(store.read(&r1, 0).records[0].message, "boot-1");
    }

    #[test]
    fn read_of_unknown_run_is_empty() {
        let store = InMemoryRunLogStore::new(limits(1024, 4));
        let page = store.read(&run("ghost", 0), 0);
        assert!(page.records.is_empty());
        assert_eq!(page.next_cursor, 0);
    }

    #[test]
    fn runs_retained_of_one_keeps_only_the_newest_run() {
        let store = InMemoryRunLogStore::new(limits(1024, 1));
        let (r0, r1) = (run("m", 0), run("m", 1));
        store.append(record(&r0, "zero"));
        store.append(record(&r1, "one"));
        let runs = store.list_runs("m");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].run.seq, 1);
        assert!(store.read(&r0, 0).records.is_empty());
        assert_eq!(store.read(&r1, 0).records[0].message, "one");
    }
}
