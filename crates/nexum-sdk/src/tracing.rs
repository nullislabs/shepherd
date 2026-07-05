//! Guest-side `tracing` facade: routes `tracing` events to a host log
//! sink so module authors write `tracing::info!(...)` with no host
//! parameter to thread.
//!
//! The subscriber is events-only: it renders each event's fields into
//! one line and forwards it at a mapped [`Level`]. Spans are inert
//! no-ops. It links `tracing-core` alone, not the subscriber registry,
//! so the wasm module stays small.
//!
//! The [`init`] call also installs a panic hook that writes the panic
//! to stderr and then reports it over the same sink. Stderr is written
//! first on purpose: host-side stderr capture still records the panic
//! even if the sink's host call traps before `panic = abort` fires.

use core::fmt::{self, Write as _};
use core::sync::atomic::{AtomicU64, Ordering};
use std::panic::PanicHookInfo;
use std::sync::Arc;

use tracing_core::field::{Field, Visit};
use tracing_core::span::{Attributes, Id, Record};
use tracing_core::{Event, LevelFilter, Metadata, Subscriber};

/// Severity forwarded to the host. One-to-one with `tracing`'s five
/// levels and with the host logging interface's enum.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Level {
    /// Verbose developer tracing.
    Trace,
    /// Operator detail when investigating.
    Debug,
    /// Steady-state events.
    Info,
    /// Recoverable problems.
    Warn,
    /// Unrecoverable problems.
    Error,
}

impl From<tracing_core::Level> for Level {
    fn from(level: tracing_core::Level) -> Self {
        // `tracing_core::Level` is a struct of associated consts, not a
        // matchable enum, so compare rather than pattern-match.
        use tracing_core::Level as T;
        if level == T::ERROR {
            Self::Error
        } else if level == T::WARN {
            Self::Warn
        } else if level == T::INFO {
            Self::Info
        } else if level == T::DEBUG {
            Self::Debug
        } else {
            Self::Trace
        }
    }
}

/// Sink the facade forwards rendered events to. Implementors carry the
/// bound host logging call; the host decides how each line is handled.
pub trait LogSink: Send + Sync {
    /// Forward one rendered line at `level`.
    fn log(&self, level: Level, message: &str);
}

/// Install the facade as the global subscriber and register the panic
/// hook, both forwarding to `sink`. The subscriber is set once; a
/// second call leaves it in place and only re-registers the panic hook.
pub fn init(sink: impl LogSink + 'static) {
    let sink: Arc<dyn LogSink> = Arc::new(sink);
    let dispatch = tracing_core::Dispatch::new(FacadeSubscriber::new(Arc::clone(&sink)));
    // A second install is a no-op: the global default is set once.
    let _ = tracing_core::dispatcher::set_global_default(dispatch);
    set_panic_hook(sink);
}

/// Build the events-only subscriber over `sink` without touching global
/// state. Test harnesses scope it with `tracing::subscriber::with_default`.
pub fn subscriber(sink: impl LogSink + 'static) -> impl Subscriber {
    FacadeSubscriber::new(Arc::new(sink))
}

fn set_panic_hook(sink: Arc<dyn LogSink>) {
    std::panic::set_hook(Box::new(move |info| {
        let message = format_panic(
            &panic_payload(info),
            info.location().map(|l| (l.file(), l.line())),
        );
        // stderr first: host-side stderr capture still records the panic
        // even if the sink's host call traps before `panic = abort` fires.
        eprintln!("{message}");
        sink.log(Level::Error, &message);
    }));
}

fn panic_payload(info: &PanicHookInfo<'_>) -> String {
    if let Some(s) = info.payload().downcast_ref::<&str>() {
        (*s).to_owned()
    } else if let Some(s) = info.payload().downcast_ref::<String>() {
        s.clone()
    } else {
        "<non-string panic payload>".to_owned()
    }
}

/// Render a panic into the reported line. Pure so it is unit-testable
/// apart from the abort path.
fn format_panic(payload: &str, location: Option<(&str, u32)>) -> String {
    match location {
        Some((file, line)) => format!("panic: {payload} at {file}:{line}"),
        None => format!("panic: {payload}"),
    }
}

struct FacadeSubscriber {
    sink: Arc<dyn LogSink>,
    next_id: AtomicU64,
}

impl FacadeSubscriber {
    fn new(sink: Arc<dyn LogSink>) -> Self {
        Self {
            sink,
            next_id: AtomicU64::new(0),
        }
    }
}

impl Subscriber for FacadeSubscriber {
    fn enabled(&self, _metadata: &Metadata<'_>) -> bool {
        // Forward everything; the host applies its own filter.
        true
    }

    fn max_level_hint(&self) -> Option<LevelFilter> {
        Some(LevelFilter::TRACE)
    }

    fn new_span(&self, _span: &Attributes<'_>) -> Id {
        // Spans are inert, but a valid non-zero id must be returned.
        let raw = self.next_id.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
        Id::from_u64(raw.max(1))
    }

    fn record(&self, _span: &Id, _values: &Record<'_>) {}

    fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

    fn event(&self, event: &Event<'_>) {
        let level = Level::from(*event.metadata().level());
        let mut visitor = LineVisitor::default();
        event.record(&mut visitor);
        let line = visitor.finish();
        self.sink.log(level, &line);
        #[cfg(feature = "stderr-echo")]
        eprintln!("[{level:?}] {line}");
    }

    fn enter(&self, _span: &Id) {}

    fn exit(&self, _span: &Id) {}
}

/// Flattens an event into `<message> key=value ...`: the `message`
/// field becomes the line body, every other field is appended as a
/// space-separated `key=value` pair in record order.
#[derive(Default)]
struct LineVisitor {
    message: String,
    fields: String,
}

impl LineVisitor {
    fn finish(mut self) -> String {
        if self.message.is_empty() {
            // A field-only event would otherwise carry a leading space.
            return self.fields.trim_start().to_owned();
        }
        self.message.push_str(&self.fields);
        self.message
    }
}

impl Visit for LineVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if field.name() == "message" {
            let _ = write!(self.message, "{value:?}");
        } else {
            let _ = write!(self.fields, " {}={value:?}", field.name());
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message.push_str(value);
        } else {
            let _ = write!(self.fields, " {}={value}", field.name());
        }
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        let _ = write!(self.fields, " {}={value}", field.name());
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        let _ = write!(self.fields, " {}={value}", field.name());
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        let _ = write!(self.fields, " {}={value}", field.name());
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    /// Capturing sink for the with_default-scoped subscriber.
    #[derive(Default)]
    struct Captured {
        lines: Mutex<Vec<(Level, String)>>,
    }

    impl LogSink for Arc<Captured> {
        fn log(&self, level: Level, message: &str) {
            self.lines.lock().unwrap().push((level, message.to_owned()));
        }
    }

    fn capture(f: impl FnOnce()) -> Vec<(Level, String)> {
        let sink = Arc::new(Captured::default());
        let subscriber = subscriber(Arc::clone(&sink));
        tracing::subscriber::with_default(subscriber, f);
        sink.lines.lock().unwrap().clone()
    }

    #[test]
    fn level_maps_one_to_one_from_tracing() {
        assert_eq!(Level::from(tracing_core::Level::TRACE), Level::Trace);
        assert_eq!(Level::from(tracing_core::Level::DEBUG), Level::Debug);
        assert_eq!(Level::from(tracing_core::Level::INFO), Level::Info);
        assert_eq!(Level::from(tracing_core::Level::WARN), Level::Warn);
        assert_eq!(Level::from(tracing_core::Level::ERROR), Level::Error);
    }

    #[test]
    fn each_macro_level_forwards_at_its_mapped_level() {
        let lines = capture(|| {
            tracing::trace!("t");
            tracing::debug!("d");
            tracing::info!("i");
            tracing::warn!("w");
            tracing::error!("e");
        });
        let levels: Vec<Level> = lines.iter().map(|(l, _)| *l).collect();
        assert_eq!(
            levels,
            vec![
                Level::Trace,
                Level::Debug,
                Level::Info,
                Level::Warn,
                Level::Error
            ]
        );
    }

    #[test]
    fn message_only_event_renders_bare_message() {
        let lines = capture(|| tracing::info!("hello world"));
        assert_eq!(lines, vec![(Level::Info, "hello world".to_owned())]);
    }

    #[test]
    fn formatted_message_renders_without_field_suffix() {
        let lines = capture(|| tracing::info!("value is {}", 41 + 1));
        assert_eq!(lines, vec![(Level::Info, "value is 42".to_owned())]);
    }

    #[test]
    fn fields_flatten_across_types_after_message() {
        let lines = capture(|| {
            tracing::warn!(
                name = "eth",
                count = 7u64,
                signed = -3i64,
                ready = true,
                answer = ?Some(9),
                "changed"
            );
        });
        assert_eq!(
            lines,
            vec![(
                Level::Warn,
                "changed name=eth count=7 signed=-3 ready=true answer=Some(9)".to_owned()
            )]
        );
    }

    #[test]
    fn fieldset_without_message_renders_only_pairs() {
        let lines = capture(|| tracing::info!(a = 1u64, b = "x"));
        assert_eq!(lines, vec![(Level::Info, "a=1 b=x".to_owned())]);
    }

    #[test]
    fn spans_are_inert_no_ops() {
        let lines = capture(|| {
            let span = tracing::info_span!("work", key = "v");
            let _entered = span.enter();
            span.record("key", "v2");
        });
        assert!(
            lines.is_empty(),
            "span lifecycle produced events: {lines:?}"
        );
    }

    #[test]
    fn format_panic_with_and_without_location() {
        assert_eq!(
            format_panic("boom", Some(("src/lib.rs", 42))),
            "panic: boom at src/lib.rs:42"
        );
        assert_eq!(format_panic("boom", None), "panic: boom");
    }
}
