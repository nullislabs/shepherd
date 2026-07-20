//! The venue registry: the keeper-facing `videre:venue/client` import
//! resolved to installed venue adapters.
//!
//! A module's `client::submit(venue, body)` reaches the host here. The
//! registry resolves the venue id to the one installed adapter that answers
//! for it, then drives a fixed sequence against that adapter: derive the
//! header, run the guard interposition seam on it (advisory-only for now:
//! see [`EgressGuard`]), and only then submit.
//! Status and cancel are pass-throughs; they are not submissions, so they
//! skip the header, the guard, and the quota.
//!
//! Invocation is serialised per adapter. A wasmtime `Store` is not `Sync`,
//! so each adapter sits behind its own async mutex: concurrent client calls
//! to the same venue queue on that mutex, while calls to different venues run
//! in parallel. The lock is held across the guest await, which is the whole
//! point - it is the actor boundary that keeps one adapter store
//! single-threaded.
//!
//! Fuel cannot cross stores, so a module that spams undecodable bodies would
//! otherwise burn an adapter's budget for free. Two mechanisms close that:
//! a per-caller quota gates every quote and submit before the adapter is
//! touched, and a decode failure (the adapter's `invalid-body`) is charged
//! to the calling module's quota, so a caller feeding garbage exhausts its
//! own budget rather than the adapter's.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::future::BoxFuture;
use nexum_status_body::StatusBody;
use tokio::sync::Mutex as AsyncMutex;
use tracing::warn;
use wasmtime::Store;

use crate::bindings::{
    IntentHeader, IntentStatus, IntentStatusUpdate, Quotation, RateLimit, SubmitOutcome,
    VenueAdapter, VenueError,
};
use crate::host::component::RuntimeTypes;
use crate::host::state::HostState;

/// Default per-caller submission budget within [`DEFAULT_QUOTA_WINDOW`].
pub const DEFAULT_QUOTA_MAX_CHARGES: u32 = 256;
/// Default sliding window the per-caller submission budget is counted over.
pub const DEFAULT_QUOTA_WINDOW: Duration = Duration::from_secs(60);

/// Venue identifier: the id an adapter registers under and a submission
/// names. Opaque beyond equality.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct VenueId(String);

impl VenueId {
    /// The id at its wire spelling.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for VenueId {
    fn from(id: String) -> Self {
        Self(id)
    }
}

impl From<&str> for VenueId {
    fn from(id: &str) -> Self {
        Self(id.to_owned())
    }
}

impl fmt::Display for VenueId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Per-caller submission quota. Both a forwarded submission and a charged
/// decode failure consume one unit; the window slides so a caller's budget
/// refills as old charges age out.
#[derive(Debug, Clone, Copy)]
pub struct SubmitQuota {
    /// Maximum charges a single caller may accrue within `window`.
    pub max_charges: u32,
    /// Sliding window the charges are counted across.
    pub window: Duration,
}

impl SubmitQuota {
    /// Pair a budget with the window it is counted over.
    pub const fn new(max_charges: u32, window: Duration) -> Self {
        Self {
            max_charges,
            window,
        }
    }
}

impl Default for SubmitQuota {
    fn default() -> Self {
        Self::new(DEFAULT_QUOTA_MAX_CHARGES, DEFAULT_QUOTA_WINDOW)
    }
}

/// The guard interposition seam. The registry runs this on the
/// adapter-derived header after `derive-header` and before `submit`.
///
/// Advisory-only: the checkpoint is not yet enforcing. A `Deny` verdict is
/// logged as a would-deny and the submission proceeds. The shipped policy is
/// the unit guard, which allows every egress; the egress-guard epic installs
/// the real facts-plus-analysers pipeline and turns the verdict enforcing,
/// without the registry changing shape.
pub trait EgressGuard: Send + Sync {
    /// Decide whether the derived header may proceed to the adapter's submit.
    fn check(&self, ctx: &GuardContext<'_>) -> GuardVerdict;
}

/// The unit guard: allow every egress.
impl EgressGuard for () {
    fn check(&self, _ctx: &GuardContext<'_>) -> GuardVerdict {
        GuardVerdict::Allow
    }
}

/// What the guard sees: who is submitting, to which venue, and the header the
/// adapter derived from the opaque body. The header is the stable ontology
/// policy has teeth on; the raw body never reaches the guard.
pub struct GuardContext<'a> {
    /// Namespace of the calling module.
    pub caller: &'a str,
    /// Venue the submission is routed to.
    pub venue: &'a VenueId,
    /// Adapter-derived header for the body.
    pub header: &'a IntentHeader,
}

/// The guard's decision on one egress.
pub enum GuardVerdict {
    /// Forward the submission to the adapter.
    Allow,
    /// Refuse the egress with an operator-facing reason. Logged, not
    /// enforced, while the seam is advisory-only.
    Deny(String),
}

/// The per-adapter invocation seam. One installed adapter answers for exactly
/// one venue; the registry owns the adapter's `Store` behind an async mutex
/// and reaches it only through this trait, so the registry's sequencing and
/// quota logic is testable against a stub that never spins up a wasmtime
/// store.
///
/// The futures are boxed so the registry can hold heterogeneous adapters
/// behind one `dyn` slot without the whole registry turning generic over an
/// adapter type it never names.
pub trait VenueInvoker: Send {
    /// Project the opaque body onto the stable header the guard runs on.
    fn derive_header<'a>(
        &'a mut self,
        body: &'a [u8],
    ) -> BoxFuture<'a, Result<IntentHeader, VenueError>>;

    /// Price the opaque body at this adapter's venue.
    fn quote<'a>(&'a mut self, body: &'a [u8]) -> BoxFuture<'a, Result<Quotation, VenueError>>;

    /// Submit the opaque body to this adapter's venue.
    fn submit<'a>(&'a mut self, body: &'a [u8])
    -> BoxFuture<'a, Result<SubmitOutcome, VenueError>>;

    /// Report where a previously submitted intent is in its life. The receipt
    /// is owned: it is used once, unlike the body a submission re-decodes.
    fn status(&mut self, receipt: Vec<u8>) -> BoxFuture<'_, Result<IntentStatus, VenueError>>;

    /// Ask the venue to withdraw an intent.
    fn cancel(&mut self, receipt: Vec<u8>) -> BoxFuture<'_, Result<(), VenueError>>;
}

/// The live adapter: a supervised wasmtime `Store` plus the `venue-adapter`
/// bindings, refuelled before each guest call. A trap is projected onto
/// `unavailable` rather than propagated: a misbehaving adapter must not be
/// the caller's fault, and it must not unwind through the registry into the
/// calling module's store.
pub struct VenueActor<T: RuntimeTypes> {
    store: Store<HostState<T>>,
    bindings: VenueAdapter,
    fuel_per_call: u64,
}

impl<T: RuntimeTypes> VenueActor<T> {
    /// Wrap an instantiated adapter store for routing.
    pub fn new(store: Store<HostState<T>>, bindings: VenueAdapter, fuel_per_call: u64) -> Self {
        Self {
            store,
            bindings,
            fuel_per_call,
        }
    }

    /// Refuel the store before a guest call so each invocation starts from a
    /// full budget, mirroring the supervisor's per-event refuel.
    fn refuel(&mut self) -> Result<(), VenueError> {
        self.store
            .set_fuel(self.fuel_per_call)
            .map_err(|e| VenueError::Unavailable(format!("adapter refuel failed: {e}")))
    }
}

/// Project a wasmtime trap into the venue-error space. The root cause is
/// carried so an operator sees why the adapter died without the wasm frame
/// list leaking to the calling module.
fn trap_to_venue_error(trap: wasmtime::Error) -> VenueError {
    VenueError::Unavailable(format!("adapter trapped: {}", trap.root_cause()))
}

impl<T: RuntimeTypes> VenueInvoker for VenueActor<T> {
    fn derive_header<'a>(
        &'a mut self,
        body: &'a [u8],
    ) -> BoxFuture<'a, Result<IntentHeader, VenueError>> {
        Box::pin(async move {
            self.refuel()?;
            match self
                .bindings
                .videre_venue_adapter()
                .call_derive_header(&mut self.store, body)
                .await
            {
                Ok(res) => res,
                Err(trap) => Err(trap_to_venue_error(trap)),
            }
        })
    }

    fn quote<'a>(&'a mut self, body: &'a [u8]) -> BoxFuture<'a, Result<Quotation, VenueError>> {
        Box::pin(async move {
            self.refuel()?;
            match self
                .bindings
                .videre_venue_adapter()
                .call_quote(&mut self.store, body)
                .await
            {
                Ok(res) => res,
                Err(trap) => Err(trap_to_venue_error(trap)),
            }
        })
    }

    fn submit<'a>(
        &'a mut self,
        body: &'a [u8],
    ) -> BoxFuture<'a, Result<SubmitOutcome, VenueError>> {
        Box::pin(async move {
            self.refuel()?;
            match self
                .bindings
                .videre_venue_adapter()
                .call_submit(&mut self.store, body)
                .await
            {
                Ok(res) => res,
                Err(trap) => Err(trap_to_venue_error(trap)),
            }
        })
    }

    fn status(&mut self, receipt: Vec<u8>) -> BoxFuture<'_, Result<IntentStatus, VenueError>> {
        Box::pin(async move {
            self.refuel()?;
            match self
                .bindings
                .videre_venue_adapter()
                .call_status(&mut self.store, &receipt)
                .await
            {
                Ok(res) => res,
                Err(trap) => Err(trap_to_venue_error(trap)),
            }
        })
    }

    fn cancel(&mut self, receipt: Vec<u8>) -> BoxFuture<'_, Result<(), VenueError>> {
        Box::pin(async move {
            self.refuel()?;
            match self
                .bindings
                .videre_venue_adapter()
                .call_cancel(&mut self.store, &receipt)
                .await
            {
                Ok(res) => res,
                Err(trap) => Err(trap_to_venue_error(trap)),
            }
        })
    }
}

/// One installed adapter behind its serialising mutex.
type AdapterSlot = Arc<AsyncMutex<dyn VenueInvoker>>;

/// Per-caller charge history, pruned to the quota window on each touch.
#[derive(Default)]
struct QuotaLedger {
    per_caller: HashMap<String, VecDeque<Instant>>,
}

/// One receipt the registry polls for status transitions. `last` starts
/// `None` so the first successful poll always reports, giving a
/// subscriber the intent's current state without waiting for a change.
struct WatchedIntent {
    venue: VenueId,
    receipt: Vec<u8>,
    last: Option<IntentStatus>,
}

/// A polled status is terminal when the intent can never change again:
/// the registry stops watching the receipt after reporting it.
fn is_terminal(status: IntentStatus) -> bool {
    matches!(
        status,
        IntentStatus::Fulfilled | IntentStatus::Cancelled | IntentStatus::Expired
    )
}

/// Lower a polled status onto the opaque status body the host `event`
/// stream carries. The registry attests the lifecycle state alone; proof
/// and failure reason ride the body only when the venue supplies them.
fn status_body(status: IntentStatus) -> StatusBody {
    use nexum_status_body::IntentStatus as Lifecycle;

    let status = match status {
        IntentStatus::Pending => Lifecycle::Pending,
        IntentStatus::Open => Lifecycle::Open,
        IntentStatus::Fulfilled => Lifecycle::Fulfilled,
        IntentStatus::Cancelled => Lifecycle::Cancelled,
        IntentStatus::Expired => Lifecycle::Expired,
    };
    StatusBody {
        status,
        proof: None,
        reason: None,
    }
}

/// The shared registry state. Cloning a [`VenueRegistry`] is an `Arc` bump;
/// every module store carries the same handle, so a submission from any
/// module reaches the same adapters and the same quota ledger.
struct VenueRegistryInner {
    adapters: HashMap<VenueId, AdapterSlot>,
    guard: Arc<dyn EgressGuard>,
    quota: SubmitQuota,
    ledger: Mutex<QuotaLedger>,
    /// Receipts under status watch, appended by accepted submissions and
    /// pruned as they reach a terminal status.
    watched: Mutex<Vec<WatchedIntent>>,
}

/// The keeper-facing venue registry, cheap to clone and shared across every
/// module store.
#[derive(Clone)]
pub struct VenueRegistry {
    inner: Arc<VenueRegistryInner>,
}

impl VenueRegistry {
    /// An empty registry: no adapters, the unit guard, the default quota.
    /// This is what an adapter store (which cannot call the client face) and
    /// the single-module `just run` path carry.
    pub fn empty() -> Self {
        VenueRegistryBuilder::new(SubmitQuota::default()).build()
    }

    /// Resolve a venue id to its installed adapter slot.
    fn resolve(&self, venue: &VenueId) -> Result<AdapterSlot, VenueError> {
        self.inner
            .adapters
            .get(venue)
            .cloned()
            .ok_or(VenueError::UnknownVenue)
    }

    /// Whether `caller` has budget left in the current window. Read-only: it
    /// prunes aged charges but does not record one.
    fn quota_admits(&self, caller: &str) -> bool {
        let mut ledger = self.inner.ledger.lock().expect("quota ledger poisoned");
        let history = ledger.per_caller.entry(caller.to_owned()).or_default();
        prune(history, self.inner.quota.window);
        (history.len() as u32) < self.inner.quota.max_charges
    }

    /// Record one charge against `caller`'s budget.
    fn charge(&self, caller: &str) {
        let mut ledger = self.inner.ledger.lock().expect("quota ledger poisoned");
        let history = ledger.per_caller.entry(caller.to_owned()).or_default();
        prune(history, self.inner.quota.window);
        history.push_back(Instant::now());
    }

    /// Submit an opaque body to `venue` on behalf of `caller`: resolve the
    /// adapter, gate on the caller's quota, derive the header, run the guard
    /// seam (advisory-only: a deny logs and the submission proceeds), then
    /// forward to the adapter. A decode failure is charged to the
    /// caller before returning, so a caller feeding garbage exhausts its own
    /// budget and is stopped at the gate on the next call rather than
    /// re-invoking the adapter.
    ///
    /// Charging is deliberately asymmetric across the two stages. Once the
    /// guard admits the header the submission is charged before the adapter
    /// call, so a forwarded submission spends one unit regardless of the
    /// venue's outcome (the adapter did the work, and a transient venue
    /// outage must not become a free retry loop). A derive-stage venue error
    /// that is not a decode failure is the venue's fault, not the caller's,
    /// so it is left uncharged and the caller may retry.
    pub async fn submit(
        &self,
        caller: &str,
        venue: &VenueId,
        body: Vec<u8>,
    ) -> Result<SubmitOutcome, VenueError> {
        let slot = self.resolve(venue)?;
        // Gate before touching the adapter so a quota-exhausted caller never
        // reaches the adapter store or its mutex. Exhaustion is retryable
        // once the window slides, so it is rate-limited, never denied.
        if !self.quota_admits(caller) {
            return Err(VenueError::RateLimited(RateLimit {
                retry_after_ms: Some(window_ms(self.inner.quota.window)),
            }));
        }
        let mut adapter = slot.lock().await;
        let header = match adapter.derive_header(&body).await {
            Ok(header) => header,
            Err(e) => {
                // Charge decode failures to the caller before the adapter is
                // invoked again; other venue errors are not the caller's fault.
                if matches!(e, VenueError::InvalidBody(_)) {
                    self.charge(caller);
                }
                return Err(e);
            }
        };
        let ctx = GuardContext {
            caller,
            venue,
            header: &header,
        };
        // Advisory-only checkpoint: a deny is logged, never enforced.
        if let GuardVerdict::Deny(reason) = self.inner.guard.check(&ctx) {
            warn!(
                caller,
                venue = %venue,
                reason,
                "egress guard would deny - advisory-only, submission proceeds",
            );
        }
        // A forwarded submission consumes one unit of the caller's budget.
        self.charge(caller);
        let outcome = adapter.submit(&body).await?;
        // An accepted receipt goes under status watch so subscribers see
        // its transitions; requires-signing has no receipt to watch yet.
        if let SubmitOutcome::Accepted(receipt) = &outcome {
            self.watch(venue, receipt.clone());
        }
        Ok(outcome)
    }

    /// Price an opaque body at `venue` on behalf of `caller`. Not a
    /// submission, so the header and guard are skipped (a quotation moves
    /// no value), but it is adapter work on a caller-supplied body: the
    /// caller's quota gates it and every quote spends one unit, so a
    /// quote spammer exhausts its own budget, not the adapter's.
    pub async fn quote(
        &self,
        caller: &str,
        venue: &VenueId,
        body: Vec<u8>,
    ) -> Result<Quotation, VenueError> {
        let slot = self.resolve(venue)?;
        if !self.quota_admits(caller) {
            return Err(VenueError::RateLimited(RateLimit {
                retry_after_ms: Some(window_ms(self.inner.quota.window)),
            }));
        }
        self.charge(caller);
        let mut adapter = slot.lock().await;
        adapter.quote(&body).await
    }

    /// Put a `(venue, receipt)` pair under status watch. Idempotent: a
    /// re-submitted receipt keeps its existing watch entry.
    fn watch(&self, venue: &VenueId, receipt: Vec<u8>) {
        let mut watched = self.inner.watched.lock().expect("watch list poisoned");
        if watched
            .iter()
            .any(|w| w.venue == *venue && w.receipt == receipt)
        {
            return;
        }
        watched.push(WatchedIntent {
            venue: venue.clone(),
            receipt,
            last: None,
        });
    }

    /// Number of receipts currently under status watch.
    pub fn watched_count(&self) -> usize {
        self.inner
            .watched
            .lock()
            .expect("watch list poisoned")
            .len()
    }

    /// Poll every watched receipt against its adapter's status export and
    /// return the transitions: statuses that differ from the last one
    /// reported for that receipt (the first successful poll always
    /// reports). A terminal status is reported once and the receipt is
    /// dropped from the watch; a failure leaves the entry untouched for
    /// the next cadence.
    pub async fn poll_status_transitions(&self) -> Vec<IntentStatusUpdate> {
        // Snapshot so the std mutex is never held across the guest await.
        let snapshot: Vec<(VenueId, Vec<u8>)> = {
            let watched = self.inner.watched.lock().expect("watch list poisoned");
            watched
                .iter()
                .map(|w| (w.venue.clone(), w.receipt.clone()))
                .collect()
        };
        let mut updates = Vec::new();
        for (venue, receipt) in snapshot {
            // Installed adapters never leave the registry, so a resolve
            // failure here is unreachable; skip defensively regardless.
            let Ok(slot) = self.resolve(&venue) else {
                continue;
            };
            let polled = {
                let mut adapter = slot.lock().await;
                adapter.status(receipt.clone()).await
            };
            match polled {
                Ok(status) => {
                    if let Some(update) = self.record_polled_status(&venue, &receipt, status) {
                        updates.push(update);
                    }
                }
                Err(err) => {
                    warn!(
                        venue = %venue,
                        error = ?err,
                        "status poll failed - retrying on the next cadence",
                    );
                }
            }
        }
        updates
    }

    /// Fold one polled status into the watch entry: `Some(update)` when it
    /// differs from the last reported status, pruning the entry when the
    /// status is terminal. `None` also covers an entry that disappeared
    /// while the poll was in flight, and an update whose status body
    /// failed to encode (the entry is left untouched for the next
    /// cadence).
    fn record_polled_status(
        &self,
        venue: &VenueId,
        receipt: &[u8],
        status: IntentStatus,
    ) -> Option<IntentStatusUpdate> {
        let mut watched = self.inner.watched.lock().expect("watch list poisoned");
        let pos = watched
            .iter()
            .position(|w| w.venue == *venue && w.receipt == receipt)?;
        let changed = watched[pos].last != Some(status);
        let update = if changed {
            match status_body(status).encode() {
                Ok(body) => Some(IntentStatusUpdate {
                    venue: venue.as_str().to_owned(),
                    receipt: receipt.to_vec(),
                    status: body,
                }),
                Err(err) => {
                    warn!(
                        venue = %venue,
                        error = %err,
                        "status body failed to encode - retrying on the next cadence",
                    );
                    return None;
                }
            }
        } else {
            None
        };
        if is_terminal(status) {
            watched.remove(pos);
        } else {
            watched[pos].last = Some(status);
        }
        update
    }

    /// Report where a previously submitted intent is in its life. Not a
    /// submission: no header, no guard, no quota, just the serialised call.
    pub async fn status(
        &self,
        venue: &VenueId,
        receipt: Vec<u8>,
    ) -> Result<IntentStatus, VenueError> {
        let slot = self.resolve(venue)?;
        let mut adapter = slot.lock().await;
        adapter.status(receipt).await
    }

    /// Ask the venue to withdraw an intent. Not a submission, so it skips the
    /// header, guard, and quota like `status`.
    pub async fn cancel(&self, venue: &VenueId, receipt: Vec<u8>) -> Result<(), VenueError> {
        let slot = self.resolve(venue)?;
        let mut adapter = slot.lock().await;
        adapter.cancel(receipt).await
    }

    /// Number of installed, routable adapters.
    pub fn venue_count(&self) -> usize {
        self.inner.adapters.len()
    }
}

/// A quota window as whole milliseconds, saturating at `u64::MAX`.
fn window_ms(window: Duration) -> u64 {
    u64::try_from(window.as_millis()).unwrap_or(u64::MAX)
}

/// Drop charge timestamps that have aged out of the window.
fn prune(history: &mut VecDeque<Instant>, window: Duration) {
    let now = Instant::now();
    while let Some(&front) = history.front() {
        if now.duration_since(front) > window {
            history.pop_front();
        } else {
            break;
        }
    }
}

/// Assembles a [`VenueRegistry`]: adapters install first (at supervisor
/// boot, before any module store carries the built registry), then the
/// registry freezes. The guard defaults to the unit guard; the egress-guard
/// epic overrides it here.
pub struct VenueRegistryBuilder {
    adapters: HashMap<VenueId, AdapterSlot>,
    guard: Arc<dyn EgressGuard>,
    quota: SubmitQuota,
}

impl VenueRegistryBuilder {
    /// Start an empty builder with the given quota and the unit guard.
    pub fn new(quota: SubmitQuota) -> Self {
        Self {
            adapters: HashMap::new(),
            guard: Arc::new(()),
            quota,
        }
    }

    /// Override the guard policy. The egress-guard epic wires the real
    /// pipeline through here; tests inject a denying policy to prove the
    /// advisory seam.
    pub fn with_guard(mut self, guard: Arc<dyn EgressGuard>) -> Self {
        self.guard = guard;
        self
    }

    /// Install an adapter under its venue id. Rejects a duplicate id: two
    /// adapters answering the same venue would silently shadow one another,
    /// which is a config error worth failing boot over.
    pub fn install(
        &mut self,
        venue: VenueId,
        invoker: impl VenueInvoker + 'static,
    ) -> Result<(), DuplicateVenue> {
        if self.adapters.contains_key(&venue) {
            return Err(DuplicateVenue { venue });
        }
        self.adapters
            .insert(venue, Arc::new(AsyncMutex::new(invoker)));
        Ok(())
    }

    /// Freeze the builder into a shared registry.
    pub fn build(self) -> VenueRegistry {
        if self.quota.max_charges == 0 {
            // A zero budget would refuse every submission; saturate up to one
            // so a misconfigured quota still admits a single submission rather
            // than bricking every venue. Mirrors the poison-policy clamp.
            warn!("submission quota max_charges is 0; clamping to 1");
        }
        let quota = SubmitQuota::new(self.quota.max_charges.max(1), self.quota.window);
        VenueRegistry {
            inner: Arc::new(VenueRegistryInner {
                adapters: self.adapters,
                guard: self.guard,
                quota,
                ledger: Mutex::new(QuotaLedger::default()),
                watched: Mutex::new(Vec::new()),
            }),
        }
    }
}

/// Two installed adapters claimed the same venue id.
#[derive(Debug, thiserror::Error)]
#[error("venue id {venue} is claimed by more than one installed adapter")]
pub struct DuplicateVenue {
    /// The colliding venue id.
    pub venue: VenueId,
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use nexum_status_body::IntentStatus as Lifecycle;

    use crate::bindings::value_flow::{Asset, AssetAmount};
    use crate::bindings::{AuthScheme, IntentHeader, Settlement, UnsignedTx};

    use super::*;

    /// The venue id every test installs its stub adapter under.
    fn cow() -> VenueId {
        VenueId::from("cow")
    }

    /// Decode an update's opaque status body.
    fn decoded(update: &IntentStatusUpdate) -> StatusBody {
        StatusBody::decode(&update.status).expect("status body decodes")
    }

    /// A body carrying a bare lifecycle state.
    fn plain(status: Lifecycle) -> StatusBody {
        StatusBody {
            status,
            proof: None,
            reason: None,
        }
    }

    /// A programmable adapter that records call counts and returns canned
    /// outcomes, so the registry's sequencing, guard seam, and quota are
    /// tested without a wasmtime store.
    #[derive(Default)]
    struct StubCalls {
        derive: AtomicUsize,
        quote: AtomicUsize,
        submit: AtomicUsize,
        status: AtomicUsize,
        cancel: AtomicUsize,
        /// Highest number of overlapping invocations observed; proves the
        /// per-adapter mutex serialises access.
        max_concurrency: AtomicUsize,
        live: AtomicUsize,
    }

    struct StubAdapter {
        calls: Arc<StubCalls>,
        derive: Result<IntentHeader, VenueError>,
        submit: Result<SubmitOutcome, VenueError>,
        /// Statuses served front-first by consecutive `status` calls;
        /// once drained, every further call reports `open`.
        status_script: VecDeque<Result<IntentStatus, VenueError>>,
    }

    impl StubAdapter {
        fn new(calls: Arc<StubCalls>) -> Self {
            Self {
                calls,
                derive: Ok(header()),
                submit: Ok(SubmitOutcome::Accepted(b"receipt".to_vec())),
                status_script: VecDeque::new(),
            }
        }

        fn with_derive(mut self, derive: Result<IntentHeader, VenueError>) -> Self {
            self.derive = derive;
            self
        }

        fn with_submit(mut self, submit: Result<SubmitOutcome, VenueError>) -> Self {
            self.submit = submit;
            self
        }

        fn with_status_script(
            mut self,
            script: impl IntoIterator<Item = Result<IntentStatus, VenueError>>,
        ) -> Self {
            self.status_script = script.into_iter().collect();
            self
        }

        async fn enter(&self) {
            let live = self.calls.live.fetch_add(1, Ordering::SeqCst) + 1;
            self.calls.max_concurrency.fetch_max(live, Ordering::SeqCst);
            // Yield inside the critical section so any missing serialisation
            // would let a second call observe `live == 2`.
            tokio::task::yield_now().await;
            self.calls.live.fetch_sub(1, Ordering::SeqCst);
        }
    }

    impl VenueInvoker for StubAdapter {
        fn derive_header<'a>(
            &'a mut self,
            _body: &'a [u8],
        ) -> BoxFuture<'a, Result<IntentHeader, VenueError>> {
            Box::pin(async move {
                self.calls.derive.fetch_add(1, Ordering::SeqCst);
                self.enter().await;
                self.derive.clone()
            })
        }

        fn quote<'a>(
            &'a mut self,
            _body: &'a [u8],
        ) -> BoxFuture<'a, Result<Quotation, VenueError>> {
            Box::pin(async move {
                self.calls.quote.fetch_add(1, Ordering::SeqCst);
                self.enter().await;
                Ok(quotation())
            })
        }

        fn submit<'a>(
            &'a mut self,
            _body: &'a [u8],
        ) -> BoxFuture<'a, Result<SubmitOutcome, VenueError>> {
            Box::pin(async move {
                self.calls.submit.fetch_add(1, Ordering::SeqCst);
                self.enter().await;
                self.submit.clone()
            })
        }

        fn status(&mut self, _receipt: Vec<u8>) -> BoxFuture<'_, Result<IntentStatus, VenueError>> {
            Box::pin(async move {
                self.calls.status.fetch_add(1, Ordering::SeqCst);
                self.status_script
                    .pop_front()
                    .unwrap_or(Ok(IntentStatus::Open))
            })
        }

        fn cancel(&mut self, _receipt: Vec<u8>) -> BoxFuture<'_, Result<(), VenueError>> {
            Box::pin(async move {
                self.calls.cancel.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
        }
    }

    /// A guard that refuses every egress with a fixed reason.
    struct DenyGuard;
    impl EgressGuard for DenyGuard {
        fn check(&self, _ctx: &GuardContext<'_>) -> GuardVerdict {
            GuardVerdict::Deny("blocked by test policy".to_owned())
        }
    }

    fn quotation() -> Quotation {
        Quotation {
            gives: AssetAmount {
                asset: Asset::Native,
                amount: vec![1],
            },
            wants: AssetAmount {
                asset: Asset::Native,
                amount: Vec::new(),
            },
            fee: AssetAmount {
                asset: Asset::Native,
                amount: Vec::new(),
            },
            valid_until_ms: 1_700_000_000_000,
        }
    }

    fn header() -> IntentHeader {
        IntentHeader {
            gives: AssetAmount {
                asset: Asset::Native,
                amount: vec![1],
            },
            wants: AssetAmount {
                asset: Asset::Native,
                amount: Vec::new(),
            },
            settlement: Settlement { chain: 1 },
            authorisation: AuthScheme::Eip712,
        }
    }

    fn registry_with(
        quota: SubmitQuota,
        guard: Option<Arc<dyn EgressGuard>>,
        adapter: StubAdapter,
    ) -> VenueRegistry {
        let mut builder = VenueRegistryBuilder::new(quota);
        if let Some(guard) = guard {
            builder = builder.with_guard(guard);
        }
        builder.install(cow(), adapter).expect("install adapter");
        builder.build()
    }

    #[tokio::test]
    async fn submit_round_trips_through_derive_guard_submit() {
        let calls = Arc::new(StubCalls::default());
        let registry = registry_with(
            SubmitQuota::default(),
            None,
            StubAdapter::new(calls.clone()),
        );

        let outcome = registry
            .submit("mod-a", &cow(), b"body".to_vec())
            .await
            .expect("submit succeeds");

        assert!(matches!(outcome, SubmitOutcome::Accepted(r) if r == b"receipt"));
        assert_eq!(calls.derive.load(Ordering::SeqCst), 1);
        assert_eq!(calls.submit.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn unknown_venue_is_rejected_without_touching_an_adapter() {
        let calls = Arc::new(StubCalls::default());
        let registry = registry_with(
            SubmitQuota::default(),
            None,
            StubAdapter::new(calls.clone()),
        );

        let err = registry
            .submit("mod-a", &VenueId::from("unlisted"), b"body".to_vec())
            .await
            .expect_err("unknown venue rejected");

        assert!(matches!(err, VenueError::UnknownVenue));
        assert_eq!(calls.derive.load(Ordering::SeqCst), 0);
        assert_eq!(calls.submit.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn guard_deny_is_advisory_and_does_not_block_submit() {
        let calls = Arc::new(StubCalls::default());
        let registry = registry_with(
            SubmitQuota::default(),
            Some(Arc::new(DenyGuard)),
            StubAdapter::new(calls.clone()),
        );

        let outcome = registry
            .submit("mod-a", &cow(), b"body".to_vec())
            .await
            .expect("advisory deny does not block");

        // The seam runs on the derived header but only logs: derive ran and
        // the submission still reached the adapter.
        assert!(matches!(outcome, SubmitOutcome::Accepted(r) if r == b"receipt"));
        assert_eq!(calls.derive.load(Ordering::SeqCst), 1);
        assert_eq!(calls.submit.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn quote_reaches_the_adapter_without_header_or_guard() {
        let calls = Arc::new(StubCalls::default());
        // A denying guard proves quotes skip the seam: no value moves.
        let registry = registry_with(
            SubmitQuota::default(),
            Some(Arc::new(DenyGuard)),
            StubAdapter::new(calls.clone()),
        );

        let quoted = registry
            .quote("mod-a", &cow(), b"body".to_vec())
            .await
            .expect("quote succeeds");

        assert_eq!(quoted, quotation());
        assert_eq!(calls.quote.load(Ordering::SeqCst), 1);
        assert_eq!(calls.derive.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn quote_spends_the_caller_quota() {
        let calls = Arc::new(StubCalls::default());
        let quota = SubmitQuota::new(1, Duration::from_secs(3600));
        let registry = registry_with(quota, None, StubAdapter::new(calls.clone()));

        assert!(registry.quote("mod-a", &cow(), b"b".to_vec()).await.is_ok());
        // The quote spent the only unit: both a further quote and a
        // submit are stopped at the gate.
        assert!(matches!(
            registry.quote("mod-a", &cow(), b"b".to_vec()).await,
            Err(VenueError::RateLimited(_))
        ));
        assert!(matches!(
            registry.submit("mod-a", &cow(), b"b".to_vec()).await,
            Err(VenueError::RateLimited(_))
        ));
        assert_eq!(calls.quote.load(Ordering::SeqCst), 1);
        assert_eq!(calls.submit.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn quote_to_an_unknown_venue_is_rejected() {
        let calls = Arc::new(StubCalls::default());
        let registry = registry_with(
            SubmitQuota::default(),
            None,
            StubAdapter::new(calls.clone()),
        );

        assert!(matches!(
            registry
                .quote("mod-a", &VenueId::from("unlisted"), b"b".to_vec())
                .await,
            Err(VenueError::UnknownVenue)
        ));
        assert_eq!(calls.quote.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn submission_quota_rate_limits_once_the_budget_is_spent() {
        let calls = Arc::new(StubCalls::default());
        let quota = SubmitQuota::new(2, Duration::from_secs(3600));
        let registry = registry_with(quota, None, StubAdapter::new(calls.clone()));

        assert!(
            registry
                .submit("mod-a", &cow(), b"b".to_vec())
                .await
                .is_ok()
        );
        assert!(
            registry
                .submit("mod-a", &cow(), b"b".to_vec())
                .await
                .is_ok()
        );
        let err = registry
            .submit("mod-a", &cow(), b"b".to_vec())
            .await
            .expect_err("third submit over quota");

        // Exhaustion is retryable once the window slides: rate-limited
        // carrying the window, never denied.
        assert!(matches!(
            err,
            VenueError::RateLimited(rl) if rl.retry_after_ms == Some(3_600_000)
        ));
        // The over-quota call is stopped at the gate, so the adapter saw only
        // the two admitted submits.
        assert_eq!(calls.submit.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn quota_is_per_caller() {
        let calls = Arc::new(StubCalls::default());
        let quota = SubmitQuota::new(1, Duration::from_secs(3600));
        let registry = registry_with(quota, None, StubAdapter::new(calls.clone()));

        assert!(
            registry
                .submit("mod-a", &cow(), b"b".to_vec())
                .await
                .is_ok()
        );
        assert!(
            registry
                .submit("mod-a", &cow(), b"b".to_vec())
                .await
                .is_err(),
            "mod-a is over its own budget"
        );
        // A different caller has its own budget.
        assert!(
            registry
                .submit("mod-b", &cow(), b"b".to_vec())
                .await
                .is_ok(),
            "mod-b has an independent budget"
        );
    }

    #[tokio::test]
    async fn decode_failures_are_charged_and_stop_re_invoking_the_adapter() {
        let calls = Arc::new(StubCalls::default());
        let quota = SubmitQuota::new(1, Duration::from_secs(3600));
        let adapter =
            StubAdapter::new(calls.clone()).with_derive(Err(VenueError::InvalidBody("bad".into())));
        let registry = registry_with(quota, None, adapter);

        // First garbage body: derive fails, the failure is charged.
        let first = registry.submit("mod-a", &cow(), b"junk".to_vec()).await;
        assert!(matches!(first, Err(VenueError::InvalidBody(_))));
        // Second: the charge from the decode failure exhausts the budget, so
        // the caller is stopped at the gate and the adapter is not re-invoked.
        let second = registry.submit("mod-a", &cow(), b"junk".to_vec()).await;
        assert!(matches!(second, Err(VenueError::RateLimited(_))));
        assert_eq!(
            calls.derive.load(Ordering::SeqCst),
            1,
            "adapter derive-header was invoked exactly once",
        );
    }

    #[tokio::test]
    async fn non_decode_venue_errors_are_not_charged() {
        let calls = Arc::new(StubCalls::default());
        let quota = SubmitQuota::new(1, Duration::from_secs(3600));
        let adapter = StubAdapter::new(calls.clone())
            .with_derive(Err(VenueError::Unavailable("rpc down".into())));
        let registry = registry_with(quota, None, adapter);

        assert!(matches!(
            registry.submit("mod-a", &cow(), b"b".to_vec()).await,
            Err(VenueError::Unavailable(_))
        ));
        // A venue-side failure did not spend the caller's budget: it may try
        // again, so derive is reached a second time.
        assert!(matches!(
            registry.submit("mod-a", &cow(), b"b".to_vec()).await,
            Err(VenueError::Unavailable(_))
        ));
        assert_eq!(calls.derive.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn status_and_cancel_pass_through_without_quota() {
        let calls = Arc::new(StubCalls::default());
        // A spent budget must not block reads: status and cancel are not
        // submissions.
        let quota = SubmitQuota::new(1, Duration::from_secs(3600));
        let registry = registry_with(quota, None, StubAdapter::new(calls.clone()));

        assert!(matches!(
            registry.status(&cow(), b"r".to_vec()).await,
            Ok(IntentStatus::Open)
        ));
        assert!(registry.cancel(&cow(), b"r".to_vec()).await.is_ok());
        assert_eq!(calls.status.load(Ordering::SeqCst), 1);
        assert_eq!(calls.cancel.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_calls_to_one_adapter_are_serialised() {
        let calls = Arc::new(StubCalls::default());
        let quota = SubmitQuota::new(1000, Duration::from_secs(3600));
        let registry = registry_with(quota, None, StubAdapter::new(calls.clone()));

        let mut handles = Vec::new();
        for _ in 0..8 {
            let registry = registry.clone();
            handles.push(tokio::spawn(async move {
                let _ = registry.submit("mod-a", &cow(), b"b".to_vec()).await;
            }));
        }
        for h in handles {
            h.await.expect("task joins");
        }
        // The adapter mutex is held across the guest await, so no two calls
        // ever overlapped inside the adapter.
        assert_eq!(calls.max_concurrency.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn duplicate_venue_id_is_rejected() {
        let mut builder = VenueRegistryBuilder::new(SubmitQuota::default());
        let a = Arc::new(StubCalls::default());
        let b = Arc::new(StubCalls::default());
        builder
            .install(cow(), StubAdapter::new(a))
            .expect("first install");
        let err = builder
            .install(cow(), StubAdapter::new(b))
            .expect_err("second install collides");
        assert_eq!(err.venue, cow());
    }

    #[test]
    fn zero_quota_saturates_to_one() {
        let registry =
            VenueRegistryBuilder::new(SubmitQuota::new(0, Duration::from_secs(60))).build();
        assert_eq!(registry.inner.quota.max_charges, 1);
    }

    // ── status watch + polling ────────────────────────────────────────

    #[tokio::test]
    async fn accepted_submission_goes_under_status_watch() {
        let calls = Arc::new(StubCalls::default());
        let registry = registry_with(SubmitQuota::default(), None, StubAdapter::new(calls));

        assert_eq!(registry.watched_count(), 0);
        registry
            .submit("mod-a", &cow(), b"body".to_vec())
            .await
            .expect("submit succeeds");
        assert_eq!(registry.watched_count(), 1);

        // Re-submitting the same receipt does not double-watch it.
        registry
            .submit("mod-a", &cow(), b"body".to_vec())
            .await
            .expect("submit succeeds");
        assert_eq!(registry.watched_count(), 1);
    }

    #[tokio::test]
    async fn requires_signing_outcome_is_not_watched() {
        let calls = Arc::new(StubCalls::default());
        let adapter =
            StubAdapter::new(calls).with_submit(Ok(SubmitOutcome::RequiresSigning(UnsignedTx {
                chain: 1,
                to: vec![0u8; 20],
                value: Vec::new(),
                data: Vec::new(),
            })));
        let registry = registry_with(SubmitQuota::default(), None, adapter);

        registry
            .submit("mod-a", &cow(), b"body".to_vec())
            .await
            .expect("submit succeeds");
        // No receipt exists yet, so there is nothing to poll.
        assert_eq!(registry.watched_count(), 0);
        assert!(registry.poll_status_transitions().await.is_empty());
    }

    #[tokio::test]
    async fn poll_reports_the_first_status_then_dedupes_repeats() {
        let calls = Arc::new(StubCalls::default());
        let registry = registry_with(
            SubmitQuota::default(),
            None,
            StubAdapter::new(calls.clone()),
        );
        registry
            .submit("mod-a", &cow(), b"body".to_vec())
            .await
            .expect("submit succeeds");

        // First poll: `last` is unset, so the current status reports.
        let first = registry.poll_status_transitions().await;
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].venue, "cow");
        assert_eq!(first[0].receipt, b"receipt");
        assert_eq!(decoded(&first[0]), plain(Lifecycle::Open));

        // Second poll: same status, nothing to report.
        assert!(registry.poll_status_transitions().await.is_empty());
        assert_eq!(calls.status.load(Ordering::SeqCst), 2);
        assert_eq!(registry.watched_count(), 1, "open is not terminal");
    }

    #[tokio::test]
    async fn poll_reports_each_transition_and_prunes_on_terminal() {
        let calls = Arc::new(StubCalls::default());
        let adapter = StubAdapter::new(calls).with_status_script([
            Ok(IntentStatus::Pending),
            Ok(IntentStatus::Pending),
            Ok(IntentStatus::Open),
            Ok(IntentStatus::Fulfilled),
        ]);
        let registry = registry_with(SubmitQuota::default(), None, adapter);
        registry
            .submit("mod-a", &cow(), b"body".to_vec())
            .await
            .expect("submit succeeds");

        let mut seen = Vec::new();
        for _ in 0..4 {
            seen.extend(registry.poll_status_transitions().await);
        }
        let statuses: Vec<StatusBody> = seen.iter().map(decoded).collect();
        assert_eq!(
            statuses,
            vec![
                plain(Lifecycle::Pending),
                plain(Lifecycle::Open),
                plain(Lifecycle::Fulfilled),
            ],
            "the repeated pending is deduplicated; each transition reports once",
        );
        assert_eq!(registry.watched_count(), 0, "fulfilled prunes the watch");
        // A further poll has nothing left to ask the adapter about.
        assert!(registry.poll_status_transitions().await.is_empty());
    }

    #[tokio::test]
    async fn poll_failure_keeps_the_watch_for_the_next_cadence() {
        let calls = Arc::new(StubCalls::default());
        let adapter = StubAdapter::new(calls)
            .with_status_script([Err(VenueError::Unavailable("venue down".into()))]);
        let registry = registry_with(SubmitQuota::default(), None, adapter);
        registry
            .submit("mod-a", &cow(), b"body".to_vec())
            .await
            .expect("submit succeeds");

        assert!(registry.poll_status_transitions().await.is_empty());
        assert_eq!(
            registry.watched_count(),
            1,
            "transient failure keeps the entry"
        );

        // The venue recovered: the next poll reports the current status.
        let updates = registry.poll_status_transitions().await;
        assert_eq!(updates.len(), 1);
        assert_eq!(decoded(&updates[0]), plain(Lifecycle::Open));
    }

    #[test]
    fn every_lifecycle_state_lowers_onto_the_status_body() {
        for (wire, lowered) in [
            (IntentStatus::Pending, Lifecycle::Pending),
            (IntentStatus::Open, Lifecycle::Open),
            (IntentStatus::Fulfilled, Lifecycle::Fulfilled),
            (IntentStatus::Cancelled, Lifecycle::Cancelled),
            (IntentStatus::Expired, Lifecycle::Expired),
        ] {
            assert_eq!(status_body(wire), plain(lowered));
        }
    }
}
