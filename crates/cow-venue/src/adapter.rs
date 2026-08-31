//! The CoW venue as a native `videre_host::VenueInvoker`.
//!
//! [`CowAdapter`] decodes [`CowIntentBody`], assembles the orderbook wire
//! bodies through [`crate::assembly`], and speaks the orderbook REST API
//! over its own [`Transport`] bounded by the configured per-request
//! timeout. Orderbook `errorType` rejections project onto `venue-error`
//! through the shipped classification table; an unsigned order submits as
//! pre-sign, success carrying the `setPreSignature` call the caller signs.
//!
//! This was a `venue-adapter` guest component until the runtime deleted the
//! extension-installed component path. The `[config]` table it booted from
//! is now [`CowConfig`], which a composition root builds directly.

use core::time::Duration;
use std::collections::BTreeSet;

use alloy_primitives::Address;
use cowprotocol::{
    ApiError, Chain, OrderCreation, OrderData, OrderKind, OrderStatus, OrderbookApiErrorType,
    QuoteAppData, QuoteRequest,
};
use futures::FutureExt;
use futures::future::BoxFuture;
use nexum_sdk::keeper::RetryAction;
use serde::Deserialize;
use url::Url;
use videre_host::{DuplicateVenue, Liveness, VenueId, VenueRegistry};
use videre_sdk::value_flow::AssetAmount;
use videre_sdk::{
    AuthScheme, IntentBody as _, IntentHeader, IntentStatus, Quotation, RateLimit, Settlement,
    SubmitOutcome, UnsignedTx, VenueError,
};

use crate::assembly;
use crate::body::{CowIntent, CowIntentBody};
use crate::classification;
use crate::order::OrderUid;
use crate::transport::{FetchError, OrderbookHttp, Transport};

/// Default per-request timeout bound.
pub const DEFAULT_TIMEOUT: Duration = nexum_sdk::http::DEFAULT_TIMEOUT;

/// The body-schema versions this venue decodes. A keeper declaring
/// `[venue] body_version` boots only when every registered venue lists it.
pub const BODY_VERSIONS: [u32; 1] = [1];

/// [`BODY_VERSIONS`] in the set shape [`VenueRegistry::register`] takes.
#[must_use]
pub fn body_versions() -> BTreeSet<u32> {
    BODY_VERSIONS.into_iter().collect()
}

/// The id the venue registers under, which is the id
/// [`CowVenue`](crate::CowVenue) routes a keeper's calls to. Registering
/// under any other id resolves to `unknown-venue` at runtime.
///
/// # Panics
///
/// Never: [`VENUE_ID`](crate::VENUE_ID) is a valid id literal.
#[must_use]
pub fn venue_id() -> VenueId {
    VenueId::new(crate::client::VENUE_ID).expect("the cow venue id is a valid id")
}

/// Register `venue` under [`venue_id`], and return its liveness flag. The
/// composition root holds the flag, one per venue.
///
/// # Errors
///
/// Returns [`DuplicateVenue`] when a live venue already claims the id.
pub fn register<T: Transport + Send + Sync + 'static>(
    registry: &VenueRegistry,
    venue: CowAdapter<T>,
) -> Result<Liveness, DuplicateVenue> {
    let liveness = Liveness::new();
    registry.register(venue_id(), liveness.clone(), body_versions(), venue)?;
    Ok(liveness)
}

/// One venue instance speaks one chain's orderbook.
#[derive(Clone, Debug)]
pub struct CowConfig {
    chain: Chain,
    base: Url,
    owner: Option<Address>,
    timeout: Duration,
}

impl CowConfig {
    /// A config for `chain`, with that chain's public orderbook, no owner,
    /// and [`DEFAULT_TIMEOUT`].
    #[must_use]
    pub fn new(chain: Chain) -> Self {
        Self {
            chain,
            base: chain.orderbook_base_url(),
            owner: None,
            timeout: DEFAULT_TIMEOUT,
        }
    }

    /// Point the venue at another orderbook, for a barn or a mock. Path
    /// joining relies on a trailing slash, so one is added when absent.
    #[must_use]
    pub fn orderbook_url(mut self, mut url: Url) -> Self {
        if !url.path().ends_with('/') {
            let path = format!("{}/", url.path());
            url.set_path(&path);
        }
        self.base = url;
        self
    }

    /// Set the owner the pre-sign path submits `from`. Without it an
    /// unsigned body is refused as [`VenueError::Unsupported`].
    #[must_use]
    pub fn owner(mut self, owner: Address) -> Self {
        self.owner = Some(owner);
        self
    }

    /// Bound every orderbook request to `timeout`. This caps the whole
    /// request, body read included; the wasi transport it replaces bounded
    /// each phase separately, so the same number is now a tighter bound.
    #[must_use]
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout.max(Duration::from_millis(1));
        self
    }

    /// The chain this venue settles on.
    #[must_use]
    pub fn chain(&self) -> Chain {
        self.chain
    }
}

/// The CoW venue behind the registry's invocation seam.
#[derive(Clone, Debug)]
pub struct CowAdapter<T = OrderbookHttp> {
    config: CowConfig,
    transport: T,
}

impl CowAdapter<OrderbookHttp> {
    /// Build the venue over a reqwest client bounded by the config timeout.
    ///
    /// # Errors
    ///
    /// Returns [`FetchError::Transport`] when the HTTP client fails to build.
    pub fn new(config: CowConfig) -> Result<Self, FetchError> {
        let transport = OrderbookHttp::new(config.timeout)?;
        Ok(Self { config, transport })
    }
}

impl<T> CowAdapter<T> {
    /// Build the venue over an explicit transport.
    pub const fn with_transport(config: CowConfig, transport: T) -> Self {
        Self { config, transport }
    }

    /// Project the body onto its header, in the SDK ontology the body
    /// codec speaks. Pure: no orderbook call, so the conformance goldens
    /// replay it without a transport.
    ///
    /// # Errors
    ///
    /// [`VenueError::InvalidBody`] when the body does not decode.
    pub fn derive_header(&self, body: &[u8]) -> Result<IntentHeader, VenueError> {
        derive_header_with(self.config.chain.id(), body)
    }
}

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
/// local derivation; a disagreement is a typed refusal.
pub(crate) async fn submit_with(
    transport: &impl Transport,
    config: &CowConfig,
    body: &[u8],
) -> Result<SubmitOutcome, VenueError> {
    match decode(body)? {
        CowIntent::Signed(signed) => {
            let order = assembly::body_to_order_data(&signed.order);
            let owner = signed.owner;
            let creation = assembly::build_order_creation(&order, &signed.signature, owner)
                .map_err(|e| VenueError::InvalidBody(e.to_string()))?;
            let uid = match post_order(transport, config, &creation).await? {
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
            let uid = match post_order(transport, config, &creation).await? {
                Posted::Accepted(uid) => reconciled_uid(uid, config, &order, owner)?,
                // Locally derived and unverified: no UID in the reply.
                Posted::AlreadyHeld => assembly::order_uid(config.chain, &order, owner),
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
    config: &CowConfig,
    order: &OrderData,
    owner: Address,
) -> Result<cowprotocol::OrderUid, VenueError> {
    let derived = assembly::order_uid(config.chain, order, owner);
    if server != derived {
        return Err(VenueError::ReceiptMismatch);
    }
    Ok(server)
}

/// Poll one receipt's orderbook lifecycle state.
pub(crate) async fn status_with(
    transport: &impl Transport,
    config: &CowConfig,
    receipt: &[u8],
) -> Result<IntentStatus, VenueError> {
    let uid = OrderUid::try_from(receipt).map_err(|_| VenueError::InvalidReceipt)?;
    let url = join(config, &format!("api/v1/orders/{uid}"))?;
    let response = call(transport, http::Method::GET, url, None).await?;
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
pub(crate) async fn quote_with(
    transport: &impl Transport,
    config: &CowConfig,
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
        transport,
        http::Method::POST,
        join(config, "api/v1/quote")?,
        Some(request),
    )
    .await?;
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

/// What a `POST /api/v1/orders` produced.
enum Posted {
    /// The orderbook accepted and assigned this UID.
    Accepted(cowprotocol::OrderUid),
    /// The orderbook already holds this exact order.
    AlreadyHeld,
}

async fn post_order(
    transport: &impl Transport,
    config: &CowConfig,
    creation: &OrderCreation,
) -> Result<Posted, VenueError> {
    let body = serde_json::to_vec(creation)
        .map_err(|e| VenueError::Unavailable(format!("order encode failed: {e}")))?;
    let response = call(
        transport,
        http::Method::POST,
        join(config, "api/v1/orders")?,
        Some(body),
    )
    .await?;
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

fn join(config: &CowConfig, path: &str) -> Result<Url, VenueError> {
    config
        .base
        .join(path)
        .map_err(|e| VenueError::Unavailable(format!("orderbook url: {e}")))
}

/// One bounded request; transport failures arrive as a typed [`VenueError`].
async fn call(
    transport: &impl Transport,
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
    Ok(transport.call(request).await?)
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

impl<T: Transport + Send + Sync> videre_host::VenueInvoker for CowAdapter<T> {
    fn derive_header<'a>(
        &'a mut self,
        body: &'a [u8],
    ) -> BoxFuture<'a, Result<wire::IntentHeader, wire::VenueError>> {
        // The free function, not the inherent method: `&mut self`
        // reborrows onto this very method if the inherent one ever goes.
        async move {
            derive_header_with(self.config.chain.id(), body)
                .map(wire::header)
                .map_err(wire::error)
        }
        .boxed()
    }

    fn quote<'a>(
        &'a mut self,
        body: &'a [u8],
    ) -> BoxFuture<'a, Result<wire::Quotation, wire::VenueError>> {
        async move {
            quote_with(&self.transport, &self.config, body)
                .await
                .map(wire::quotation)
                .map_err(wire::error)
        }
        .boxed()
    }

    fn submit<'a>(
        &'a mut self,
        body: &'a [u8],
    ) -> BoxFuture<'a, Result<wire::SubmitOutcome, wire::VenueError>> {
        async move {
            submit_with(&self.transport, &self.config, body)
                .await
                .map(wire::outcome)
                .map_err(wire::error)
        }
        .boxed()
    }

    fn status(
        &mut self,
        receipt: Vec<u8>,
    ) -> BoxFuture<'_, Result<wire::IntentStatus, wire::VenueError>> {
        async move {
            status_with(&self.transport, &self.config, &receipt)
                .await
                .map(wire::status)
                .map_err(wire::error)
        }
        .boxed()
    }

    fn cancel(&mut self, _receipt: Vec<u8>) -> BoxFuture<'_, Result<(), wire::VenueError>> {
        // Off-chain cancellation is an owner-signed request; the venue
        // structurally holds no keys.
        async { Err(wire::VenueError::Unsupported) }.boxed()
    }
}

/// Lower the SDK's intent ontology onto the host's. Both are bindgen
/// projections of `videre:types`, so every arm is a rename.
mod wire {
    use videre_sdk as sdk;

    use videre_host::bindings::{AuthScheme, RateLimit, Settlement, UnsignedTx, value_flow};
    pub use videre_host::bindings::{
        IntentHeader, IntentStatus, Quotation, SubmitOutcome, VenueError,
    };

    pub fn header(header: sdk::IntentHeader) -> IntentHeader {
        IntentHeader {
            gives: amount(header.gives),
            wants: amount(header.wants),
            settlement: Settlement {
                chain: header.settlement.chain,
            },
            authorisation: match header.authorisation {
                sdk::AuthScheme::Eip712 => AuthScheme::Eip712,
                sdk::AuthScheme::Eip1271 => AuthScheme::Eip1271,
            },
        }
    }

    pub fn quotation(quotation: sdk::Quotation) -> Quotation {
        Quotation {
            gives: amount(quotation.gives),
            wants: amount(quotation.wants),
            fee: amount(quotation.fee),
            valid_until_ms: quotation.valid_until_ms,
        }
    }

    pub fn outcome(outcome: sdk::SubmitOutcome) -> SubmitOutcome {
        match outcome {
            sdk::SubmitOutcome::Accepted(receipt) => SubmitOutcome::Accepted(receipt),
            sdk::SubmitOutcome::RequiresSigning(tx) => SubmitOutcome::RequiresSigning(UnsignedTx {
                chain: tx.chain,
                to: tx.to,
                value: tx.value,
                data: tx.data,
            }),
        }
    }

    pub fn status(status: sdk::IntentStatus) -> IntentStatus {
        match status {
            sdk::IntentStatus::Pending => IntentStatus::Pending,
            sdk::IntentStatus::Open => IntentStatus::Open,
            sdk::IntentStatus::Fulfilled => IntentStatus::Fulfilled,
            sdk::IntentStatus::Cancelled => IntentStatus::Cancelled,
            sdk::IntentStatus::Expired => IntentStatus::Expired,
        }
    }

    pub fn error(err: sdk::VenueError) -> VenueError {
        match err {
            sdk::VenueError::UnknownVenue => VenueError::UnknownVenue,
            sdk::VenueError::InvalidBody(detail) => VenueError::InvalidBody(detail),
            sdk::VenueError::Unsupported => VenueError::Unsupported,
            sdk::VenueError::Denied(detail) => VenueError::Denied(detail),
            sdk::VenueError::RateLimited(limit) => VenueError::RateLimited(RateLimit {
                retry_after_ms: limit.retry_after_ms,
            }),
            sdk::VenueError::Unavailable(detail) => VenueError::Unavailable(detail),
            sdk::VenueError::Timeout => VenueError::Timeout,
            sdk::VenueError::InvalidReceipt => VenueError::InvalidReceipt,
            sdk::VenueError::ReceiptMismatch => VenueError::ReceiptMismatch,
        }
    }

    fn amount(amount: sdk::value_flow::AssetAmount) -> value_flow::AssetAmount {
        value_flow::AssetAmount {
            asset: match amount.asset {
                sdk::value_flow::Asset::Native => value_flow::Asset::Native,
                sdk::value_flow::Asset::Erc20(erc20) => {
                    value_flow::Asset::Erc20(value_flow::Erc20 { token: erc20.token })
                }
            },
            amount: amount.amount,
        }
    }
}

#[cfg(test)]
mod tests {
    use alloy_primitives::U256;
    use futures::executor::block_on;
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

    fn config() -> CowConfig {
        CowConfig::new(Chain::try_from(SEPOLIA).expect("sepolia is supported"))
            .orderbook_url(Url::parse("https://orderbook.test/").expect("test url parses"))
            .timeout(Duration::from_secs(5))
    }

    fn with_owner(owner: Address) -> CowConfig {
        config().owner(owner)
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

    fn expected_uid(config: &CowConfig) -> cowprotocol::OrderUid {
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

    /// Run a body through submit over `fetch`, in the shape the tests read.
    fn submit(
        fetch: &MockFetch,
        config: &CowConfig,
        body: &[u8],
    ) -> Result<SubmitOutcome, VenueError> {
        block_on(submit_with(fetch, config, body))
    }

    fn status(
        fetch: &MockFetch,
        config: &CowConfig,
        receipt: &[u8],
    ) -> Result<IntentStatus, VenueError> {
        block_on(status_with(fetch, config, receipt))
    }

    fn quote(fetch: &MockFetch, config: &CowConfig, body: &[u8]) -> Result<Quotation, VenueError> {
        block_on(quote_with(fetch, config, body))
    }

    #[test]
    fn config_defaults_resolve_from_the_chain() {
        let parsed = CowConfig::new(Chain::Mainnet);
        assert_eq!(parsed.chain.id(), 1);
        assert_eq!(parsed.base.as_str(), "https://api.cow.fi/mainnet/");
        assert_eq!(parsed.owner, None);
        assert_eq!(parsed.timeout, DEFAULT_TIMEOUT);
    }

    #[test]
    fn config_overrides_apply_and_the_base_gains_its_slash() {
        let parsed = CowConfig::new(Chain::try_from(SEPOLIA).expect("sepolia is supported"))
            .orderbook_url(Url::parse("https://barn.test/sepolia").expect("test url parses"))
            .owner(owner())
            .timeout(Duration::from_millis(1500));
        assert_eq!(parsed.base.as_str(), "https://barn.test/sepolia/");
        assert_eq!(parsed.owner, Some(owner()));
        assert_eq!(parsed.timeout, Duration::from_millis(1500));
    }

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

    #[test]
    fn signed_submit_posts_eip1271_and_returns_the_uid_receipt() {
        let config = config();
        let uid = expected_uid(&config);
        let fetch = MockFetch::default();
        fetch.respond_to(http::Method::POST, ORDERS, 201, format!("\"{uid}\""));

        let outcome = submit(&fetch, &config, &signed_bytes()).expect("accepted");
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

        let outcome = submit(&fetch, &config, &signed_bytes()).expect("held is success");
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
            submit(&fetch, &config, &signed_bytes()),
            Err(VenueError::ReceiptMismatch)
        ));
    }

    #[test]
    fn unsigned_submit_requires_signing_the_presign_call() {
        let config = with_owner(owner());
        let uid = expected_uid(&config);
        let fetch = MockFetch::default();
        fetch.respond_to(http::Method::POST, ORDERS, 201, format!("\"{uid}\""));

        let outcome = submit(&fetch, &config, &order_bytes()).expect("accepted");
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
            submit(&fetch, &config(), &order_bytes()),
            Err(VenueError::Unsupported)
        ));
        assert_eq!(fetch.request_count(), 0);
    }

    #[test]
    fn rejections_project_through_the_classification_table() {
        let config = config();
        let fetch = MockFetch::default();

        reject(&fetch, "InvalidSignature");
        assert!(matches!(
            submit(&fetch, &config, &signed_bytes()),
            Err(VenueError::Denied(detail)) if detail.contains("InvalidSignature")
        ));

        // A drop-on-repeat row stays `denied` on the wire; the
        // errorType prefix carries the one-shot grace to the client.
        reject(&fetch, "InvalidEip1271Signature");
        assert!(matches!(
            submit(&fetch, &config, &signed_bytes()),
            Err(VenueError::Denied(detail))
                if detail.starts_with("InvalidEip1271Signature:")
        ));

        reject(&fetch, "TooManyLimitOrders");
        assert!(matches!(
            submit(&fetch, &config, &signed_bytes()),
            Err(VenueError::RateLimited(rl)) if rl.retry_after_ms == Some(30_000)
        ));

        reject(&fetch, "InsufficientFee");
        assert!(matches!(
            submit(&fetch, &config, &signed_bytes()),
            Err(VenueError::Unavailable(detail)) if detail.contains("InsufficientFee")
        ));
    }

    #[test]
    fn transport_shapes_stay_typed() {
        let config = config();
        let fetch = MockFetch::default();

        fetch.respond_to(http::Method::POST, ORDERS, 429, "slow down");
        assert!(matches!(
            submit(&fetch, &config, &signed_bytes()),
            Err(VenueError::RateLimited(rl)) if rl.retry_after_ms.is_none()
        ));

        fetch.respond_to(http::Method::POST, ORDERS, 503, "maintenance");
        assert!(matches!(
            submit(&fetch, &config, &signed_bytes()),
            Err(VenueError::Unavailable(_))
        ));

        fetch.fail_with(
            http::Method::POST,
            ORDERS,
            FetchError::Timeout("first byte".to_owned()),
        );
        assert!(matches!(
            submit(&fetch, &config, &signed_bytes()),
            Err(VenueError::Timeout)
        ));

        fetch.fail_with(http::Method::POST, ORDERS, FetchError::Denied);
        assert!(matches!(
            submit(&fetch, &config, &signed_bytes()),
            Err(VenueError::Denied(_))
        ));
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

    fn status_url(uid: &OrderUid) -> String {
        format!("https://orderbook.test/api/v1/orders/{uid}")
    }

    #[test]
    fn status_maps_the_orderbook_lifecycle() {
        let config = config();
        let uid = OrderUid([0xAB; 56]);
        let fetch = MockFetch::default();
        for (wire, expected) in [
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
                status(&fetch, &config, uid.as_bytes()).expect("known status"),
                expected,
            );
        }
    }

    #[test]
    fn status_refuses_a_short_receipt_and_retries_not_found() {
        let config = config();
        let fetch = MockFetch::default();
        assert!(matches!(
            status(&fetch, &config, &[0xAB; 3]),
            Err(VenueError::InvalidReceipt)
        ));

        let uid = OrderUid([0xAB; 56]);
        fetch.respond_to(http::Method::GET, status_url(&uid), 404, "not found");
        assert!(matches!(
            status(&fetch, &config, uid.as_bytes()),
            Err(VenueError::Unavailable(_))
        ));
    }

    #[test]
    fn quote_prices_the_body_and_pins_its_terms() {
        let config = config();
        let fetch = MockFetch::default();
        let quoted = serde_json::json!({
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
        fetch.respond_to(http::Method::POST, QUOTE, 200, quoted.to_string());

        let quotation = quote(&fetch, &config, &signed_bytes()).expect("quoted");
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
            quote(&fetch, &config(), &order_bytes()),
            Err(VenueError::Unsupported)
        ));
        assert_eq!(fetch.request_count(), 0);
    }

    /// One canned reply. `MockFetch` is `RefCell`-backed and so not `Sync`;
    /// the registry needs a `Sync` transport, so the registered-venue tests
    /// answer from here instead.
    struct Canned {
        status: u16,
        body: Vec<u8>,
    }

    impl Transport for Canned {
        fn call(
            &self,
            _request: http::Request<Vec<u8>>,
        ) -> impl Future<Output = Result<http::Response<Vec<u8>>, FetchError>> + Send {
            let response = http::Response::builder()
                .status(self.status)
                .body(self.body.clone())
                .expect("test response builds");
            core::future::ready(Ok(response))
        }
    }

    /// A venue over a canned accept of the signed body's own UID.
    fn accepting_venue() -> (CowAdapter<Canned>, cowprotocol::OrderUid) {
        let config = with_owner(owner());
        let uid = expected_uid(&config);
        let canned = Canned {
            status: 201,
            body: format!("\"{uid}\"").into_bytes(),
        };
        (CowAdapter::with_transport(config, canned), uid)
    }

    /// The `VenueInvoker` face answers in the host ontology.
    #[tokio::test]
    async fn the_invoker_face_lowers_onto_the_host_ontology() {
        use videre_host::VenueInvoker;
        use videre_host::bindings as wire;

        let (mut venue, uid) = accepting_venue();
        let header = VenueInvoker::derive_header(&mut venue, &signed_bytes())
            .await
            .expect("valid body");
        assert_eq!(header.settlement.chain, SEPOLIA);
        assert_eq!(header.authorisation, wire::AuthScheme::Eip1271);

        let outcome = VenueInvoker::submit(&mut venue, &signed_bytes())
            .await
            .expect("accepted");
        assert_eq!(
            outcome,
            wire::SubmitOutcome::Accepted(uid.as_slice().to_vec()),
        );

        assert_eq!(
            venue.cancel(uid.as_slice().to_vec()).await,
            Err(wire::VenueError::Unsupported),
            "the venue holds no keys, so cancellation stays unsupported",
        );
    }

    #[test]
    fn the_venue_declares_body_version_one() {
        // Pinned to the literal the keeper handshake reads. The manifest
        // `[venue] body_versions` used to be the install-time authority;
        // this is what is left of that gate.
        assert_eq!(body_versions(), BTreeSet::from([1]));
    }

    #[test]
    fn the_registered_id_is_the_one_the_keeper_client_routes_to() {
        assert_eq!(venue_id().as_str(), crate::client::VENUE_ID);
        assert_eq!(crate::client::VENUE_ID, "cow");
    }

    /// The configured timeout reaches the wire, not just the config struct.
    #[tokio::test]
    async fn a_slow_orderbook_trips_the_configured_timeout() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(201).set_delay(Duration::from_secs(5)))
            .mount(&server)
            .await;

        let config = config()
            .orderbook_url(Url::parse(&server.uri()).expect("the mock uri parses"))
            .timeout(Duration::from_millis(50));
        let venue = CowAdapter::new(config).expect("the http client builds");

        let outcome = submit_with(&venue.transport, &venue.config, &signed_bytes()).await;
        assert!(
            matches!(outcome, Err(VenueError::Timeout)),
            "a 50ms bound must trip on a 5s reply, got {outcome:?}",
        );
    }

    /// The registered venue is routable and publishes its body versions.
    #[tokio::test]
    async fn a_registered_cow_venue_is_routable() {
        use videre_host::{SubmitQuota, VenueRegistryBuilder};

        let registry = VenueRegistryBuilder::new(SubmitQuota::default()).build();
        let id = venue_id();
        let (venue, uid) = accepting_venue();

        let liveness = register(&registry, venue).expect("first registration");
        assert!(liveness.is_alive());
        assert_eq!(registry.body_versions().get(&id), Some(&body_versions()));

        let outcome = registry
            .submit("ccow-monitor", &id, signed_bytes())
            .await
            .expect("the registry routes to the cow venue");
        assert_eq!(
            outcome,
            videre_host::bindings::SubmitOutcome::Accepted(uid.as_slice().to_vec()),
        );
    }

    /// The shared compliance fixture: one owner-configured config drives
    /// both auth paths.
    struct CowReconcile;

    impl CowReconcile {
        fn cfg() -> CowConfig {
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
            super::tests::submit(fetch, &Self::cfg(), body).map_err(VenueFault::from)
        }

        fn status(fetch: &MockFetch, receipt: &[u8]) -> Result<IntentStatus, VenueFault> {
            super::tests::status(fetch, &Self::cfg(), receipt).map_err(VenueFault::from)
        }
    }

    videre_test::venue_reconcile_compliance!(CowReconcile);
}
