//! Per-store stdout/stderr capture: a [`StdoutStream`] that line-buffers
//! the guest's byte stream and routes each complete line as a
//! [`LogRecord`]. Installed in place of `inherit_stdio`, so guest output
//! is tagged with its run and source rather than merged onto host stdio.

use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use tokio::io::AsyncWrite;
use wasmtime_wasi::cli::{IsTerminal, StdoutStream};

use tracing_core::Level;

use super::{LogRecord, LogRouter, LogSource, RunId};

/// Upper bound on an in-flight line held without a newline. A guest that
/// floods a stream without ever terminating a line cannot grow host
/// memory without limit: the buffer is force-flushed as one record once
/// it crosses this size.
const MAX_LINE_BYTES: usize = 1 << 20;

/// Per-store stdout or stderr sink handed to `WasiCtxBuilder`. Each call
/// to [`StdoutStream::async_stream`] yields a fresh line-splitting writer
/// bound to the same run and source.
pub struct StdioStream {
    router: Arc<LogRouter>,
    run: RunId,
    source: LogSource,
}

impl StdioStream {
    /// Sink routing `source` lines for `run` through `router`.
    pub fn new(router: Arc<LogRouter>, run: RunId, source: LogSource) -> Self {
        Self {
            router,
            run,
            source,
        }
    }
}

impl IsTerminal for StdioStream {
    fn is_terminal(&self) -> bool {
        false
    }
}

impl StdoutStream for StdioStream {
    fn async_stream(&self) -> Box<dyn AsyncWrite + Send + Sync> {
        Box::new(LineWriter {
            router: self.router.clone(),
            run: self.run.clone(),
            source: self.source,
            buf: Vec::new(),
        })
    }
}

/// Line-splitting writer: buffers raw bytes and emits one record per
/// newline. Cutting only at `\n` (never a UTF-8 continuation byte) means
/// a multi-byte code point split across writes is always reassembled in
/// the buffer before the line is decoded.
struct LineWriter {
    router: Arc<LogRouter>,
    run: RunId,
    source: LogSource,
    buf: Vec<u8>,
}

impl LineWriter {
    /// Route every complete line in the buffer, then force-flush an
    /// over-long unterminated remainder.
    fn drain(&mut self) {
        while let Some(nl) = self.buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=nl).collect();
            route_line(
                &self.router,
                &self.run,
                self.source,
                &line[..line.len() - 1],
            );
        }
        if self.buf.len() > MAX_LINE_BYTES {
            let chunk = std::mem::take(&mut self.buf);
            route_line(&self.router, &self.run, self.source, &chunk);
        }
    }

    /// Emit any buffered partial line. Idempotent: the buffer is taken,
    /// so a shutdown flush and the drop guard never double-emit.
    fn flush_remainder(&mut self) {
        if self.buf.is_empty() {
            return;
        }
        let rest = std::mem::take(&mut self.buf);
        route_line(&self.router, &self.run, self.source, &rest);
    }
}

/// Level a captured line carries: stdout is informational, stderr is a
/// warning. Documented alongside the `[limits.logs]` knobs.
fn level_for(source: LogSource) -> Level {
    match source {
        LogSource::Stderr => Level::WARN,
        _ => Level::INFO,
    }
}

/// Decode one line's bytes and route it, dropping a trailing `\r` (so
/// CRLF output is clean) and skipping empties.
fn route_line(router: &LogRouter, run: &RunId, source: LogSource, bytes: &[u8]) {
    let bytes = bytes.strip_suffix(b"\r").unwrap_or(bytes);
    if bytes.is_empty() {
        return;
    }
    let message = String::from_utf8_lossy(bytes).into_owned();
    router.record(LogRecord::now(
        run.clone(),
        source,
        level_for(source),
        message,
    ));
}

impl AsyncWrite for LineWriter {
    fn poll_write(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.buf.extend_from_slice(data);
        self.drain();
        Poll::Ready(Ok(data.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        // A flush is not an end-of-line; partial lines stay buffered.
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.flush_remainder();
        Poll::Ready(Ok(()))
    }
}

impl Drop for LineWriter {
    fn drop(&mut self) {
        // A store dropped on module death must not lose the final
        // unterminated line.
        self.flush_remainder();
    }
}

#[cfg(test)]
mod tests {
    use parking_lot::Mutex;
    use tokio::io::AsyncWriteExt;

    use super::*;
    use crate::host::logs::{LogPipeline, LogRecord, LogSource, RunId, RunLogStore};

    /// Capturing store that records every appended message so a test can
    /// assert the exact line boundaries the writer produced.
    #[derive(Default)]
    struct CaptureStore {
        records: Mutex<Vec<LogRecord>>,
    }

    impl RunLogStore for CaptureStore {
        fn append(&self, record: LogRecord) {
            self.records.lock().push(record);
        }
        fn list_runs(&self, _module: &str) -> Vec<crate::host::logs::RunMeta> {
            Vec::new()
        }
        fn read(&self, _run: &RunId, _cursor: u64) -> crate::host::logs::LogPage {
            crate::host::logs::LogPage::default()
        }
    }

    fn setup(source: LogSource) -> (LineWriter, Arc<CaptureStore>) {
        let store = Arc::new(CaptureStore::default());
        let pipeline = LogPipeline::new(store.clone());
        let writer = LineWriter {
            router: pipeline.router(),
            run: RunId::new("m", 0),
            source,
            buf: Vec::new(),
        };
        (writer, store)
    }

    fn messages(store: &CaptureStore) -> Vec<String> {
        store
            .records
            .lock()
            .iter()
            .map(|r| r.message.clone())
            .collect()
    }

    #[tokio::test]
    async fn splits_on_newlines() {
        let (mut w, store) = setup(LogSource::Stdout);
        w.write_all(b"alpha\nbeta\n").await.unwrap();
        assert_eq!(messages(&store), ["alpha", "beta"]);
    }

    #[tokio::test]
    async fn buffers_a_partial_line_until_the_newline_arrives() {
        let (mut w, store) = setup(LogSource::Stdout);
        w.write_all(b"partial").await.unwrap();
        assert!(messages(&store).is_empty(), "no newline yet");
        w.write_all(b" line\n").await.unwrap();
        assert_eq!(messages(&store), ["partial line"]);
    }

    #[tokio::test]
    async fn reassembles_a_utf8_code_point_split_across_writes() {
        // The euro sign is three bytes; splitting mid-code-point across
        // two writes must not corrupt the decoded line.
        let euro = "\u{20ac}".as_bytes();
        let (mut w, store) = setup(LogSource::Stdout);
        w.write_all(&euro[..1]).await.unwrap();
        w.write_all(&euro[1..]).await.unwrap();
        w.write_all(b"\n").await.unwrap();
        assert_eq!(messages(&store), ["\u{20ac}"]);
    }

    #[tokio::test]
    async fn interleaved_writes_accumulate_into_one_line() {
        let (mut w, store) = setup(LogSource::Stdout);
        for chunk in [&b"a"[..], b"b", b"c", b"\n", b"d", b"e", b"\n"] {
            w.write_all(chunk).await.unwrap();
        }
        assert_eq!(messages(&store), ["abc", "de"]);
    }

    #[tokio::test]
    async fn final_unterminated_line_is_flushed_on_drop() {
        let (mut w, store) = setup(LogSource::Stdout);
        w.write_all(b"no trailing newline").await.unwrap();
        assert!(messages(&store).is_empty(), "buffered, not yet flushed");
        drop(w);
        assert_eq!(messages(&store), ["no trailing newline"]);
    }

    #[tokio::test]
    async fn empty_lines_are_skipped() {
        let (mut w, store) = setup(LogSource::Stdout);
        w.write_all(b"\n\nkept\n\n").await.unwrap();
        assert_eq!(messages(&store), ["kept"]);
    }

    #[tokio::test]
    async fn trailing_carriage_return_is_trimmed() {
        let (mut w, store) = setup(LogSource::Stdout);
        w.write_all(b"crlf\r\n").await.unwrap();
        assert_eq!(messages(&store), ["crlf"]);
    }

    #[tokio::test]
    async fn stderr_lines_carry_the_warn_level() {
        let (mut w, store) = setup(LogSource::Stderr);
        w.write_all(b"oops\n").await.unwrap();
        let records = store.records.lock();
        assert_eq!(records[0].source, LogSource::Stderr);
        assert_eq!(records[0].level, Level::WARN);
    }

    #[tokio::test]
    async fn over_long_unterminated_line_is_force_flushed() {
        let (mut w, store) = setup(LogSource::Stdout);
        let flood = vec![b'x'; MAX_LINE_BYTES + 1];
        w.write_all(&flood).await.unwrap();
        // The force-flush bounds host memory without waiting for a newline.
        assert_eq!(messages(&store).len(), 1);
        assert_eq!(messages(&store)[0].len(), MAX_LINE_BYTES + 1);
    }
}
