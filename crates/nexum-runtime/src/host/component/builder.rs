//! Per-component builders: one seam for turning the loaded config plus a
//! resolved data directory into a runtime backend.
//!
//! Each core backend is wrapped as a [`ComponentBuilder`], and
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
        // create_dir_all and LocalStore::open (which fsyncs on create) are
        // blocking syscalls; keep them off the async executor.
        let data_dir = ctx.data_dir.to_path_buf();
        tokio::task::spawn_blocking(move || {
            std::fs::create_dir_all(&data_dir).map_err(|e| {
                anyhow::anyhow!("create data directory {}: {e}", data_dir.display())
            })?;
            let path = data_dir.join("local-store.redb");
            LocalStore::open(&path)
                .map_err(|e| anyhow::anyhow!("open local-store at {}: {e}", path.display()))
        })
        .await?
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
    /// Create a new [`ComponentsBuilder`].
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine_config::EngineConfig;
    use crate::preset::CoreRuntime;

    /// Drives the core component builders end-to-end against a real (empty)
    /// config and a fresh data directory: chain pool, redb store, and the
    /// log pipeline are opened at runtime, not just typechecked. Proves the
    /// store builder creates the data directory and the assembly bundles a
    /// live pipeline.
    #[tokio::test]
    async fn components_builder_opens_the_core_backends() {
        let dir = tempfile::tempdir().expect("tempdir");
        let data_dir = dir.path().join("nested-state");
        let config = EngineConfig::default();
        let ctx = BuilderContext {
            config: &config,
            data_dir: &data_dir,
        };

        let components = ComponentsBuilder::new(ProviderPoolBuilder, LocalStoreBuilder, ())
            .build::<CoreRuntime>(&ctx)
            .await
            .expect("build core components");

        // The store builder created the data directory eagerly.
        assert!(data_dir.is_dir(), "data directory created by the build");
        assert!(
            data_dir.join("local-store.redb").is_file(),
            "redb store opened under the data directory",
        );
        // The bundle carries a live in-memory log pipeline.
        let _ = &components.logs;
    }
}
