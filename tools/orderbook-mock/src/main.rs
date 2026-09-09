//! Mock CoW orderbook for shepherd load tests.
//!
//! Serves `POST /api/v1/orders`: accepts any body, returns a synthetic
//! 56-byte OrderUid as a JSON hex string. CLI knobs `--port`,
//! `--latency-ms`, and `--error-rate` (fraction of responses returning
//! a recognised `ApiError` envelope, exercising the strategy's `Drop` /
//! `TryNextBlock` paths). Not a faithful simulator.

#![cfg_attr(not(test), warn(unused_crate_dependencies))]

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use clap::Parser;
use rand::RngExt;
use serde::Serialize;
use tracing::info;

/// CLI for the mock orderbook.
#[derive(Debug, Parser)]
#[command(
    name = "orderbook-mock",
    about = "Mock CoW orderbook backing the shepherd load test."
)]
struct Cli {
    /// TCP port to listen on.
    #[arg(long, default_value_t = 9999)]
    port: u16,

    /// Artificial latency (milliseconds) injected into every response.
    #[arg(long, default_value_t = 0)]
    latency_ms: u64,

    /// Fraction of `POST /api/v1/orders` responses returning an error
    /// envelope (0.0 all success, 1.0 all error). Errors cycle
    /// `InsufficientFee` (transient) and `InvalidSignature` (permanent).
    #[arg(long, default_value_t = 0.0)]
    error_rate: f64,

    /// Chain whose settlement domain the returned UID is derived under.
    /// A UID from the wrong domain is refused by the submitting venue.
    #[arg(long, default_value_t = 1)]
    chain_id: u64,
}

#[derive(Debug, Default)]
struct Counters {
    submits_ok: AtomicU64,
    submits_err: AtomicU64,
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
    });
    (StatusCode::OK, axum::Json(body))
}

async fn post_orders(State(state): State<Arc<AppState>>, body: String) -> impl IntoResponse {
    if state.cli.latency_ms > 0 {
        tokio::time::sleep(Duration::from_millis(state.cli.latency_ms)).await;
    }

    let roll = rand::rng().random::<f64>();
    if roll < state.cli.error_rate {
        state.counters.submits_err.fetch_add(1, Ordering::Relaxed);
        // Alternate transient + permanent so the load test exercises
        // both `TryNextBlock` and `Drop` paths through
        // `cow_venue::classification::classify`.
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
            axum::Json(
                serde_json::to_value(api)
                    .expect("ApiError holds only &'static str fields, serialisation is infallible"),
            ),
        )
            .into_response();
    }

    // The UID is derived from the posted order, not invented. A
    // submitting venue re-derives it locally and refuses a receipt that
    // disagrees, so a synthetic UID can never complete a submit.
    let uid = match derive_uid(&body, state.cli.chain_id) {
        Ok(uid) => uid,
        Err(why) => {
            state.counters.submits_err.fetch_add(1, Ordering::Relaxed);
            tracing::warn!("refusing an order this mock cannot derive a UID for: {why}");
            // Deliberately not a real `errorType`: this is the mock
            // failing to read the request, not the orderbook refusing
            // the order, and borrowing a classified type would make a
            // broken fixture look like a venue policy the table has an
            // opinion about.
            return (
                StatusCode::BAD_REQUEST,
                axum::Json(serde_json::json!({
                    "description": format!("mock could not derive a UID: {why}"),
                })),
            )
                .into_response();
        }
    };
    state.counters.submits_ok.fetch_add(1, Ordering::Relaxed);
    (StatusCode::CREATED, format!("\"{uid}\"")).into_response()
}

/// The UID a real orderbook would assign to this posted order.
///
/// `OrderCreation::order_data` projects back the twelve signed fields
/// the UID was computed against, so the derivation here is the same one
/// the submitting venue checks with, not a second implementation of it.
fn derive_uid(body: &str, chain_id: u64) -> Result<cowprotocol::OrderUid, String> {
    let creation: cowprotocol::OrderCreation =
        serde_json::from_str(body).map_err(|e| format!("body is not an OrderCreation: {e}"))?;
    let chain =
        cowprotocol::Chain::try_from(chain_id).map_err(|_| format!("unknown chain {chain_id}"))?;
    Ok(creation
        .order_data()
        .uid(&chain.settlement_domain(), creation.from))
}

/// Inline hex encoder; keeps the mock's dependency surface minimal.
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
            .with_state(state)
    }

    fn default_cli() -> Cli {
        Cli {
            port: 0,
            latency_ms: 0,
            error_rate: 0.0,
            chain_id: 1,
        }
    }

    /// A minimal order in the shape the venue posts.
    fn creation() -> cowprotocol::OrderCreation {
        serde_json::from_str(
            r#"{
              "sellToken": "0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2",
              "buyToken": "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48",
              "receiver": "0x00112233445566778899aabbccddeeff00112233",
              "sellAmount": "1000000000000000",
              "buyAmount": "1000000",
              "validTo": 2000000000,
              "appData": "0x0000000000000000000000000000000000000000000000000000000000000000",
              "feeAmount": "0",
              "kind": "sell",
              "partiallyFillable": false,
              "sellTokenBalance": "erc20",
              "buyTokenBalance": "erc20",
              "signingScheme": "presign",
              "signature": "0x",
              "from": "0x00112233445566778899aabbccddeeff00112233"
            }"#,
        )
        .expect("the fixture is a valid OrderCreation")
    }

    /// The venue re-derives the UID locally and refuses a receipt that
    /// disagrees, so echoing the order's own UID is the whole point.
    #[tokio::test]
    async fn post_orders_returns_the_uid_derived_from_the_order() {
        let order = creation();
        let expected = order
            .order_data()
            .uid(&cowprotocol::Chain::Mainnet.settlement_domain(), order.from);
        let app = router_with(default_cli());
        let resp = app
            .oneshot(
                Request::post("/api/v1/orders")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&order).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(
            std::str::from_utf8(&body).unwrap(),
            format!("\"{expected}\""),
        );
    }

    /// A body this mock cannot read is refused rather than answered with
    /// something the venue would reject anyway, and it carries no
    /// `errorType`: the mock failed to read the request, so it must not
    /// look like a venue policy the classification table has an opinion
    /// about.
    #[tokio::test]
    async fn an_underivable_body_is_refused_without_an_error_type() {
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
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(
            parsed.get("errorType").is_none(),
            "a mock-side failure must not borrow a classified errorType: {parsed}",
        );
    }

    #[tokio::test]
    async fn error_rate_one_always_returns_envelope() {
        let app = router_with(Cli {
            error_rate: 1.0,
            ..default_cli()
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
