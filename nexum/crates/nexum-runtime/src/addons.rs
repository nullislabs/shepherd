//! Cross-cutting runtime add-ons: process-wide facilities that attach to the
//! launch path without the core knowing their concrete type. An add-on
//! installs a facility from the resolved config and returns a handle the
//! launcher keeps alive for the run.

use tracing::info;

use crate::engine_config::MetricsSection;

/// Inputs an add-on reads at install time.
pub struct AddOnsContext<'a> {
    /// Resolved `[engine.metrics]` config.
    pub metrics: &'a MetricsSection,
}

/// A live add-on installation, retained by the launcher for the run.
pub struct AddOnHandle {
    /// The add-on's name, for diagnostics.
    pub name: &'static str,
}

impl AddOnHandle {
    /// A handle for an add-on that needs no teardown resource.
    pub fn named(name: &'static str) -> Self {
        Self { name }
    }
}

/// A process-wide facility attached to the launch path.
pub trait RuntimeAddOn {
    /// Install the facility, returning its live handle.
    fn install(&self, ctx: &AddOnsContext<'_>) -> anyhow::Result<AddOnHandle>;
}

/// An owned, ordered add-on set.
pub type AddOns = Vec<Box<dyn RuntimeAddOn>>;

/// The Prometheus exporter add-on. With `[engine.metrics].enabled = true` it
/// binds an HTTP listener serving `/metrics`; otherwise it installs the
/// recorder alone so `metrics::counter!` call sites stay live but no port opens.
pub struct PrometheusAddOn;

impl RuntimeAddOn for PrometheusAddOn {
    fn install(&self, ctx: &AddOnsContext<'_>) -> anyhow::Result<AddOnHandle> {
        if ctx.metrics.enabled {
            let addr: std::net::SocketAddr = ctx.metrics.bind_addr.parse().map_err(|e| {
                anyhow::anyhow!(
                    "invalid [engine.metrics].bind_addr `{}`: {e}",
                    ctx.metrics.bind_addr
                )
            })?;
            metrics_exporter_prometheus::PrometheusBuilder::new()
                .with_http_listener(addr)
                .install()
                .map_err(|e| anyhow::anyhow!("install Prometheus exporter on {addr}: {e}"))?;
            info!(addr = %addr, "metrics exporter listening at /metrics");
        } else {
            // Recorder installed globally so metrics call sites stay live;
            // no HTTP port is opened. It accumulates samples in memory, unread.
            metrics_exporter_prometheus::PrometheusBuilder::new()
                .install_recorder()
                .map_err(|e| anyhow::anyhow!("install Prometheus recorder: {e}"))?;
        }
        Ok(AddOnHandle::named("prometheus"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine_config::MetricsSection;

    /// An enabled exporter with an unparseable bind address fails at install.
    #[test]
    fn prometheus_add_on_rejects_an_invalid_bind_addr() {
        let metrics = MetricsSection {
            enabled: true,
            bind_addr: "not-a-socket-addr".to_owned(),
        };
        let ctx = AddOnsContext { metrics: &metrics };
        let err = match PrometheusAddOn.install(&ctx) {
            Ok(_) => panic!("invalid bind_addr must not install"),
            Err(err) => err,
        };
        assert!(err.to_string().contains("bind_addr"), "{err}");
    }
}
