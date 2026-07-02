//! `nexum:host/http`: manifest allowlist check, then `Unsupported`.
//!
//! Real `fetch` lands in 0.3. The allowlist is enforced now so a
//! module that ships with an empty (or no) `[capabilities.http].allow`
//! gets denied loudly, matching the "no implicit network" stance.

use tracing::warn;

use crate::bindings::HostError;
use crate::bindings::nexum;
use crate::bindings::nexum::host::types::HostErrorKind;
use crate::host::error::unimplemented;
use crate::host::state::HostState;
use crate::manifest::{extract_host, host_allowed};

impl nexum::host::http::Host for HostState {
    async fn fetch(
        &mut self,
        req: nexum::host::http::Request,
    ) -> Result<nexum::host::http::Response, HostError> {
        let host = match extract_host(&req.url) {
            Some(h) => h,
            None => {
                return Err(HostError {
                    domain: "http".into(),
                    kind: HostErrorKind::InvalidInput,
                    code: 0,
                    message: format!("not an http(s) URL: {}", req.url),
                    data: None,
                });
            }
        };
        if !host_allowed(&host, &self.http_allowlist) {
            warn!(host = %host, "[http] denied by allowlist");
            return Err(HostError {
                domain: "http".into(),
                kind: HostErrorKind::Denied,
                code: 0,
                message: format!(
                    "host {host} not in [capabilities.http].allow; \
                     add it to module.toml to permit"
                ),
                data: None,
            });
        }
        Err(unimplemented(
            "http",
            "fetch not implemented in 0.2 reference runtime (allowlist passed)",
        ))
    }
}
