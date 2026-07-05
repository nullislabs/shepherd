//! Per-component builders: one seam for turning the loaded config plus a
//! resolved data directory into a runtime backend.
//!
//! Each backend that boot used to open through a scattered `from_config`
//! or `open` call is wrapped as a [`ComponentBuilder`], and
//! [`ComponentsBuilder`] assembles the core seams (plus the lattice `Ext`
//! payload) into a [`Components`] bundle. The composition root names the
//! concrete builders once; boot drives them through this trait.

use std::future::Future;
use std::path::Path;

use crate::host::component::{Components, RuntimeTypes};
use crate::host::local_store_redb::LocalStore;
use crate::host::logs::LogPipeline;
use crate::host::provider_pool::ProviderPool;

/// Shared inputs every component builder reads: the loaded engine config
/// and the resolved data directory backends open their files under.
pub struct BuilderContext<'a> {
    /// The loaded engine config.
    pub config: &'a crate::engine_config::EngineConfig,
    /// Directory backends root their on-disk state at.
    pub data_dir: &'a Path,
}

/// Builds one runtime backend from the shared [`BuilderContext`]. The
/// `impl Future + Send` form lets a builder connect over the network
/// (the chain provider does) while staying usable from a spawned task.
pub trait ComponentBuilder {
    /// The backend this builder produces.
    type Output;

    /// Open the backend, consuming the builder.
    fn build(
        self,
        ctx: &BuilderContext<'_>,
    ) -> impl Future<Output = anyhow::Result<Self::Output>> + Send;
}

/// Builds the chain [`ProviderPool`] from `[chains]`.
pub struct ProviderPoolBuilder;

impl ComponentBuilder for ProviderPoolBuilder {
    type Output = ProviderPool;

    async fn build(self, ctx: &BuilderContext<'_>) -> anyhow::Result<ProviderPool> {
        ProviderPool::from_config(ctx.config)
            .await
            .map_err(Into::into)
    }
}

/// Builds the [`LocalStore`] at `data_dir/local-store.redb`, creating the
/// data directory if it does not exist.
pub struct LocalStoreBuilder;

impl ComponentBuilder for LocalStoreBuilder {
    type Output = LocalStore;

    async fn build(self, ctx: &BuilderContext<'_>) -> anyhow::Result<LocalStore> {
        std::fs::create_dir_all(ctx.data_dir).map_err(|e| {
            anyhow::anyhow!("create data directory {}: {e}", ctx.data_dir.display())
        })?;
        let path = ctx.data_dir.join("local-store.redb");
        LocalStore::open(&path)
            .map_err(|e| anyhow::anyhow!("open local-store at {}: {e}", path.display()))
    }
}

/// The empty extension payload: a no-op builder for a core-only lattice
/// (`Ext = ()`).
impl ComponentBuilder for () {
    type Output = ();

    async fn build(self, _ctx: &BuilderContext<'_>) -> anyhow::Result<()> {
        Ok(())
    }
}

/// Assembles the core backend builders and the lattice `Ext` builder into
/// a [`Components`] bundle. The log pipeline is sized from `[limits.logs]`
/// and built here; the embedder retains its read handle by cloning
/// [`Components::logs`] after the build.
pub struct ComponentsBuilder<C, S, E> {
    /// Builds the chain backend ([`RuntimeTypes::Chain`]).
    pub chain: C,
    /// Builds the store backend ([`RuntimeTypes::Store`]).
    pub store: S,
    /// Builds the extension payload ([`RuntimeTypes::Ext`]).
    pub ext: E,
}

impl<C, S, E> ComponentsBuilder<C, S, E> {
    /// Name the three component builders.
    pub fn new(chain: C, store: S, ext: E) -> Self {
        Self { chain, store, ext }
    }

    /// Drive each builder against `ctx`, then bundle the backends with a
    /// fresh log pipeline. The builder outputs must match the lattice
    /// seams: chain to [`RuntimeTypes::Chain`], store to
    /// [`RuntimeTypes::Store`], ext to [`RuntimeTypes::Ext`].
    pub async fn build<T>(self, ctx: &BuilderContext<'_>) -> anyhow::Result<Components<T>>
    where
        T: RuntimeTypes,
        C: ComponentBuilder<Output = T::Chain>,
        S: ComponentBuilder<Output = T::Store>,
        E: ComponentBuilder<Output = T::Ext>,
    {
        let chain = self.chain.build(ctx).await?;
        let store = self.store.build(ctx).await?;
        let ext = self.ext.build(ctx).await?;
        let logs = LogPipeline::in_memory(ctx.config.limits.logs());
        Ok(Components {
            chain,
            store,
            ext,
            logs,
        })
    }
}
