//! Extension seam: what one extension contributes to the host (namespace,
//! capabilities, linker hook, optional service, provider kind, event sources,
//! and manifest-section install predicates).

use std::any::Any;
use std::collections::{BTreeMap, BTreeSet};
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use futures::Stream;
use nexum_tasks::{TaskExecutor, TaskExit, TaskSet};
use wasmtime::Store;
use wasmtime::component::{Component, Linker};

use crate::bindings::nexum::host::types::Event;
use crate::engine_config::EngineConfig;
use crate::host::actor::Liveness;
use crate::host::component::RuntimeTypes;
use crate::host::state::HostState;
use crate::manifest::{ExtensionSections, NamespaceCaps};

/// One runtime extension; a module importing its interface boots only if both
/// the linker entry and the capability namespace are registered.
pub trait Extension<T: RuntimeTypes>: Send + Sync + 'static {
    /// Namespace this extension owns; keys its service in [`HostServices`].
    fn namespace(&self) -> &'static str;

    /// Capability namespace merged into enforcement.
    fn capabilities(&self) -> NamespaceCaps;

    /// Add the extension's imports to a worker linker, after core interfaces
    /// and before instantiation.
    fn link(&self, linker: &mut Linker<HostState<T>>) -> anyhow::Result<()>;

    /// Host service this extension owns, published under its namespace.
    fn service(&self) -> Option<Arc<dyn HostService>> {
        None
    }

    /// Provider kind this extension installs.
    fn provider(&self) -> Option<Box<dyn ProviderKind<T>>> {
        None
    }

    /// Manifest section names this extension claims; an unclaimed non-core
    /// section is refused at boot.
    fn manifest_sections(&self) -> &'static [&'static str] {
        &[]
    }

    /// Admit one provider at install over its manifest sections; `Err`
    /// refuses fail-fast.
    fn admit_provider(&self, provider: &str, sections: &ExtensionSections) -> anyhow::Result<()> {
        let _ = (provider, sections);
        Ok(())
    }

    /// Admit one worker at install over its and the loaded providers'
    /// sections; `Err` refuses fail-fast.
    fn admit_worker(
        &self,
        worker: &str,
        sections: &ExtensionSections,
        providers: &[ProviderManifest],
    ) -> anyhow::Result<()> {
        let _ = (worker, sections, providers);
        Ok(())
    }

    /// Subscription kinds this extension's event sources emit; an unknown
    /// non-core kind is refused at boot.
    fn subscriptions(&self) -> &'static [&'static str] {
        &[]
    }

    /// Open the extension's event sources after boot; the event loop merges
    /// and dispatches them.
    fn events(&self, sources: &mut EventSources<'_>) -> anyhow::Result<Vec<ExtensionEventStream>> {
        let _ = sources;
        Ok(Vec::new())
    }
}

/// Event dispatched to every module with a `[[subscription]]` of `kind` whose
/// filters match `attrs`.
pub struct ExtensionEvent {
    /// Manifest subscription kind that routes this event.
    pub kind: &'static str,
    /// Routing attributes a subscription's filters match against.
    pub attrs: Vec<(&'static str, String)>,
    /// The host event delivered to each matching module.
    pub event: Event,
}

/// A stream of extension events the event loop merges and drives.
pub type ExtensionEventStream = Pin<Box<dyn Stream<Item = ExtensionEvent> + Send>>;

/// Launch inputs for [`Extension::events`].
pub struct EventSources<'a> {
    /// The loaded engine config.
    pub config: &'a EngineConfig,
    /// Extension-owned services, as booted.
    pub services: &'a HostServices,
    /// Extension subscription kinds declared by at least one module.
    pub subscribed: &'a BTreeSet<String>,
    executor: &'a TaskExecutor,
    tasks: &'a mut TaskSet,
}

impl<'a> EventSources<'a> {
    /// Bundle the launch inputs for one [`Extension::events`] pass.
    pub fn new(
        config: &'a EngineConfig,
        services: &'a HostServices,
        subscribed: &'a BTreeSet<String>,
        executor: &'a TaskExecutor,
        tasks: &'a mut TaskSet,
    ) -> Self {
        Self {
            config,
            services,
            subscribed,
            executor,
            tasks,
        }
    }

    /// Spawn an event-source task; it must end when its stream's receiver
    /// drops.
    pub fn spawn(&mut self, task: impl Future<Output = ()> + Send + 'static) {
        self.tasks.push(self.executor.spawn(async move {
            task.await;
            TaskExit::ReceiverGone
        }));
    }
}

/// Type-erased host service an extension owns, downcast at the call site.
pub trait HostService: Any + Send + Sync + 'static {}

/// A provider component kind; the host holds an instance behind the owning
/// extension's serialized service.
#[async_trait]
pub trait ProviderKind<T: RuntimeTypes>: Send + Sync + 'static {
    /// Manifest kind this provider answers for.
    fn kind(&self) -> &'static str;

    /// Adds the provider's imports to a provider linker.
    fn link(&self, linker: &mut Linker<HostState<T>>) -> anyhow::Result<()>;

    /// Instantiate and install one provider; [`Installed::Dead`] is a failed
    /// guest `init`, `Err` a boot error.
    async fn install(
        &self,
        instance: ProviderInstance<'_, T>,
        service: &Arc<dyn HostService>,
    ) -> anyhow::Result<Installed>;
}

/// One provider instance ready to install.
pub struct ProviderInstance<'a, T: RuntimeTypes> {
    /// Compiled provider component.
    pub component: &'a Component,
    /// Linker carrying the kind's imports plus the WASI base.
    pub linker: &'a Linker<HostState<T>>,
    /// Store the instance runs in; the kind takes ownership.
    pub store: Store<HostState<T>>,
    /// Manifest `[config]` handed to the guest `init`.
    pub config: Vec<(String, String)>,
    /// The provider's extension-owned manifest sections.
    pub sections: &'a ExtensionSections,
    /// Fuel budget applied before each routed guest call.
    pub fuel_per_call: u64,
    /// Shared liveness the instance reports traps on.
    pub liveness: Liveness,
}

/// One loaded provider as [`Extension::admit_worker`] sees it.
#[derive(Clone, Debug)]
pub struct ProviderManifest {
    /// The provider's namespace (its manifest name).
    pub name: String,
    /// Registered kind spelling.
    pub kind: &'static str,
    /// The provider's extension-owned manifest sections.
    pub sections: ExtensionSections,
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

/// Immutable per-namespace service map, built once at boot.
#[derive(Clone, Default)]
pub struct HostServices(Arc<BTreeMap<&'static str, Arc<dyn HostService>>>);

impl std::fmt::Debug for HostServices {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_set().entries(self.0.keys()).finish()
    }
}

impl HostServices {
    /// Collect each extension's service under its namespace; refuses a
    /// duplicate.
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

    /// Service under `namespace` downcast to `S`; `None` if absent or
    /// mismatched.
    pub fn get<S: HostService>(&self, namespace: &str) -> Option<Arc<S>> {
        downcast_service(self.0.get(namespace)?)
    }

    /// The raw type-erased service under `namespace`.
    pub fn raw(&self, namespace: &str) -> Option<&Arc<dyn HostService>> {
        self.0.get(namespace)
    }

    /// Publish `service` under `namespace`, refusing a duplicate.
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
            HostServices::from_extensions(&[ext("acme", Arc::new(Registry(7)))]).expect("build");

        let registry = services.get::<Registry>("acme").expect("registered");
        assert_eq!(registry.0, 7);
        assert!(services.get::<Clockwork>("acme").is_none());
        assert!(services.get::<Registry>("absent").is_none());
        assert!(services.raw("acme").is_some());
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
            ext("acme", Arc::new(Registry(1))),
            ext("acme", Arc::new(Clockwork)),
        ])
        .expect_err("duplicate namespace");
        assert!(err.to_string().contains("acme"), "{err}");
    }
}
