//! The CoW venue adapter: the `venue-adapter` component slice.
//!
//! `CowAdapter` decodes [`CowIntentBody`], assembles the orderbook
//! wire bodies through [`crate::assembly`], and speaks the orderbook
//! REST API over the scoped wasi:http transport bounded by the
//! configured per-request timeout. Orderbook `errorType` rejections
//! project onto `venue-error` through the shipped classification table;
//! an unsigned order submits as pre-sign, success carrying the
//! `setPreSignature` call the host signs.
//!
//! `[config]` keys: `chain` (required, decimal chain id), optional
//! `orderbook-url`, `owner` (hex address enabling the pre-sign path),
//! `http-timeout-ms` (per-request bound, default the SDK per-phase
//! timeout), `dry-run` ("true" suppresses order posts and status
//! reads, default "false"; quotes still go to the orderbook).

use core::time::Duration;
use std::sync::{PoisonError, RwLock};

use alloy_primitives::Address;
use cowprotocol::{
    ApiError, Chain, OrderCreation, OrderData, OrderKind, OrderStatus, OrderbookApiErrorType,
    QuoteAppData, QuoteRequest,
};
use nexum_sdk::keeper::RetryAction;
use serde::Deserialize;
use url::Url;
use videre_sdk::transport::http::Fetch;
use videre_sdk::value_flow::AssetAmount;
use videre_sdk::{
    AuthScheme, IntentBody as _, IntentHeader, IntentStatus, Quotation, RateLimit, Settlement,
    SubmitOutcome, UnsignedTx, VenueError,
};

use crate::assembly;
use crate::body::{CowIntent, CowIntentBody};
use crate::classification;
use crate::order::OrderUid;

/// The CoW venue's `venue-adapter` export type; the component face
/// comes from `#[videre_sdk::venue]` on the
/// [`VenueAdapter`](videre_sdk::VenueAdapter) impl.
pub struct CowAdapter;

// The reconcile floor: an already-held re-POST folds to the same accept
// outcome (`submit_with`, both auth paths) and status GETs the
// body-derived uid, so the adapter honours the contract.
impl videre_sdk::client::sealed::SealedReconcile for CowAdapter {}
impl videre_sdk::VenueReconcile for CowAdapter {}

/// Default per-request timeout bound: the SDK's per-phase default.
const DEFAULT_TIMEOUT: Duration = videre_sdk::transport::http::DEFAULT_TIMEOUT;

/// Parsed `[config]`: one adapter instance speaks one chain's orderbook.
#[derive(Clone, Debug)]
pub(crate) struct AdapterConfig {
    pub(crate) chain: Chain,
    pub(crate) base: Url,
    pub(crate) owner: Option<Address>,
    pub(crate) timeout: Duration,
    pub(crate) dry_run: bool,
}

impl AdapterConfig {
    /// Parse the wire config table. Unknown keys are ignored; a
    /// malformed value fails init typedly.
    pub(crate) fn parse(config: &[(String, String)]) -> Result<Self, videre_sdk::Fault> {
        let invalid = |key: &str, value: &str| {
            videre_sdk::Fault::InvalidInput(format!("config {key} is invalid: {value}"))
        };
        let mut chain = None;
        let mut base = None;
        let mut owner = None;
        let mut timeout = DEFAULT_TIMEOUT;
        let mut dry_run = false;
        for (key, value) in config {
            match key.as_str() {
                "chain" => {
                    let id: u64 = value.parse().map_err(|_| invalid(key, value))?;
                    chain = Some(Chain::try_from(id).map_err(|_| invalid(key, value))?);
                }
                "orderbook-url" => {
                    let mut url: Url = value.parse().map_err(|_| invalid(key, value))?;
                    // Path joining relies on a trailing slash.
                    if !url.path().ends_with('/') {
                        let path = format!("{}/", url.path());
                        url.set_path(&path);
                    }
                    base = Some(url);
                }
                "owner" => {
                    owner = Some(value.parse::<Address>().map_err(|_| invalid(key, value))?);
                }
                "http-timeout-ms" => {
                    let ms: u64 = value.parse().map_err(|_| invalid(key, value))?;
                    timeout = Duration::from_millis(ms.max(1));
                }
                "dry-run" => {
                    dry_run = match value.as_str() {
                        "true" => true,
                        "false" => false,
                        _ => return Err(invalid(key, value)),
                    };
                }
                _ => {}
            }
        }
        let chain = chain.ok_or_else(|| {
            videre_sdk::Fault::InvalidInput("config requires a chain id".to_owned())
        })?;
        Ok(Self {
            chain,
            base: base.unwrap_or_else(|| chain.orderbook_base_url()),
            owner,
            timeout,
            dry_run,
        })
    }
}

/// Configured adapter state; `init` replaces it whole.
static CONFIG: RwLock<Option<AdapterConfig>> = RwLock::new(None);

pub(crate) fn store_config(config: AdapterConfig) {
    *CONFIG.write().unwrap_or_else(PoisonError::into_inner) = Some(config);
}

/// The stored config, or a typed refusal when `init` has not run.
pub(crate) fn config() -> Result<AdapterConfig, VenueError> {
    CONFIG
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
        .ok_or_else(|| VenueError::Unavailable("adapter not initialised".to_owned()))
}

// ── intent functions, transport-injected for host-free tests ─────────

/// Decode the versioned wire body into its single published intent sum.
fn decode(body: &[u8]) -> Result<CowIntent, VenueError> {
    let CowIntentBody::V1(intent) = CowIntentBody::from_bytes(body)?;
    Ok(intent)
}

/// Derive the intent header for `chain`: the sell side gives, the buy
/// side wants, authorisation by intent kind.
pub(crate) fn derive_header_with(chain: u64, body: &[u8]) -> Result<IntentHeader, VenueError> {
    let intent = decode(body)?;
    let (order, authorisation) = match &intent {
        CowIntent::Order(order) => (order, AuthScheme::Eip712),
        CowIntent::Signed(signed) => (&signed.order, AuthScheme::Eip1271),
    };
    Ok(IntentHeader {
        gives: AssetAmount::erc20(order.sell_token, order.sell_amount),
        wants: AssetAmount::erc20(order.buy_token, order.buy_amount),
        settlement: Settlement { chain },
        authorisation,
    })
}

/// Submit one intent. A signed order posts EIP-1271, its receipt the
/// canonical UID; an unsigned order posts pre-sign, success carrying
/// the `setPreSignature` call. An already-held rejection is success on
/// the client-derived UID. An accepted UID is reconciled against the
/// local derivation; a disagreement is a typed refusal. In dry-run
/// mode the body still assembles and validates, the post is skipped,
/// and the outcome carries the client-derived UID in the live shape.
pub(crate) fn submit_with(
    fetch: &impl Fetch,
    config: &AdapterConfig,
    body: &[u8],
) -> Result<SubmitOutcome, VenueError> {
    match decode(body)? {
        CowIntent::Signed(signed) => {
            let order = assembly::body_to_order_data(&signed.order);
            let owner = signed.owner;
            let creation = assembly::build_order_creation(&order, &signed.signature, owner)
                .map_err(|e| VenueError::InvalidBody(e.to_string()))?;
            if config.dry_run {
                let uid = assembly::order_uid(config.chain, &order, owner);
                tracing::info!("dry-run suppressed signed post; orderUid {uid}");
                return Ok(SubmitOutcome::Accepted(uid.as_slice().to_vec()));
            }
            let uid = match post_order(fetch, config, &creation)? {
                Posted::Accepted(uid) => reconciled_uid(uid, config, &order, owner)?,
                // Locally derived and unverified: no UID in the reply.
                Posted::AlreadyHeld => assembly::order_uid(config.chain, &order, owner),
            };
            Ok(SubmitOutcome::Accepted(uid.as_slice().to_vec()))
        }
        CowIntent::Order(wire) => {
            // Pre-sign needs an owner for `from` and the on-chain call;
            // an unconfigured deployment refuses rather than guesses.
            let owner = config.owner.ok_or(VenueError::Unsupported)?;
            let order = assembly::body_to_order_data(&wire);
            let creation = assembly::build_presign_creation(&order, owner)
                .map_err(|e| VenueError::InvalidBody(e.to_string()))?;
            let uid = if config.dry_run {
                let uid = assembly::order_uid(config.chain, &order, owner);
                tracing::info!("dry-run suppressed pre-sign post; orderUid {uid}");
                uid
            } else {
                match post_order(fetch, config, &creation)? {
                    Posted::Accepted(uid) => reconciled_uid(uid, config, &order, owner)?,
                    // Locally derived and unverified: no UID in the reply.
                    Posted::AlreadyHeld => assembly::order_uid(config.chain, &order, owner),
                }
            };
            Ok(SubmitOutcome::RequiresSigning(UnsignedTx {
                chain: config.chain.id(),
                to: config.chain.settlement().as_slice().to_vec(),
                value: Vec::new(),
                data: assembly::set_pre_signature_calldata(&uid),
            }))
        }
    }
}

/// Refuse an accepted receipt whose UID disagrees with the local
/// derivation.
fn reconciled_uid(
    server: cowprotocol::OrderUid,
    config: &AdapterConfig,
    order: &OrderData,
    owner: Address,
) -> Result<cowprotocol::OrderUid, VenueError> {
    let derived = assembly::order_uid(config.chain, order, owner);
    if server != derived {
        return Err(VenueError::ReceiptMismatch);
    }
    Ok(server)
}

/// Poll one receipt's orderbook lifecycle state. In dry-run mode a
/// valid receipt reports `open` without an orderbook read.
pub(crate) fn status_with(
    fetch: &impl Fetch,
    config: &AdapterConfig,
    receipt: &[u8],
) -> Result<IntentStatus, VenueError> {
    let uid = OrderUid::try_from(receipt).map_err(|_| VenueError::InvalidReceipt)?;
    if config.dry_run {
        return Ok(IntentStatus::Open);
    }
    let url = join(config, &format!("api/v1/orders/{uid}"))?;
    let response = call(fetch, http::Method::GET, url, None)?;
    if response.status() == http::StatusCode::NOT_FOUND {
        // A just-accepted order can lag the read path; not-found stays
        // retryable rather than killing the watch.
        return Err(VenueError::Unavailable("order not found".to_owned()));
    }
    if !response.status().is_success() {
        return Err(refusal_for_read(&response));
    }
    /// The one server field the lifecycle projection reads.
    #[derive(Deserialize)]
    struct OrderStatusView {
        status: OrderStatus,
    }
    let view: OrderStatusView = serde_json::from_slice(response.body())
        .map_err(|e| VenueError::Unavailable(format!("order decode failed: {e}")))?;
    Ok(match view.status {
        OrderStatus::PresignaturePending => IntentStatus::Pending,
        OrderStatus::Open => IntentStatus::Open,
        OrderStatus::Fulfilled => IntentStatus::Fulfilled,
        OrderStatus::Cancelled => IntentStatus::Cancelled,
        OrderStatus::Expired => IntentStatus::Expired,
    })
}

/// Price one intent body: an indicative orderbook quote.
pub(crate) fn quote_with(
    fetch: &impl Fetch,
    config: &AdapterConfig,
    body: &[u8],
) -> Result<Quotation, VenueError> {
    let intent = decode(body)?;
    let (wire, from) = match &intent {
        CowIntent::Order(order) => (order, config.owner.ok_or(VenueError::Unsupported)?),
        CowIntent::Signed(signed) => (&signed.order, signed.owner),
    };
    let order = assembly::body_to_order_data(wire);
    let request = serde_json::to_vec(&quote_request(&order, from))
        .map_err(|e| VenueError::Unavailable(format!("quote encode failed: {e}")))?;
    let response = call(
        fetch,
        http::Method::POST,
        join(config, "api/v1/quote")?,
        Some(request),
    )?;
    if !response.status().is_success() {
        return Err(refusal_for_read(&response));
    }
    let quoted: cowprotocol::OrderQuoteResponse = serde_json::from_slice(response.body())
        .map_err(|e| VenueError::Unavailable(format!("quote decode failed: {e}")))?;
    Ok(Quotation {
        gives: AssetAmount::erc20(order.sell_token, quoted.quote.sell_amount),
        wants: AssetAmount::erc20(order.buy_token, quoted.quote.buy_amount),
        fee: AssetAmount::erc20(order.sell_token, quoted.quote.fee_amount),
        valid_until_ms: u64::from(quoted.quote.valid_to).saturating_mul(1000),
    })
}

/// A quote request pinned to the body's own terms.
fn quote_request(order: &OrderData, from: Address) -> QuoteRequest {
    let mut request = match order.kind {
        OrderKind::Sell => QuoteRequest::sell_before_fee(
            order.sell_token,
            order.buy_token,
            from,
            order.sell_amount,
        ),
        OrderKind::Buy => {
            QuoteRequest::buy_after_fee(order.sell_token, order.buy_token, from, order.buy_amount)
        }
    };
    request.receiver = order.receiver;
    request.valid_to = Some(order.valid_to);
    request.app_data = Some(QuoteAppData::Hash(order.app_data));
    request.partially_fillable = Some(order.partially_fillable);
    request.sell_token_balance = Some(order.sell_token_balance);
    request.buy_token_balance = Some(order.buy_token_balance);
    request
}

// ── orderbook wire plumbing ──────────────────────────────────────────

/// What a `POST /api/v1/orders` produced.
enum Posted {
    /// The orderbook accepted and assigned this UID.
    Accepted(cowprotocol::OrderUid),
    /// The orderbook already holds this exact order.
    AlreadyHeld,
}

fn post_order(
    fetch: &impl Fetch,
    config: &AdapterConfig,
    creation: &OrderCreation,
) -> Result<Posted, VenueError> {
    let body = serde_json::to_vec(creation)
        .map_err(|e| VenueError::Unavailable(format!("order encode failed: {e}")))?;
    let response = call(
        fetch,
        http::Method::POST,
        join(config, "api/v1/orders")?,
        Some(body),
    )?;
    if response.status().is_success() {
        let uid: cowprotocol::OrderUid = serde_json::from_slice(response.body())
            .map_err(|e| VenueError::Unavailable(format!("uid decode failed: {e}")))?;
        return Ok(Posted::Accepted(uid));
    }
    match refusal_for_submit(&response) {
        Refusal::AlreadyHeld => Ok(Posted::AlreadyHeld),
        Refusal::Error(err) => Err(err),
    }
}

fn join(config: &AdapterConfig, path: &str) -> Result<Url, VenueError> {
    config
        .base
        .join(path)
        .map_err(|e| VenueError::Unavailable(format!("orderbook url: {e}")))
}

/// One bounded request; transport failures arrive as a typed [`VenueError`].
fn call(
    fetch: &impl Fetch,
    method: http::Method,
    url: Url,
    json: Option<Vec<u8>>,
) -> Result<http::Response<Vec<u8>>, VenueError> {
    let mut builder = http::Request::builder().method(method).uri(url.as_str());
    if json.is_some() {
        builder = builder.header(http::header::CONTENT_TYPE, "application/json");
    }
    let request = builder
        .body(json.unwrap_or_default())
        .map_err(|e| VenueError::Unavailable(format!("request build failed: {e}")))?;
    Ok(fetch.fetch(request)?)
}

/// A non-2xx submit reply; already-held is a success shape here. Reads
/// use [`refusal_for_read`] instead.
enum Refusal {
    /// Already-held: success wearing an error status.
    AlreadyHeld,
    /// Everything else, as a reported `venue-error`.
    Error(VenueError),
}

/// Project a non-2xx submit reply: throttles first, server failures
/// stay retryable, and only a structured 4xx envelope reaches the
/// classification table.
fn refusal_for_submit(response: &http::Response<Vec<u8>>) -> Refusal {
    let status = response.status();
    if status == http::StatusCode::TOO_MANY_REQUESTS {
        return Refusal::Error(VenueError::RateLimited(RateLimit {
            retry_after_ms: retry_after_ms(response),
        }));
    }
    if status.is_server_error() {
        return Refusal::Error(VenueError::Unavailable(format!(
            "orderbook status {status}"
        )));
    }
    match serde_json::from_slice::<ApiError>(response.body()) {
        Ok(api) if classification::is_already_submitted(api.error_kind()) => Refusal::AlreadyHeld,
        Ok(api) => Refusal::Error(classified(&api)),
        Err(_) => Refusal::Error(VenueError::Unavailable(format!(
            "orderbook status {status}"
        ))),
    }
}

/// Project a non-2xx read reply; already-held has no read meaning and
/// collapses to an error.
fn refusal_for_read(response: &http::Response<Vec<u8>>) -> VenueError {
    match refusal_for_submit(response) {
        Refusal::AlreadyHeld => VenueError::Unavailable("order already held".to_owned()),
        Refusal::Error(err) => err,
    }
}

/// `Retry-After` in milliseconds, when the reply carries the
/// delta-seconds form.
fn retry_after_ms(response: &http::Response<Vec<u8>>) -> Option<u64> {
    response
        .headers()
        .get(http::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .map(|seconds| seconds.saturating_mul(1000))
}

/// Fold a structured rejection through the shipped table: transient
/// rows retry as `unavailable`, throttle rows carry their backoff as
/// `rate-limited`, permanent rows (and any future action) are `denied`.
fn classified(api: &ApiError) -> VenueError {
    let detail = format!("{}: {}", api.error_type, api.description);
    let action = match api.error_kind() {
        // A wire `errorType` the upstream enum does not know is by
        // definition unlisted, hence permanent.
        OrderbookApiErrorType::Unknown(_) => RetryAction::Drop,
        kind => classification::classify(kind),
    };
    match action {
        RetryAction::TryNextBlock => VenueError::Unavailable(detail),
        RetryAction::Backoff { seconds } => VenueError::RateLimited(RateLimit {
            retry_after_ms: Some(seconds.saturating_mul(1000)),
        }),
        // The one-shot grace is client-side; the wire stays `denied`,
        // and the errorType prefix in the detail carries it across.
        RetryAction::DropOnRepeat => VenueError::Denied(detail),
        RetryAction::Drop => VenueError::Denied(detail),
        _ => VenueError::Denied(detail),
    }
}

// The component-ABI export glue only exists on the wasm build; the
// native build keeps the same trait impl (for conformance suites)
// without export symbols no native linker accepts.
#[cfg(not(target_arch = "wasm32"))]
use wit_bindgen as _;

/// The component face: `#[videre_sdk::venue]` derives the world from
/// `module.toml`; the transport is wasi:http behind the configured
/// [`BoundedFetch`](videre_sdk::transport::BoundedFetch).
mod export {
    use videre_sdk::VenueAdapter;
    use videre_sdk::transport::BoundedFetch;
    use videre_sdk::transport::http::WasiFetch;
    #[cfg(not(target_arch = "wasm32"))]
    use videre_sdk::{Config, Fault};
    use videre_sdk::{IntentHeader, IntentStatus, Quotation, SubmitOutcome, VenueError};

    use super::{AdapterConfig, CowAdapter};

    /// Stderr-backed tracing sink; the host captures guest stderr as
    /// tagged log records.
    struct StderrSink;

    impl nexum_sdk::tracing::LogSink for StderrSink {
        fn log(&self, level: tracing::Level, message: &str) {
            eprintln!("{level} {message}");
        }
    }

    #[cfg_attr(target_arch = "wasm32", videre_sdk::venue)]
    impl VenueAdapter for CowAdapter {
        fn init(config: Config) -> Result<(), Fault> {
            nexum_sdk::tracing::init(StderrSink);
            AdapterConfig::parse(&config).map(super::store_config)
        }

        fn body_versions() -> Vec<u32> {
            // Must equal the manifest `[venue] body_versions`; install
            // asserts it.
            vec![1]
        }

        fn derive_header(body: Vec<u8>) -> Result<IntentHeader, VenueError> {
            super::derive_header_with(super::config()?.chain.id(), &body)
        }

        fn quote(body: Vec<u8>) -> Result<Quotation, VenueError> {
            let config = super::config()?;
            super::quote_with(
                &BoundedFetch::new(WasiFetch, config.timeout),
                &config,
                &body,
            )
        }

        fn submit(body: Vec<u8>) -> Result<SubmitOutcome, VenueError> {
            let config = super::config()?;
            super::submit_with(
                &BoundedFetch::new(WasiFetch, config.timeout),
                &config,
                &body,
            )
        }

        fn status(receipt: Vec<u8>) -> Result<IntentStatus, VenueError> {
            let config = super::config()?;
            super::status_with(
                &BoundedFetch::new(WasiFetch, config.timeout),
                &config,
                &receipt,
            )
        }

        fn cancel(_receipt: Vec<u8>) -> Result<(), VenueError> {
            // Off-chain cancellation is an owner-signed request; the
            // adapter structurally holds no keys.
            Err(VenueError::Unsupported)
        }
    }
}

#[cfg(test)]
mod tests {
    use alloy_primitives::U256;
    use videre_sdk::transport::BoundedFetch;
    use videre_sdk::transport::http::FetchError;
    use videre_sdk::value_flow::{Asset, Erc20};
    use videre_sdk::{IntentBody as _, VenueFault};
    use videre_test::MockFetch;
    use videre_test::reconcile::ReconcileFixture;

    use super::*;
    use crate::body::{CowIntent, CowIntentBody};
    use crate::order::{BuyToken, OrderBody, SellToken, SignedOrder};

    const SEPOLIA: u64 = 11_155_111;
    const ORDERS: &str = "https://orderbook.test/api/v1/orders";
    const QUOTE: &str = "https://orderbook.test/api/v1/quote";

    fn config() -> AdapterConfig {
        AdapterConfig {
            chain: Chain::try_from(SEPOLIA).expect("sepolia is supported"),
            base: Url::parse("https://orderbook.test/").expect("test url parses"),
            owner: None,
            timeout: Duration::from_secs(5),
            dry_run: false,
        }
    }

    fn with_owner(owner: Address) -> AdapterConfig {
        AdapterConfig {
            owner: Some(owner),
            ..config()
        }
    }

    fn dry(config: AdapterConfig) -> AdapterConfig {
        AdapterConfig {
            dry_run: true,
            ..config
        }
    }

    fn owner() -> Address {
        Address::repeat_byte(0x55)
    }

    fn order_body() -> OrderBody {
        OrderBody::sell(
            SellToken(Address::repeat_byte(0x11)),
            U256::from(42u64),
            BuyToken(Address::repeat_byte(0x22)),
            U256::from(41u64),
            1_700_000_000,
        )
        .app_data([0x44; 32])
        .build()
    }

    fn signed_bytes() -> Vec<u8> {
        CowIntentBody::V1(CowIntent::Signed(SignedOrder {
            order: order_body(),
            owner: owner(),
            signature: vec![0xC0, 0xFF, 0xEE],
        }))
        .to_bytes()
        .expect("body encodes")
    }

    fn order_bytes() -> Vec<u8> {
        CowIntentBody::V1(CowIntent::Order(order_body()))
            .to_bytes()
            .expect("body encodes")
    }

    fn expected_uid(config: &AdapterConfig) -> cowprotocol::OrderUid {
        let order = assembly::body_to_order_data(&order_body());
        assembly::order_uid(config.chain, &order, owner())
    }

    fn reject(fetch: &MockFetch, error_type: &str) {
        fetch.respond_to(
            http::Method::POST,
            ORDERS,
            400,
            format!(r#"{{"errorType":"{error_type}","description":"d"}}"#),
        );
    }

    // ── config ───────────────────────────────────────────────────────

    #[test]
    fn config_defaults_resolve_from_the_chain() {
        let pairs = [("chain".to_owned(), "1".to_owned())];
        let parsed = AdapterConfig::parse(&pairs).expect("chain alone suffices");
        assert_eq!(parsed.chain.id(), 1);
        assert_eq!(parsed.base.as_str(), "https://api.cow.fi/mainnet/");
        assert_eq!(parsed.owner, None);
        assert_eq!(parsed.timeout, DEFAULT_TIMEOUT);
    }

    #[test]
    fn config_overrides_parse_and_the_base_gains_its_slash() {
        let pairs = [
            ("chain".to_owned(), SEPOLIA.to_string()),
            (
                "orderbook-url".to_owned(),
                "https://barn.test/sepolia".to_owned(),
            ),
            ("owner".to_owned(), format!("{:#x}", owner())),
            ("http-timeout-ms".to_owned(), "1500".to_owned()),
            ("name".to_owned(), "cow".to_owned()),
        ];
        let parsed = AdapterConfig::parse(&pairs).expect("overrides parse");
        assert_eq!(parsed.base.as_str(), "https://barn.test/sepolia/");
        assert_eq!(parsed.owner, Some(owner()));
        assert_eq!(parsed.timeout, Duration::from_millis(1500));
    }

    #[test]
    fn config_dry_run_defaults_off_and_parses_strictly() {
        let chain = ("chain".to_owned(), "1".to_owned());
        let parsed =
            AdapterConfig::parse(std::slice::from_ref(&chain)).expect("chain alone suffices");
        assert!(!parsed.dry_run);
        for (value, expected) in [("true", true), ("false", false)] {
            let pairs = [chain.clone(), ("dry-run".to_owned(), value.to_owned())];
            let parsed = AdapterConfig::parse(&pairs).expect("literal parses");
            assert_eq!(parsed.dry_run, expected);
        }
        for bad in ["yes", "1", "TRUE"] {
            let pairs = [chain.clone(), ("dry-run".to_owned(), bad.to_owned())];
            assert!(matches!(
                AdapterConfig::parse(&pairs),
                Err(videre_sdk::Fault::InvalidInput(msg))
                    if msg == format!("config dry-run is invalid: {bad}")
            ));
        }
    }

    #[test]
    fn config_refuses_a_missing_or_malformed_chain() {
        assert!(matches!(
            AdapterConfig::parse(&[]),
            Err(videre_sdk::Fault::InvalidInput(_))
        ));
        for bad in ["x", "0"] {
            let pairs = [("chain".to_owned(), bad.to_owned())];
            assert!(matches!(
                AdapterConfig::parse(&pairs),
                Err(videre_sdk::Fault::InvalidInput(_))
            ));
        }
    }

    // ── derive-header ────────────────────────────────────────────────

    #[test]
    fn header_projects_sides_minimally_and_auth_by_kind() {
        let header = derive_header_with(SEPOLIA, &signed_bytes()).expect("valid body");
        assert_eq!(
            header.gives.asset,
            Asset::Erc20(Erc20 {
                token: vec![0x11; 20]
            })
        );
        assert_eq!(header.gives.amount, vec![42]);
        assert_eq!(
            header.wants.asset,
            Asset::Erc20(Erc20 {
                token: vec![0x22; 20]
            })
        );
        assert_eq!(header.wants.amount, vec![41]);
        assert_eq!(header.settlement.chain, SEPOLIA);
        assert!(matches!(header.authorisation, AuthScheme::Eip1271));

        let presign = derive_header_with(SEPOLIA, &order_bytes()).expect("valid body");
        assert!(matches!(presign.authorisation, AuthScheme::Eip712));
    }

    #[test]
    fn header_refuses_a_malformed_body() {
        assert!(matches!(
            derive_header_with(SEPOLIA, &[9, 9, 9]),
            Err(VenueError::InvalidBody(_))
        ));
    }

    // ── submit ───────────────────────────────────────────────────────

    #[test]
    fn signed_submit_posts_eip1271_and_returns_the_uid_receipt() {
        let config = config();
        let uid = expected_uid(&config);
        let fetch = MockFetch::default();
        fetch.respond_to(http::Method::POST, ORDERS, 201, format!("\"{uid}\""));

        let outcome = submit_with(&fetch, &config, &signed_bytes()).expect("accepted");
        let SubmitOutcome::Accepted(receipt) = outcome else {
            panic!("signed submit must accept");
        };
        assert_eq!(receipt, uid.as_slice());

        let request = fetch.last_request().expect("one request");
        assert_eq!(request.uri, ORDERS);
        let posted: serde_json::Value = serde_json::from_slice(&request.body).expect("posted JSON");
        assert_eq!(posted["signingScheme"], "eip1271");
        assert_eq!(
            posted["from"].as_str().map(str::to_lowercase),
            Some(format!("{:#x}", owner())),
        );
        assert_eq!(posted["appData"], format!("0x{}", "44".repeat(32)));
    }

    #[test]
    fn already_held_is_success_with_the_derived_uid() {
        let config = config();
        let fetch = MockFetch::default();
        reject(&fetch, "DuplicatedOrder");

        let outcome = submit_with(&fetch, &config, &signed_bytes()).expect("held is success");
        let SubmitOutcome::Accepted(receipt) = outcome else {
            panic!("already-held must accept");
        };
        assert_eq!(receipt, expected_uid(&config).as_slice());
    }

    #[test]
    fn an_accepted_uid_disagreeing_with_the_derivation_is_refused() {
        let config = config();
        let mut drifted = expected_uid(&config);
        drifted.0[0] ^= 0x01;
        let fetch = MockFetch::default();
        fetch.respond_to(http::Method::POST, ORDERS, 201, format!("\"{drifted}\""));

        assert!(matches!(
            submit_with(&fetch, &config, &signed_bytes()),
            Err(VenueError::ReceiptMismatch)
        ));
    }

    #[test]
    fn unsigned_submit_requires_signing_the_presign_call() {
        let config = with_owner(owner());
        let uid = expected_uid(&config);
        let fetch = MockFetch::default();
        fetch.respond_to(http::Method::POST, ORDERS, 201, format!("\"{uid}\""));

        let outcome = submit_with(&fetch, &config, &order_bytes()).expect("accepted");
        let SubmitOutcome::RequiresSigning(tx) = outcome else {
            panic!("unsigned submit must require signing");
        };
        assert_eq!(tx.chain, SEPOLIA);
        assert_eq!(tx.to, config.chain.settlement().as_slice());
        assert!(tx.value.is_empty());
        assert_eq!(tx.data, assembly::set_pre_signature_calldata(&uid));

        let posted: serde_json::Value =
            serde_json::from_slice(&fetch.last_request().expect("one request").body)
                .expect("posted JSON");
        assert_eq!(posted["signingScheme"], "presign");
    }

    #[test]
    fn unsigned_submit_without_an_owner_is_unsupported() {
        let fetch = MockFetch::default();
        assert!(matches!(
            submit_with(&fetch, &config(), &order_bytes()),
            Err(VenueError::Unsupported)
        ));
        assert_eq!(fetch.request_count(), 0);
    }

    #[test]
    fn dry_run_signed_submit_accepts_the_derived_uid_without_posting() {
        let config = dry(config());
        let uid = expected_uid(&config);
        let fetch = MockFetch::default();

        let (outcome, logs) =
            nexum_sdk_test::capture_tracing(|| submit_with(&fetch, &config, &signed_bytes()));
        let SubmitOutcome::Accepted(receipt) = outcome.expect("accepted") else {
            panic!("dry-run signed submit must accept");
        };
        assert_eq!(receipt, uid.as_slice());
        assert_eq!(fetch.request_count(), 0);
        logs.expect_one(|e| {
            e.message.contains("dry-run suppressed signed post")
                && e.message.contains(&uid.to_string())
        });
    }

    #[test]
    fn dry_run_presign_submit_requires_signing_without_posting() {
        let config = dry(with_owner(owner()));
        let uid = expected_uid(&config);
        let fetch = MockFetch::default();

        let (outcome, logs) =
            nexum_sdk_test::capture_tracing(|| submit_with(&fetch, &config, &order_bytes()));
        let SubmitOutcome::RequiresSigning(tx) = outcome.expect("requires signing") else {
            panic!("dry-run pre-sign submit must require signing, as live does");
        };
        assert_eq!(tx.chain, SEPOLIA);
        assert_eq!(tx.to, config.chain.settlement().as_slice());
        assert_eq!(tx.data, assembly::set_pre_signature_calldata(&uid));
        assert_eq!(fetch.request_count(), 0);
        logs.expect_one(|e| {
            e.message.contains("dry-run suppressed pre-sign post")
                && e.message.contains(&uid.to_string())
        });
    }

    #[test]
    fn dry_run_still_refuses_a_body_the_live_path_would() {
        let fetch = MockFetch::default();
        let body = CowIntentBody::V1(CowIntent::Signed(SignedOrder {
            order: order_body(),
            owner: Address::ZERO,
            signature: vec![0xC0, 0xFF, 0xEE],
        }))
        .to_bytes()
        .expect("body encodes");

        assert!(matches!(
            submit_with(&fetch, &dry(config()), &body),
            Err(VenueError::InvalidBody(_))
        ));
        assert_eq!(fetch.request_count(), 0);
    }

    #[test]
    fn dry_run_status_reports_open_without_polling() {
        let fetch = MockFetch::default();
        let uid = OrderUid([0xAB; 56]);

        assert_eq!(
            status_with(&fetch, &dry(config()), uid.as_bytes()).expect("open"),
            IntentStatus::Open,
        );
        assert_eq!(fetch.request_count(), 0);
    }

    #[test]
    fn rejections_project_through_the_classification_table() {
        let config = config();
        let fetch = MockFetch::default();

        reject(&fetch, "InvalidSignature");
        assert!(matches!(
            submit_with(&fetch, &config, &signed_bytes()),
            Err(VenueError::Denied(detail)) if detail.contains("InvalidSignature")
        ));

        // A drop-on-repeat row stays `denied` on the wire; the
        // errorType prefix carries the one-shot grace to the client.
        reject(&fetch, "InvalidEip1271Signature");
        assert!(matches!(
            submit_with(&fetch, &config, &signed_bytes()),
            Err(VenueError::Denied(detail))
                if detail.starts_with("InvalidEip1271Signature:")
        ));

        reject(&fetch, "TooManyLimitOrders");
        assert!(matches!(
            submit_with(&fetch, &config, &signed_bytes()),
            Err(VenueError::RateLimited(rl)) if rl.retry_after_ms == Some(30_000)
        ));

        reject(&fetch, "InsufficientFee");
        assert!(matches!(
            submit_with(&fetch, &config, &signed_bytes()),
            Err(VenueError::Unavailable(detail)) if detail.contains("InsufficientFee")
        ));
    }

    #[test]
    fn transport_shapes_stay_typed() {
        let config = config();
        let fetch = MockFetch::default();

        fetch.respond_to(http::Method::POST, ORDERS, 429, "slow down");
        assert!(matches!(
            submit_with(&fetch, &config, &signed_bytes()),
            Err(VenueError::RateLimited(rl)) if rl.retry_after_ms.is_none()
        ));

        fetch.respond_to(http::Method::POST, ORDERS, 503, "maintenance");
        assert!(matches!(
            submit_with(&fetch, &config, &signed_bytes()),
            Err(VenueError::Unavailable(_))
        ));

        fetch.fail_with(
            http::Method::POST,
            ORDERS,
            FetchError::Timeout("first byte".to_owned()),
        );
        assert!(matches!(
            submit_with(&fetch, &config, &signed_bytes()),
            Err(VenueError::Timeout)
        ));

        fetch.fail_with(http::Method::POST, ORDERS, FetchError::Denied);
        assert!(matches!(
            submit_with(&fetch, &config, &signed_bytes()),
            Err(VenueError::Denied(_))
        ));
    }

    #[test]
    fn requests_ride_the_configured_timeout_bound() {
        let config = config();
        let uid = expected_uid(&config);
        let fetch = MockFetch::default();
        fetch.respond_to(http::Method::POST, ORDERS, 201, format!("\"{uid}\""));

        let timed = BoundedFetch::new(&fetch, config.timeout);
        submit_with(&timed, &config, &signed_bytes()).expect("accepted");
        let options = fetch.last_request().expect("one request").options;
        assert_eq!(options.connect_timeout, config.timeout);
        assert_eq!(options.first_byte_timeout, config.timeout);
        assert_eq!(options.between_bytes_timeout, config.timeout);
    }

    #[test]
    fn retry_after_header_survives_as_milliseconds() {
        let response = http::Response::builder()
            .status(429)
            .header(http::header::RETRY_AFTER, "7")
            .body(Vec::new())
            .expect("test response builds");
        let Refusal::Error(VenueError::RateLimited(rl)) = refusal_for_submit(&response) else {
            panic!("429 must rate-limit");
        };
        assert_eq!(rl.retry_after_ms, Some(7_000));
    }

    // ── status ───────────────────────────────────────────────────────

    fn status_url(uid: &OrderUid) -> String {
        format!("https://orderbook.test/api/v1/orders/{uid}")
    }

    #[test]
    fn status_maps_the_orderbook_lifecycle() {
        let config = config();
        let uid = OrderUid([0xAB; 56]);
        let fetch = MockFetch::default();
        for (wire, status) in [
            ("presignaturePending", IntentStatus::Pending),
            ("open", IntentStatus::Open),
            ("fulfilled", IntentStatus::Fulfilled),
            ("cancelled", IntentStatus::Cancelled),
            ("expired", IntentStatus::Expired),
        ] {
            fetch.respond_to(
                http::Method::GET,
                status_url(&uid),
                200,
                format!(r#"{{"status":"{wire}","uid":"{uid}"}}"#),
            );
            assert_eq!(
                status_with(&fetch, &config, uid.as_bytes()).expect("known status"),
                status,
            );
        }
    }

    #[test]
    fn status_refuses_a_short_receipt_and_retries_not_found() {
        let config = config();
        let fetch = MockFetch::default();
        assert!(matches!(
            status_with(&fetch, &config, &[0xAB; 3]),
            Err(VenueError::InvalidReceipt)
        ));

        let uid = OrderUid([0xAB; 56]);
        fetch.respond_to(http::Method::GET, status_url(&uid), 404, "not found");
        assert!(matches!(
            status_with(&fetch, &config, uid.as_bytes()),
            Err(VenueError::Unavailable(_))
        ));
    }

    // ── quote ────────────────────────────────────────────────────────

    #[test]
    fn quote_prices_the_body_and_pins_its_terms() {
        let config = config();
        let fetch = MockFetch::default();
        let quote = serde_json::json!({
            "quote": {
                "sellToken": format!("0x{}", "11".repeat(20)),
                "buyToken": format!("0x{}", "22".repeat(20)),
                "receiver": null,
                "sellAmount": "42",
                "buyAmount": "40",
                "validTo": 1_700_000_000u32,
                "appData": format!("0x{}", "44".repeat(32)),
                "feeAmount": "2",
                "kind": "sell",
                "partiallyFillable": false,
                "sellTokenBalance": "erc20",
                "buyTokenBalance": "erc20",
                "signingScheme": "eip1271",
            },
            "from": format!("{:#x}", owner()),
            "expiration": "2026-01-01T00:00:00Z",
            "id": 7,
            "verified": true,
        });
        fetch.respond_to(http::Method::POST, QUOTE, 200, quote.to_string());

        let quotation = quote_with(&fetch, &config, &signed_bytes()).expect("quoted");
        assert_eq!(quotation.gives.amount, vec![42]);
        assert_eq!(quotation.wants.amount, vec![40]);
        assert_eq!(quotation.fee.amount, vec![2]);
        assert_eq!(quotation.valid_until_ms, 1_700_000_000_000);

        let posted: serde_json::Value =
            serde_json::from_slice(&fetch.last_request().expect("one request").body)
                .expect("posted JSON");
        assert_eq!(posted["kind"], "sell");
        assert_eq!(posted["sellAmountBeforeFee"], "42");
        assert_eq!(posted["validTo"], 1_700_000_000u32);
    }

    #[test]
    fn quote_for_an_unsigned_body_needs_the_configured_owner() {
        let fetch = MockFetch::default();
        assert!(matches!(
            quote_with(&fetch, &config(), &order_bytes()),
            Err(VenueError::Unsupported)
        ));
        assert_eq!(fetch.request_count(), 0);
    }

    // ── reconcile contract ───────────────────────────────────────────

    /// The shared compliance fixture: one owner-configured config drives
    /// both auth paths.
    struct CowReconcile;

    impl CowReconcile {
        fn cfg() -> AdapterConfig {
            with_owner(owner())
        }

        fn uid() -> cowprotocol::OrderUid {
            expected_uid(&Self::cfg())
        }
    }

    impl ReconcileFixture for CowReconcile {
        fn signed_body() -> Vec<u8> {
            signed_bytes()
        }

        fn presign_body() -> Vec<u8> {
            order_bytes()
        }

        fn receipt() -> Vec<u8> {
            Self::uid().as_slice().to_vec()
        }

        fn program_accept(fetch: &MockFetch) {
            fetch.respond_to(
                http::Method::POST,
                ORDERS,
                201,
                format!("\"{}\"", Self::uid()),
            );
        }

        fn program_already_held(fetch: &MockFetch) {
            reject(fetch, "DuplicatedOrder");
        }

        fn program_absent(fetch: &MockFetch) {
            let uid = OrderUid::try_from(Self::receipt().as_slice()).expect("uid is 56 bytes");
            fetch.respond_to(http::Method::GET, status_url(&uid), 404, "not found");
        }

        fn submit(fetch: &MockFetch, body: &[u8]) -> Result<SubmitOutcome, VenueFault> {
            submit_with(fetch, &Self::cfg(), body).map_err(VenueFault::from)
        }

        fn status(fetch: &MockFetch, receipt: &[u8]) -> Result<IntentStatus, VenueFault> {
            status_with(fetch, &Self::cfg(), receipt).map_err(VenueFault::from)
        }
    }

    videre_test::venue_reconcile_compliance!(CowReconcile);
}
