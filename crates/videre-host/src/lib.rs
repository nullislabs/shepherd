//! The videre venue platform, packaged as one [`nexum_runtime`]
//! extension: the venue-adapter provider kind, the [`VenueRegistry`]
//! service, the advisory [`EgressGuard`] seam, and the keeper-facing
//! `videre:venue/client` interface. A composition root registers it all
//! with `builder.with_extensions([Arc::new(videre_host::platform(cfg))])`.

#![cfg_attr(not(test), warn(unused_crate_dependencies))]

pub mod bindings;
mod client;
mod registry;

use std::sync::Arc;
use std::time::Duration;

use nexum_runtime::bindings::nexum::host::types::Event;
use nexum_runtime::engine_config::EngineConfig;
use nexum_runtime::host::component::RuntimeTypes;
use nexum_runtime::host::extension::{
    EventSources, Extension, ExtensionEvent, ExtensionEventStream, HostService, ProviderKind,
};
use nexum_runtime::host::state::HostState;
use nexum_runtime::manifest::NamespaceCaps;
use tokio::sync::mpsc;
use wasmtime::component::{HasSelf, Linker};

pub use registry::{
    DuplicateVenue, EgressGuard, GuardContext, GuardVerdict, IntentStatusUpdate, VenueActor,
    VenueAdapterKind, VenueId, VenueInvoker, VenueRegistry, VenueRegistryBuilder,
};

/// The subscription kind the platform's status poller emits.
const INTENT_STATUS_KIND: &str = "intent-status";

/// Buffer for the status poll channel; small because the event loop
/// drains in real time.
const STATUS_CHANNEL_BUF: usize = 64;

/// The venue platform over the config-resolved quota and watch policy,
/// with the unit guard. The single registration entrypoint.
pub fn platform(config: &EngineConfig) -> Videre {
    Videre::from_registry(
        VenueRegistryBuilder::new(config.limits.quota())
            .with_watch_limit(config.limits.watch())
            .build(),
    )
}

/// The videre platform as one runtime extension. Registers the
/// `videre:venue/client` interface and its capability namespace on every
/// worker linker, publishes the [`VenueRegistry`] service, installs the
/// venue-adapter provider kind, and opens the status-poll event source.
pub struct Videre {
    registry: Arc<VenueRegistry>,
}

impl Videre {
    /// Assemble over a pre-built registry, for a custom [`EgressGuard`]
    /// or policy; [`platform`] covers the config-resolved default.
    pub fn from_registry(registry: VenueRegistry) -> Self {
        Self {
            registry: Arc::new(registry),
        }
    }
}

impl<T: RuntimeTypes> Extension<T> for Videre {
    fn namespace(&self) -> &'static str {
        VenueRegistry::NAMESPACE
    }

    /// Only the keeper-facing `client` interface is a capability; the
    /// `videre:types` and `videre:value-flow` packages are type-only and
    /// need no declaration.
    fn capabilities(&self) -> NamespaceCaps {
        NamespaceCaps {
            prefix: "videre:venue/",
            ifaces: &["client"],
        }
    }

    fn link(&self, linker: &mut Linker<HostState<T>>) -> anyhow::Result<()> {
        bindings::client::add_to_linker::<HostState<T>, HasSelf<HostState<T>>>(linker, |state| {
            state
        })?;
        Ok(())
    }

    fn service(&self) -> Option<Arc<dyn HostService>> {
        Some(Arc::clone(&self.registry) as Arc<dyn HostService>)
    }

    fn provider(&self) -> Option<Box<dyn ProviderKind<T>>> {
        Some(Box::new(VenueAdapterKind))
    }

    fn subscriptions(&self) -> &'static [&'static str] {
        &[INTENT_STATUS_KIND]
    }

    /// The status poll source: on every cadence tick, poll each installed
    /// adapter's status export through the shared registry and forward the
    /// observed transitions. Opened only when a module subscribes and at
    /// least one venue is installed.
    fn events(&self, sources: &mut EventSources<'_>) -> anyhow::Result<Vec<ExtensionEventStream>> {
        if !sources.subscribed.contains(INTENT_STATUS_KIND) {
            return Ok(Vec::new());
        }
        let registry = (*self.registry).clone();
        if registry.venue_count() == 0 {
            return Ok(Vec::new());
        }
        let cadence = sources.config.limits.status_poll_interval();
        let (tx, rx) = mpsc::channel::<ExtensionEvent>(STATUS_CHANNEL_BUF);
        sources.spawn(status_poll_task(registry, cadence, tx));
        let stream = futures::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|item| (item, rx))
        });
        Ok(vec![Box::pin(stream)])
    }
}

/// Poll loop behind [`Extension::events`]. Sleeps the cadence first so
/// the engine's boot dispatch settles before the first poll; ends when
/// the event loop drops its receiver.
async fn status_poll_task(
    registry: VenueRegistry,
    cadence: Duration,
    tx: mpsc::Sender<ExtensionEvent>,
) {
    loop {
        tokio::time::sleep(cadence).await;
        for update in registry.poll_status_transitions().await {
            let event = ExtensionEvent {
                kind: INTENT_STATUS_KIND,
                attrs: vec![("venue", update.venue.clone())],
                event: Event::IntentStatus(update),
            };
            if tx.send(event).await.is_err() {
                // Receiver dropped -> engine shutting down.
                return;
            }
        }
    }
}
