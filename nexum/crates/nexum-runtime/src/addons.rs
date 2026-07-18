//! Cross-cutting runtime add-ons: process-wide facilities that attach to
//! the launch path without the core knowing their concrete type.
//!
//! An add-on installs a facility from the resolved config (a metrics
//! recorder today) and hands back a handle the launcher keeps alive for the
//! run. The composition root chooses the set, so an embedder omits or
//! replaces any of them instead of inheriting a fixed install.
//!
//! A future control-surface add-on (an admin or RPC socket) slots in beside
//! [`PrometheusAddOn`]: implement [`RuntimeAddOn`], read its own section
//! from [`AddOnsContext`], and add it to the launcher's list at the
//! composition root.

use tracing::info;

use crate::engine_config::MetricsSection;

/// Inputs an add-on reads at install time. Grows as add-ons are added: a
/// future control-surface add-on carries its own resolved section here.
pub struct AddOnsContext<'a> {
    /// Resolved `[engine.metrics]` config.
    pub metrics: &'a MetricsSection,
}

/// A live add-on installation, retained by the launcher for the length of
/// the run. Names the add-on for diagnostics; an add-on that needs RAII
/// teardown grows a resource slot here when one arrives.
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

/// A process-wide facility attached to the launch path. `install` reads the
/// resolved config from `ctx` and returns a handle the launcher retains.
pub trait RuntimeAddOn {
    /// Install the facility, returning its live handle.
    fn install(&self, ctx: &AddOnsContext<'_>) -> anyhow::Result<AddOnHandle>;
}

/// An owned, ordered add-on set gathered behind one value. A preset or
/// composition root returns this so a heterogeneous set travels together;
/// the launcher borrows each element to install it.
pub type AddOns = Vec<Box<dyn RuntimeAddOn>>;

/// The Prometheus exporter add-on. With `[engine.metrics].enabled = true`
/// it binds an HTTP listener serving `/metrics`; otherwise it installs the
/// recorder alone so `metrics::counter!` call sites stay live but no port
/// opens. The same binary thus runs in CI without binding a port and in
/// production with observability by flipping one config flag.
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

    /// An enabled exporter with an unparseable bind address surfaces the
    /// wrapped error at install, before any recorder is touched.
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
