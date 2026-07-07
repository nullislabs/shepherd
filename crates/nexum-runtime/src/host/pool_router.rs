//! The intent pool router: the strategy-facing `nexum:intent/pool` import
//! resolved to installed venue adapters.
//!
//! A module's `pool::submit(venue, body)` reaches the host here. The router
//! resolves the venue id to the one installed adapter that answers for it,
//! then drives a fixed sequence against that adapter: derive the header,
//! run the guard interposition seam on it, and only then submit. Status and
//! cancel are pass-throughs; they are not submissions, so they skip the
//! header, the guard, and the quota.
//!
//! Invocation is serialised per adapter. A wasmtime `Store` is not `Sync`,
//! so each adapter sits behind its own async mutex: concurrent pool calls to
//! the same venue queue on that mutex, while calls to different venues run
//! in parallel. The lock is held across the guest await, which is the whole
//! point - it is the actor boundary that keeps one adapter store
//! single-threaded.
//!
//! Fuel cannot cross stores, so a module that spams undecodable bodies would
//! otherwise burn an adapter's budget for free. Two mechanisms close that:
//! a per-caller submission quota gates every submit before the adapter is
//! touched, and a decode failure (the adapter's `invalid-body`) is charged
//! to the calling module's quota, so a caller feeding garbage exhausts its
//! own budget rather than the adapter's.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::future::BoxFuture;
use tokio::sync::Mutex as AsyncMutex;
use tracing::warn;
use wasmtime::Store;

use crate::bindings::{
    IntentHeader, IntentStatus, IntentStatusUpdate, SubmitOutcome, VenueAdapter, VenueError,
};
use crate::host::component::RuntimeTypes;
use crate::host::state::HostState;

/// Default per-caller submission budget within [`DEFAULT_QUOTA_WINDOW`].
pub const DEFAULT_QUOTA_MAX_CHARGES: u32 = 256;
/// Default sliding window the per-caller submission budget is counted over.
pub const DEFAULT_QUOTA_WINDOW: Duration = Duration::from_secs(60);

/// Per-caller submission quota. Both a forwarded submission and a charged
/// decode failure consume one unit; the window slides so a caller's budget
/// refills as old charges age out.
#[derive(Debug, Clone, Copy)]
pub struct PoolQuota {
    /// Maximum charges a single caller may accrue within `window`.
    pub max_charges: u32,
    /// Sliding window the charges are counted across.
    pub window: Duration,
}

impl PoolQuota {
    /// Pair a budget with the window it is counted over.
    pub const fn new(max_charges: u32, window: Duration) -> Self {
        Self {
            max_charges,
            window,
        }
    }
}

impl Default for PoolQuota {
    fn default() -> Self {
        Self::new(DEFAULT_QUOTA_MAX_CHARGES, DEFAULT_QUOTA_WINDOW)
    }
}

/// The guard interposition seam. The router runs this on the adapter-derived
/// header after `derive-header` and before `submit`. The shipped policy is a
/// no-op that allows every egress; the egress-guard epic replaces the
/// installed policy with the real facts-plus-analysers pipeline without the
/// router changing shape.
pub trait GuardPolicy: Send + Sync {
    /// Decide whether the derived header may proceed to the adapter's submit.
    fn check(&self, ctx: &GuardContext<'_>) -> GuardVerdict;
}

/// What the guard sees: who is submitting, to which venue, and the header the
/// adapter derived from the opaque body. The header is the stable ontology
/// policy has teeth on; the raw body never reaches the guard.
pub struct GuardContext<'a> {
    /// Namespace of the calling module.
    pub caller: &'a str,
    /// Venue id the submission is routed to.
    pub venue: &'a str,
    /// Adapter-derived header for the body.
    pub header: &'a IntentHeader,
}

/// The guard's decision on one egress.
pub enum GuardVerdict {
    /// Forward the submission to the adapter.
    Allow,
    /// Refuse the egress with an operator-facing reason.
    Deny(String),
}

/// The shipped no-op policy: allow every egress. Named so the composition
/// root reads plainly and the egress-guard epic has an obvious thing to swap.
pub struct AllowAllGuard;

impl GuardPolicy for AllowAllGuard {
    fn check(&self, _ctx: &GuardContext<'_>) -> GuardVerdict {
        GuardVerdict::Allow
    }
}

/// The per-adapter invocation seam. One installed adapter answers for exactly
/// one venue; the router owns the adapter's `Store` behind an async mutex and
/// reaches it only through this trait, so the router's sequencing and quota
/// logic is testable against a stub that never spins up a wasmtime store.
///
/// The futures are boxed so the router can hold heterogeneous adapters behind
/// one `dyn` slot without the whole router turning generic over an adapter
/// type it never names.
pub trait VenueInvoker: Send {
    /// Project the opaque body onto the stable header the guard runs on.
    fn derive_header<'a>(
        &'a mut self,
        body: &'a [u8],
    ) -> BoxFuture<'a, Result<IntentHeader, VenueError>>;

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
/// `internal-error` rather than propagated: a misbehaving adapter must not be
/// the caller's fault, and it must not unwind through the router into the
/// calling module's store.
pub struct AdapterActor<T: RuntimeTypes> {
    store: Store<HostState<T>>,
    bindings: VenueAdapter,
    fuel_per_call: u64,
}

impl<T: RuntimeTypes> AdapterActor<T> {
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
            .map_err(|e| VenueError::InternalError(format!("adapter refuel failed: {e}")))
    }
}

/// Project a wasmtime trap into the venue-error space. The root cause is
/// carried so an operator sees why the adapter died without the wasm frame
/// list leaking to the calling module.
fn trap_to_venue_error(trap: wasmtime::Error) -> VenueError {
    VenueError::InternalError(format!("adapter trapped: {}", trap.root_cause()))
}

impl<T: RuntimeTypes> VenueInvoker for AdapterActor<T> {
    fn derive_header<'a>(
        &'a mut self,
        body: &'a [u8],
    ) -> BoxFuture<'a, Result<IntentHeader, VenueError>> {
        Box::pin(async move {
            self.refuel()?;
            match self
                .bindings
                .nexum_intent_adapter()
                .call_derive_header(&mut self.store, body)
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
                .nexum_intent_adapter()
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
                .nexum_intent_adapter()
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
                .nexum_intent_adapter()
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

/// One receipt the router polls for status transitions. `last` starts
/// `None` so the first successful poll always reports, giving a
/// subscriber the intent's current state without waiting for a change.
struct WatchedIntent {
    venue: String,
    receipt: Vec<u8>,
    last: Option<IntentStatus>,
}

/// A polled status is terminal when the intent can never change again:
/// the router stops watching the receipt after reporting it.
fn is_terminal(status: &IntentStatus) -> bool {
    matches!(
        status,
        IntentStatus::Settled(_)
            | IntentStatus::Failed(_)
            | IntentStatus::Expired
            | IntentStatus::Cancelled
    )
}

/// The shared router state. Cloning a [`PoolRouter`] is an `Arc` bump; every
/// module store carries the same handle, so a submission from any module
/// reaches the same adapters and the same quota ledger.
struct PoolRouterInner {
    adapters: HashMap<String, AdapterSlot>,
    guard: Arc<dyn GuardPolicy>,
    quota: PoolQuota,
    ledger: Mutex<QuotaLedger>,
    /// Receipts under status watch, appended by accepted submissions and
    /// pruned as they reach a terminal status.
    watched: Mutex<Vec<WatchedIntent>>,
}

/// The strategy-facing pool router, cheap to clone and shared across every
/// module store.
#[derive(Clone)]
pub struct PoolRouter {
    inner: Arc<PoolRouterInner>,
}

impl PoolRouter {
    /// An empty router: no adapters, the no-op guard, the default quota. This
    /// is what an adapter store (which cannot call pool) and the single-module
    /// `just run` path carry.
    pub fn empty() -> Self {
        PoolRouterBuilder::new(PoolQuota::default()).build()
    }

    /// Resolve a venue id to its installed adapter slot.
    fn resolve(&self, venue: &str) -> Result<AdapterSlot, VenueError> {
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
    /// seam, then forward to the adapter. A decode failure is charged to the
    /// caller before returning, so a caller feeding garbage exhausts its own
    /// budget and is stopped at the gate on the next call rather than
    /// re-invoking the adapter.
    pub async fn submit(
        &self,
        caller: &str,
        venue: &str,
        body: Vec<u8>,
    ) -> Result<SubmitOutcome, VenueError> {
        let slot = self.resolve(venue)?;
        // Gate before touching the adapter so a quota-exhausted caller never
        // reaches the adapter store or its mutex.
        if !self.quota_admits(caller) {
            return Err(VenueError::Denied(format!(
                "submission quota exhausted for caller {caller}"
            )));
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
        if let GuardVerdict::Deny(reason) = self.inner.guard.check(&ctx) {
            return Err(VenueError::Denied(reason));
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

    /// Put a `(venue, receipt)` pair under status watch. Idempotent: a
    /// re-submitted receipt keeps its existing watch entry.
    fn watch(&self, venue: &str, receipt: Vec<u8>) {
        let mut watched = self.inner.watched.lock().expect("watch list poisoned");
        if watched
            .iter()
            .any(|w| w.venue == venue && w.receipt == receipt)
        {
            return;
        }
        watched.push(WatchedIntent {
            venue: venue.to_owned(),
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
    /// dropped from the watch; a transport failure leaves the entry
    /// untouched for the next cadence, except `invalid-receipt`, which
    /// means the venue disowns the receipt, so watching is pointless.
    pub async fn poll_status_transitions(&self) -> Vec<IntentStatusUpdate> {
        // Snapshot so the std mutex is never held across the guest await.
        let snapshot: Vec<(String, Vec<u8>)> = {
            let watched = self.inner.watched.lock().expect("watch list poisoned");
            watched
                .iter()
                .map(|w| (w.venue.clone(), w.receipt.clone()))
                .collect()
        };
        let mut updates = Vec::new();
        for (venue, receipt) in snapshot {
            // Installed adapters never leave the router, so a resolve
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
                Err(VenueError::InvalidReceipt) => {
                    warn!(venue = %venue, "venue disowns a watched receipt - dropping it");
                    self.unwatch(&venue, &receipt);
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
    /// while the poll was in flight.
    fn record_polled_status(
        &self,
        venue: &str,
        receipt: &[u8],
        status: IntentStatus,
    ) -> Option<IntentStatusUpdate> {
        let mut watched = self.inner.watched.lock().expect("watch list poisoned");
        let pos = watched
            .iter()
            .position(|w| w.venue == venue && w.receipt == receipt)?;
        let changed = watched[pos].last.as_ref() != Some(&status);
        if is_terminal(&status) {
            watched.remove(pos);
        } else {
            watched[pos].last = Some(status.clone());
        }
        changed.then(|| IntentStatusUpdate {
            venue: venue.to_owned(),
            receipt: receipt.to_vec(),
            status,
        })
    }

    /// Drop a `(venue, receipt)` pair from the status watch.
    fn unwatch(&self, venue: &str, receipt: &[u8]) {
        let mut watched = self.inner.watched.lock().expect("watch list poisoned");
        watched.retain(|w| !(w.venue == venue && w.receipt == receipt));
    }

    /// Report where a previously submitted intent is in its life. Not a
    /// submission: no header, no guard, no quota, just the serialised call.
    pub async fn status(&self, venue: &str, receipt: Vec<u8>) -> Result<IntentStatus, VenueError> {
        let slot = self.resolve(venue)?;
        let mut adapter = slot.lock().await;
        adapter.status(receipt).await
    }

    /// Ask the venue to withdraw an intent. Not a submission, so it skips the
    /// header, guard, and quota like `status`.
    pub async fn cancel(&self, venue: &str, receipt: Vec<u8>) -> Result<(), VenueError> {
        let slot = self.resolve(venue)?;
        let mut adapter = slot.lock().await;
        adapter.cancel(receipt).await
    }

    /// Number of installed, routable adapters.
    pub fn venue_count(&self) -> usize {
        self.inner.adapters.len()
    }
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

/// Assembles a [`PoolRouter`]: adapters install first (at supervisor boot,
/// before any module store carries the built router), then the router
/// freezes. The guard defaults to the no-op [`AllowAllGuard`]; the
/// egress-guard epic overrides it here.
pub struct PoolRouterBuilder {
    adapters: HashMap<String, AdapterSlot>,
    guard: Arc<dyn GuardPolicy>,
    quota: PoolQuota,
}

impl PoolRouterBuilder {
    /// Start an empty builder with the given quota and the no-op guard.
    pub fn new(quota: PoolQuota) -> Self {
        Self {
            adapters: HashMap::new(),
            guard: Arc::new(AllowAllGuard),
            quota,
        }
    }

    /// Override the guard policy. The egress-guard epic wires the real
    /// pipeline through here; tests inject a denying policy to prove the seam.
    pub fn with_guard(mut self, guard: Arc<dyn GuardPolicy>) -> Self {
        self.guard = guard;
        self
    }

    /// Install an adapter under its venue id. Rejects a duplicate id: two
    /// adapters answering the same venue would silently shadow one another,
    /// which is a config error worth failing boot over.
    pub fn install(
        &mut self,
        venue: String,
        invoker: impl VenueInvoker + 'static,
    ) -> Result<(), DuplicateVenue> {
        if self.adapters.contains_key(&venue) {
            return Err(DuplicateVenue { venue });
        }
        self.adapters
            .insert(venue, Arc::new(AsyncMutex::new(invoker)));
        Ok(())
    }

    /// Freeze the builder into a shared router.
    pub fn build(self) -> PoolRouter {
        if self.quota.max_charges == 0 {
            // A zero budget would deny every submission; saturate up to one so
            // a misconfigured quota still admits a single submission rather
            // than bricking every venue. Mirrors the poison-policy clamp.
            warn!("pool submission quota max_charges is 0; clamping to 1");
        }
        let quota = PoolQuota::new(self.quota.max_charges.max(1), self.quota.window);
        PoolRouter {
            inner: Arc::new(PoolRouterInner {
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
#[error("venue id {venue:?} is claimed by more than one installed adapter")]
pub struct DuplicateVenue {
    /// The colliding venue id.
    pub venue: String,
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::bindings::nexum::intent::types::UnsignedTx;
    use crate::bindings::value_flow::Settlement;
    use crate::bindings::{AuthScheme, IntentHeader};

    use super::*;

    /// A programmable adapter that records call counts and returns canned
    /// outcomes, so the router's sequencing, guard seam, and quota are tested
    /// without a wasmtime store.
    #[derive(Default)]
    struct StubCalls {
        derive: AtomicUsize,
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
    impl GuardPolicy for DenyGuard {
        fn check(&self, _ctx: &GuardContext<'_>) -> GuardVerdict {
            GuardVerdict::Deny("blocked by test policy".to_owned())
        }
    }

    fn header() -> IntentHeader {
        IntentHeader {
            gives: Vec::new(),
            wants: Vec::new(),
            valid_until: None,
            settlement: Settlement::EvmChain(1),
            authorisation: AuthScheme::Unsigned,
        }
    }

    fn router_with(
        quota: PoolQuota,
        guard: Option<Arc<dyn GuardPolicy>>,
        adapter: StubAdapter,
    ) -> PoolRouter {
        let mut builder = PoolRouterBuilder::new(quota);
        if let Some(guard) = guard {
            builder = builder.with_guard(guard);
        }
        builder
            .install("cow".to_owned(), adapter)
            .expect("install adapter");
        builder.build()
    }

    #[tokio::test]
    async fn submit_round_trips_through_derive_guard_submit() {
        let calls = Arc::new(StubCalls::default());
        let router = router_with(PoolQuota::default(), None, StubAdapter::new(calls.clone()));

        let outcome = router
            .submit("mod-a", "cow", b"body".to_vec())
            .await
            .expect("submit succeeds");

        assert!(matches!(outcome, SubmitOutcome::Accepted(r) if r == b"receipt"));
        assert_eq!(calls.derive.load(Ordering::SeqCst), 1);
        assert_eq!(calls.submit.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn unknown_venue_is_rejected_without_touching_an_adapter() {
        let calls = Arc::new(StubCalls::default());
        let router = router_with(PoolQuota::default(), None, StubAdapter::new(calls.clone()));

        let err = router
            .submit("mod-a", "unlisted", b"body".to_vec())
            .await
            .expect_err("unknown venue rejected");

        assert!(matches!(err, VenueError::UnknownVenue));
        assert_eq!(calls.derive.load(Ordering::SeqCst), 0);
        assert_eq!(calls.submit.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn guard_deny_blocks_submit_after_deriving_the_header() {
        let calls = Arc::new(StubCalls::default());
        let router = router_with(
            PoolQuota::default(),
            Some(Arc::new(DenyGuard)),
            StubAdapter::new(calls.clone()),
        );

        let err = router
            .submit("mod-a", "cow", b"body".to_vec())
            .await
            .expect_err("guard denies");

        assert!(matches!(err, VenueError::Denied(reason) if reason.contains("test policy")));
        // The seam runs on the derived header, then blocks: derive ran, submit
        // did not.
        assert_eq!(calls.derive.load(Ordering::SeqCst), 1);
        assert_eq!(calls.submit.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn submission_quota_denies_once_the_budget_is_spent() {
        let calls = Arc::new(StubCalls::default());
        let quota = PoolQuota::new(2, Duration::from_secs(3600));
        let router = router_with(quota, None, StubAdapter::new(calls.clone()));

        assert!(router.submit("mod-a", "cow", b"b".to_vec()).await.is_ok());
        assert!(router.submit("mod-a", "cow", b"b".to_vec()).await.is_ok());
        let err = router
            .submit("mod-a", "cow", b"b".to_vec())
            .await
            .expect_err("third submit over quota");

        assert!(matches!(err, VenueError::Denied(reason) if reason.contains("quota")));
        // The over-quota call is stopped at the gate, so the adapter saw only
        // the two admitted submits.
        assert_eq!(calls.submit.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn quota_is_per_caller() {
        let calls = Arc::new(StubCalls::default());
        let quota = PoolQuota::new(1, Duration::from_secs(3600));
        let router = router_with(quota, None, StubAdapter::new(calls.clone()));

        assert!(router.submit("mod-a", "cow", b"b".to_vec()).await.is_ok());
        assert!(
            router.submit("mod-a", "cow", b"b".to_vec()).await.is_err(),
            "mod-a is over its own budget"
        );
        // A different caller has its own budget.
        assert!(
            router.submit("mod-b", "cow", b"b".to_vec()).await.is_ok(),
            "mod-b has an independent budget"
        );
    }

    #[tokio::test]
    async fn decode_failures_are_charged_and_stop_re_invoking_the_adapter() {
        let calls = Arc::new(StubCalls::default());
        let quota = PoolQuota::new(1, Duration::from_secs(3600));
        let adapter =
            StubAdapter::new(calls.clone()).with_derive(Err(VenueError::InvalidBody("bad".into())));
        let router = router_with(quota, None, adapter);

        // First garbage body: derive fails, the failure is charged.
        let first = router.submit("mod-a", "cow", b"junk".to_vec()).await;
        assert!(matches!(first, Err(VenueError::InvalidBody(_))));
        // Second: the charge from the decode failure exhausts the budget, so
        // the caller is stopped at the gate and the adapter is not re-invoked.
        let second = router.submit("mod-a", "cow", b"junk".to_vec()).await;
        assert!(matches!(second, Err(VenueError::Denied(_))));
        assert_eq!(
            calls.derive.load(Ordering::SeqCst),
            1,
            "adapter derive-header was invoked exactly once",
        );
    }

    #[tokio::test]
    async fn non_decode_venue_errors_are_not_charged() {
        let calls = Arc::new(StubCalls::default());
        let quota = PoolQuota::new(1, Duration::from_secs(3600));
        let adapter = StubAdapter::new(calls.clone())
            .with_derive(Err(VenueError::Unavailable("rpc down".into())));
        let router = router_with(quota, None, adapter);

        assert!(matches!(
            router.submit("mod-a", "cow", b"b".to_vec()).await,
            Err(VenueError::Unavailable(_))
        ));
        // A venue-side failure did not spend the caller's budget: it may try
        // again, so derive is reached a second time.
        assert!(matches!(
            router.submit("mod-a", "cow", b"b".to_vec()).await,
            Err(VenueError::Unavailable(_))
        ));
        assert_eq!(calls.derive.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn status_and_cancel_pass_through_without_quota() {
        let calls = Arc::new(StubCalls::default());
        // A spent budget must not block reads: status and cancel are not
        // submissions.
        let quota = PoolQuota::new(1, Duration::from_secs(3600));
        let router = router_with(quota, None, StubAdapter::new(calls.clone()));

        assert!(matches!(
            router.status("cow", b"r".to_vec()).await,
            Ok(IntentStatus::Open)
        ));
        assert!(router.cancel("cow", b"r".to_vec()).await.is_ok());
        assert_eq!(calls.status.load(Ordering::SeqCst), 1);
        assert_eq!(calls.cancel.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_calls_to_one_adapter_are_serialised() {
        let calls = Arc::new(StubCalls::default());
        let quota = PoolQuota::new(1000, Duration::from_secs(3600));
        let router = router_with(quota, None, StubAdapter::new(calls.clone()));

        let mut handles = Vec::new();
        for _ in 0..8 {
            let router = router.clone();
            handles.push(tokio::spawn(async move {
                let _ = router.submit("mod-a", "cow", b"b".to_vec()).await;
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
        let mut builder = PoolRouterBuilder::new(PoolQuota::default());
        let a = Arc::new(StubCalls::default());
        let b = Arc::new(StubCalls::default());
        builder
            .install("cow".to_owned(), StubAdapter::new(a))
            .expect("first install");
        let err = builder
            .install("cow".to_owned(), StubAdapter::new(b))
            .expect_err("second install collides");
        assert_eq!(err.venue, "cow");
    }

    #[test]
    fn zero_quota_saturates_to_one() {
        let router = PoolRouterBuilder::new(PoolQuota::new(0, Duration::from_secs(60))).build();
        assert_eq!(router.inner.quota.max_charges, 1);
    }

    // ── status watch + polling ────────────────────────────────────────

    #[tokio::test]
    async fn accepted_submission_goes_under_status_watch() {
        let calls = Arc::new(StubCalls::default());
        let router = router_with(PoolQuota::default(), None, StubAdapter::new(calls));

        assert_eq!(router.watched_count(), 0);
        router
            .submit("mod-a", "cow", b"body".to_vec())
            .await
            .expect("submit succeeds");
        assert_eq!(router.watched_count(), 1);

        // Re-submitting the same receipt does not double-watch it.
        router
            .submit("mod-a", "cow", b"body".to_vec())
            .await
            .expect("submit succeeds");
        assert_eq!(router.watched_count(), 1);
    }

    #[tokio::test]
    async fn requires_signing_outcome_is_not_watched() {
        let calls = Arc::new(StubCalls::default());
        let adapter =
            StubAdapter::new(calls).with_submit(Ok(SubmitOutcome::RequiresSigning(UnsignedTx {
                chain_id: 1,
                to: vec![0u8; 20],
                value: Vec::new(),
                input: Vec::new(),
            })));
        let router = router_with(PoolQuota::default(), None, adapter);

        router
            .submit("mod-a", "cow", b"body".to_vec())
            .await
            .expect("submit succeeds");
        // No receipt exists yet, so there is nothing to poll.
        assert_eq!(router.watched_count(), 0);
        assert!(router.poll_status_transitions().await.is_empty());
    }

    #[tokio::test]
    async fn poll_reports_the_first_status_then_dedupes_repeats() {
        let calls = Arc::new(StubCalls::default());
        let router = router_with(PoolQuota::default(), None, StubAdapter::new(calls.clone()));
        router
            .submit("mod-a", "cow", b"body".to_vec())
            .await
            .expect("submit succeeds");

        // First poll: `last` is unset, so the current status reports.
        let first = router.poll_status_transitions().await;
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].venue, "cow");
        assert_eq!(first[0].receipt, b"receipt");
        assert_eq!(first[0].status, IntentStatus::Open);

        // Second poll: same status, nothing to report.
        assert!(router.poll_status_transitions().await.is_empty());
        assert_eq!(calls.status.load(Ordering::SeqCst), 2);
        assert_eq!(router.watched_count(), 1, "open is not terminal");
    }

    #[tokio::test]
    async fn poll_reports_each_transition_and_prunes_on_terminal() {
        let calls = Arc::new(StubCalls::default());
        let adapter = StubAdapter::new(calls).with_status_script([
            Ok(IntentStatus::Pending),
            Ok(IntentStatus::Pending),
            Ok(IntentStatus::Open),
            Ok(IntentStatus::Settled(Some(b"tx".to_vec()))),
        ]);
        let router = router_with(PoolQuota::default(), None, adapter);
        router
            .submit("mod-a", "cow", b"body".to_vec())
            .await
            .expect("submit succeeds");

        let mut seen = Vec::new();
        for _ in 0..4 {
            seen.extend(router.poll_status_transitions().await);
        }
        let statuses: Vec<&IntentStatus> = seen.iter().map(|u| &u.status).collect();
        assert_eq!(
            statuses,
            vec![
                &IntentStatus::Pending,
                &IntentStatus::Open,
                &IntentStatus::Settled(Some(b"tx".to_vec())),
            ],
            "the repeated pending is deduplicated; each transition reports once",
        );
        assert_eq!(router.watched_count(), 0, "settled prunes the watch");
        // A further poll has nothing left to ask the adapter about.
        assert!(router.poll_status_transitions().await.is_empty());
    }

    #[tokio::test]
    async fn poll_failure_keeps_the_watch_for_the_next_cadence() {
        let calls = Arc::new(StubCalls::default());
        let adapter = StubAdapter::new(calls)
            .with_status_script([Err(VenueError::Unavailable("venue down".into()))]);
        let router = router_with(PoolQuota::default(), None, adapter);
        router
            .submit("mod-a", "cow", b"body".to_vec())
            .await
            .expect("submit succeeds");

        assert!(router.poll_status_transitions().await.is_empty());
        assert_eq!(
            router.watched_count(),
            1,
            "transient failure keeps the entry"
        );

        // The venue recovered: the next poll reports the current status.
        let updates = router.poll_status_transitions().await;
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].status, IntentStatus::Open);
    }

    #[tokio::test]
    async fn disowned_receipt_is_dropped_from_the_watch() {
        let calls = Arc::new(StubCalls::default());
        let adapter = StubAdapter::new(calls).with_status_script([Err(VenueError::InvalidReceipt)]);
        let router = router_with(PoolQuota::default(), None, adapter);
        router
            .submit("mod-a", "cow", b"body".to_vec())
            .await
            .expect("submit succeeds");

        assert!(router.poll_status_transitions().await.is_empty());
        assert_eq!(
            router.watched_count(),
            0,
            "a receipt the venue disowns is never polled again",
        );
    }
}
