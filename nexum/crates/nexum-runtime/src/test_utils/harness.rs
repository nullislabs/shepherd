//! In-process test harness: launch one module over the mock assembly and
//! drive it from a test.
//!
//! [`TestRuntime`] wraps the public builder path over [`MockTypes`] with a
//! manually-driven [`ManualClock`]. Program the mocks and read effects
//! through [`chain`](TestRuntime::chain), [`clock`](TestRuntime::clock),
//! [`store`](TestRuntime::store) and [`logs`](TestRuntime::logs). Events
//! dispatch on the spawned event-loop task, so
//! [`wait_for_log`](TestRuntime::wait_for_log) polls for an observable
//! effect. Bind an extension payload through
//! [`builder_with_ext`](TestRuntime::builder_with_ext).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use alloy_rpc_types_eth::{Header, Log};

use super::clock::ManualClock;
use super::{MockChainProvider, MockStateStore, MockTypes, Prebuilt};
use crate::builder::{RuntimeBuilder, RuntimeHandle};
use crate::engine_config::{EngineConfig, ModuleLimits};
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

/// Builder for a [`TestRuntime`]; the launched handle shares the same mock
/// backends.
pub struct TestRuntimeBuilder<E = ()>
where
    E: Clone + Send + Sync + 'static,
{
    wasm: PathBuf,
    manifest: ManifestSource,
    extensions: Vec<Arc<dyn Extension<MockTypes<E>>>>,
    ext: E,
    limits: ModuleLimits,
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
    /// Start a harness binding `ext` as the extension payload; pair with
    /// [`extension`](TestRuntimeBuilder::extension) to register its linker
    /// hook and capability namespace.
    pub fn builder_with_ext(wasm: impl Into<PathBuf>, ext: E) -> TestRuntimeBuilder<E> {
        TestRuntimeBuilder {
            wasm: wasm.into(),
            manifest: ManifestSource::None,
            extensions: Vec::new(),
            ext,
            limits: ModuleLimits::default(),
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

    /// Register an extension.
    pub fn extension(mut self, extension: Arc<dyn Extension<MockTypes<E>>>) -> Self {
        self.extensions.push(extension);
        self
    }

    /// Register several extensions at once.
    pub fn extensions(
        mut self,
        extensions: impl IntoIterator<Item = Arc<dyn Extension<MockTypes<E>>>>,
    ) -> Self {
        self.extensions.extend(extensions);
        self
    }

    /// Replace the `[limits]` the launch resolves; defaults to the
    /// production defaults.
    pub fn limits(mut self, limits: ModuleLimits) -> Self {
        self.limits = limits;
        self
    }

    /// The mock chain backend; the launched handle shares this instance.
    pub fn chain(&self) -> &MockChainProvider {
        &self.chain
    }

    /// The mock state store; the launched handle shares this instance.
    pub fn store(&self) -> &MockStateStore {
        &self.store
    }

    /// The manual clock installed as the per-store WASI clock override.
    pub fn clock(&self) -> &ManualClock {
        &self.clock
    }

    /// Open the module and start the runtime through the public builder path.
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
        config.limits = self.limits;

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

/// A launched in-process runtime over the mock assembly; dropping it fires
/// the shutdown trigger.
pub struct TestRuntime<E = ()> {
    handle: RuntimeHandle,
    chain: MockChainProvider,
    store: MockStateStore,
    clock: ManualClock,
    ext: E,
    // Holds any inline manifest for the lifetime of the harness; dropped
    // when the `TestRuntime` is dropped (or consumed by `wait`).
    _tmp: tempfile::TempDir,
}

impl<E> TestRuntime<E> {
    /// The mock chain backend.
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

    /// The shared log pipeline.
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

    /// Await a `module` log record whose message contains `needle`.
    /// Notification-driven, so it resolves as soon as the dispatched event's
    /// record lands; the 5s bound is a failure backstop.
    pub async fn wait_for_log(&self, module: &str, needle: &str) -> anyhow::Result<LogRecord> {
        let logs = self.logs();
        let appended = logs.appended();
        let matched = async {
            loop {
                // Arm the waiter before reading so an append landing between
                // the read and the await wakes us rather than being lost.
                let notified = appended.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if let Some(record) = logs.list_runs(module).into_iter().find_map(|meta| {
                    logs.read(&meta.run, 0)
                        .records
                        .into_iter()
                        .find(|record| record.message.contains(needle))
                }) {
                    return record;
                }
                notified.await;
            }
        };
        tokio::time::timeout(Duration::from_secs(5), matched)
            .await
            .map_err(|_| anyhow::anyhow!("no {module} log record matched {needle:?} within 5s"))
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

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::host::extension::Extension;
    use crate::manifest::NamespaceCaps;

    /// The pre-built module wasm named `file`, or `None` with a skip note.
    fn module_wasm_or_skip(file: &str) -> Option<PathBuf> {
        // Workspace root: the topmost ancestor with a `Cargo.toml`.
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        let root = manifest
            .ancestors()
            .filter(|d| d.join("Cargo.toml").is_file())
            .last()
            .unwrap_or(manifest);
        let wasm = root.join("target/wasm32-wasip2/release").join(file);
        if wasm.exists() {
            Some(wasm)
        } else {
            eprintln!(
                "SKIP: {} not found - run the `just ci` wasm build to enable the harness E2E tests",
                wasm.display()
            );
            None
        }
    }

    /// The pre-built example module, or `None` with a skip note.
    fn example_wasm_or_skip() -> Option<PathBuf> {
        module_wasm_or_skip("example.wasm")
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

    /// A chain-log manifest for the example module on `chain_id`, with no
    /// address or topic filter so any pushed log matches.
    fn chain_log_manifest(name: &str, chain_id: u64) -> String {
        format!(
            r#"
[module]
name = "{name}"

[capabilities]
required = ["logging"]

[[subscription]]
kind     = "chain-log"
chain_id = {chain_id}
"#
        )
    }

    /// A header carrying just the block number.
    fn header_numbered(number: u64) -> Header {
        let mut header: Header = Header::default();
        header.inner.number = number;
        header
    }

    /// End-to-end: launch the example module from an inline manifest, inject
    /// a block header, and read the module's log line back.
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

    /// End-to-end on the chain-log leg: launch with a `chain-log`
    /// subscription, inject a log, and read the module's log line back.
    #[tokio::test]
    async fn harness_dispatches_chain_logs() {
        let Some(wasm) = example_wasm_or_skip() else {
            return;
        };

        let mut rt = TestRuntime::builder(wasm)
            .manifest_inline(chain_log_manifest("example", 1))
            .launch()
            .await
            .expect("launch example on the chain-log leg");

        rt.push_chain_log(Log::default());
        rt.wait_for_log("example", "received 1 chain-log entries")
            .await
            .expect("the chain-log line lands after dispatch");

        rt.shutdown();
        rt.wait().await.expect("clean shutdown");
    }

    /// The extension slot threads through the harness: a trivial extension
    /// and an ext payload compose, the module dispatches, and the harness
    /// hands the payload back.
    #[tokio::test]
    async fn harness_threads_an_extension_and_ext_payload() {
        let Some(wasm) = example_wasm_or_skip() else {
            return;
        };

        struct CountingExtension(Arc<AtomicUsize>);

        impl Extension<MockTypes<Arc<AtomicUsize>>> for CountingExtension {
            fn namespace(&self) -> &'static str {
                "test"
            }
            fn capabilities(&self) -> NamespaceCaps {
                NamespaceCaps {
                    prefix: "test:ext/",
                    ifaces: &[],
                }
            }
            fn link(
                &self,
                _linker: &mut wasmtime::component::Linker<
                    crate::host::state::HostState<MockTypes<Arc<AtomicUsize>>>,
                >,
            ) -> anyhow::Result<()> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        }

        let calls = Arc::new(AtomicUsize::new(0));
        let extension = Arc::new(CountingExtension(calls.clone()));

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

    /// [`TestRuntimeBuilder::limits`] reaches the launch: a one-byte log ring
    /// keeps only the newest record, evicting the init line.
    #[tokio::test]
    async fn harness_threads_module_limits() {
        use crate::engine_config::LogLimitsSection;

        let Some(wasm) = example_wasm_or_skip() else {
            return;
        };

        let mut rt = TestRuntime::builder(wasm)
            .manifest_inline(block_manifest("example", 1))
            .limits(ModuleLimits {
                logs: LogLimitsSection {
                    bytes_per_run: Some(1),
                    runs_retained: None,
                },
                ..Default::default()
            })
            .launch()
            .await
            .expect("launch example with tight log limits");

        rt.push_block(header_numbered(19_000_000));
        rt.wait_for_log("example", "block 19000000")
            .await
            .expect("the on_event log line lands after dispatch");

        let runs = rt.logs().list_runs("example");
        assert_eq!(runs.len(), 1, "one run recorded");
        let page = rt.logs().read(&runs[0].run, 0);
        assert_eq!(
            page.records.len(),
            1,
            "the one-byte ring keeps only the newest record",
        );
        assert!(page.records[0].message.contains("block 19000000"));

        rt.shutdown();
        rt.wait().await.expect("clean shutdown");
    }

    /// End to end on the chain-request leg: program the mock `eth_call`,
    /// launch price-alert, inject a block, and read its alert line back; the
    /// programmed answer is above threshold, so the module logs TRIGGERED.
    #[tokio::test]
    async fn harness_serves_chain_requests_to_the_module() {
        use crate::host::component::ChainMethod;

        let Some(wasm) = module_wasm_or_skip("price_alert.wasm") else {
            return;
        };

        /// One 32-byte ABI word as zero-padded hex.
        fn word(v: u128) -> String {
            format!("{v:064x}")
        }
        // latestRoundData() -> (roundId, answer, startedAt, updatedAt,
        // answeredInRound), answer = 3000 * 10^8, above the 2500.00
        // threshold below.
        let result = format!(
            "\"0x{}{}{}{}{}\"",
            word(1),
            word(300_000_000_000),
            word(0),
            word(0),
            word(1),
        );

        let builder = TestRuntime::builder(wasm).manifest_inline(
            r#"
[module]
name = "price-alert"

[capabilities]
required = ["logging", "chain"]

[[subscription]]
kind     = "block"
chain_id = 1

[config]
oracle_address = "0x694AA1769357215DE4FAC081bf1f309aDC325306"
decimals = "8"
threshold = "2500.00"
direction = "above"
"#,
        );
        builder.chain().on_method(ChainMethod::EthCall, result);

        let mut rt = builder
            .launch()
            .await
            .expect("launch price-alert over the harness");

        rt.push_block(header_numbered(19_000_000));
        rt.wait_for_log("price-alert", "TRIGGERED")
            .await
            .expect("the alert line lands after the oracle read");

        let requests = rt.chain().recorded_requests();
        assert!(
            requests.iter().any(|r| {
                matches!(r.method, ChainMethod::EthCall)
                    && r.params_json
                        .contains("0x694aa1769357215de4fac081bf1f309adc325306")
            }),
            "the module's eth_call reached the mock, got: {requests:?}",
        );

        rt.shutdown();
        rt.wait().await.expect("clean shutdown");
    }

    /// Both block and chain-log events dispatch in one session: the `biased`
    /// select in `run()` delivers both kinds without starvation.
    #[tokio::test]
    async fn harness_delivers_block_and_chain_log_events_without_starvation() {
        let Some(wasm) = example_wasm_or_skip() else {
            return;
        };

        let mut rt = TestRuntime::builder(wasm)
            .manifest_inline(
                r#"
[module]
name = "example"

[capabilities]
required = ["logging"]

[[subscription]]
kind     = "block"
chain_id = 1

[[subscription]]
kind     = "chain-log"
chain_id = 1
"#,
            )
            .launch()
            .await
            .expect("launch example subscribed to both blocks and chain-logs");

        // Both events are queued before either is awaited, so the biased
        // select genuinely arbitrates between two ready streams — a
        // sequential push→wait→push→wait would never create contention.
        rt.push_block(header_numbered(42));
        rt.push_chain_log(Log::default());

        rt.wait_for_log("example", "block 42 on chain")
            .await
            .expect("block event dispatched");
        rt.wait_for_log("example", "received 1 chain-log entries")
            .await
            .expect("chain-log event dispatched — neither event kind starved the other");

        rt.shutdown();
        rt.wait().await.expect("clean shutdown");
    }

    /// Blocks pushed in order arrive in the same order; the stream, select,
    /// and dispatch path preserve delivery order, asserted on the module's
    /// own log records.
    #[tokio::test]
    async fn harness_delivers_blocks_in_push_order() {
        let Some(wasm) = example_wasm_or_skip() else {
            return;
        };

        let mut rt = TestRuntime::builder(wasm)
            .manifest_inline(block_manifest("example", 1))
            .launch()
            .await
            .expect("launch example over the harness");

        rt.push_block(header_numbered(7));
        rt.push_block(header_numbered(8));
        rt.push_block(header_numbered(9));

        // The last block's log line proves all three dispatches completed.
        rt.wait_for_log("example", "block 9 on chain")
            .await
            .expect("final block dispatched");

        // Recover the per-block log lines in record order and assert the
        // sequence matches the push order exactly.
        let logs = rt.logs();
        let numbers: Vec<u64> = logs
            .list_runs("example")
            .into_iter()
            .flat_map(|meta| logs.read(&meta.run, 0).records)
            .filter_map(|record| {
                let rest = record.message.strip_prefix("block ")?;
                rest.split(' ').next()?.parse().ok()
            })
            .collect();
        assert_eq!(
            numbers,
            vec![7, 8, 9],
            "blocks must be dispatched in push order",
        );

        rt.shutdown();
        rt.wait().await.expect("clean shutdown");
    }

    /// Shutdown never destroys completed work: a picked-up block finishes its
    /// wasmtime call and its log record survives `wait()`. Proven by
    /// re-reading the record after full teardown.
    #[tokio::test]
    async fn harness_shutdown_preserves_completed_dispatch() {
        let Some(wasm) = example_wasm_or_skip() else {
            return;
        };

        let mut rt = TestRuntime::builder(wasm)
            .manifest_inline(block_manifest("example", 1))
            .launch()
            .await
            .expect("launch example over the harness");

        rt.push_block(header_numbered(1));
        rt.wait_for_log("example", "block 1 on chain")
            .await
            .expect("dispatch completed before shutdown");

        let logs = rt.logs().clone();
        rt.shutdown();
        rt.wait().await.expect("no panic or corruption on shutdown");

        let survived = logs.list_runs("example").into_iter().any(|meta| {
            logs.read(&meta.run, 0)
                .records
                .iter()
                .any(|r| r.message.contains("block 1 on chain"))
        });
        assert!(
            survived,
            "the completed dispatch's log record must survive engine teardown",
        );
    }

    /// `[limits.chain].response_body_max_bytes` is enforced on the real
    /// `chain::request` path: an over-cap response is rejected before the
    /// guest copy, and the module observes the typed `invalid-input` fault.
    #[tokio::test]
    async fn harness_enforces_chain_response_cap_on_the_request_path() {
        use crate::engine_config::ChainLimitsSection;
        use crate::host::component::ChainMethod;

        let Some(wasm) = module_wasm_or_skip("price_alert.wasm") else {
            return;
        };

        // A syntactically valid oracle answer, ~330 bytes - far over the
        // 16-byte cap below, so the module must never see it.
        fn word(v: u128) -> String {
            format!("{v:064x}")
        }
        let result = format!(
            "\"0x{}{}{}{}{}\"",
            word(1),
            word(300_000_000_000),
            word(0),
            word(0),
            word(1),
        );

        let builder = TestRuntime::builder(wasm)
            .manifest_inline(
                r#"
[module]
name = "price-alert"

[capabilities]
required = ["logging", "chain"]

[[subscription]]
kind     = "block"
chain_id = 1

[config]
oracle_address = "0x694AA1769357215DE4FAC081bf1f309aDC325306"
decimals = "8"
threshold = "2500.00"
direction = "above"
"#,
            )
            .limits(ModuleLimits {
                chain: ChainLimitsSection {
                    response_body_max_bytes: Some(16),
                },
                ..Default::default()
            });
        builder.chain().on_method(ChainMethod::EthCall, result);

        let mut rt = builder
            .launch()
            .await
            .expect("launch price-alert with a 16-byte chain response cap");

        rt.push_block(header_numbered(19_000_000));
        let record = rt
            .wait_for_log("price-alert", "exceeds the configured cap")
            .await
            .expect("the module logs the guest-visible cap fault");
        assert!(
            record.message.contains("eth_call failed"),
            "the cap surfaces as a failed eth_call, got: {}",
            record.message,
        );

        // The module never saw the oracle answer, so it must not trigger.
        let runs = rt.logs().list_runs("price-alert");
        let triggered = runs.into_iter().any(|meta| {
            rt.logs()
                .read(&meta.run, 0)
                .records
                .iter()
                .any(|r| r.message.contains("TRIGGERED"))
        });
        assert!(!triggered, "an over-cap response must never reach classify");

        rt.shutdown();
        rt.wait().await.expect("clean shutdown");
    }

    /// A dropped block stream is not the end of dispatch: the reconnect task
    /// reopens the subscription after backoff and the re-armed mock resumes
    /// delivery.
    #[tokio::test]
    async fn harness_resumes_dispatch_after_a_dropped_block_stream() {
        let Some(wasm) = example_wasm_or_skip() else {
            return;
        };

        let mut rt = TestRuntime::builder(wasm)
            .manifest_inline(block_manifest("example", 1))
            .launch()
            .await
            .expect("launch example over the harness");

        rt.push_block(header_numbered(41));
        rt.wait_for_log("example", "block 41 on chain")
            .await
            .expect("the pre-drop block dispatches");

        rt.chain().close_block_stream();
        rt.push_block(header_numbered(42));
        rt.wait_for_log("example", "block 42 on chain")
            .await
            .expect("dispatch resumes once the reconnect task reopens the stream");

        rt.shutdown();
        rt.wait().await.expect("clean shutdown");
    }

    /// The guest observes the `WasiClockOverride`: pin the harness clock,
    /// dispatch a block, and check the clock-reader fixture logs the pinned
    /// wall time, not the ambient host clock.
    #[tokio::test]
    async fn harness_guest_observes_the_clock_override() {
        use std::time::{Duration, UNIX_EPOCH};

        let Some(wasm) = module_wasm_or_skip("clock_reader.wasm") else {
            return;
        };

        // A round instant far from the ambient clock: a stale ambient read
        // would land in the 1.7-billion-plus range of the present, so an
        // exact match on this value can only come from the override.
        const PINNED_SECS: u64 = 1_700_000_000;

        let builder = TestRuntime::builder(wasm).manifest_inline(block_manifest("clock-reader", 1));
        builder
            .clock()
            .set(UNIX_EPOCH + Duration::from_secs(PINNED_SECS));

        let mut rt = builder
            .launch()
            .await
            .expect("launch clock-reader over the harness");

        rt.push_block(header_numbered(19_000_000));
        let record = rt
            .wait_for_log("clock-reader", &format!("clock wall {PINNED_SECS}"))
            .await
            .expect("the guest logs its wall-clock reading after dispatch");

        // The line is a host-interface log carrying exactly the pinned
        // seconds, parsed back to guard against a substring false positive.
        assert_eq!(
            record.source,
            crate::host::logs::LogSource::HostInterface,
            "the fixture logs through the host interface",
        );
        let logged: u64 = record
            .message
            .rsplit(' ')
            .next()
            .and_then(|s| s.parse().ok())
            .expect("the log line ends in the wall-clock seconds");
        assert_eq!(
            logged, PINNED_SECS,
            "the guest read the overridden wall clock, not the ambient host clock",
        );

        rt.shutdown();
        rt.wait().await.expect("clean shutdown");
    }
}
