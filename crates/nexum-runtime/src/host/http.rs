//! wasi:http outgoing gate: every guest request funnels through
//! [`HttpGate::send_request`], which enforces the per-module
//! `[capabilities.http].allow` list, clamps the guest-settable timeouts
//! to the engine's `[limits.http]` maxima, and bounds the exchange with
//! a total deadline plus a response-body cap before handing the request
//! to the backend. The host does not follow redirects, so each hop is a
//! fresh guest request that re-enters this gate.

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::Bytes;
use http_body::{Body, Frame, SizeHint};
use http_body_util::BodyExt;
use tracing::warn;
use wasmtime_wasi_http::p2::bindings::http::types::ErrorCode;
use wasmtime_wasi_http::p2::body::{HyperIncomingBody, HyperOutgoingBody};
use wasmtime_wasi_http::p2::types::{HostFutureIncomingResponse, OutgoingRequestConfig};
use wasmtime_wasi_http::p2::{
    HttpResult, WasiHttpCtxView, WasiHttpHooks, WasiHttpView, default_send_request_handler,
};

use super::component::RuntimeTypes;
use super::state::HostState;
use crate::engine_config::OutboundHttpLimits;
use crate::manifest::host_allowed;

/// Per-module outbound HTTP policy: the manifest allowlist, the
/// engine's outbound limits, and the module name for log attribution.
pub struct HttpGate {
    module: String,
    allowlist: Vec<String>,
    limits: OutboundHttpLimits,
}

impl HttpGate {
    /// Gate for `module` with its `[capabilities.http].allow` entries
    /// and the engine's `[limits.http]` outbound limits.
    pub fn new(
        module: impl Into<String>,
        allowlist: Vec<String>,
        limits: OutboundHttpLimits,
    ) -> Self {
        Self {
            module: module.into(),
            allowlist,
            limits,
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
        Ok(send_with_limits(
            request,
            clamp(config, &self.limits),
            self.limits,
        ))
    }
}

/// Clamp the guest-settable timeouts to the engine maxima. Guest values
/// above a maximum are lowered, never rejected. The linked handler
/// substitutes its own fixed default for unset request-options before
/// this hook runs, so an unset timeout also clamps down: each maximum
/// doubles as the effective default.
fn clamp(mut config: OutgoingRequestConfig, limits: &OutboundHttpLimits) -> OutgoingRequestConfig {
    config.connect_timeout = config.connect_timeout.min(limits.connect_timeout_max);
    config.first_byte_timeout = config.first_byte_timeout.min(limits.first_byte_timeout_max);
    config.between_bytes_timeout = config
        .between_bytes_timeout
        .min(limits.between_bytes_timeout_max);
    config
}

/// Dispatch through the default backend, bounded by the engine's total
/// deadline and response-body cap. The `timeout_at` covers connect,
/// TLS, request write, and response headers; the same deadline instant
/// is armed inside the [`CappedBody`] wrapping the response body, so a
/// consuming guest gets `ConnectionReadTimeout` mid-body. The deadline
/// is unconditional: the connection driver is raced against it in its
/// own task and aborted when it fires, so a guest that parks the
/// response without ever reading the body cannot hold the socket past
/// the deadline.
fn send_with_limits(
    request: http::Request<HyperOutgoingBody>,
    config: OutgoingRequestConfig,
    limits: OutboundHttpLimits,
) -> HostFutureIncomingResponse {
    let handle = wasmtime_wasi::runtime::spawn(async move {
        let deadline = tokio::time::Instant::now() + limits.total_deadline;
        let sent =
            tokio::time::timeout_at(deadline, default_send_request_handler(request, config)).await;
        let result = match sent {
            Ok(Ok(mut incoming)) => {
                // Dropping the inner worker handle aborts the hyper
                // connection driver, closing the socket at the
                // deadline regardless of guest polling. A guest drop
                // of the response still cascades: it drops this
                // wrapper handle, which aborts the race, which drops
                // the worker.
                incoming.worker = incoming.worker.map(|worker| {
                    wasmtime_wasi::runtime::spawn(async move {
                        let _ = tokio::time::timeout_at(deadline, worker).await;
                    })
                });
                incoming.resp = incoming.resp.map(|body| {
                    CappedBody::new(body, limits.response_body_max_bytes, deadline).boxed_unsync()
                });
                Ok(incoming)
            }
            Ok(Err(code)) => Err(code),
            Err(_) => Err(ErrorCode::ConnectionTimeout),
        };
        Ok(result)
    });
    HostFutureIncomingResponse::pending(handle)
}

/// Response-body wrapper enforcing the size cap and the total deadline
/// while the guest streams the body.
///
/// Exceeding the cap yields `HttpResponseBodySize(cap)`; the deadline
/// firing mid-body yields `ConnectionReadTimeout`, the code the backend
/// uses for its own read-phase timeouts.
struct CappedBody {
    inner: HyperIncomingBody,
    /// Bytes still admissible under the cap.
    remaining: u64,
    /// Configured cap, echoed in the error payload.
    cap: u64,
    /// Sleep armed at the request's total deadline.
    deadline: Pin<Box<tokio::time::Sleep>>,
}

impl CappedBody {
    fn new(inner: HyperIncomingBody, cap: u64, deadline: tokio::time::Instant) -> Self {
        Self {
            inner,
            remaining: cap,
            cap,
            deadline: Box::pin(tokio::time::sleep_until(deadline)),
        }
    }
}

impl Body for CappedBody {
    type Data = Bytes;
    type Error = ErrorCode;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, ErrorCode>>> {
        let me = Pin::into_inner(self);
        if let Poll::Ready(()) = me.deadline.as_mut().poll(cx) {
            return Poll::Ready(Some(Err(ErrorCode::ConnectionReadTimeout)));
        }
        match Pin::new(&mut me.inner).poll_frame(cx) {
            Poll::Ready(Some(Ok(frame))) => {
                if let Some(data) = frame.data_ref() {
                    let len = data.len() as u64;
                    if len > me.remaining {
                        return Poll::Ready(Some(Err(ErrorCode::HttpResponseBodySize(Some(
                            me.cap,
                        )))));
                    }
                    me.remaining -= len;
                }
                Poll::Ready(Some(Ok(frame)))
            }
            other => other,
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
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

    use http_body_util::{Empty, Full};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use wasmtime_wasi_http::p2::types::IncomingResponse;

    use super::*;

    fn uri(s: &str) -> http::Uri {
        s.parse().expect("test URI parses")
    }

    fn allow(entries: &[&str]) -> Vec<String> {
        entries.iter().map(|s| s.to_string()).collect()
    }

    /// Generous limits so a test trips only the one it tightens.
    fn limits() -> OutboundHttpLimits {
        OutboundHttpLimits {
            connect_timeout_max: Duration::from_secs(10),
            first_byte_timeout_max: Duration::from_secs(10),
            between_bytes_timeout_max: Duration::from_secs(10),
            total_deadline: Duration::from_secs(10),
            response_body_max_bytes: 1 << 20,
        }
    }

    fn denied(u: &str, entries: &[&str]) -> bool {
        matches!(
            admit(&uri(u), &allow(entries)),
            Err(ErrorCode::HttpRequestDenied)
        )
    }

    #[test]
    fn exact_host_passes() {
        assert!(
            admit(
                &uri("https://api.acme.example/v1/x"),
                &allow(&["api.acme.example"])
            )
            .is_ok()
        );
        assert!(
            admit(
                &uri("http://api.acme.example/"),
                &allow(&["api.acme.example"])
            )
            .is_ok()
        );
    }

    #[test]
    fn off_list_host_is_denied() {
        assert!(denied("https://evil.example/", &["api.acme.example"]));
        assert!(denied(
            "https://api.acme.example.evil.example/",
            &["api.acme.example"]
        ));
    }

    #[test]
    fn empty_allowlist_denies_everything() {
        assert!(denied("https://api.acme.example/", &[]));
        assert!(denied("http://127.0.0.1/", &[]));
    }

    #[test]
    fn matching_is_case_insensitive() {
        assert!(
            admit(
                &uri("https://API.ACME.EXAMPLE/"),
                &allow(&["api.acme.example"])
            )
            .is_ok()
        );
        assert!(
            admit(
                &uri("https://api.acme.example/"),
                &allow(&["API.ACME.EXAMPLE"])
            )
            .is_ok()
        );
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
        assert!(denied(
            "https://sub.api.acme.example/",
            &["api.acme.example"]
        ));
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
        let list = allow(&["api.acme.example"]);
        assert!(admit(&uri("https://api.acme.example:8443/v1"), &list).is_ok());
        assert!(admit(&uri("http://api.acme.example:80/v1"), &list).is_ok());
        assert!(denied("https://evil.example:443/", &["api.acme.example"]));
        // A port spelled in the allowlist entry never matches: entries
        // are hosts, not authorities.
        assert!(denied(
            "https://api.acme.example:8443/",
            &["api.acme.example:8443"]
        ));
    }

    // ----------------- SSRF-style bypass regressions (#57) ---------
    //
    // `http::Uri` resolves the authority per RFC 3986 before `admit`
    // ever sees a host string, so these are regression guards on the
    // parser's behaviour, not on `admit` itself. Each case names the
    // trick and asserts the real target host - never the attacker's
    // decoy - is what `host_allowed` sees.

    #[test]
    fn userinfo_prefix_does_not_leak_a_different_host_into_the_allowlist() {
        // `http://allowed.com@evil.com/` - "allowed.com" is userinfo,
        // "evil.com" is the host. A parser that mistook the text before
        // `@` for the host would wrongly admit this against an
        // `allowed.com` allowlist entry.
        assert!(denied("http://allowed.com@evil.com/", &["allowed.com"]));
        assert_eq!(uri("http://allowed.com@evil.com/").host(), Some("evil.com"));
    }

    #[test]
    fn userinfo_matching_an_allowlist_entry_grants_nothing() {
        // `http://evil.com@allowed.com/` - the real host is
        // "allowed.com" and is correctly admitted; "evil.com" sitting in
        // userinfo must never itself satisfy an allowlist entry.
        assert!(
            admit(
                &uri("http://evil.com@allowed.com/"),
                &allow(&["allowed.com"])
            )
            .is_ok()
        );
        assert!(denied("http://evil.com@allowed.com/", &["evil.com"]));
    }

    #[test]
    fn backslash_in_the_authority_fails_to_parse_rather_than_bypassing() {
        // Backslash-as-slash confusion is a known SSRF trick against
        // parsers that normalise `\` to `/`. `http::Uri` does neither:
        // a backslash anywhere in the authority is rejected at parse
        // time. Checked against both entry points a backslash-bearing
        // authority could reach this gate through: the full-URI parser
        // (what this module's `uri()` test helper uses) and
        // `http::uri::Authority`, the type `wasmtime-wasi-http` builds
        // directly from the guest's `authority` string
        // (`Uri::builder().authority(...)`) - the seam a wasm guest
        // actually exercises. Both reject identically, so a request
        // built from one of these strings never reaches `admit`.
        for bad in [
            "evil.com\\allowed.com",
            "evil.com\\@allowed.com",
            "allowed.com\\.evil.com",
        ] {
            assert!(
                http::uri::Authority::try_from(bad).is_err(),
                "expected Authority::try_from to reject {bad:?}"
            );
            assert!(
                format!("http://{bad}/").parse::<http::Uri>().is_err(),
                "expected a full-URI parse error for {bad:?}"
            );
        }
    }

    #[test]
    fn numeric_ip_encodings_never_normalise_to_the_dotted_form_an_allowlist_names() {
        // `host_allowed` is an exact/wildcard string match with no IP
        // normalisation (see `admit`'s doc comment). Decimal, octal, and
        // hex encodings of 127.0.0.1 are valid `http::Uri` hosts but are
        // different strings from "127.0.0.1", so none of them satisfy an
        // allowlist entry naming the dotted-quad form - locking in that
        // a future refactor doesn't "helpfully" start normalising these
        // and turn a same-string match into an equivalent-address match.
        for evil in [
            "2130706433",
            "0177.0.0.1",
            "0x7f.0.0.1",
            "[::ffff:127.0.0.1]",
        ] {
            assert!(
                denied(&format!("http://{evil}/"), &["127.0.0.1"]),
                "{evil:?} must not satisfy a 127.0.0.1 allowlist entry"
            );
        }
    }

    #[test]
    fn fragment_and_query_after_the_host_do_not_influence_the_host_check() {
        // Historical bug (see issue #57): a naive host-extractor could
        // be fooled by a `/`-bearing query string or fragment appended
        // after the real host. `http::Uri::host` is unaffected by
        // either - the decoy text never becomes part of the host.
        assert!(
            admit(
                &uri("http://allowed.com#@evil.com/"),
                &allow(&["allowed.com"])
            )
            .is_ok()
        );
        assert!(
            admit(
                &uri("http://allowed.com?@evil.com/"),
                &allow(&["allowed.com"])
            )
            .is_ok()
        );
        assert_eq!(
            uri("http://allowed.com#@evil.com/").host(),
            Some("allowed.com")
        );
        assert_eq!(
            uri("http://allowed.com?@evil.com/").host(),
            Some("allowed.com")
        );
    }

    #[test]
    fn both_schemes_are_gated_identically() {
        for scheme in ["http", "https"] {
            assert!(
                admit(
                    &uri(&format!("{scheme}://api.acme.example/")),
                    &allow(&["api.acme.example"])
                )
                .is_ok()
            );
            assert!(denied(
                &format!("{scheme}://evil.example/"),
                &["api.acme.example"]
            ));
        }
    }

    #[test]
    fn uri_without_authority_is_invalid_not_denied() {
        assert!(matches!(
            admit(&uri("/relative/path"), &allow(&["api.acme.example"])),
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
        let mut gate = HttpGate::new("test-module", allow(&["api.acme.example"]), limits());
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
        let mut gate = HttpGate::new("test-module", allow(&["127.0.0.1"]), limits());
        assert!(
            gate.send_request(request("http://127.0.0.1:1/x"), config())
                .is_ok()
        );
    }

    // ----------------- timeout clamping ----------------------------

    fn config_with(timeout: Duration) -> OutgoingRequestConfig {
        OutgoingRequestConfig {
            use_tls: false,
            connect_timeout: timeout,
            first_byte_timeout: timeout,
            between_bytes_timeout: timeout,
        }
    }

    #[test]
    fn clamp_lowers_each_timeout_above_its_maximum() {
        // 600 s is also what the linked handler substitutes for unset
        // request-options, so this doubles as the unset case: unset
        // resolves to the engine maximum.
        let clamped = clamp(config_with(Duration::from_secs(600)), &limits());
        assert_eq!(clamped.connect_timeout, Duration::from_secs(10));
        assert_eq!(clamped.first_byte_timeout, Duration::from_secs(10));
        assert_eq!(clamped.between_bytes_timeout, Duration::from_secs(10));
    }

    #[test]
    fn clamp_keeps_timeouts_below_the_maximum() {
        let clamped = clamp(config_with(Duration::from_secs(1)), &limits());
        assert_eq!(clamped.connect_timeout, Duration::from_secs(1));
        assert_eq!(clamped.first_byte_timeout, Duration::from_secs(1));
        assert_eq!(clamped.between_bytes_timeout, Duration::from_secs(1));
    }

    #[test]
    fn clamp_keeps_timeouts_at_the_maximum() {
        let clamped = clamp(config_with(Duration::from_secs(10)), &limits());
        assert_eq!(clamped.connect_timeout, Duration::from_secs(10));
        assert_eq!(clamped.first_byte_timeout, Duration::from_secs(10));
        assert_eq!(clamped.between_bytes_timeout, Duration::from_secs(10));
    }

    #[test]
    fn clamp_applies_each_maximum_independently() {
        let mut l = limits();
        l.first_byte_timeout_max = Duration::from_millis(50);
        let clamped = clamp(config_with(Duration::from_secs(5)), &l);
        assert_eq!(clamped.connect_timeout, Duration::from_secs(5));
        assert_eq!(clamped.first_byte_timeout, Duration::from_millis(50));
        assert_eq!(clamped.between_bytes_timeout, Duration::from_secs(5));
    }

    // ----------------- deadline + body cap -------------------------

    /// A detached executor for test-server tasks.
    fn test_executor() -> nexum_tasks::TaskExecutor {
        nexum_tasks::TaskManager::new().executor()
    }

    /// One-connection loopback server: reads the request, writes
    /// `response`, then either closes or holds the socket open so the
    /// client sees a stall instead of EOF. Panic-free: any IO failure
    /// just ends the task and the client side times out.
    async fn spawn_server(response: Vec<u8>, hold_open: bool) -> std::net::SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback listener");
        let addr = listener.local_addr().expect("listener has a local addr");
        test_executor().spawn(async move {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let mut buf = [0u8; 1024];
            let _ = sock.read(&mut buf).await;
            let _ = sock.write_all(&response).await;
            let _ = sock.flush().await;
            if hold_open {
                tokio::time::sleep(Duration::from_secs(30)).await;
            }
        });
        addr
    }

    async fn resolve(pending: HostFutureIncomingResponse) -> Result<IncomingResponse, ErrorCode> {
        match pending {
            HostFutureIncomingResponse::Pending(handle) => {
                handle.await.expect("send task never traps")
            }
            _ => panic!("send_request returns a pending response"),
        }
    }

    async fn send_to(
        addr: std::net::SocketAddr,
        limits: OutboundHttpLimits,
    ) -> Result<IncomingResponse, ErrorCode> {
        let mut gate = HttpGate::new("test-module", allow(&["127.0.0.1"]), limits);
        let pending = gate
            .send_request(request(&format!("http://{addr}/x")), config_10s())
            .expect("listed host admitted");
        resolve(pending).await
    }

    fn config_10s() -> OutgoingRequestConfig {
        config_with(Duration::from_secs(10))
    }

    #[tokio::test]
    async fn request_under_all_limits_succeeds() {
        let addr = spawn_server(
            b"HTTP/1.1 200 OK\r\ncontent-length: 5\r\n\r\nhello".to_vec(),
            false,
        )
        .await;
        let incoming = send_to(addr, limits()).await.expect("response arrives");
        assert_eq!(incoming.resp.status(), 200);
        let body = incoming
            .resp
            .into_body()
            .collect()
            .await
            .expect("body is under the cap");
        assert_eq!(body.to_bytes().as_ref(), b"hello");
    }

    #[tokio::test]
    async fn total_deadline_fires_on_a_stalled_server() {
        // Accepts, never responds; every per-phase maximum is 10 s, so
        // only the total deadline can end the wait.
        let addr = spawn_server(Vec::new(), true).await;
        let mut l = limits();
        l.total_deadline = Duration::from_millis(250);
        let err = send_to(addr, l).await.expect_err("deadline fires");
        assert!(matches!(err, ErrorCode::ConnectionTimeout));
    }

    #[tokio::test]
    async fn total_deadline_fires_while_the_body_stalls() {
        // Headers plus 16 of 100000 promised body bytes, then a stall:
        // the deadline covers body streaming via the CappedBody wrapper.
        let mut response = b"HTTP/1.1 200 OK\r\ncontent-length: 100000\r\n\r\n".to_vec();
        response.extend_from_slice(&[b'x'; 16]);
        let addr = spawn_server(response, true).await;
        let mut l = limits();
        l.total_deadline = Duration::from_millis(300);
        let incoming = send_to(addr, l).await.expect("headers arrive in time");
        let err = incoming
            .resp
            .into_body()
            .collect()
            .await
            .expect_err("deadline fires mid-body");
        assert!(matches!(err, ErrorCode::ConnectionReadTimeout));
    }

    #[tokio::test]
    async fn deadline_tears_down_a_parked_unread_response() {
        // The guest obtains the response and never polls the body, so
        // the body-side deadline never runs; the raced connection
        // driver alone must close the socket, observable server-side
        // as EOF on a blocking read.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback listener");
        let addr = listener.local_addr().expect("listener has a local addr");
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        test_executor().spawn(async move {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let mut buf = [0u8; 1024];
            let _ = sock.read(&mut buf).await;
            let _ = sock
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 100000\r\n\r\n")
                .await;
            let _ = sock.flush().await;
            loop {
                match sock.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
            }
            let _ = tx.send(());
        });
        let mut l = limits();
        l.total_deadline = Duration::from_millis(300);
        let parked = send_to(addr, l).await.expect("headers arrive in time");
        let closed = tokio::time::timeout(Duration::from_secs(5), rx).await;
        assert!(
            closed.is_ok(),
            "server must see the close at the deadline while the response is parked"
        );
        drop(parked);
    }

    #[tokio::test]
    async fn oversized_response_body_fails_with_the_cap_in_the_error() {
        let mut response = b"HTTP/1.1 200 OK\r\ncontent-length: 4096\r\n\r\n".to_vec();
        response.extend_from_slice(&[b'x'; 4096]);
        let addr = spawn_server(response, false).await;
        let mut l = limits();
        l.response_body_max_bytes = 1024;
        let incoming = send_to(addr, l).await.expect("headers arrive");
        let err = incoming
            .resp
            .into_body()
            .collect()
            .await
            .expect_err("body exceeds the cap");
        assert!(matches!(err, ErrorCode::HttpResponseBodySize(Some(1024))));
    }

    #[tokio::test]
    async fn body_at_exactly_the_cap_passes() {
        let inner: HyperIncomingBody = Full::new(Bytes::from(vec![b'a'; 64]))
            .map_err(|_| unreachable!("infallible body error"))
            .boxed_unsync();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let body = CappedBody::new(inner, 64, deadline);
        let collected = body.collect().await.expect("exact-cap body passes");
        assert_eq!(collected.to_bytes().len(), 64);
    }
}
