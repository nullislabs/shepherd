//! `nexum:host/logging`: constructs a `HostInterface` [`LogRecord`] from
//! the guest's `log` call and hands it to the shared router, which tags
//! it with the run and fans it to the tracing consumer and the store.

use crate::bindings::nexum;
use crate::host::component::RuntimeTypes;
use crate::host::logs::{LogLevel, LogRecord, LogSource};
use crate::host::state::HostState;

impl<T: RuntimeTypes> nexum::host::logging::Host for HostState<T> {
    async fn log(&mut self, level: nexum::host::logging::Level, message: String) {
        use nexum::host::logging::Level;
        let level = match level {
            Level::Trace => LogLevel::Trace,
            Level::Debug => LogLevel::Debug,
            Level::Info => LogLevel::Info,
            Level::Warn => LogLevel::Warn,
            Level::Error => LogLevel::Error,
        };
        self.log_router.record(LogRecord::now(
            self.run.clone(),
            LogSource::HostInterface,
            level,
            message,
        ));
    }
}
