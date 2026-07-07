//! `nexum:host/types` and the intent vocabulary it uses are type-only
//! interfaces (no functions). The generated traits are empty; we just
//! provide the marker impls.

use crate::bindings::nexum;
use crate::host::component::RuntimeTypes;
use crate::host::state::HostState;

impl<T: RuntimeTypes> nexum::host::types::Host for HostState<T> {}

impl<T: RuntimeTypes> nexum::intent::types::Host for HostState<T> {}

impl<T: RuntimeTypes> nexum::value_flow::types::Host for HostState<T> {}
