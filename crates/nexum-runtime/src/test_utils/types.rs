//! The [`RuntimeTypes`] lattice over the in-process mocks.

use std::marker::PhantomData;

use crate::host::component::RuntimeTypes;
use crate::test_utils::{MockChainProvider, MockStateStore};

/// Lattice binding the mock backends. The extension slot is the type
/// parameter `E` (default `()`); an extension crate binds its own payload as
/// `MockTypes<MyExt>`. A type-level marker, only ever named.
pub struct MockTypes<E = ()>(PhantomData<fn() -> E>);

impl<E: Clone + Send + Sync + 'static> crate::sealed::SealedRuntimeTypes for MockTypes<E> {}

impl<E: Clone + Send + Sync + 'static> RuntimeTypes for MockTypes<E> {
    type Chain = MockChainProvider;
    type Store = MockStateStore;
    type Ext = E;
}
