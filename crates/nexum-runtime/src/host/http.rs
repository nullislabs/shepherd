//! wasi:http outgoing gate: every guest request funnels through
//! [`HttpGate::send_request`], which enforces the per-module
//! `[capabilities.http].allow` list before handing the request to the
//! backend. The host does not follow redirects, so each hop is a fresh
//! guest request that re-enters this gate.

use tracing::warn;
use wasmtime_wasi_http::p2::bindings::http::types::ErrorCode;
use wasmtime_wasi_http::p2::body::HyperOutgoingBody;
use wasmtime_wasi_http::p2::types::{HostFutureIncomingResponse, OutgoingRequestConfig};
use wasmtime_wasi_http::p2::{
    HttpResult, WasiHttpCtxView, WasiHttpHooks, WasiHttpView, default_send_request,
};

use super::component::RuntimeTypes;
use super::state::HostState;
use crate::manifest::host_allowed;

/// Per-module outbound HTTP policy: the manifest allowlist plus the
/// module name for log attribution.
pub struct HttpGate {
    module: String,
    allowlist: Vec<String>,
}

impl HttpGate {
    /// Gate for `module` with its `[capabilities.http].allow` entries.
    pub fn new(module: impl Into<String>, allowlist: Vec<String>) -> Self {
        Self {
            module: module.into(),
            allowlist,
        }
    }
}

impl WasiHttpHooks for HttpGate {
    fn send_request(
        &mut self,
        request: http::Request<HyperOutgoingBody>,
        config: OutgoingRequestConfig,
    ) -> HttpResult<HostFutureIncomingResponse> {
        if let Err(code) = admit(request.uri(), &self.allowlist) {
            // Log the host only: paths and query strings are
            // guest-supplied and may carry credentials.
            warn!(
                module = %self.module,
                host = request.uri().host().unwrap_or("<none>"),
                "[http] outbound request denied by allowlist",
            );
            return Err(code.into());
        }
        Ok(default_send_request(request, config))
    }
}

/// Allowlist decision for one outgoing request URI.
///
/// Matching is host-only: ports and scheme are ignored (the handler
/// admits only http/https before this point), and comparison is
/// case-insensitive with exact or `*.suffix` wildcard semantics per
/// [`host_allowed`]. IPv6 literals keep their brackets, so allowlist
/// entries use the bracketed form.
///
/// The check is name-based and precedes resolution: the connection
/// re-resolves the name, so there is no IP pinning and no defence
/// against DNS rebinding or names resolving to internal addresses.
/// The operator vouches for the names they allowlist.
fn admit(uri: &http::Uri, allowlist: &[String]) -> Result<(), ErrorCode> {
    let Some(host) = uri.host() else {
        return Err(ErrorCode::HttpRequestUriInvalid);
    };
    if host_allowed(host, allowlist) {
        Ok(())
    } else {
        Err(ErrorCode::HttpRequestDenied)
    }
}

impl<T: RuntimeTypes> WasiHttpView for HostState<T> {
    fn http(&mut self) -> WasiHttpCtxView<'_> {
        WasiHttpCtxView {
            ctx: &mut self.http_ctx,
            table: &mut self.table,
            hooks: &mut self.http_gate,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use bytes::Bytes;
    use http_body_util::{BodyExt, Empty};

    use super::*;

    fn uri(s: &str) -> http::Uri {
        s.parse().expect("test URI parses")
    }

    fn allow(entries: &[&str]) -> Vec<String> {
        entries.iter().map(|s| s.to_string()).collect()
    }

    fn denied(u: &str, entries: &[&str]) -> bool {
        matches!(
            admit(&uri(u), &allow(entries)),
            Err(ErrorCode::HttpRequestDenied)
        )
    }

    #[test]
    fn exact_host_passes() {
        assert!(admit(&uri("https://api.cow.fi/v1/x"), &allow(&["api.cow.fi"])).is_ok());
        assert!(admit(&uri("http://api.cow.fi/"), &allow(&["api.cow.fi"])).is_ok());
    }

    #[test]
    fn off_list_host_is_denied() {
        assert!(denied("https://evil.example/", &["api.cow.fi"]));
        assert!(denied("https://api.cow.fi.evil.example/", &["api.cow.fi"]));
    }

    #[test]
    fn empty_allowlist_denies_everything() {
        assert!(denied("https://api.cow.fi/", &[]));
        assert!(denied("http://127.0.0.1/", &[]));
    }

    #[test]
    fn matching_is_case_insensitive() {
        assert!(admit(&uri("https://API.COW.FI/"), &allow(&["api.cow.fi"])).is_ok());
        assert!(admit(&uri("https://api.cow.fi/"), &allow(&["API.COW.FI"])).is_ok());
    }

    #[test]
    fn wildcard_matches_subdomains_but_not_the_suffix_itself() {
        let list = allow(&["*.discord.com"]);
        assert!(admit(&uri("https://gateway.discord.com/"), &list).is_ok());
        assert!(admit(&uri("https://a.b.discord.com/"), &list).is_ok());
        assert!(denied("https://discord.com/", &["*.discord.com"]));
        assert!(denied("https://notdiscord.com/", &["*.discord.com"]));
    }

    #[test]
    fn exact_entry_does_not_match_subdomains() {
        assert!(denied("https://sub.api.cow.fi/", &["api.cow.fi"]));
    }

    #[test]
    fn ipv4_literal_matches_only_when_listed() {
        assert!(admit(&uri("http://127.0.0.1/x"), &allow(&["127.0.0.1"])).is_ok());
        assert!(denied("http://127.0.0.2/x", &["127.0.0.1"]));
        // A listed name never admits an IP literal for that name.
        assert!(denied("http://93.184.216.34/", &["example.com"]));
    }

    #[test]
    fn ipv6_literal_uses_bracketed_form() {
        assert!(admit(&uri("http://[::1]:8080/x"), &allow(&["[::1]"])).is_ok());
        assert!(denied("http://[::1]/x", &["::1"]));
        assert!(denied("http://[2001:db8::1]/", &["[::1]"]));
    }

    #[test]
    fn ports_do_not_affect_matching() {
        let list = allow(&["api.cow.fi"]);
        assert!(admit(&uri("https://api.cow.fi:8443/v1"), &list).is_ok());
        assert!(admit(&uri("http://api.cow.fi:80/v1"), &list).is_ok());
        assert!(denied("https://evil.example:443/", &["api.cow.fi"]));
        // A port spelled in the allowlist entry never matches: entries
        // are hosts, not authorities.
        assert!(denied("https://api.cow.fi:8443/", &["api.cow.fi:8443"]));
    }

    #[test]
    fn both_schemes_are_gated_identically() {
        for scheme in ["http", "https"] {
            assert!(
                admit(
                    &uri(&format!("{scheme}://api.cow.fi/")),
                    &allow(&["api.cow.fi"])
                )
                .is_ok()
            );
            assert!(denied(
                &format!("{scheme}://evil.example/"),
                &["api.cow.fi"]
            ));
        }
    }

    #[test]
    fn uri_without_authority_is_invalid_not_denied() {
        assert!(matches!(
            admit(&uri("/relative/path"), &allow(&["api.cow.fi"])),
            Err(ErrorCode::HttpRequestUriInvalid)
        ));
    }

    fn request(u: &str) -> http::Request<HyperOutgoingBody> {
        let body = Empty::<Bytes>::new()
            .map_err(|_| unreachable!("infallible body error"))
            .boxed_unsync();
        http::Request::builder()
            .method(http::Method::GET)
            .uri(u)
            .body(body)
            .expect("test request builds")
    }

    fn config() -> OutgoingRequestConfig {
        OutgoingRequestConfig {
            use_tls: false,
            connect_timeout: Duration::from_secs(1),
            first_byte_timeout: Duration::from_secs(1),
            between_bytes_timeout: Duration::from_secs(1),
        }
    }

    #[tokio::test]
    async fn send_request_denies_off_list_host_with_http_request_denied() {
        let mut gate = HttpGate::new("test-module", allow(&["api.cow.fi"]));
        let Err(err) = gate.send_request(request("http://evil.example/x"), config()) else {
            panic!("off-list host must be denied");
        };
        assert!(matches!(
            err.downcast_ref(),
            Some(ErrorCode::HttpRequestDenied)
        ));
    }

    #[tokio::test]
    async fn send_request_admits_listed_host() {
        // Nothing listens on 127.0.0.1:1; admission only hands the
        // request to the backend, so the returned future is pending.
        let mut gate = HttpGate::new("test-module", allow(&["127.0.0.1"]));
        assert!(
            gate.send_request(request("http://127.0.0.1:1/x"), config())
                .is_ok()
        );
    }
}
