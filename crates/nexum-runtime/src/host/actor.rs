//! The supervised host-actor primitive: one component instance the host
//! holds and others call. The store is refuelled before each guest call,
//! a trap is projected onto a typed fault instead of unwinding into the
//! caller, and each instance sits behind an [`ActorSlot`] async mutex held
//! across the guest await, so one store never runs two guest calls at once.

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use tokio::sync::Mutex as AsyncMutex;
use wasmtime::Store;

use super::component::RuntimeTypes;
use super::state::HostState;

/// One supervised actor behind its serialising mutex. A wasmtime `Store`
/// is not `Sync`; concurrent callers queue here.
pub type ActorSlot<A> = Arc<AsyncMutex<A>>;

/// Shared liveness of one supervised component. The store marks it dead on
/// a trap, recording when, so the supervisor's restart sweep can count the
/// backoff from the death rather than from the sweep that observed it.
/// Cloning shares the flag. Starts alive.
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

    /// Mark the component dead: its store trapped and is unusable. Keeps
    /// the first death instant when already dead.
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

    /// The flag, recovered from a poisoned lock: the state is a bare
    /// `Option<Instant>`, valid under any interleaving.
    fn lock(&self) -> MutexGuard<'_, Option<Instant>> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

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
/// [`ActorFault`] and recorded on the shared [`Liveness`].
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

    /// Refuel, then run one guest call against the store. A trap marks the
    /// shared liveness dead: the store is poisoned until reinstantiated.
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
