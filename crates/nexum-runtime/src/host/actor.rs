//! The supervised host-actor primitive: one component instance the host
//! holds and others call. The store is refuelled before each guest call,
//! a trap is projected onto a typed fault instead of unwinding into the
//! caller, and each instance sits behind an [`ActorSlot`] async mutex held
//! across the guest await, so one store never runs two guest calls at once.

use std::sync::Arc;

use tokio::sync::Mutex as AsyncMutex;
use wasmtime::Store;

use super::component::RuntimeTypes;
use super::state::HostState;

/// One supervised actor behind its serialising mutex. A wasmtime `Store`
/// is not `Sync`; concurrent callers queue here.
pub type ActorSlot<A> = Arc<AsyncMutex<A>>;

/// A guest call failed outside the component's typed error space.
#[derive(Debug, thiserror::Error)]
pub enum ActorFault {
    /// The pre-call refuel failed; the guest was never entered.
    #[error("refuel failed: {0}")]
    Refuel(wasmtime::Error),
    /// The guest trapped. Carries the root cause only; the wasm frame
    /// list stays out of the caller-facing message.
    #[error("trapped: {}", .0.root_cause())]
    Trap(wasmtime::Error),
}

/// A supervised component store: refuelled before each guest call so every
/// invocation starts from a full budget, with traps projected onto
/// [`ActorFault`].
pub struct SupervisedStore<T: RuntimeTypes> {
    store: Store<HostState<T>>,
    fuel_per_call: u64,
}

impl<T: RuntimeTypes> SupervisedStore<T> {
    /// Supervise an instantiated store with a per-call fuel budget.
    pub fn new(store: Store<HostState<T>>, fuel_per_call: u64) -> Self {
        Self {
            store,
            fuel_per_call,
        }
    }

    /// Refuel, then run one guest call against the store.
    pub async fn call<R>(
        &mut self,
        call: impl AsyncFnOnce(&mut Store<HostState<T>>) -> wasmtime::Result<R>,
    ) -> Result<R, ActorFault> {
        self.store
            .set_fuel(self.fuel_per_call)
            .map_err(ActorFault::Refuel)?;
        call(&mut self.store).await.map_err(ActorFault::Trap)
    }
}
