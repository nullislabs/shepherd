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
pub use nexum_sdk_test::{ChainCall, MockChain};
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

// ------------------------------------------------------------ messaging

/// One recorded [`MessagingHost::publish`] invocation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublishRecord {
    /// Content topic the adapter published to.
    pub content_topic: String,
    /// Payload bytes, verbatim.
    pub payload: Vec<u8>,
}

/// In-memory [`MessagingHost`]. Seeded messages answer queries,
/// publishes are recorded for assertion, and an optional topic scope
/// mirrors the host's `messaging_topics` grant. Seeded history and
/// published records are deliberately separate stores: a query answers
/// from what the test seeded, never from what the adapter published.
#[derive(Default)]
pub struct MockMessaging {
    history: RefCell<Vec<Message>>,
    published: RefCell<Vec<PublishRecord>>,
    scope: RefCell<Option<Vec<String>>>,
    faults: RefCell<Vec<(String, Fault)>>,
}

impl MockMessaging {
    /// Seed one message into the queryable history.
    pub fn seed(&self, message: Message) {
        self.history.borrow_mut().push(message);
    }

    /// Seed a payload on `content_topic` at `timestamp` (ms since the
    /// Unix epoch, UTC), with no sender.
    pub fn seed_payload(
        &self,
        content_topic: impl Into<String>,
        payload: impl Into<Vec<u8>>,
        timestamp: u64,
    ) {
        self.seed(Message {
            content_topic: content_topic.into(),
            payload: payload.into(),
            timestamp,
            sender: None,
        });
    }

    /// Confine the mock to `topics`, mirroring the adapter's
    /// `messaging_topics` grant: any other topic fails as
    /// [`Fault::Denied`]. Untouched, every topic is allowed.
    pub fn scope_topics(&self, topics: impl IntoIterator<Item = impl Into<String>>) {
        *self.scope.borrow_mut() = Some(topics.into_iter().map(Into::into).collect());
    }

    /// Inject a fault for any operation on a topic starting with
    /// `prefix`. Multiple patterns can be registered; the first
    /// matching one fires.
    pub fn fail_on(&self, prefix: impl Into<String>, fault: Fault) {
        self.faults.borrow_mut().push((prefix.into(), fault));
    }

    /// All publishes received, in arrival order.
    pub fn published(&self) -> Vec<PublishRecord> {
        self.published.borrow().clone()
    }

    /// Last publish received, if any.
    pub fn last_published(&self) -> Option<PublishRecord> {
        self.published.borrow().last().cloned()
    }

    /// Total publish count.
    pub fn publish_count(&self) -> usize {
        self.published.borrow().len()
    }

    fn admit(&self, content_topic: &str) -> Result<(), Fault> {
        for (prefix, fault) in self.faults.borrow().iter() {
            if content_topic.starts_with(prefix.as_str()) {
                return Err(fault.clone());
            }
        }
        if let Some(scope) = self.scope.borrow().as_ref()
            && !scope.iter().any(|topic| topic == content_topic)
        {
            return Err(Fault::Denied(format!(
                "MockMessaging: {content_topic} is outside the scoped topics"
            )));
        }
        Ok(())
    }
}

impl MessagingHost for MockMessaging {
    fn publish(&self, content_topic: &str, payload: &[u8]) -> Result<(), Fault> {
        self.admit(content_topic)?;
        self.published.borrow_mut().push(PublishRecord {
            content_topic: content_topic.to_owned(),
            payload: payload.to_vec(),
        });
        Ok(())
    }

    /// Answer from the seeded history: exact-topic matches whose
    /// timestamp lies within the inclusive `start_time..=end_time`
    /// window, in seed order. Seed order is delivery order, so a
    /// `limit` keeps the newest matches: the tail.
    fn query(
        &self,
        content_topic: &str,
        start_time: Option<u64>,
        end_time: Option<u64>,
        limit: Option<u32>,
    ) -> Result<Vec<Message>, Fault> {
        self.admit(content_topic)?;
        let mut matches: Vec<Message> = self
            .history
            .borrow()
            .iter()
            .filter(|message| {
                message.content_topic == content_topic
                    && start_time.is_none_or(|start| message.timestamp >= start)
                    && end_time.is_none_or(|end| message.timestamp <= end)
            })
            .cloned()
            .collect();
        if let Some(limit) = limit {
            let keep = usize::try_from(limit).unwrap_or(usize::MAX);
            if matches.len() > keep {
                matches.drain(..matches.len() - keep);
            }
        }
        Ok(matches)
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
    fn messaging_records_publishes_and_answers_from_seeds() {
        let messaging = MockMessaging::default();
        messaging.seed_payload("/acme/1/orders/proto", b"one".to_vec(), 10);
        messaging.seed_payload("/acme/1/orders/proto", b"two".to_vec(), 20);
        messaging.seed_payload("/acme/1/other/proto", b"noise".to_vec(), 15);

        messaging.publish("/acme/1/orders/proto", b"out").unwrap();
        assert_eq!(messaging.publish_count(), 1);
        assert_eq!(
            messaging.last_published().unwrap(),
            PublishRecord {
                content_topic: "/acme/1/orders/proto".to_owned(),
                payload: b"out".to_vec(),
            },
        );

        // Publishes never leak into query results.
        let all = messaging
            .query("/acme/1/orders/proto", None, None, None)
            .unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].payload, b"one");
        assert_eq!(all[1].payload, b"two");
    }

    #[test]
    fn messaging_query_applies_bounds_and_limit() {
        let messaging = MockMessaging::default();
        for (payload, ts) in [(b"a", 10u64), (b"b", 20), (b"c", 30), (b"d", 40)] {
            messaging.seed_payload("/t", payload.to_vec(), ts);
        }

        let window = messaging.query("/t", Some(20), Some(30), None).unwrap();
        assert_eq!(window.len(), 2);
        assert_eq!(window[0].payload, b"b");

        // A limit keeps the newest matches: the tail of the window.
        let limited = messaging.query("/t", None, None, Some(2)).unwrap();
        assert_eq!(limited.len(), 2);
        assert_eq!(limited[0].payload, b"c");
        assert_eq!(limited[1].payload, b"d");
    }

    #[test]
    fn messaging_scope_denies_off_grant_topics() {
        let messaging = MockMessaging::default();
        messaging.scope_topics(["/acme/1/orders/proto"]);

        messaging.publish("/acme/1/orders/proto", b"ok").unwrap();
        let err = messaging.publish("/other", b"no").unwrap_err();
        assert!(matches!(err, Fault::Denied(_)));
        let err = messaging.query("/other", None, None, None).unwrap_err();
        assert!(matches!(err, Fault::Denied(_)));
        // The refused publish was never recorded.
        assert_eq!(messaging.publish_count(), 1);
    }

    #[test]
    fn messaging_fault_injection_fires_by_prefix() {
        let messaging = MockMessaging::default();
        messaging.fail_on("/flaky", Fault::Timeout);
        assert!(matches!(
            messaging.publish("/flaky/topic", b"x").unwrap_err(),
            Fault::Timeout,
        ));
        messaging.publish("/steady", b"x").unwrap();
    }

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
