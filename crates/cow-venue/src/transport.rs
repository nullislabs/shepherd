//! The venue's outbound HTTP seam and its native client.
//!
//! A native venue owns its transport: there is no scoped `wasi:http`
//! import to reach the orderbook through. [`Transport`] keeps the
//! orderbook speaker testable, and [`OrderbookHttp`] is the shipped
//! reqwest-backed implementation.

use core::time::Duration;

/// Why one request failed. Reused from the guest SDK so the venue keeps
/// a single transport-failure vocabulary and its `From` conversion into
/// [`VenueError`](videre_sdk::VenueError).
pub use nexum_sdk::http::FetchError;

/// One bounded orderbook request, response body fully buffered.
///
/// The future is `Send` because the registry drives the venue on a
/// multi-threaded runtime and boxes every call.
pub trait Transport {
    /// Perform `request`, resolving to the buffered response.
    fn call(
        &self,
        request: http::Request<Vec<u8>>,
    ) -> impl Future<Output = Result<http::Response<Vec<u8>>, FetchError>> + Send;
}

/// The shipped transport: one reqwest client whose connect and overall
/// timeouts are the venue's configured per-request bound.
#[derive(Clone, Debug)]
pub struct OrderbookHttp {
    client: reqwest::Client,
}

impl OrderbookHttp {
    /// Build a client bounding every request to `timeout`.
    ///
    /// Redirects are refused. The wasi outgoing handler never followed
    /// one, and a 307 from the orderbook would otherwise re-send a signed
    /// order to a host the operator never named.
    ///
    /// # Errors
    ///
    /// Returns [`FetchError::Transport`] when the TLS backend fails to
    /// initialise.
    pub fn new(timeout: Duration) -> Result<Self, FetchError> {
        let client = reqwest::Client::builder()
            .connect_timeout(timeout)
            .timeout(timeout)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| FetchError::Transport(format!("http client: {e}")))?;
        Ok(Self { client })
    }
}

impl Transport for OrderbookHttp {
    async fn call(
        &self,
        request: http::Request<Vec<u8>>,
    ) -> Result<http::Response<Vec<u8>>, FetchError> {
        let request = reqwest::Request::try_from(request)
            .map_err(|e| FetchError::InvalidRequest(e.to_string()))?;
        let response = self.client.execute(request).await.map_err(sent_failure)?;
        let status = response.status();
        let headers = response.headers().clone();
        let body = response.bytes().await.map_err(sent_failure)?;

        let mut out = http::Response::builder().status(status);
        if let Some(slot) = out.headers_mut() {
            *slot = headers;
        }
        out.body(body.to_vec())
            .map_err(|e| FetchError::Transport(format!("response build: {e}")))
    }
}

/// Project a reqwest failure onto the transport vocabulary.
fn sent_failure(err: reqwest::Error) -> FetchError {
    if err.is_timeout() {
        return FetchError::Timeout(err.to_string());
    }
    if err.is_builder() {
        return FetchError::InvalidRequest(err.to_string());
    }
    FetchError::Transport(err.to_string())
}

/// The conformance kit's in-memory mock is a transport that resolves at
/// once, so the orderbook speaker is exercised without a socket. The mock
/// is answered before the future is built, so nothing non-`Send` is held
/// across a suspension point.
#[cfg(test)]
impl Transport for videre_test::MockFetch {
    fn call(
        &self,
        request: http::Request<Vec<u8>>,
    ) -> impl Future<Output = Result<http::Response<Vec<u8>>, FetchError>> + Send {
        core::future::ready(nexum_sdk::http::Fetch::fetch(self, request))
    }
}

#[cfg(test)]
mod tests {
    use wiremock::matchers::{body_string, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::{Duration, FetchError, OrderbookHttp, Transport};

    /// Method, path, headers and body survive the reqwest conversion, and
    /// the reply's status, headers and body come back whole.
    #[tokio::test]
    async fn a_request_round_trips_through_the_reqwest_client() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/orders"))
            .and(header("content-type", "application/json"))
            .and(body_string("{\"a\":1}"))
            .respond_with(
                ResponseTemplate::new(429)
                    .insert_header("retry-after", "7")
                    .set_body_string("slow down"),
            )
            .mount(&server)
            .await;

        let transport = OrderbookHttp::new(Duration::from_secs(5)).expect("the client builds");
        let request = http::Request::post(format!("{}/api/v1/orders", server.uri()))
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(b"{\"a\":1}".to_vec())
            .expect("test request builds");

        let response = transport.call(request).await.expect("the server answers");
        assert_eq!(response.status(), 429);
        assert_eq!(
            response.headers().get(http::header::RETRY_AFTER),
            Some(&http::HeaderValue::from_static("7")),
            "the response headers must survive, the retry hint reads them",
        );
        assert_eq!(response.body(), b"slow down");
    }

    /// A redirect is returned as itself, never followed: a signed order
    /// must not be re-sent to a host the operator never named.
    #[tokio::test]
    async fn a_redirect_is_never_followed() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(307).insert_header("location", "https://evil.test/orders"),
            )
            .mount(&server)
            .await;

        let transport = OrderbookHttp::new(Duration::from_secs(5)).expect("the client builds");
        let request = http::Request::get(format!("{}/api/v1/orders", server.uri()))
            .body(Vec::new())
            .expect("test request builds");

        let response = transport.call(request).await.expect("the server answers");
        assert_eq!(response.status(), 307);
    }

    /// A refused connection is retryable, not a terminal refusal.
    #[tokio::test]
    async fn an_unreachable_orderbook_fails_typedly() {
        let transport = OrderbookHttp::new(Duration::from_millis(500)).expect("the client builds");
        let request = http::Request::get("http://127.0.0.1:1/api/v1/orders")
            .body(Vec::new())
            .expect("test request builds");
        let err = transport.call(request).await.expect_err("port 1 refuses");
        assert!(matches!(
            err,
            FetchError::Transport(_) | FetchError::Timeout(_),
        ));
    }
}
