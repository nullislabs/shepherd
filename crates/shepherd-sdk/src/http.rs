//! Outbound HTTP over wasi:http for guest modules.
//!
//! `fetch` performs one synchronous request through the host's
//! wasi:http outgoing handler. The host admits or denies every request
//! against the module's `[capabilities.http].allow` list before any
//! connection is made; a denial surfaces as [`FetchError::Denied`], so
//! modules can tell policy refusals from transport failures.
//!
//! The request/response/error types compile on every target so
//! strategy logic can be unit-tested host-side against the [`Fetch`]
//! seam; the `fetch` implementation itself only exists on
//! `wasm32-wasip2`.

use core::time::Duration;

use strum::IntoStaticStr;

/// Timeout applied by [`Request::new`] to connect, first byte, and
/// between-bytes unless the caller overrides [`Request::timeout`].
/// Keeps an event handler from hanging on a stalled upstream.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// HTTP request method.
///
/// `IntoStaticStr` yields the canonical uppercase token (`"GET"`) for
/// log and metric labels.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, IntoStaticStr)]
#[strum(serialize_all = "UPPERCASE")]
pub enum Method {
    /// GET
    Get,
    /// HEAD
    Head,
    /// POST
    Post,
    /// PUT
    Put,
    /// DELETE
    Delete,
    /// PATCH
    Patch,
}

/// One outbound HTTP request. Build with [`Request::get`] /
/// [`Request::post`] / [`Request::new`], then pass to `fetch` or a
/// [`Fetch`] implementation.
#[derive(Clone, Debug)]
pub struct Request {
    /// Request method.
    pub method: Method,
    /// Absolute URL. The URL's host must be on the module's
    /// `[capabilities.http].allow` list or the host denies the request.
    pub url: String,
    /// Header name/value pairs sent with the request.
    pub headers: Vec<(String, String)>,
    /// Request body; empty for body-less methods.
    pub body: Vec<u8>,
    /// Per-phase timeout (connect / first byte / between bytes).
    /// `None` defers to the host's own limits.
    pub timeout: Option<Duration>,
}

impl Request {
    /// Request with [`DEFAULT_TIMEOUT`], no headers, and an empty body.
    pub fn new(method: Method, url: impl Into<String>) -> Self {
        Self {
            method,
            url: url.into(),
            headers: Vec::new(),
            body: Vec::new(),
            timeout: Some(DEFAULT_TIMEOUT),
        }
    }

    /// GET `url`.
    pub fn get(url: impl Into<String>) -> Self {
        Self::new(Method::Get, url)
    }

    /// POST `body` to `url`.
    pub fn post(url: impl Into<String>, body: impl Into<Vec<u8>>) -> Self {
        Self {
            body: body.into(),
            ..Self::new(Method::Post, url)
        }
    }

    /// Append one header.
    #[must_use]
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// Override the per-phase timeout.
    #[must_use]
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }
}

/// A fully-buffered HTTP response.
#[derive(Clone, Debug)]
pub struct Response {
    /// HTTP status code.
    pub status: u16,
    /// Response header name/value pairs; non-UTF-8 header values are
    /// replaced lossily.
    pub headers: Vec<(String, String)>,
    /// Complete response body.
    pub body: Vec<u8>,
}

impl Response {
    /// Whether the status is in the 2xx range.
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// Why a fetch failed, folded down from the wasi:http error codes.
///
/// `IntoStaticStr` yields a snake_case label per variant for log and
/// metric fields.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum FetchError {
    /// The host's `[capabilities.http].allow` list refused the request
    /// before any connection was made.
    #[error("denied by the module's http allowlist")]
    Denied,
    /// The request never left the guest: malformed URL, method, or
    /// header.
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    /// A configured timeout elapsed.
    #[error("timeout: {0}")]
    Timeout(String),
    /// Connection or protocol failure after the allowlist admitted the
    /// request.
    #[error("transport failure: {0}")]
    Transport(String),
}

/// Seam between strategy logic and the wasi:http transport, in the
/// mould of the `host` module's traits: strategies take `&impl Fetch`
/// and tests slot in a stub; module glue passes [`WasiFetch`].
pub trait Fetch {
    /// Perform one request, blocking until the response body is fully
    /// buffered.
    fn fetch(&self, request: Request) -> Result<Response, FetchError>;
}

/// [`Fetch`] adapter over the host's wasi:http outgoing handler.
///
/// Guest-only glue: the type exists on every target so module
/// `lib.rs` glue compiles host-side for unit tests, but calling
/// [`Fetch::fetch`] off the wasm guest is unimplemented.
#[derive(Clone, Copy, Debug, Default)]
pub struct WasiFetch;

impl Fetch for WasiFetch {
    #[cfg(all(target_arch = "wasm32", target_os = "wasi"))]
    fn fetch(&self, request: Request) -> Result<Response, FetchError> {
        fetch(request)
    }

    #[cfg(not(all(target_arch = "wasm32", target_os = "wasi")))]
    fn fetch(&self, _request: Request) -> Result<Response, FetchError> {
        unimplemented!("wasi:http fetch is only available in a wasm32-wasip2 guest")
    }
}

#[cfg(all(target_arch = "wasm32", target_os = "wasi"))]
mod wasi_impl {
    use wstd::http::ErrorCode;

    use super::{FetchError, Method, Request, Response};

    /// Perform `request` through the host's wasi:http outgoing
    /// handler, blocking the (single-threaded) guest until the
    /// response body is fully buffered.
    pub fn fetch(request: Request) -> Result<Response, FetchError> {
        wstd::runtime::block_on(fetch_async(request))
    }

    async fn fetch_async(request: Request) -> Result<Response, FetchError> {
        let mut builder = wstd::http::Request::builder()
            .method(to_http_method(request.method))
            .uri(request.url.as_str());
        for (name, value) in &request.headers {
            builder = builder.header(name, value);
        }
        let outgoing = builder
            .body(request.body)
            .map_err(|e| FetchError::InvalidRequest(e.to_string()))?;

        let mut client = wstd::http::Client::new();
        if let Some(timeout) = request.timeout {
            client.set_connect_timeout(timeout);
            client.set_first_byte_timeout(timeout);
            client.set_between_bytes_timeout(timeout);
        }

        let response = client.send(outgoing).await.map_err(map_error)?;
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_owned(),
                    String::from_utf8_lossy(value.as_bytes()).into_owned(),
                )
            })
            .collect();
        let mut body = response.into_body();
        let bytes = body.bytes_contents().await.map_err(map_error)?;
        Ok(Response {
            status,
            headers,
            body: bytes.to_vec(),
        })
    }

    fn to_http_method(method: Method) -> wstd::http::Method {
        match method {
            Method::Get => wstd::http::Method::GET,
            Method::Head => wstd::http::Method::HEAD,
            Method::Post => wstd::http::Method::POST,
            Method::Put => wstd::http::Method::PUT,
            Method::Delete => wstd::http::Method::DELETE,
            Method::Patch => wstd::http::Method::PATCH,
        }
    }

    /// Fold the wasi:http error code carried inside the client error
    /// into [`FetchError`]. Codes that do not identify a policy,
    /// timeout, or request-shape failure are transport failures.
    fn map_error(error: wstd::http::Error) -> FetchError {
        let Some(code) = error.downcast_ref::<ErrorCode>() else {
            return FetchError::Transport(format!("{error:#}"));
        };
        match code {
            ErrorCode::HttpRequestDenied => FetchError::Denied,
            ErrorCode::DnsTimeout
            | ErrorCode::ConnectionTimeout
            | ErrorCode::ConnectionReadTimeout
            | ErrorCode::ConnectionWriteTimeout
            | ErrorCode::HttpResponseTimeout => FetchError::Timeout(code.to_string()),
            ErrorCode::HttpRequestMethodInvalid
            | ErrorCode::HttpRequestUriInvalid
            | ErrorCode::HttpRequestUriTooLong
            | ErrorCode::HttpRequestLengthRequired => FetchError::InvalidRequest(code.to_string()),
            other => FetchError::Transport(other.to_string()),
        }
    }
}

#[cfg(all(target_arch = "wasm32", target_os = "wasi"))]
pub use wasi_impl::fetch;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_builders_compose() {
        let req = Request::post("https://api.cow.fi/mainnet/api/v1/orders", b"{}".to_vec())
            .header("content-type", "application/json")
            .timeout(Duration::from_secs(5));
        assert_eq!(req.method, Method::Post);
        assert_eq!(req.url, "https://api.cow.fi/mainnet/api/v1/orders");
        assert_eq!(
            req.headers,
            vec![("content-type".to_owned(), "application/json".to_owned())]
        );
        assert_eq!(req.body, b"{}");
        assert_eq!(req.timeout, Some(Duration::from_secs(5)));
    }

    #[test]
    fn new_request_carries_default_timeout() {
        assert_eq!(
            Request::get("https://api.cow.fi/").timeout,
            Some(DEFAULT_TIMEOUT)
        );
    }

    #[test]
    fn method_labels_are_canonical_tokens() {
        assert_eq!(<&'static str>::from(Method::Get), "GET");
        assert_eq!(<&'static str>::from(Method::Delete), "DELETE");
    }

    #[test]
    fn fetch_error_labels_are_snake_case() {
        assert_eq!(<&'static str>::from(&FetchError::Denied), "denied");
        assert_eq!(
            <&'static str>::from(&FetchError::Transport("x".into())),
            "transport"
        );
    }

    #[test]
    fn response_success_range() {
        let resp = |status| Response {
            status,
            headers: vec![],
            body: vec![],
        };
        assert!(resp(200).is_success());
        assert!(resp(204).is_success());
        assert!(!resp(301).is_success());
        assert!(!resp(404).is_success());
    }
}
