//! In-memory mocks for the three transports a venue adapter is
//! granted: chain RPC, messaging, and outbound HTTP.
//!
//! [`MockTransport`] composes the three behind the same seams the SDK
//! wrappers implement ([`ChainHost`], [`MessagingHost`], [`Fetch`]), so
//! adapter logic written against `&impl Seam` runs unchanged in unit
//! tests. Scoping mirrors the host's: [`MockMessaging::scope_topics`]
//! plays the adapter's `messaging_topics` grant and refuses off-scope
//! topics as a typed `denied`, exactly as the host would.

use std::cell::RefCell;
use std::collections::HashMap;

use nexum_sdk::host::{ChainError, ChainHost, Fault};
use nexum_sdk::http::{Fetch, FetchError, FetchOptions};
pub use nexum_sdk_test::{ChainCall, MockChain, MockMessaging, PublishRecord};
pub use videre_sdk::transport::{Message, MessagingHost};

/// Composed in-memory transport. Each field exposes the per-seam mock
/// so tests can program responses and assert on calls.
#[derive(Default)]
pub struct MockTransport {
    /// `nexum:host/chain` mock.
    pub chain: MockChain,
    /// `nexum:host/messaging` mock.
    pub messaging: MockMessaging,
    /// Outbound wasi:http mock.
    pub http: MockFetch,
}

impl MockTransport {
    /// Fresh empty transport. Equivalent to `Default::default`.
    pub fn new() -> Self {
        Self::default()
    }
}

impl ChainHost for MockTransport {
    fn request(&self, chain_id: u64, method: &str, params: &str) -> Result<String, ChainError> {
        self.chain.request(chain_id, method, params)
    }
}

impl MessagingHost for MockTransport {
    fn publish(&self, content_topic: &str, payload: &[u8]) -> Result<(), Fault> {
        self.messaging.publish(content_topic, payload)
    }

    fn query(
        &self,
        content_topic: &str,
        start_time: Option<u64>,
        end_time: Option<u64>,
        limit: Option<u32>,
    ) -> Result<Vec<Message>, Fault> {
        self.messaging
            .query(content_topic, start_time, end_time, limit)
    }
}

impl Fetch for MockTransport {
    fn fetch_with(
        &self,
        request: http::Request<Vec<u8>>,
        options: FetchOptions,
    ) -> Result<http::Response<Vec<u8>>, FetchError> {
        self.http.fetch_with(request, options)
    }
}

// ------------------------------------------------------------ http

/// One recorded [`Fetch::fetch_with`] invocation.
#[derive(Clone, Debug)]
pub struct RecordedRequest {
    /// HTTP method.
    pub method: http::Method,
    /// Full request URI, verbatim.
    pub uri: String,
    /// Request body bytes.
    pub body: Vec<u8>,
    /// The per-phase timeouts the caller applied.
    pub options: FetchOptions,
}

/// A programmed response, rebuilt into an `http::Response` per call
/// because the standard response type is not `Clone`.
#[derive(Clone, Debug)]
struct StoredResponse {
    status: http::StatusCode,
    body: Vec<u8>,
}

/// In-memory [`Fetch`] backed by a `(method, uri)` -> response map.
/// Records every request so tests can assert dispatch shape; an
/// allowlist refusal is programmed as [`FetchError::Denied`] via
/// [`fail_with`](Self::fail_with).
#[derive(Default)]
pub struct MockFetch {
    responses: RefCell<HashMap<(http::Method, String), Result<StoredResponse, FetchError>>>,
    requests: RefCell<Vec<RecordedRequest>>,
}

impl MockFetch {
    /// Program a response for the `(method, uri)` pair. Overwrites any
    /// prior entry.
    ///
    /// # Panics
    ///
    /// On a `status` outside the valid HTTP range.
    pub fn respond_to(
        &self,
        method: http::Method,
        uri: impl Into<String>,
        status: u16,
        body: impl Into<Vec<u8>>,
    ) {
        let status =
            http::StatusCode::from_u16(status).expect("MockFetch: status must be a valid code");
        self.responses.borrow_mut().insert(
            (method, uri.into()),
            Ok(StoredResponse {
                status,
                body: body.into(),
            }),
        );
    }

    /// Program a failure for the `(method, uri)` pair. Overwrites any
    /// prior entry.
    pub fn fail_with(&self, method: http::Method, uri: impl Into<String>, error: FetchError) {
        self.responses
            .borrow_mut()
            .insert((method, uri.into()), Err(error));
    }

    /// All requests received, in arrival order.
    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.requests.borrow().clone()
    }

    /// Last request received, if any.
    pub fn last_request(&self) -> Option<RecordedRequest> {
        self.requests.borrow().last().cloned()
    }

    /// Total request count.
    pub fn request_count(&self) -> usize {
        self.requests.borrow().len()
    }
}

impl Fetch for MockFetch {
    fn fetch_with(
        &self,
        request: http::Request<Vec<u8>>,
        options: FetchOptions,
    ) -> Result<http::Response<Vec<u8>>, FetchError> {
        let method = request.method().clone();
        let uri = request.uri().to_string();
        self.requests.borrow_mut().push(RecordedRequest {
            method: method.clone(),
            uri: uri.clone(),
            body: request.body().clone(),
            options,
        });
        match self.responses.borrow().get(&(method.clone(), uri.clone())) {
            Some(Ok(stored)) => Ok(http::Response::builder()
                .status(stored.status)
                .body(stored.body.clone())
                .expect("a stored response always rebuilds")),
            Some(Err(err)) => Err(err.clone()),
            None => Err(FetchError::Transport(format!(
                "MockFetch: no response configured for {method} {uri}"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fetch_returns_programmed_response_and_records_the_request() {
        let fetch = MockFetch::default();
        fetch.respond_to(
            http::Method::GET,
            "https://venue.example/api/v1/quote",
            200,
            br#"{"price":"1"}"#.to_vec(),
        );

        let request = http::Request::builder()
            .method(http::Method::GET)
            .uri("https://venue.example/api/v1/quote")
            .body(Vec::new())
            .unwrap();
        let response = fetch.fetch(request).unwrap();
        assert_eq!(response.status(), http::StatusCode::OK);
        assert_eq!(response.body(), br#"{"price":"1"}"#);

        assert_eq!(fetch.request_count(), 1);
        let recorded = fetch.last_request().unwrap();
        assert_eq!(recorded.method, http::Method::GET);
        assert_eq!(recorded.uri, "https://venue.example/api/v1/quote");
        assert_eq!(recorded.options, FetchOptions::default());
    }

    #[test]
    fn fetch_unconfigured_and_programmed_failures() {
        let fetch = MockFetch::default();
        fetch.fail_with(
            http::Method::POST,
            "https://venue.example/api/v1/orders",
            FetchError::Denied,
        );

        let denied = http::Request::builder()
            .method(http::Method::POST)
            .uri("https://venue.example/api/v1/orders")
            .body(b"order".to_vec())
            .unwrap();
        assert_eq!(fetch.fetch(denied).unwrap_err(), FetchError::Denied);

        let stray = http::Request::builder()
            .uri("https://nowhere.example/")
            .body(Vec::new())
            .unwrap();
        let err = fetch.fetch(stray).unwrap_err();
        assert!(matches!(err, FetchError::Transport(msg) if msg.contains("MockFetch")));
        // Refused and unconfigured requests are still recorded.
        assert_eq!(fetch.request_count(), 2);
    }

    #[test]
    fn transport_dispatches_through_every_seam() {
        let transport = MockTransport::new();
        transport
            .chain
            .respond_to("eth_blockNumber", "[]", Ok("\"0x1\"".to_owned()));
        transport.messaging.seed_payload("/t", b"m".to_vec(), 1);
        transport
            .http
            .respond_to(http::Method::GET, "https://venue.example/", 204, Vec::new());

        // Through the seams an adapter's logic is written against.
        let chain: &dyn ChainHost = &transport;
        assert_eq!(
            chain.request(1, "eth_blockNumber", "[]").unwrap(),
            "\"0x1\""
        );

        let messaging: &dyn MessagingHost = &transport;
        messaging.publish("/t", b"out").unwrap();
        assert_eq!(messaging.query("/t", None, None, None).unwrap().len(), 1);

        let request = http::Request::builder()
            .uri("https://venue.example/")
            .body(Vec::new())
            .unwrap();
        let response = transport.fetch(request).unwrap();
        assert_eq!(response.status(), http::StatusCode::NO_CONTENT);

        assert_eq!(transport.chain.call_count(), 1);
        assert_eq!(transport.messaging.publish_count(), 1);
        assert_eq!(transport.http.request_count(), 1);
    }
}
