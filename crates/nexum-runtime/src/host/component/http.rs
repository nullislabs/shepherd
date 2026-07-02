//! HTTP seam. The seam speaks `http` crate types; the WIT-record to
//! `http::Request`/`http::Response` conversion lives in the WIT glue,
//! not behind this trait. The 0.2 runtime has no real fetch backend;
//! the default rejects every request as `Unsupported`. Allowlist policy
//! stays in the WIT glue, ahead of this seam.

use std::future::Future;
use std::time::Duration;

/// Outbound HTTP backend for `http::fetch` (post-allowlist).
pub trait HttpClient {
    /// Execute `req`; `timeout` is the guest-requested per-request cap.
    fn fetch(
        &self,
        req: http::Request<Vec<u8>>,
        timeout: Option<Duration>,
    ) -> impl Future<Output = Result<http::Response<Vec<u8>>, HttpError>> + Send;
}

/// Errors surfaced by an [`HttpClient`].
///
/// `IntoStaticStr` yields the snake_case variant name for metric labels
/// and structured-log fields.
#[derive(Debug, thiserror::Error, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
#[non_exhaustive]
pub enum HttpError {
    /// The reference runtime has no fetch backend wired in.
    #[error("fetch not implemented in 0.2 reference runtime (allowlist passed)")]
    Unsupported,
}

/// Default backend for 0.2: rejects every request as `Unsupported`,
/// byte-identical to the guest-visible stub message.
#[derive(Debug, Default, Clone, Copy)]
pub struct UnsupportedHttp;

impl HttpClient for UnsupportedHttp {
    fn fetch(
        &self,
        _req: http::Request<Vec<u8>>,
        _timeout: Option<Duration>,
    ) -> impl Future<Output = Result<http::Response<Vec<u8>>, HttpError>> + Send {
        std::future::ready(Err(HttpError::Unsupported))
    }
}
