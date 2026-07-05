//! The domain-free [`RuntimeTypes`] lattice over the in-process mocks.

use crate::host::component::RuntimeTypes;
use crate::test_utils::{MockChainProvider, MockStateStore};

/// Lattice binding the mock backends with an empty extension slot (`Ext =
/// ()`), so an assembly composes entirely through the public type-state path.
#[derive(Clone, Copy, Default)]
pub struct MockTypes;

impl RuntimeTypes for MockTypes {
    type Chain = MockChainProvider;
    type Store = MockStateStore;
    type Ext = ();
}
