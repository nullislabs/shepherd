//! Mock CoW orderbook for shepherd load tests (COW-1079).
//!
//! Serves the two endpoints shepherd's `cow-api` host backend hits on
//! every order submission:
//!
//! - `POST /api/v1/orders` - accepts any body, returns a synthetic
//!   56-byte OrderUid as a JSON-encoded hex string. Counts a request
//!   for the operator report.
//! - `GET  /api/v1/app_data/{hash}` - returns the empty appData
//!   document so `resolve_app_data` (COW-1074) is satisfied without
//!   needing a real registry.
//!
//! Operator knobs (CLI):
//! - `--port` (default 9999)
//! - `--latency-ms` artificial latency injected into every response
//! - `--error-rate` fraction of `POST /api/v1/orders` responses that
//!   return a recognised `ApiError` envelope; lets the load test
//!   exercise the strategy's `Drop` / `TryNextBlock` paths.
//!
//! Not a faithful orderbook simulator - the load test cares about
//! shepherd's throughput when the orderbook responds quickly, not
//! about the orderbook's own behaviour. For real-orderbook fidelity
//! see COW-1078 (backtest against live `/api/v1/quote`).

#![cfg_attr(not(test), warn(unused_crate_dependencies))]

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use clap::Parser;
use rand::Rng;
use serde::Serialize;
use tracing::info;

/// CLI for the mock orderbook.
#[derive(Debug, Parser)]
#[command(
    name = "orderbook-mock",
    about = "Mock CoW orderbook backing the shepherd COW-1079 load test."
)]
struct Cli {
    /// TCP port to listen on.
    #[arg(long, default_value_t = 9999)]
    port: u16,

    /// Artificial latency (milliseconds) injected into every response.
    #[arg(long, default_value_t = 0)]
    latency_ms: u64,

    /// Fraction of POST /api/v1/orders responses that return a
    /// recognised error envelope instead of a 201 success. 0.0 = all
    /// success; 1.0 = all error. Errors cycle between
    /// `InsufficientFee` (transient -> TryNextBlock) and
    /// `InvalidSignature` (permanent -> Drop).
    #[arg(long, default_value_t = 0.0)]
    error_rate: f64,
}

#[derive(Debug, Default)]
struct Counters {
    submits_ok: AtomicU64,
    submits_err: AtomicU64,
    app_data_lookups: AtomicU64,
}

struct AppState {
    cli: Cli,
    counters: Counters,
}

impl AppState {
    fn new(cli: Cli) -> Self {
        Self {
            cli,
            counters: Counters::default(),
        }
    }
}

#[derive(Debug, Serialize)]
struct ApiError {
    #[serde(rename = "errorType")]
    error_type: &'static str,
    description: &'static str,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    let cli = Cli::parse();
    let port = cli.port;
    let state = Arc::new(AppState::new(cli));

    let app = Router::new()
        .route("/api/v1/orders", post(post_orders))
        .route("/api/v1/app_data/:hash", get(get_app_data))
        .route("/healthz", get(healthz))
        .route("/_stats", get(stats))
        .with_state(state.clone());

    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    info!(
        port = port,
        latency_ms = state.cli.latency_ms,
        error_rate = state.cli.error_rate,
        "orderbook-mock listening"
    );
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let shutdown = async {
        let _ = tokio::signal::ctrl_c().await;
        info!("orderbook-mock shutting down");
    };
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await?;
    Ok(())
}

async fn healthz() -> &'static str {
    "ok"
}

async fn stats(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let body = serde_json::json!({
        "submits_ok": state.counters.submits_ok.load(Ordering::Relaxed),
        "submits_err": state.counters.submits_err.load(Ordering::Relaxed),
        "app_data_lookups": state.counters.app_data_lookups.load(Ordering::Relaxed),
    });
    (StatusCode::OK, axum::Json(body))
}

async fn post_orders(State(state): State<Arc<AppState>>, body: String) -> impl IntoResponse {
    if state.cli.latency_ms > 0 {
        tokio::time::sleep(Duration::from_millis(state.cli.latency_ms)).await;
    }

    let roll = rand::thread_rng().r#gen::<f64>();
    if roll < state.cli.error_rate {
        state.counters.submits_err.fetch_add(1, Ordering::Relaxed);
        // Alternate transient + permanent so the load test exercises
        // both `TryNextBlock` and `Drop` paths through
        // `shepherd_sdk::cow::classify_api_error`.
        let n = state.counters.submits_err.load(Ordering::Relaxed);
        let api = if n.is_multiple_of(2) {
            ApiError {
                error_type: "InsufficientFee",
                description: "load-test: forced retriable",
            }
        } else {
            ApiError {
                error_type: "InvalidSignature",
                description: "load-test: forced permanent",
            }
        };
        return (
            StatusCode::BAD_REQUEST,
            axum::Json(serde_json::to_value(api).unwrap()),
        )
            .into_response();
    }

    // Synthesise a deterministic-per-call OrderUid. The orderbook's
    // real UID is `keccak(orderData) ++ owner ++ validTo`; for the
    // load test the only requirement is that each response is a valid
    // 56-byte hex (224 bits) so the host's cowprotocol decoder
    // accepts it.
    let n = state.counters.submits_ok.fetch_add(1, Ordering::Relaxed);
    let _ = body; // intentionally ignored; load test does not validate the OrderCreation shape
    let mut uid = [0u8; 56];
    uid[0..8].copy_from_slice(&n.to_be_bytes());
    let uid_hex = format!("\"0x{}\"", hex_encode_inline(&uid));
    (StatusCode::CREATED, uid_hex).into_response()
}

async fn get_app_data(
    State(state): State<Arc<AppState>>,
    Path(_hash): Path<String>,
) -> impl IntoResponse {
    if state.cli.latency_ms > 0 {
        tokio::time::sleep(Duration::from_millis(state.cli.latency_ms)).await;
    }
    state
        .counters
        .app_data_lookups
        .fetch_add(1, Ordering::Relaxed);
    // The empty appData document - keccak256("{}") matches the
    // EMPTY_APP_DATA_HASH the test EOA and load-gen will sign over.
    let body = serde_json::json!({ "fullAppData": "{}" });
    (StatusCode::OK, axum::Json(body)).into_response()
}

/// Tiny inline hex encoder - the mock does not depend on `alloy` to
/// keep its dependency surface minimal. (The engine uses
/// `alloy_primitives::hex::encode_prefixed` instead; that rule
/// applies to the engine, not to one-off test tooling.)
fn hex_encode_inline(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        write!(s, "{b:02x}").expect("writing to String never fails");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    fn router_with(cli: Cli) -> Router {
        let state = Arc::new(AppState::new(cli));
        Router::new()
            .route("/api/v1/orders", post(post_orders))
            .route("/api/v1/app_data/:hash", get(get_app_data))
            .with_state(state)
    }

    fn default_cli() -> Cli {
        Cli {
            port: 0,
            latency_ms: 0,
            error_rate: 0.0,
        }
    }

    #[tokio::test]
    async fn post_orders_returns_56_byte_hex_uid() {
        let app = router_with(default_cli());
        let resp = app
            .oneshot(
                Request::post("/api/v1/orders")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"any":"body"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let s = std::str::from_utf8(&body).unwrap();
        // JSON-encoded string: "0x..." (1 + 2 + 112 + 1 = 116 chars)
        assert!(s.starts_with("\"0x"));
        assert_eq!(s.len(), 116);
    }

    #[tokio::test]
    async fn get_app_data_returns_empty_document() {
        let app = router_with(default_cli());
        let resp = app
            .oneshot(
                Request::get("/api/v1/app_data/0xdeadbeef")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed["fullAppData"], "{}");
    }

    #[tokio::test]
    async fn error_rate_one_always_returns_envelope() {
        let app = router_with(Cli {
            port: 0,
            latency_ms: 0,
            error_rate: 1.0,
        });
        let resp = app
            .oneshot(
                Request::post("/api/v1/orders")
                    .body(Body::from(""))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let err_type = parsed["errorType"].as_str().unwrap();
        assert!(
            matches!(err_type, "InsufficientFee" | "InvalidSignature"),
            "got {err_type}"
        );
    }
}
