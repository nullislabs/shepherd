//! `nexum:host/http`: manifest allowlist check, WIT-to-`http` crate
//! conversion, then the [`HttpClient`] seam (a stub in 0.2).
//!
//! The allowlist is enforced now so a module that ships with an empty
//! (or no) `[capabilities.http].allow` gets denied loudly, matching the
//! "no implicit network" stance. The `http`-crate request/response
//! translation lives here so the seam trait speaks typed values.

use std::time::Duration;

use tracing::warn;

use crate::bindings::HostError;
use crate::bindings::nexum;
use crate::bindings::nexum::host::types::HostErrorKind;
use crate::host::component::{ChainProvider, CowApi, HttpClient, StateHandle};
use crate::host::state::HostState;
use crate::manifest::{extract_host, host_allowed};

impl<C, W, S, H> nexum::host::http::Host for HostState<C, W, S, H>
where
    C: ChainProvider + Send + Sync,
    W: CowApi + Send + Sync,
    S: StateHandle + Send + Sync,
    H: HttpClient + Send + Sync,
{
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
        let (request, timeout) = wit_to_request(req)?;
        self.http
            .fetch(request, timeout)
            .await
            .map_err(HostError::from)
            .map(response_to_wit)
    }
}

/// Build an `InvalidInput` HTTP `HostError` from a message.
fn invalid_input(message: String) -> HostError {
    HostError {
        domain: "http".into(),
        kind: HostErrorKind::InvalidInput,
        code: 0,
        message,
        data: None,
    }
}

/// Translate the WIT `request` record into an `http::Request`, plus the
/// guest-requested timeout. Malformed method / URI / headers surface as
/// `InvalidInput`.
fn wit_to_request(
    req: nexum::host::http::Request,
) -> Result<(http::Request<Vec<u8>>, Option<Duration>), HostError> {
    let method = http::Method::from_bytes(req.method.to_ascii_uppercase().as_bytes())
        .map_err(|_| invalid_input(format!("unsupported HTTP method: {}", req.method)))?;
    let uri = req
        .url
        .parse::<http::Uri>()
        .map_err(|e| invalid_input(format!("invalid URL {:?}: {e}", req.url)))?;

    let mut builder = http::Request::builder().method(method).uri(uri);
    for header in req.headers {
        let name = http::HeaderName::from_bytes(header.name.as_bytes())
            .map_err(|e| invalid_input(format!("invalid header name {:?}: {e}", header.name)))?;
        let value = http::HeaderValue::from_str(&header.value).map_err(|e| {
            invalid_input(format!("invalid header value for {:?}: {e}", header.name))
        })?;
        builder = builder.header(name, value);
    }

    let body = req.body.unwrap_or_default();
    let timeout = req
        .timeout_ms
        .map(|ms| Duration::from_millis(u64::from(ms)));
    let request = builder
        .body(body)
        .map_err(|e| invalid_input(format!("malformed request: {e}")))?;
    Ok((request, timeout))
}

/// Translate an `http::Response` back into the WIT `response` record.
fn response_to_wit(resp: http::Response<Vec<u8>>) -> nexum::host::http::Response {
    let status = resp.status().as_u16();
    let headers = resp
        .headers()
        .iter()
        .map(|(name, value)| nexum::host::http::Header {
            name: String::from_utf8_lossy(name.as_str().as_bytes()).into_owned(),
            value: String::from_utf8_lossy(value.as_bytes()).into_owned(),
        })
        .collect();
    let body = resp.into_body();
    nexum::host::http::Response {
        status,
        headers,
        body,
    }
}
