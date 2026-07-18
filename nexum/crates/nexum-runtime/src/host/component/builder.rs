//! Per-component builders: one seam for turning the loaded config plus a
//! resolved data directory into a runtime backend.
//!
//! Each core backend is wrapped as a [`ComponentBuilder`], and
//! [`ComponentsBuilder`] assembles the core seams (plus the lattice `Ext`
//! payload and the log pipeline) into a [`Components`] bundle. The
//! composition root names the concrete builders once; boot drives them
//! through this trait.

use std::future::Future;
use std::path::Path;

use nexum_tasks::TaskExecutor;

use crate::host::component::{Components, RuntimeTypes};
use crate::host::local_store_redb::LocalStore;
use crate::host::logs::LogPipeline;
use crate::host::provider_pool::ProviderPool;
use crate::host::remote_store_bee::RemoteStore;

/// Shared inputs every component builder reads: the loaded engine config,
/// the resolved data directory backends open their files under, and the
/// executor blocking opens run on.
pub struct BuilderContext<'a> {
    /// The loaded engine config.
    pub config: &'a crate::engine_config::EngineConfig,
    /// Directory backends root their on-disk state at.
    pub data_dir: &'a Path,
    /// Runs blocking open work off the async executor.
    pub executor: &'a TaskExecutor,
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
        ctx.executor
            .spawn_blocking(move || {
                std::fs::create_dir_all(&data_dir).map_err(|e| {
                    anyhow::anyhow!("create data directory {}: {e}", data_dir.display())
                })?;
                let path = data_dir.join("local-store.redb");
                LocalStore::open(&path)
                    .map_err(|e| anyhow::anyhow!("open local-store at {}: {e}", path.display()))
            })
            .join()
            .await
            .ok_or_else(|| anyhow::anyhow!("local-store open task ended abnormally"))?
    }
}

/// Builds the [`RemoteStore`] from `[remote_store]`; an absent table
/// yields a disabled handle.
pub struct RemoteStoreBuilder;

impl ComponentBuilder for RemoteStoreBuilder {
    type Output = RemoteStore;

    async fn build(self, ctx: &BuilderContext<'_>) -> anyhow::Result<RemoteStore> {
        RemoteStore::from_config(ctx.config.remote_store.as_ref()).map_err(Into::into)
    }
}

/// Builds the default [`LogPipeline`]: the byte-bounded in-memory backend
/// sized from `[limits.logs]`.
pub struct LogPipelineBuilder;

impl ComponentBuilder for LogPipelineBuilder {
    type Output = LogPipeline;

    async fn build(self, ctx: &BuilderContext<'_>) -> anyhow::Result<LogPipeline> {
        Ok(LogPipeline::in_memory(ctx.config.limits.logs()))
    }
}

/// Names the component slot whose build failed. The leaf cause stays an
/// `anyhow::Error` because the backends fail for heterogeneous reasons
/// (I/O for the store, network for the chain).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BuildError {
    /// The chain backend builder failed.
    #[error("build the chain backend: {0}")]
    Chain(anyhow::Error),
    /// The store backend builder failed.
    #[error("build the store backend: {0}")]
    Store(anyhow::Error),
    /// The extension payload builder failed.
    #[error("build the extension payload: {0}")]
    Ext(anyhow::Error),
    /// The log pipeline builder failed.
    #[error("build the log pipeline: {0}")]
    Logs(anyhow::Error),
    /// The remote-store builder failed.
    #[error("build the remote-store backend: {0}")]
    Remote(anyhow::Error),
}

/// The empty extension payload: a no-op builder for a core-only lattice
/// (`Ext = ()`).
impl ComponentBuilder for () {
    type Output = ();

    async fn build(self, _ctx: &BuilderContext<'_>) -> anyhow::Result<()> {
        Ok(())
    }
}

/// Assembles the core backend builders, the lattice `Ext` builder, and the
/// log pipeline builder into a [`Components`] bundle. The logs slot defaults
/// to [`LogPipelineBuilder`] and the remote slot to [`RemoteStoreBuilder`];
/// the embedder retains the read handle by cloning [`Components::logs`]
/// after the build.
pub struct ComponentsBuilder<C, S, E, L = LogPipelineBuilder, R = RemoteStoreBuilder> {
    /// Builds the chain backend ([`RuntimeTypes::Chain`]).
    pub chain: C,
    /// Builds the store backend ([`RuntimeTypes::Store`]).
    pub store: S,
    /// Builds the extension payload ([`RuntimeTypes::Ext`]).
    pub ext: E,
    /// Builds the shared [`LogPipeline`].
    pub logs: L,
    /// Builds the shared [`RemoteStore`].
    pub remote: R,
}

impl<C, S, E> ComponentsBuilder<C, S, E> {
    /// Create a new [`ComponentsBuilder`] with the default log pipeline
    /// and remote-store builders.
    pub fn new(chain: C, store: S, ext: E) -> Self {
        Self {
            chain,
            store,
            ext,
            logs: LogPipelineBuilder,
            remote: RemoteStoreBuilder,
        }
    }
}

impl<C, S, E, L, R> ComponentsBuilder<C, S, E, L, R> {
    /// Replace the log pipeline builder.
    pub fn with_logs<L2>(self, logs: L2) -> ComponentsBuilder<C, S, E, L2, R> {
        ComponentsBuilder {
            chain: self.chain,
            store: self.store,
            ext: self.ext,
            logs,
            remote: self.remote,
        }
    }

    /// Replace the remote-store builder.
    pub fn with_remote<R2>(self, remote: R2) -> ComponentsBuilder<C, S, E, L, R2> {
        ComponentsBuilder {
            chain: self.chain,
            store: self.store,
            ext: self.ext,
            logs: self.logs,
            remote,
        }
    }

    /// Drive each builder against `ctx` and bundle the backends. The
    /// builder outputs must match the lattice seams: chain to
    /// [`RuntimeTypes::Chain`], store to [`RuntimeTypes::Store`], ext to
    /// [`RuntimeTypes::Ext`]; logs always yields a [`LogPipeline`] and
    /// remote a [`RemoteStore`]. A failing sub-build returns the
    /// [`BuildError`] variant naming that slot.
    pub async fn build<T>(self, ctx: &BuilderContext<'_>) -> Result<Components<T>, BuildError>
    where
        T: RuntimeTypes,
        C: ComponentBuilder<Output = T::Chain>,
        S: ComponentBuilder<Output = T::Store>,
        E: ComponentBuilder<Output = T::Ext>,
        L: ComponentBuilder<Output = LogPipeline>,
        R: ComponentBuilder<Output = RemoteStore>,
    {
        let chain = self.chain.build(ctx).await.map_err(BuildError::Chain)?;
        let store = self.store.build(ctx).await.map_err(BuildError::Store)?;
        let ext = self.ext.build(ctx).await.map_err(BuildError::Ext)?;
        let logs = self.logs.build(ctx).await.map_err(BuildError::Logs)?;
        let remote = self.remote.build(ctx).await.map_err(BuildError::Remote)?;
        Ok(Components {
            chain,
            store,
            ext,
            logs,
            remote,
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
        let tasks = nexum_tasks::TaskManager::new();
        let executor = tasks.executor();
        let ctx = BuilderContext {
            config: &config,
            data_dir: &data_dir,
            executor: &executor,
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

    /// `with_logs` substitutes the log pipeline builder: the bundle carries
    /// the exact pipeline the custom builder yields.
    #[tokio::test]
    async fn with_logs_substitutes_the_pipeline() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = EngineConfig::default();
        let tasks = nexum_tasks::TaskManager::new();
        let executor = tasks.executor();
        let ctx = BuilderContext {
            config: &config,
            data_dir: dir.path(),
            executor: &executor,
        };

        let custom = LogPipeline::in_memory(config.limits.logs());
        let components = ComponentsBuilder::new(ProviderPoolBuilder, LocalStoreBuilder, ())
            .with_logs(crate::test_utils::Prebuilt(custom.clone()))
            .build::<CoreRuntime>(&ctx)
            .await
            .expect("build with a custom log pipeline");

        assert!(
            std::sync::Arc::ptr_eq(&components.logs.router(), &custom.router()),
            "bundle carries the substituted pipeline",
        );
    }
}
