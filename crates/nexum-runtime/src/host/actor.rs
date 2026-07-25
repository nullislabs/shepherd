//! Supervised host-actor primitive: one component instance the host holds and
//! others call. The store is refuelled before each guest call, traps project
//! onto a typed fault, and an [`ActorSlot`] mutex serialises calls so one
//! store never runs two at once.

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use tokio::sync::Mutex as AsyncMutex;
use wasmtime::Store;

use super::component::RuntimeTypes;
use super::state::HostState;

/// One supervised actor behind its serialising mutex; concurrent callers
/// queue.
pub type ActorSlot<A> = Arc<AsyncMutex<A>>;

/// Shared liveness of one supervised component; a trap marks it dead and
/// records when, for backoff from the death instant. Clone shares the flag,
/// starts alive.
#[derive(Clone, Debug, Default)]
pub struct Liveness(Arc<Mutex<Option<Instant>>>);

impl Liveness {
    /// Whether the component is currently callable.
    pub fn is_alive(&self) -> bool {
        self.lock().is_none()
    }

    /// When the component died, while it is dead.
    pub fn dead_since(&self) -> Option<Instant> {
        *self.lock()
    }

    /// Mark dead, keeping the first death instant if already dead.
    pub fn mark_dead(&self) {
        let mut died_at = self.lock();
        if died_at.is_none() {
            *died_at = Some(Instant::now());
        }
    }

    /// Mark the component alive again after a restart.
    pub fn mark_alive(&self) {
        *self.lock() = None;
    }

    /// The flag, recovered from a poisoned lock.
    fn lock(&self) -> MutexGuard<'_, Option<Instant>> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// A guest call failed outside the component's typed error space.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ActorFault {
    /// The pre-call refuel failed; the guest was never entered.
    #[error("refuel failed: {0}")]
    Refuel(wasmtime::Error),
    /// The guest trapped; carries the root cause only.
    #[error("trapped: {}", .0.root_cause())]
    Trap(wasmtime::Error),
}

/// A supervised component store: refuelled before each guest call, with traps
/// projected onto [`ActorFault`] and recorded on [`Liveness`].
pub struct SupervisedStore<T: RuntimeTypes> {
    store: Store<HostState<T>>,
    fuel_per_call: u64,
    liveness: Liveness,
}

impl<T: RuntimeTypes> SupervisedStore<T> {
    /// Supervise an instantiated store with a per-call fuel budget,
    /// reporting traps on `liveness`.
    pub fn new(store: Store<HostState<T>>, fuel_per_call: u64, liveness: Liveness) -> Self {
        Self {
            store,
            fuel_per_call,
            liveness,
        }
    }

    /// Refuel, then run one guest call; a trap marks liveness dead until
    /// reinstantiated.
    pub async fn call<R>(
        &mut self,
        call: impl AsyncFnOnce(&mut Store<HostState<T>>) -> wasmtime::Result<R>,
    ) -> Result<R, ActorFault> {
        self.store
            .set_fuel(self.fuel_per_call)
            .map_err(ActorFault::Refuel)?;
        call(&mut self.store).await.map_err(|trap| {
            self.liveness.mark_dead();
            ActorFault::Trap(trap)
        })
    }
}
