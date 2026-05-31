mod manifest;

use std::path::PathBuf;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use wasmtime::component::{Component, Linker, ResourceTable};
use wasmtime::error::Context as _;
use wasmtime::{Engine, Store};
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

// Both packages are listed explicitly so wit-parser can resolve the
// cross-package reference natively — no vendored deps/ tree needed.
// World name is fully qualified.
wasmtime::component::bindgen!({
    path: ["../../wit/nexum-host", "../../wit/shepherd-cow"],
    world: "shepherd:cow/shepherd",
    imports: { default: async },
    exports: { default: async },
});

use nexum::host::types::HostErrorKind;

struct HostState {
    wasi: WasiCtx,
    table: ResourceTable,
    /// Origin for `clock::monotonic-ns`. Differences between successive
    /// readings are the only meaningful values.
    monotonic_baseline: Instant,
    /// Per-module `[capabilities.http].allow` allowlist (from nexum.toml).
    /// Consulted by `http::fetch` before any outbound call.
    http_allowlist: Vec<String>,
}

impl WasiView for HostState {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

fn unimplemented(domain: &str, detail: impl Into<String>) -> HostError {
    HostError {
        domain: domain.into(),
        kind: HostErrorKind::Unsupported,
        code: 501,
        message: detail.into(),
        data: None,
    }
}

// -- Stub implementations for host interfaces --

impl nexum::host::types::Host for HostState {}

impl shepherd::cow::cow_api::Host for HostState {
    async fn request(
        &mut self,
        _chain_id: u64,
        method: String,
        path: String,
        _body: Option<String>,
    ) -> Result<String, HostError> {
        let start = Instant::now();
        eprintln!("[cow-api] {method} {path}");
        let result = Err(unimplemented(
            "cow-api",
            format!("not implemented: {method} {path}"),
        ));
        eprintln!("[timing] cow-api::request: {:?}", start.elapsed());
        result
    }

    async fn submit_order(
        &mut self,
        _chain_id: u64,
        _order_data: Vec<u8>,
    ) -> Result<String, HostError> {
        let start = Instant::now();
        eprintln!("[cow-api] submit-order");
        let result = Err(unimplemented("cow-api", "submit-order not implemented"));
        eprintln!("[timing] cow-api::submit-order: {:?}", start.elapsed());
        result
    }
}

impl nexum::host::chain::Host for HostState {
    async fn request(
        &mut self,
        _chain_id: u64,
        method: String,
        _params: String,
    ) -> Result<String, HostError> {
        let start = Instant::now();
        eprintln!("[chain] request: {method}");
        let result = Err(HostError {
            domain: "chain".into(),
            kind: HostErrorKind::Unsupported,
            code: -32601,
            message: format!("method not implemented: {method}"),
            data: None,
        });
        eprintln!("[timing] chain::request: {:?}", start.elapsed());
        result
    }

    async fn request_batch(
        &mut self,
        chain_id: u64,
        requests: Vec<nexum::host::chain::RpcRequest>,
    ) -> Result<Vec<nexum::host::chain::RpcResult>, HostError> {
        let start = Instant::now();
        eprintln!("[chain] request-batch: {} calls", requests.len());
        let mut out = Vec::with_capacity(requests.len());
        for req in requests {
            match self.request(chain_id, req.method, req.params).await {
                Ok(s) => out.push(nexum::host::chain::RpcResult::Ok(s)),
                Err(e) => out.push(nexum::host::chain::RpcResult::Err(e)),
            }
        }
        eprintln!("[timing] chain::request-batch: {:?}", start.elapsed());
        Ok(out)
    }
}

impl nexum::host::identity::Host for HostState {
    async fn accounts(&mut self) -> Result<Vec<Vec<u8>>, HostError> {
        let start = Instant::now();
        eprintln!("[identity] accounts");
        let result = Ok(vec![]);
        eprintln!("[timing] identity::accounts: {:?}", start.elapsed());
        result
    }

    async fn sign(&mut self, _account: Vec<u8>, _message: Vec<u8>) -> Result<Vec<u8>, HostError> {
        let start = Instant::now();
        eprintln!("[identity] sign");
        let result = Err(unimplemented("identity", "sign not implemented"));
        eprintln!("[timing] identity::sign: {:?}", start.elapsed());
        result
    }

    async fn sign_typed_data(
        &mut self,
        _account: Vec<u8>,
        _typed_data: String,
    ) -> Result<Vec<u8>, HostError> {
        let start = Instant::now();
        eprintln!("[identity] sign-typed-data");
        let result = Err(unimplemented("identity", "sign-typed-data not implemented"));
        eprintln!("[timing] identity::sign-typed-data: {:?}", start.elapsed());
        result
    }
}

impl nexum::host::local_store::Host for HostState {
    async fn get(&mut self, key: String) -> Result<Option<Vec<u8>>, HostError> {
        let start = Instant::now();
        eprintln!("[local-store] get: {key}");
        let result = Ok(None);
        eprintln!("[timing] local-store::get: {:?}", start.elapsed());
        result
    }

    async fn set(&mut self, key: String, _value: Vec<u8>) -> Result<(), HostError> {
        let start = Instant::now();
        eprintln!("[local-store] set: {key}");
        let result = Ok(());
        eprintln!("[timing] local-store::set: {:?}", start.elapsed());
        result
    }

    async fn delete(&mut self, key: String) -> Result<(), HostError> {
        let start = Instant::now();
        eprintln!("[local-store] delete: {key}");
        let result = Ok(());
        eprintln!("[timing] local-store::delete: {:?}", start.elapsed());
        result
    }

    async fn list_keys(&mut self, prefix: String) -> Result<Vec<String>, HostError> {
        let start = Instant::now();
        eprintln!("[local-store] list-keys: {prefix}");
        let result = Ok(vec![]);
        eprintln!("[timing] local-store::list-keys: {:?}", start.elapsed());
        result
    }
}

impl nexum::host::remote_store::Host for HostState {
    async fn upload(&mut self, _data: Vec<u8>) -> Result<Vec<u8>, HostError> {
        let start = Instant::now();
        let result = Err(unimplemented("remote-store", "upload not implemented"));
        eprintln!("[timing] remote-store::upload: {:?}", start.elapsed());
        result
    }

    async fn download(&mut self, _reference: Vec<u8>) -> Result<Vec<u8>, HostError> {
        let start = Instant::now();
        let result = Err(unimplemented("remote-store", "download not implemented"));
        eprintln!("[timing] remote-store::download: {:?}", start.elapsed());
        result
    }

    async fn read_feed(
        &mut self,
        _owner: Vec<u8>,
        _topic: Vec<u8>,
    ) -> Result<Option<Vec<u8>>, HostError> {
        let start = Instant::now();
        let result = Err(unimplemented("remote-store", "read-feed not implemented"));
        eprintln!("[timing] remote-store::read-feed: {:?}", start.elapsed());
        result
    }

    async fn write_feed(&mut self, _topic: Vec<u8>, _data: Vec<u8>) -> Result<Vec<u8>, HostError> {
        let start = Instant::now();
        let result = Err(unimplemented("remote-store", "write-feed not implemented"));
        eprintln!("[timing] remote-store::write-feed: {:?}", start.elapsed());
        result
    }
}

impl nexum::host::messaging::Host for HostState {
    async fn publish(&mut self, content_topic: String, _payload: Vec<u8>) -> Result<(), HostError> {
        let start = Instant::now();
        eprintln!("[messaging] publish: {content_topic}");
        let result = Err(unimplemented("messaging", "publish not implemented"));
        eprintln!("[timing] messaging::publish: {:?}", start.elapsed());
        result
    }

    async fn query(
        &mut self,
        content_topic: String,
        _start_time: Option<u64>,
        _end_time: Option<u64>,
        _limit: Option<u32>,
    ) -> Result<Vec<nexum::host::types::Message>, HostError> {
        let start = Instant::now();
        eprintln!("[messaging] query: {content_topic}");
        let result = Ok(vec![]);
        eprintln!("[timing] messaging::query: {:?}", start.elapsed());
        result
    }
}

impl nexum::host::logging::Host for HostState {
    async fn log(&mut self, level: nexum::host::logging::Level, message: String) {
        let start = Instant::now();
        let level_str = match level {
            nexum::host::logging::Level::Trace => "TRACE",
            nexum::host::logging::Level::Debug => "DEBUG",
            nexum::host::logging::Level::Info => "INFO",
            nexum::host::logging::Level::Warn => "WARN",
            nexum::host::logging::Level::Error => "ERROR",
        };
        eprintln!("[{level_str}] {message}");
        eprintln!("[timing] logging::log: {:?}", start.elapsed());
    }
}

// -- Additive 0.2 capabilities --

impl nexum::host::clock::Host for HostState {
    async fn now_ms(&mut self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }

    async fn monotonic_ns(&mut self) -> u64 {
        self.monotonic_baseline.elapsed().as_nanos() as u64
    }
}

impl nexum::host::random::Host for HostState {
    async fn fill(&mut self, len: u32) -> Vec<u8> {
        let mut buf = vec![0u8; len as usize];
        // getrandom 0.4: fill() returns Result<(), Error>. CSPRNG failures
        // are exceptionally rare on supported platforms; on failure we
        // return zero-filled bytes — guests that need a strong-failure
        // signal should use identity or chain primitives instead.
        let _ = getrandom::fill(&mut buf);
        buf
    }
}

impl nexum::host::http::Host for HostState {
    async fn fetch(
        &mut self,
        req: nexum::host::http::Request,
    ) -> Result<nexum::host::http::Response, HostError> {
        let start = Instant::now();
        eprintln!("[http] {} {}", req.method, req.url);

        // Manifest allowlist enforcement runs before any I/O. Hosts that
        // never link a manifest leave `http_allowlist` empty, which denies
        // every request — matching the "no implicit network" stance.
        let host = match manifest::extract_host(&req.url) {
            Some(h) => h,
            None => {
                eprintln!("[timing] http::fetch: {:?}", start.elapsed());
                return Err(HostError {
                    domain: "http".into(),
                    kind: HostErrorKind::InvalidInput,
                    code: 0,
                    message: format!("not an http(s) URL: {}", req.url),
                    data: None,
                });
            }
        };
        if !manifest::host_allowed(host, &self.http_allowlist) {
            eprintln!("[http] denied by allowlist: {host}");
            eprintln!("[timing] http::fetch: {:?}", start.elapsed());
            return Err(HostError {
                domain: "http".into(),
                kind: HostErrorKind::Denied,
                code: 0,
                message: format!(
                    "host {host} not in [capabilities.http].allow; \
                     add it to nexum.toml to permit"
                ),
                data: None,
            });
        }

        // 0.2: allowlist passed, but the reference runtime does not perform
        // real HTTP yet. Real fetch lands in 0.3.
        let result = Err(unimplemented(
            "http",
            "fetch not implemented in 0.2 reference runtime (allowlist passed)",
        ));
        eprintln!("[timing] http::fetch: {:?}", start.elapsed());
        result
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let wasm_path = args.next().ok_or_else(|| {
        anyhow::anyhow!("usage: nexum-engine <path-to-component.wasm> [<nexum.toml>]")
    })?;
    let explicit_manifest = args.next().map(PathBuf::from);

    println!("nexum-engine: loading component from {wasm_path}");

    // Load the manifest from the explicit path if given, otherwise from
    // `nexum.toml` next to the component file. Missing → fallback (with
    // deprecation warning).
    let manifest_path = explicit_manifest.or_else(|| {
        PathBuf::from(&wasm_path)
            .parent()
            .map(|p| p.join("nexum.toml"))
    });
    let loaded = match manifest_path.as_deref() {
        Some(p) if p.exists() => {
            println!("nexum-engine: loading manifest from {}", p.display());
            manifest::load(p)?
        }
        _ => manifest::fallback_manifest(),
    };

    let mut config = wasmtime::Config::new();
    config.wasm_component_model(true);
    let engine = Engine::new(&config)?;

    let start = Instant::now();
    let component =
        Component::from_file(&engine, &wasm_path).context("failed to load component")?;
    eprintln!("[timing] component load: {:?}", start.elapsed());

    let mut linker = Linker::<HostState>::new(&engine);
    Shepherd::add_to_linker::<HostState, wasmtime::component::HasSelf<HostState>>(
        &mut linker,
        |state| state,
    )?;
    wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;

    let wasi = WasiCtxBuilder::new().inherit_stdio().build();

    let mut store = Store::new(
        &engine,
        HostState {
            wasi,
            table: ResourceTable::new(),
            monotonic_baseline: Instant::now(),
            http_allowlist: loaded.http_allowlist,
        },
    );

    let start = Instant::now();
    let bindings = Shepherd::instantiate_async(&mut store, &component, &linker)
        .await
        .context("failed to instantiate component")?;
    eprintln!("[timing] component instantiate: {:?}", start.elapsed());

    println!("nexum-engine: calling init...");
    // 0.2: [config] is stringly-typed (typed variant deferred to 0.3).
    // Fall back to a single ("name", "<module>") pair if the manifest has
    // no [config] section so the example module still has something to log.
    let config_entries: Config = if loaded.config.is_empty() {
        vec![("name".into(), loaded.manifest.module.name.clone())]
    } else {
        loaded.config
    };
    let start = Instant::now();
    match bindings.call_init(&mut store, &config_entries).await? {
        Ok(()) => println!("nexum-engine: init succeeded"),
        Err(e) => println!(
            "nexum-engine: init failed: {}::{:?} {} ({})",
            e.domain, e.kind, e.message, e.code
        ),
    }
    eprintln!("[timing] call_init: {:?}", start.elapsed());

    // Dispatch a test block event (timestamps are ms since Unix epoch, UTC).
    println!("nexum-engine: dispatching test block event...");
    let block = nexum::host::types::Block {
        chain_id: 1,
        number: 19_000_000,
        hash: vec![0xab; 32],
        timestamp: 1_700_000_000_000,
    };
    let event = nexum::host::types::Event::Block(block);
    let start = Instant::now();
    match bindings.call_on_event(&mut store, &event).await? {
        Ok(()) => println!("nexum-engine: on-event succeeded"),
        Err(e) => println!(
            "nexum-engine: on-event failed: {}::{:?} {} ({})",
            e.domain, e.kind, e.message, e.code
        ),
    }
    eprintln!("[timing] call_on_event: {:?}", start.elapsed());

    println!("nexum-engine: done");
    Ok(())
}
