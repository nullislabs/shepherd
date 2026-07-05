//! The [`RuntimeTypes`] lattice over the in-process mocks.

use std::marker::PhantomData;

use crate::host::component::RuntimeTypes;
use crate::test_utils::{MockChainProvider, MockStateStore};

/// Lattice binding the mock backends. The extension slot is the type
/// parameter `E`, defaulting to the empty payload (`()`) so an assembly with
/// no extensions composes exactly as a domain-free lattice. An extension
/// crate binds its own `Ext` payload through the same mocks by naming
/// `MockTypes<MyExt>`.
///
/// This is a type-level marker: it is only ever named, never constructed, so
/// it derives no traits and holds no value.
pub struct MockTypes<E = ()>(PhantomData<fn() -> E>);

impl<E: Clone + Send + Sync + 'static> RuntimeTypes for MockTypes<E> {
    type Chain = MockChainProvider;
    type Store = MockStateStore;
    type Ext = E;
}
