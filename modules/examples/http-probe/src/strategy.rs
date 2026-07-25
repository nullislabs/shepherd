//! Pure strategy logic for the http-probe module.
//!
//! HTTP flows through the [`Fetch`] seam; `lib.rs` hands [`on_block`]
//! `nexum_sdk::http::WasiFetch`, tests hand it a stub fetcher.

use nexum_sdk::config::{self, ConfigError};
use nexum_sdk::host::Fault;
use nexum_sdk::http::{Fetch, FetchError};

/// Settings parsed from `[config]` at `init`.
#[derive(Clone, Debug)]
pub struct Settings {
    /// Allowlisted URL fetched on every matching block.
    pub probe_url: String,
    /// Off-list URL; anything other than a denial is a failure.
    pub denied_url: String,
    /// Only probe every Nth block.
    pub every_n_blocks: u64,
}

/// Probe the allowlisted URL, then verify the off-list URL is denied.
/// `Err` when either leg misbehaves.
pub fn on_block<F: Fetch>(
    fetcher: &F,
    settings: &Settings,
    block_number: u64,
) -> Result<(), Fault> {
    if !block_number.is_multiple_of(settings.every_n_blocks) {
        return Ok(());
    }
    probe_allowlisted(fetcher, &settings.probe_url)?;
    probe_denied(fetcher, &settings.denied_url)
}

/// Fetch the allowlisted URL and log its status; a fetch error is a
/// fault.
fn probe_allowlisted<F: Fetch>(fetcher: &F, url: &str) -> Result<(), Fault> {
    let response = fetcher
        .fetch(get_request(url)?)
        .map_err(|e| fetch_err(url, &e))?;
    tracing::info!(
        "http-probe {url} -> {} ({} body bytes)",
        response.status().as_u16(),
        response.body().len(),
    );
    Ok(())
}

/// Fetch the off-list URL and demand [`FetchError::Denied`]; any other
/// outcome means the allowlist gate did not hold.
fn probe_denied<F: Fetch>(fetcher: &F, url: &str) -> Result<(), Fault> {
    match fetcher.fetch(get_request(url)?) {
        Err(FetchError::Denied) => {
            tracing::info!("http-probe {url} denied by allowlist, as expected");
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

/// Body-less GET for `url`; a malformed URL is an invalid-input fault.
fn get_request(url: &str) -> Result<http::Request<Vec<u8>>, Fault> {
    http::Request::get(url)
        .body(Vec::new())
        .map_err(|e| invalid_input(format!("probe url {url}: {e}")))
}

/// Lift a [`FetchError`] into a [`Fault`], preserving the case.
fn fetch_err(url: &str, error: &FetchError) -> Fault {
    let detail = format!("fetch {url}: {error}");
    match error {
        FetchError::Denied => Fault::Denied(detail),
        FetchError::InvalidRequest(_) => Fault::InvalidInput(detail),
        FetchError::Timeout(_) => Fault::Timeout,
        // `FetchError` is `#[non_exhaustive]`: a future case folds to
        // retryable `unavailable` with its detail.
        _ => Fault::Unavailable(detail),
    }
}

fn internal(message: String) -> Fault {
    Fault::Internal(message)
}

/// Parse `module.toml::[config]` into a typed [`Settings`].
pub fn parse_config(entries: &[(String, String)]) -> Result<Settings, Fault> {
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

fn invalid_input(message: String) -> Fault {
    Fault::InvalidInput(message)
}

fn config_err(e: ConfigError) -> Fault {
    invalid_input(e.to_string())
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use nexum_sdk::Level;
    use nexum_sdk::host::Fault;
    use nexum_sdk::http::FetchOptions;
    use nexum_sdk_test::capture_tracing;

    use super::*;

    /// Stub fetcher: replays canned outcomes in order, records URLs.
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

        let (result, logs) = capture_tracing(|| on_block(&fetcher, &settings(), 42));
        result.unwrap();

        assert_eq!(
            *fetcher.urls.borrow(),
            vec![settings().probe_url, settings().denied_url],
        );
        assert_eq!(logs.count_at(Level::INFO), 2);
        assert_eq!(logs.count_at(Level::WARN), 0);
        let events = logs.events();
        assert_eq!(
            events[0].message,
            format!("http-probe {} -> 200 (7 body bytes)", settings().probe_url),
        );
        assert_eq!(
            events[1].message,
            format!(
                "http-probe {} denied by allowlist, as expected",
                settings().denied_url
            ),
        );
    }

    #[test]
    fn probe_transport_failure_is_unavailable_fault() {
        let fetcher = StubFetch::new(vec![Err(FetchError::Transport(
            "connection refused".into(),
        ))]);

        let err = on_block(&fetcher, &settings(), 1).unwrap_err();
        let Fault::Unavailable(message) = err else {
            panic!("expected unavailable fault, got {err:?}");
        };
        assert!(message.contains("connection refused"));
    }

    #[test]
    fn denied_url_answering_is_internal_error() {
        let fetcher = StubFetch::new(vec![ok_response(200, b"ok"), ok_response(200, b"leak")]);

        let err = on_block(&fetcher, &settings(), 1).unwrap_err();
        let Fault::Internal(message) = err else {
            panic!("expected internal fault, got {err:?}");
        };
        assert!(message.contains("expected"));
    }

    #[test]
    fn denied_url_failing_differently_is_internal_error() {
        let fetcher = StubFetch::new(vec![
            ok_response(200, b"ok"),
            Err(FetchError::Timeout("connection timeout".into())),
        ]);

        let err = on_block(&fetcher, &settings(), 1).unwrap_err();
        let Fault::Internal(message) = err else {
            panic!("expected internal fault, got {err:?}");
        };
        assert!(message.contains("connection timeout"));
    }

    #[test]
    fn throttle_skips_non_multiple_blocks() {
        let fetcher = StubFetch::new(vec![]);
        let cfg = Settings {
            every_n_blocks: 5,
            ..settings()
        };

        let (result, logs) = capture_tracing(|| on_block(&fetcher, &cfg, 7));
        result.unwrap();

        assert!(fetcher.urls.borrow().is_empty());
        assert!(logs.is_empty());
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
        let Fault::InvalidInput(message) = missing else {
            panic!("expected invalid-input fault, got {missing:?}");
        };
        assert!(message.contains("denied_url"));

        let zero = parse_config(&[
            ("probe_url".to_owned(), "https://a/".to_owned()),
            ("denied_url".to_owned(), "https://b/".to_owned()),
            ("every_n_blocks".to_owned(), "0".to_owned()),
        ])
        .unwrap_err();
        let Fault::InvalidInput(message) = zero else {
            panic!("expected invalid-input fault, got {zero:?}");
        };
        assert!(message.contains("every_n_blocks"));
    }
}
