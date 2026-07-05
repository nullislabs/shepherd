//! Pure strategy logic for the http-probe module.
//!
//! All HTTP flows through the [`Fetch`] seam and all logging through
//! [`LoggingHost`], so the whole strategy is unit-testable host-free:
//! tests hand [`on_block`] a stub fetcher and a
//! `shepherd_sdk_test::MockHost`; the `lib.rs` glue hands it
//! `nexum_sdk::http::WasiFetch` and the `WitBindgenHost` adapter.

use nexum_sdk::http::{Fetch, FetchError};
use shepherd_sdk::config::{self, ConfigError};
use shepherd_sdk::host::{HostError, HostErrorKind, LogLevel, LoggingHost};

/// Resolved settings parsed from `[config]` at `init` and read on
/// every event.
#[derive(Clone, Debug)]
pub struct Settings {
    /// URL fetched on every matching block; its host must be on the
    /// module's allowlist.
    pub probe_url: String,
    /// URL whose host is deliberately off-list; anything other than a
    /// denial is a failure.
    pub denied_url: String,
    /// Only probe every Nth block.
    pub every_n_blocks: u64,
}

/// Entry point: probe the allowlisted URL, then verify the off-list
/// URL is denied. Returns `Err` when either leg misbehaves so the
/// runtime records a host-error for the dispatch.
pub fn on_block<F: Fetch, L: LoggingHost>(
    fetcher: &F,
    host: &L,
    settings: &Settings,
    block_number: u64,
) -> Result<(), HostError> {
    if !block_number.is_multiple_of(settings.every_n_blocks) {
        return Ok(());
    }
    probe_allowlisted(fetcher, host, &settings.probe_url)?;
    probe_denied(fetcher, host, &settings.denied_url)
}

/// Fetch the allowlisted URL and log its status; any fetch error is
/// surfaced as a host-error for this dispatch.
fn probe_allowlisted<F: Fetch, L: LoggingHost>(
    fetcher: &F,
    host: &L,
    url: &str,
) -> Result<(), HostError> {
    let response = fetcher
        .fetch(get_request(url)?)
        .map_err(|e| fetch_err(url, &e))?;
    host.log(
        LogLevel::Info,
        &format!(
            "http-probe {url} -> {} ({} body bytes)",
            response.status().as_u16(),
            response.body().len(),
        ),
    );
    Ok(())
}

/// Fetch the off-list URL and demand [`FetchError::Denied`]; a
/// response or any other error means the allowlist gate did not hold.
fn probe_denied<F: Fetch, L: LoggingHost>(
    fetcher: &F,
    host: &L,
    url: &str,
) -> Result<(), HostError> {
    match fetcher.fetch(get_request(url)?) {
        Err(FetchError::Denied) => {
            host.log(
                LogLevel::Info,
                &format!("http-probe {url} denied by allowlist, as expected"),
            );
            Ok(())
        }
        Ok(response) => Err(internal(format!(
            "expected {url} to be denied by the allowlist, got status {}",
            response.status().as_u16(),
        ))),
        Err(other) => Err(internal(format!(
            "expected {url} to be denied by the allowlist, got: {other}",
        ))),
    }
}

/// Build a body-less GET for `url`; a malformed URL is a config error
/// surfaced as an invalid-input host-error.
fn get_request(url: &str) -> Result<http::Request<Vec<u8>>, HostError> {
    http::Request::get(url)
        .body(Vec::new())
        .map_err(|e| invalid_input(format!("probe url {url}: {e}")))
}

/// Lift a [`FetchError`] into the module's `HostError`, preserving the
/// policy/timeout/input/transport distinction in the error kind.
fn fetch_err(url: &str, error: &FetchError) -> HostError {
    let kind = match error {
        FetchError::Denied => HostErrorKind::Denied,
        FetchError::InvalidRequest(_) => HostErrorKind::InvalidInput,
        FetchError::Timeout(_) => HostErrorKind::Timeout,
        FetchError::Transport(_) => HostErrorKind::Unavailable,
    };
    HostError {
        domain: "http-probe".into(),
        kind,
        code: 0,
        message: format!("http-probe: fetch {url}: {error}"),
        data: None,
    }
}

fn internal(message: String) -> HostError {
    HostError {
        domain: "http-probe".into(),
        kind: HostErrorKind::Internal,
        code: 0,
        message,
        data: None,
    }
}

/// Parse `module.toml::[config]` into a typed [`Settings`].
pub fn parse_config(entries: &[(String, String)]) -> Result<Settings, HostError> {
    let probe_url = config::get_required(entries, "probe_url").map_err(config_err)?;
    let denied_url = config::get_required(entries, "denied_url").map_err(config_err)?;
    let every_n_blocks = match config::get_optional(entries, "every_n_blocks") {
        Some(raw) => raw
            .parse::<u64>()
            .map_err(|e| invalid_input(format!("every_n_blocks: {e}")))?,
        None => 1,
    };
    if every_n_blocks == 0 {
        return Err(invalid_input("every_n_blocks must be >= 1".to_owned()));
    }
    Ok(Settings {
        probe_url: probe_url.to_owned(),
        denied_url: denied_url.to_owned(),
        every_n_blocks,
    })
}

fn invalid_input(message: String) -> HostError {
    HostError {
        domain: "http-probe".into(),
        kind: HostErrorKind::InvalidInput,
        code: 0,
        message: format!("http-probe: invalid [config]: {message}"),
        data: None,
    }
}

fn config_err(e: ConfigError) -> HostError {
    invalid_input(e.to_string())
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use nexum_sdk::http::FetchOptions;
    use shepherd_sdk::host::HostErrorKind as Kind;
    use shepherd_sdk_test::MockHost;

    use super::*;

    /// Stub fetcher: replays canned outcomes in call order and records
    /// the requested URLs.
    struct StubFetch {
        outcomes: RefCell<Vec<Result<http::Response<Vec<u8>>, FetchError>>>,
        urls: RefCell<Vec<String>>,
    }

    impl StubFetch {
        fn new(outcomes: Vec<Result<http::Response<Vec<u8>>, FetchError>>) -> Self {
            Self {
                outcomes: RefCell::new(outcomes),
                urls: RefCell::new(Vec::new()),
            }
        }
    }

    impl Fetch for StubFetch {
        fn fetch_with(
            &self,
            request: http::Request<Vec<u8>>,
            _options: FetchOptions,
        ) -> Result<http::Response<Vec<u8>>, FetchError> {
            self.urls.borrow_mut().push(request.uri().to_string());
            self.outcomes.borrow_mut().remove(0)
        }
    }

    fn ok_response(status: u16, body: &[u8]) -> Result<http::Response<Vec<u8>>, FetchError> {
        Ok(http::Response::builder()
            .status(status)
            .body(body.to_vec())
            .unwrap())
    }

    fn settings() -> Settings {
        Settings {
            probe_url: "https://api.cow.fi/mainnet/api/v1/version".into(),
            denied_url: "https://example.com/".into(),
            every_n_blocks: 1,
        }
    }

    #[test]
    fn happy_path_logs_status_and_denial() {
        let fetcher = StubFetch::new(vec![
            ok_response(200, b"\"1.2.3\""),
            Err(FetchError::Denied),
        ]);
        let host = MockHost::new();

        on_block(&fetcher, &host, &settings(), 42).unwrap();

        assert_eq!(
            *fetcher.urls.borrow(),
            vec![settings().probe_url, settings().denied_url],
        );
        assert!(host.logging.contains("-> 200 (7 body bytes)"));
        assert!(host.logging.contains("denied by allowlist, as expected"));
    }

    #[test]
    fn probe_transport_failure_is_unavailable_host_error() {
        let fetcher = StubFetch::new(vec![Err(FetchError::Transport(
            "connection refused".into(),
        ))]);
        let host = MockHost::new();

        let err = on_block(&fetcher, &host, &settings(), 1).unwrap_err();
        assert!(matches!(err.kind, Kind::Unavailable));
        assert!(err.message.contains("connection refused"));
    }

    #[test]
    fn denied_url_answering_is_internal_error() {
        let fetcher = StubFetch::new(vec![ok_response(200, b"ok"), ok_response(200, b"leak")]);
        let host = MockHost::new();

        let err = on_block(&fetcher, &host, &settings(), 1).unwrap_err();
        assert!(matches!(err.kind, Kind::Internal));
        assert!(err.message.contains("expected"));
    }

    #[test]
    fn denied_url_failing_differently_is_internal_error() {
        let fetcher = StubFetch::new(vec![
            ok_response(200, b"ok"),
            Err(FetchError::Timeout("connection timeout".into())),
        ]);
        let host = MockHost::new();

        let err = on_block(&fetcher, &host, &settings(), 1).unwrap_err();
        assert!(matches!(err.kind, Kind::Internal));
        assert!(err.message.contains("connection timeout"));
    }

    #[test]
    fn throttle_skips_non_multiple_blocks() {
        let fetcher = StubFetch::new(vec![]);
        let host = MockHost::new();
        let cfg = Settings {
            every_n_blocks: 5,
            ..settings()
        };

        on_block(&fetcher, &host, &cfg, 7).unwrap();

        assert!(fetcher.urls.borrow().is_empty());
        assert!(host.logging.lines().is_empty());
    }

    #[test]
    fn parse_config_happy_path() {
        let entries = vec![
            ("probe_url".to_owned(), "https://api.cow.fi/x".to_owned()),
            ("denied_url".to_owned(), "https://example.com/".to_owned()),
            ("every_n_blocks".to_owned(), "3".to_owned()),
        ];
        let cfg = parse_config(&entries).unwrap();
        assert_eq!(cfg.probe_url, "https://api.cow.fi/x");
        assert_eq!(cfg.denied_url, "https://example.com/");
        assert_eq!(cfg.every_n_blocks, 3);
    }

    #[test]
    fn parse_config_defaults_every_n_blocks() {
        let entries = vec![
            ("probe_url".to_owned(), "https://a/".to_owned()),
            ("denied_url".to_owned(), "https://b/".to_owned()),
        ];
        assert_eq!(parse_config(&entries).unwrap().every_n_blocks, 1);
    }

    #[test]
    fn parse_config_rejects_missing_urls_and_zero_throttle() {
        let missing =
            parse_config(&[("probe_url".to_owned(), "https://a/".to_owned())]).unwrap_err();
        assert!(matches!(missing.kind, Kind::InvalidInput));
        assert!(missing.message.contains("denied_url"));

        let zero = parse_config(&[
            ("probe_url".to_owned(), "https://a/".to_owned()),
            ("denied_url".to_owned(), "https://b/".to_owned()),
            ("every_n_blocks".to_owned(), "0".to_owned()),
        ])
        .unwrap_err();
        assert!(matches!(zero.kind, Kind::InvalidInput));
        assert!(zero.message.contains("every_n_blocks"));
    }
}
