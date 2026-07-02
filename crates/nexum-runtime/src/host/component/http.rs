//! HTTP seam. The 0.2 runtime has no real fetch backend; the default
//! mirrors the current stub: always `Unsupported`. Allowlist policy
//! stays in the WIT glue, not behind this seam.

use std::future::Future;

use crate::bindings::HostError;
use crate::bindings::nexum::host::http::{Request, Response};
use crate::host::error::unimplemented;

/// Outbound HTTP backend for `http::fetch` (post-allowlist).
pub trait HttpClient {
    /// Execute `req` and return the response.
    fn fetch(&self, req: Request) -> impl Future<Output = Result<Response, HostError>> + Send;
}

/// Default backend for 0.2: rejects every request as `Unsupported`,
/// byte-identical to today's stub in the http host impl.
#[derive(Debug, Default, Clone, Copy)]
pub struct UnsupportedHttp;

impl HttpClient for UnsupportedHttp {
    fn fetch(&self, _req: Request) -> impl Future<Output = Result<Response, HostError>> + Send {
        std::future::ready(Err(unimplemented(
            "http",
            "fetch not implemented in 0.2 reference runtime (allowlist passed)",
        )))
    }
}
