//! The extension seam: a linker hook plus the capability namespace an
//! extension contributes, assembled at the composition root and threaded
//! into every module linker.

use std::sync::Arc;

use wasmtime::component::Linker;

use crate::host::component::RuntimeTypes;
use crate::host::state::HostState;
use crate::manifest::NamespaceCaps;

/// Adds an extension's WIT interfaces to a module linker. Runs after the
/// core interfaces and before instantiation. Takes only `&mut Linker`, so
/// the seam stays compatible with a future per-extension router that
/// serialises access to the non-`Sync` wasmtime `Store`.
pub type LinkerHook<T> = Arc<dyn Fn(&mut Linker<HostState<T>>) -> anyhow::Result<()> + Send + Sync>;

/// One runtime extension: how to wire its interfaces into a module linker,
/// and the capability namespace enforcement must recognise for it. The two
/// travel together: a module that imports an extension interface boots only
/// if the linker entry AND the capability namespace are both registered
/// before instantiation.
pub struct Extension<T: RuntimeTypes> {
    /// Linker contribution: adds the extension's imports to a module linker.
    pub link: LinkerHook<T>,
    /// Capability namespace this extension owns, merged into enforcement so
    /// a module importing the extension's interfaces still validates.
    pub capabilities: NamespaceCaps,
}

impl<T: RuntimeTypes> Clone for Extension<T> {
    fn clone(&self) -> Self {
        Self {
            link: Arc::clone(&self.link),
            capabilities: self.capabilities,
        }
    }
}
