//! The extension seam: what one extension contributes to the host - a
//! namespace, a capability namespace, a linker hook, an optional host
//! service, and an optional provider kind. Assembled at the composition
//! root and threaded into every module linker.

use std::any::Any;
use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use wasmtime::Store;
use wasmtime::component::{Component, Linker};

use crate::host::actor::Liveness;
use crate::host::component::RuntimeTypes;
use crate::host::state::HostState;
use crate::manifest::NamespaceCaps;

/// One runtime extension. A module that imports an extension interface
/// boots only if the linker entry AND the capability namespace are both
/// registered before instantiation.
pub trait Extension<T: RuntimeTypes>: Send + Sync + 'static {
    /// Namespace this extension owns; keys its service in [`HostServices`].
    fn namespace(&self) -> &'static str;

    /// Capability namespace merged into enforcement so a module importing
    /// the extension's interfaces still validates.
    fn capabilities(&self) -> NamespaceCaps;

    /// Adds the extension's imports to a worker linker. Runs after the
    /// core interfaces and before instantiation. Takes only `&mut Linker`,
    /// so the seam stays compatible with a future per-extension router
    /// that serializes access to the non-`Sync` wasmtime `Store`.
    fn link(&self, linker: &mut Linker<HostState<T>>) -> anyhow::Result<()>;

    /// Host service this extension owns, published under its namespace on
    /// [`HostServices`].
    fn service(&self) -> Option<Arc<dyn HostService>> {
        None
    }

    /// Provider kind this extension installs.
    fn provider(&self) -> Option<Box<dyn ProviderKind<T>>> {
        None
    }
}

/// A type-erased host service an extension owns. Held per namespace on
/// `HostState::services` and downcast at the call site. Kept synchronous
/// so it stays `dyn`-compatible.
pub trait HostService: Any + Send + Sync + 'static {}

/// A provider component kind: the host holds an instance behind the owning
/// extension's serialized service; others call it. `async_trait` carries
/// the one cold `dyn` boot path until `async_fn_in_dyn_trait` stabilizes.
#[async_trait]
pub trait ProviderKind<T: RuntimeTypes>: Send + Sync + 'static {
    /// Manifest kind this provider answers for.
    fn kind(&self) -> &'static str;

    /// Adds the provider's imports to a provider linker.
    fn link(&self, linker: &mut Linker<HostState<T>>) -> anyhow::Result<()>;

    /// Instantiate one provider and install it behind the owning service.
    /// [`Installed::Dead`] reports a failed guest `init`; an `Err` is a
    /// boot error.
    async fn install(
        &self,
        instance: ProviderInstance<'_, T>,
        service: &Arc<dyn HostService>,
    ) -> anyhow::Result<Installed>;
}

/// One provider instance ready to install: the compiled component, the
/// linker the kind's [`ProviderKind::link`] populated, the supervised
/// store, the manifest `[config]`, and the per-call fuel budget.
pub struct ProviderInstance<'a, T: RuntimeTypes> {
    /// Compiled provider component.
    pub component: &'a Component,
    /// Linker carrying the kind's imports plus the WASI base.
    pub linker: &'a Linker<HostState<T>>,
    /// Store the instance runs in; the kind takes ownership.
    pub store: Store<HostState<T>>,
    /// Manifest `[config]` handed to the guest `init`.
    pub config: Vec<(String, String)>,
    /// Fuel budget applied before each routed guest call.
    pub fuel_per_call: u64,
    /// Shared liveness the installed instance reports traps on and the
    /// supervisor's restart sweep reads.
    pub liveness: Liveness,
}

/// Outcome of one provider install.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Installed {
    /// `init` succeeded; the instance is installed and routable.
    Live,
    /// `init` returned a fault; the instance is loaded but not routable.
    Dead,
}

/// Downcast a type-erased service to `S`. `None` when the type differs.
pub fn downcast_service<S: HostService>(service: &Arc<dyn HostService>) -> Option<Arc<S>> {
    let service = Arc::clone(service);
    let erased: Arc<dyn Any + Send + Sync> = service;
    erased.downcast().ok()
}

/// Immutable per-namespace service map: each extension's [`HostService`]
/// under its [`Extension::namespace`], built once at boot and shared by
/// every module store.
#[derive(Clone, Default)]
pub struct HostServices(Arc<BTreeMap<&'static str, Arc<dyn HostService>>>);

impl std::fmt::Debug for HostServices {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_set().entries(self.0.keys()).finish()
    }
}

impl HostServices {
    /// Collect each extension's service under its namespace. Refuses a
    /// duplicate namespace.
    pub fn from_extensions<T: RuntimeTypes>(
        extensions: &[Arc<dyn Extension<T>>],
    ) -> anyhow::Result<Self> {
        let mut map = BTreeMap::new();
        for ext in extensions {
            let Some(service) = ext.service() else {
                continue;
            };
            let namespace = ext.namespace();
            if map.insert(namespace, service).is_some() {
                anyhow::bail!("duplicate extension service namespace {namespace}");
            }
        }
        Ok(Self(Arc::new(map)))
    }

    /// The service under `namespace`, downcast to its concrete type.
    /// `None` when the namespace is absent or the type does not match.
    pub fn get<S: HostService>(&self, namespace: &str) -> Option<Arc<S>> {
        downcast_service(self.0.get(namespace)?)
    }

    /// The raw type-erased service under `namespace`.
    pub fn raw(&self, namespace: &str) -> Option<&Arc<dyn HostService>> {
        self.0.get(namespace)
    }

    /// Publish `service` under `namespace`, refusing a duplicate. The boot
    /// path seeds a service no extension registers yet.
    pub fn with_service(
        self,
        namespace: &'static str,
        service: Arc<dyn HostService>,
    ) -> anyhow::Result<Self> {
        let mut map = Arc::unwrap_or_clone(self.0);
        if map.insert(namespace, service).is_some() {
            anyhow::bail!("duplicate extension service namespace {namespace}");
        }
        Ok(Self(Arc::new(map)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::supervisor::TestTypes;

    struct Registry(u64);
    impl HostService for Registry {}

    struct Clockwork;
    impl HostService for Clockwork {}

    struct ServiceExt {
        namespace: &'static str,
        service: Option<Arc<dyn HostService>>,
    }

    impl Extension<TestTypes> for ServiceExt {
        fn namespace(&self) -> &'static str {
            self.namespace
        }
        fn capabilities(&self) -> NamespaceCaps {
            NamespaceCaps {
                prefix: "test:ext/",
                ifaces: &[],
            }
        }
        fn link(&self, _linker: &mut Linker<HostState<TestTypes>>) -> anyhow::Result<()> {
            Ok(())
        }
        fn service(&self) -> Option<Arc<dyn HostService>> {
            self.service.as_ref().map(Arc::clone)
        }
    }

    fn ext(
        namespace: &'static str,
        service: Arc<dyn HostService>,
    ) -> Arc<dyn Extension<TestTypes>> {
        Arc::new(ServiceExt {
            namespace,
            service: Some(service),
        })
    }

    /// A registered service comes back under its namespace, downcast to
    /// its concrete type; a wrong type or an absent namespace is `None`.
    #[test]
    fn get_downcasts_by_namespace() {
        let services =
            HostServices::from_extensions(&[ext("videre", Arc::new(Registry(7)))]).expect("build");

        let registry = services.get::<Registry>("videre").expect("registered");
        assert_eq!(registry.0, 7);
        assert!(services.get::<Clockwork>("videre").is_none());
        assert!(services.get::<Registry>("absent").is_none());
        assert!(services.raw("videre").is_some());
    }

    /// A serviceless extension contributes nothing to the map.
    #[test]
    fn serviceless_extension_is_absent() {
        let serviceless: Arc<dyn Extension<TestTypes>> = Arc::new(ServiceExt {
            namespace: "quiet",
            service: None,
        });
        let services = HostServices::from_extensions(&[serviceless]).expect("build");
        assert!(services.raw("quiet").is_none());
    }

    /// Two services under one namespace refuse to build.
    #[test]
    fn duplicate_namespace_is_refused() {
        let err = HostServices::from_extensions(&[
            ext("videre", Arc::new(Registry(1))),
            ext("videre", Arc::new(Clockwork)),
        ])
        .expect_err("duplicate namespace");
        assert!(err.to_string().contains("videre"), "{err}");
    }
}
