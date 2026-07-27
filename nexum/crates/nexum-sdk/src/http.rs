//! Outbound HTTP over wasi:http for guest modules.
//!
//! `fetch` performs one synchronous request through the host's
//! wasi:http outgoing handler. The host admits or denies each request
//! against `[capabilities.http].allow` before connecting; a denial
//! surfaces as [`FetchError::Denied`], distinct from a transport
//! failure. Requests and responses are the [`http`] crate's
//! `Request<Vec<u8>>` / `Response<Vec<u8>>`. The [`Fetch`] seam,
//! [`FetchError`], and [`FetchOptions`] compile on every target for
//! host-side tests; `fetch` itself exists only on `wasm32-wasip2`.

use core::time::Duration;

use strum::IntoStaticStr;

/// Per-phase timeout [`FetchOptions::default`] applies to connect,
/// first byte, and between bytes.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Per-phase wasi:http timeouts that have no home on [`http::Request`].
/// `Default` applies [`DEFAULT_TIMEOUT`] to every phase.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FetchOptions {
    /// Time allowed to establish the connection.
    pub connect_timeout: Duration,
    /// Time allowed for the first response byte.
    pub first_byte_timeout: Duration,
    /// Time allowed between consecutive response body bytes.
    pub between_bytes_timeout: Duration,
}

impl Default for FetchOptions {
    fn default() -> Self {
        Self {
            connect_timeout: DEFAULT_TIMEOUT,
            first_byte_timeout: DEFAULT_TIMEOUT,
            between_bytes_timeout: DEFAULT_TIMEOUT,
        }
    }
}

/// Why a fetch failed, folded down from the wasi:http error codes.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
#[non_exhaustive]
pub enum FetchError {
    /// The `[capabilities.http].allow` list refused the request before
    /// any connection was made.
    #[error("denied by the module's http allowlist")]
    Denied,
    /// The request never left the guest: malformed URL, method, or
    /// header.
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    /// A configured timeout elapsed.
    #[error("timeout: {0}")]
    Timeout(String),
    /// Connection or protocol failure after the request was admitted.
    #[error("transport failure: {0}")]
    Transport(String),
}

/// Seam between module logic and the wasi:http transport; module glue
/// passes [`WasiFetch`], tests a stub.
pub trait Fetch {
    /// Perform one request, blocking until the response body is fully
    /// buffered.
    fn fetch_with(
        &self,
        request: http::Request<Vec<u8>>,
        options: FetchOptions,
    ) -> Result<http::Response<Vec<u8>>, FetchError>;

    /// Perform one request with [`FetchOptions::default`].
    fn fetch(
        &self,
        request: http::Request<Vec<u8>>,
    ) -> Result<http::Response<Vec<u8>>, FetchError> {
        self.fetch_with(request, FetchOptions::default())
    }
}

/// A shared reference forwards to its referent.
impl<F: Fetch + ?Sized> Fetch for &F {
    fn fetch_with(
        &self,
        request: http::Request<Vec<u8>>,
        options: FetchOptions,
    ) -> Result<http::Response<Vec<u8>>, FetchError> {
        (**self).fetch_with(request, options)
    }
}

/// [`Fetch`] adapter over the host's wasi:http outgoing handler. Exists
/// on every target for host-side tests, but [`Fetch::fetch_with`] is
/// unimplemented off the wasm guest.
#[derive(Clone, Copy, Debug, Default)]
pub struct WasiFetch;

impl Fetch for WasiFetch {
    #[cfg(all(target_arch = "wasm32", target_os = "wasi"))]
    fn fetch_with(
        &self,
        request: http::Request<Vec<u8>>,
        options: FetchOptions,
    ) -> Result<http::Response<Vec<u8>>, FetchError> {
        fetch_with(request, options)
    }

    #[cfg(not(all(target_arch = "wasm32", target_os = "wasi")))]
    fn fetch_with(
        &self,
        _request: http::Request<Vec<u8>>,
        _options: FetchOptions,
    ) -> Result<http::Response<Vec<u8>>, FetchError> {
        unimplemented!("wasi:http fetch is only available in a wasm32-wasip2 guest")
    }
}

#[cfg(all(target_arch = "wasm32", target_os = "wasi"))]
mod wasi_impl {
    use wstd::http::ErrorCode;

    use super::{FetchError, FetchOptions};

    /// Perform `request` with [`FetchOptions::default`].
    pub fn fetch(request: http::Request<Vec<u8>>) -> Result<http::Response<Vec<u8>>, FetchError> {
        fetch_with(request, FetchOptions::default())
    }

    /// Perform `request` through the host's wasi:http outgoing handler,
    /// blocking until the response body is fully buffered. The body is
    /// bounded only by the module's memory limit.
    pub fn fetch_with(
        request: http::Request<Vec<u8>>,
        options: FetchOptions,
    ) -> Result<http::Response<Vec<u8>>, FetchError> {
        wstd::runtime::block_on(fetch_async(request, options))
    }

    async fn fetch_async(
        request: http::Request<Vec<u8>>,
        options: FetchOptions,
    ) -> Result<http::Response<Vec<u8>>, FetchError> {
        let mut client = wstd::http::Client::new();
        client.set_connect_timeout(options.connect_timeout);
        client.set_first_byte_timeout(options.first_byte_timeout);
        client.set_between_bytes_timeout(options.between_bytes_timeout);

        let response = client.send(request).await.map_err(map_error)?;
        let (parts, mut body) = response.into_parts();
        let bytes = body.bytes_contents().await.map_err(map_error)?;
        Ok(http::Response::from_parts(parts, bytes.to_vec()))
    }

    /// Fold the client error's wasi:http error code into [`FetchError`];
    /// anything not a policy, timeout, or request-shape failure is
    /// transport.
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
pub use wasi_impl::{fetch, fetch_with};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_options_apply_the_default_timeout_per_phase() {
        let opts = FetchOptions::default();
        assert_eq!(opts.connect_timeout, DEFAULT_TIMEOUT);
        assert_eq!(opts.first_byte_timeout, DEFAULT_TIMEOUT);
        assert_eq!(opts.between_bytes_timeout, DEFAULT_TIMEOUT);
    }

    #[test]
    fn fetch_error_labels_are_snake_case() {
        assert_eq!(<&'static str>::from(&FetchError::Denied), "denied");
        assert_eq!(
            <&'static str>::from(&FetchError::Transport("x".into())),
            "transport"
        );
    }

    /// [`Fetch::fetch`] delegates to `fetch_with` with default options.
    #[test]
    fn fetch_delegates_to_fetch_with_default_options() {
        use core::cell::Cell;

        struct Spy {
            seen: Cell<Option<FetchOptions>>,
        }

        impl Fetch for Spy {
            fn fetch_with(
                &self,
                _request: http::Request<Vec<u8>>,
                options: FetchOptions,
            ) -> Result<http::Response<Vec<u8>>, FetchError> {
                self.seen.set(Some(options));
                Ok(http::Response::new(Vec::new()))
            }
        }

        let spy = Spy {
            seen: Cell::new(None),
        };
        let request = http::Request::get("https://api.cow.fi/")
            .body(Vec::new())
            .unwrap();
        spy.fetch(request).unwrap();
        assert_eq!(spy.seen.get(), Some(FetchOptions::default()));
    }
}
