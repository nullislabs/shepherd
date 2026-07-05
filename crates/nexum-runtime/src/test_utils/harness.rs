//! In-process test harness: launch one module over the mock assembly and
//! drive it from a test, with no supervisor ceremony.
//!
//! [`TestRuntime`] wraps the public builder path over [`MockTypes`]: it opens
//! a module from a wasm path plus a manifest (a path, or inline TOML written
//! to a temp file), installs a manually-driven [`ManualClock`], and returns a
//! handle bundling the running [`RuntimeHandle`] with the retained mock
//! handles. A test programs chain responses and injects block headers or chain
//! logs through [`chain`](TestRuntime::chain), advances guest time through
//! [`clock`](TestRuntime::clock), reads what a module wrote through
//! [`store`](TestRuntime::store), and reads runs and log pages through
//! [`logs`](TestRuntime::logs), then [`shutdown`](TestRuntime::shutdown) and
//! [`wait`](TestRuntime::wait).
//!
//! Events dispatch on the spawned event-loop task, so a test injects an event
//! and then awaits an observable effect;
//! [`wait_for_log`](TestRuntime::wait_for_log) polls the log pipeline for one.
//!
//! The extension slot is the lattice type parameter: pass an `Ext` payload
//! and any [`Extension`]s through
//! [`builder_with_ext`](TestRuntime::builder_with_ext) to drive an extension
//! crate's backend through the same harness.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use alloy_rpc_types_eth::{Header, Log};

use super::clock::ManualClock;
use super::{MockChainProvider, MockStateStore, MockTypes, Prebuilt};
use crate::builder::{RuntimeBuilder, RuntimeHandle};
use crate::engine_config::EngineConfig;
use crate::host::component::ComponentsBuilder;
use crate::host::extension::Extension;
use crate::host::logs::{LogPipeline, LogRecord};

/// Where the module manifest comes from.
enum ManifestSource {
    /// No manifest; the loader falls back to a sibling `module.toml`.
    None,
    /// An existing manifest file.
    Path(PathBuf),
    /// Manifest TOML written to a temp file at launch.
    Inline(String),
}

/// Builder for a [`TestRuntime`]. Program the mock backends through
/// [`chain`](Self::chain) / [`store`](Self::store) / [`clock`](Self::clock)
/// before [`launch`](Self::launch); the launched handle shares the same
/// backends.
pub struct TestRuntimeBuilder<E = ()>
where
    E: Clone + Send + Sync + 'static,
{
    wasm: PathBuf,
    manifest: ManifestSource,
    extensions: Vec<Extension<MockTypes<E>>>,
    ext: E,
    chain: MockChainProvider,
    store: MockStateStore,
    clock: ManualClock,
}

impl TestRuntime<()> {
    /// Start a harness for the module at `wasm`, with an empty extension slot.
    pub fn builder(wasm: impl Into<PathBuf>) -> TestRuntimeBuilder<()> {
        TestRuntime::builder_with_ext(wasm, ())
    }
}

impl<E: Clone + Send + Sync + 'static> TestRuntime<E> {
    /// Start a harness whose lattice binds `ext` as the extension payload.
    /// Pair with [`extension`](TestRuntimeBuilder::extension) to register the
    /// extension's linker hook and capability namespace.
    pub fn builder_with_ext(wasm: impl Into<PathBuf>, ext: E) -> TestRuntimeBuilder<E> {
        TestRuntimeBuilder {
            wasm: wasm.into(),
            manifest: ManifestSource::None,
            extensions: Vec::new(),
            ext,
            chain: MockChainProvider::new(),
            store: MockStateStore::new(),
            clock: ManualClock::new(),
        }
    }
}

impl<E: Clone + Send + Sync + 'static> TestRuntimeBuilder<E> {
    /// Load the manifest from an existing file.
    pub fn manifest_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.manifest = ManifestSource::Path(path.into());
        self
    }

    /// Write `toml` to a temp file at launch and load the module from it.
    pub fn manifest_inline(mut self, toml: impl Into<String>) -> Self {
        self.manifest = ManifestSource::Inline(toml.into());
        self
    }

    /// Register an extension's linker hook and capability namespace.
    pub fn extension(mut self, extension: Extension<MockTypes<E>>) -> Self {
        self.extensions.push(extension);
        self
    }

    /// Register several extensions at once.
    pub fn extensions(
        mut self,
        extensions: impl IntoIterator<Item = Extension<MockTypes<E>>>,
    ) -> Self {
        self.extensions.extend(extensions);
        self
    }

    /// The mock chain backend, for programming request responses before
    /// launch. The launched handle shares this instance.
    pub fn chain(&self) -> &MockChainProvider {
        &self.chain
    }

    /// The mock state store. The launched handle shares this instance.
    pub fn store(&self) -> &MockStateStore {
        &self.store
    }

    /// The manual clock installed as the per-store WASI clock override.
    pub fn clock(&self) -> &ManualClock {
        &self.clock
    }

    /// Open the module and start the runtime. Composes entirely through the
    /// public builder path over [`MockTypes`].
    pub async fn launch(self) -> anyhow::Result<TestRuntime<E>> {
        // A temp directory roots any inline manifest and stands in as the
        // (unused, in-memory backends) state directory.
        let tmp = tempfile::tempdir()?;

        let manifest = match self.manifest {
            ManifestSource::None => None,
            ManifestSource::Path(path) => Some(path),
            ManifestSource::Inline(toml) => {
                let path = tmp.path().join("module.toml");
                std::fs::write(&path, toml)?;
                Some(path)
            }
        };

        let mut config = EngineConfig::default();
        config.engine.state_dir = tmp.path().to_path_buf();

        let handle = RuntimeBuilder::new(&config)
            .with_types::<MockTypes<E>>()
            .with_extensions(self.extensions)
            .with_module_source(Some(self.wasm), manifest)
            .with_wasi_clocks(self.clock.as_override())
            .with_components(ComponentsBuilder::new(
                Prebuilt(self.chain.clone()),
                Prebuilt(self.store.clone()),
                Prebuilt(self.ext.clone()),
            ))
            .with_add_ons(&[])
            .launch()
            .await?;

        Ok(TestRuntime {
            handle,
            chain: self.chain,
            store: self.store,
            clock: self.clock,
            ext: self.ext,
            _tmp: tmp,
        })
    }
}

/// A launched in-process runtime over the mock assembly. Holds the running
/// [`RuntimeHandle`] and the retained mock handles; dropping it fires the
/// shutdown trigger.
pub struct TestRuntime<E = ()> {
    handle: RuntimeHandle,
    chain: MockChainProvider,
    store: MockStateStore,
    clock: ManualClock,
    ext: E,
    // Holds any inline manifest for the run; dropped with the handle.
    _tmp: tempfile::TempDir,
}

impl<E> TestRuntime<E> {
    /// The mock chain backend: program request responses, inject block
    /// headers with [`push_block`](Self::push_block), and inject logs with
    /// [`push_chain_log`](Self::push_chain_log).
    pub fn chain(&self) -> &MockChainProvider {
        &self.chain
    }

    /// The mock state store, for asserting on what a module wrote.
    pub fn store(&self) -> &MockStateStore {
        &self.store
    }

    /// The manual clock driving guest-visible time.
    pub fn clock(&self) -> &ManualClock {
        &self.clock
    }

    /// The extension payload bound into the lattice ext slot.
    pub fn ext(&self) -> &E {
        &self.ext
    }

    /// The shared log pipeline: read runs and log pages here.
    pub fn logs(&self) -> &LogPipeline {
        self.handle.logs()
    }

    /// Deliver a block header to the module's open block subscription.
    pub fn push_block(&self, header: Header) {
        self.chain.push_block(header);
    }

    /// Deliver a log to the module's open chain-log subscription.
    pub fn push_chain_log(&self, log: Log) {
        self.chain.push_chain_log(log);
    }

    /// Poll the log pipeline until a `module` record's message contains
    /// `needle`, returning that record. Errors if none appears within a few
    /// seconds. Use it to await an injected event's effect: the record only
    /// lands once the event-loop task has dispatched the event.
    pub async fn wait_for_log(&self, module: &str, needle: &str) -> anyhow::Result<LogRecord> {
        let logs = self.logs();
        poll(Duration::from_secs(5), || {
            logs.list_runs(module).into_iter().find_map(|meta| {
                logs.read(&meta.run, 0)
                    .records
                    .into_iter()
                    .find(|record| record.message.contains(needle))
            })
        })
        .await
        .ok_or_else(|| anyhow::anyhow!("no {module} log record matched {needle:?} in time"))
    }

    /// Signal the event loop to stop; the in-flight dispatch finishes first.
    pub fn shutdown(&mut self) {
        self.handle.shutdown();
    }

    /// Await the event loop's completion after a [`shutdown`](Self::shutdown).
    pub async fn wait(self) -> anyhow::Result<()> {
        self.handle.wait().await
    }
}

/// Re-poll `f` on a short cadence until it yields a value or `timeout`
/// elapses. Sleeping yields to the runtime so the spawned event-loop task
/// makes progress between polls.
async fn poll<T>(timeout: Duration, mut f: impl FnMut() -> Option<T>) -> Option<T> {
    let start = Instant::now();
    loop {
        if let Some(value) = f() {
            return Some(value);
        }
        if start.elapsed() >= timeout {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::host::extension::Extension;
    use crate::manifest::NamespaceCaps;

    /// The pre-built example module, or `None` (with a skip note) when the
    /// wasm fixture is not built.
    fn example_wasm_or_skip() -> Option<PathBuf> {
        let wasm = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("repo root")
            .join("target/wasm32-wasip2/release/example.wasm");
        if wasm.exists() {
            Some(wasm)
        } else {
            eprintln!(
                "SKIP: {} not found - run `just build-module` to enable the harness E2E tests",
                wasm.display()
            );
            None
        }
    }

    /// A block-only manifest for the example module on `chain_id`.
    fn block_manifest(name: &str, chain_id: u64) -> String {
        format!(
            r#"
[module]
name = "{name}"

[capabilities]
required = ["logging"]

[[subscription]]
kind     = "block"
chain_id = {chain_id}
"#
        )
    }

    /// A header carrying just the block number the event loop projects onto
    /// the dispatched block.
    fn header_numbered(number: u64) -> Header {
        let mut header: Header = Header::default();
        header.inner.number = number;
        header
    }

    /// End-to-end through the harness: launch the example module from an
    /// inline manifest, inject a block header, and read the module's log line
    /// back off the pipeline. Locks the harness ergonomics against the boot
    /// plus dispatch plus log-read path.
    #[tokio::test]
    async fn harness_launches_dispatches_and_reads_logs() {
        let Some(wasm) = example_wasm_or_skip() else {
            return;
        };

        let mut rt = TestRuntime::builder(wasm)
            .manifest_inline(block_manifest("example", 1))
            .launch()
            .await
            .expect("launch example over the harness");

        rt.push_block(header_numbered(19_000_000));
        let record = rt
            .wait_for_log("example", "block 19000000")
            .await
            .expect("the on_event log line lands after dispatch");
        assert_eq!(
            record.source,
            crate::host::logs::LogSource::HostInterface,
            "the example module logs through the host interface",
        );

        rt.shutdown();
        rt.wait().await.expect("clean shutdown");
    }

    /// The extension slot threads through the same harness: a trivial
    /// extension (no-op linker hook, empty capability namespace) and an ext
    /// payload compose through the mock lattice, the module boots and
    /// dispatches, and the harness hands the payload back.
    #[tokio::test]
    async fn harness_threads_an_extension_and_ext_payload() {
        let Some(wasm) = example_wasm_or_skip() else {
            return;
        };

        let calls = Arc::new(AtomicUsize::new(0));
        let hooked = calls.clone();
        let extension = Extension::<MockTypes<Arc<AtomicUsize>>> {
            link: Arc::new(move |_linker| {
                hooked.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }),
            capabilities: NamespaceCaps {
                prefix: "test:ext/",
                ifaces: &[],
            },
        };

        let mut rt = TestRuntime::builder_with_ext(wasm, calls.clone())
            .extension(extension)
            .manifest_inline(block_manifest("example", 1))
            .launch()
            .await
            .expect("launch with a trivial extension");

        // The extension's linker hook ran during boot, and the payload the
        // harness threaded is the one it hands back.
        assert!(
            calls.load(Ordering::SeqCst) >= 1,
            "the extension linker hook ran at boot",
        );
        assert!(Arc::ptr_eq(rt.ext(), &calls), "the ext payload is retained");

        rt.push_block(header_numbered(21_000_000));
        rt.wait_for_log("example", "block 21000000")
            .await
            .expect("the module dispatched under the extension-bearing lattice");

        rt.shutdown();
        rt.wait().await.expect("clean shutdown");
    }
}
