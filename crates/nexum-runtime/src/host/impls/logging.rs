//! `nexum:host/logging`: routes guest log lines through the host's
//! `tracing` subscriber, tagged with the module namespace.

use crate::bindings::nexum;
use crate::host::state::HostState;

impl nexum::host::logging::Host for HostState {
    async fn log(&mut self, level: nexum::host::logging::Level, message: String) {
        let module = self.module_namespace.as_str();
        match level {
            nexum::host::logging::Level::Trace => tracing::trace!(module, "{}", message),
            nexum::host::logging::Level::Debug => tracing::debug!(module, "{}", message),
            nexum::host::logging::Level::Info => tracing::info!(module, "{}", message),
            nexum::host::logging::Level::Warn => tracing::warn!(module, "{}", message),
            nexum::host::logging::Level::Error => tracing::error!(module, "{}", message),
        }
    }
}
