//! End-to-end over a forked mainnet: the shipped `shepherd` binary
//! indexes a real `ConditionalOrderCreated` from the deployed fork
//! registry and polls it through the structured generator wire.
//!
//! This drives the binary rather than `BootScenario`, because that
//! harness wires a `FakeNode` and has no seam for a real endpoint. Only
//! the binary reads `[chains.1] rpc_url`, so only the binary can be
//! pointed at anvil.
//!
//! Skipped unless `SHEPHERD_FORK_RPC` names a mainnet endpoint to fork,
//! since CI reaches no such node. Needs `anvil` and `cast` on `PATH` and
//! `just build-modules` already run.

use std::io::{BufRead, BufReader};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::Duration;

/// The fork's mainnet registry, as `modules/ccow-monitor/component.toml`
/// pins it. Deployed at block 25674440 and never used, so every log the
/// run sees is one it created.
const REGISTRY: &str = "0xf9ba6F64c9b41Df1cEe76A50e2039D3847064232";

/// `OwnedTWAP`, deployed by the same CREATE2 broadcast as the registry.
/// `Owned` gates only `setDescriptor` and `setModule`, so registering
/// against it needs no owner.
const HANDLER: &str = "0x4e17a65d14e7f37d2a9f0389f17efd41aaa64c91";

/// `CowAccount7702`, the delegate #658 names for the acceptance smoke.
///
/// The registry builds an ERC-1271 signature only for a POST, and that
/// path asks the owner for `supportsInterface`. A bare EOA answers with
/// empty returndata and the whole poll reverts with no payload; this
/// account answers `FnSelectorNotRecognized`, a real revert, which
/// `_buildSignature` catches and treats as a non-Safe wallet.
const ACCOUNT_7702: &str = "0x15236F06922A288B68e57Ab42e397920a3F3Bb99";

const WETH: &str = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2";
const USDC: &str = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48";

/// How long to wait on any one engine log line.
const LOG_TIMEOUT: Duration = Duration::from_secs(90);

/// A child killed when the test ends, however it ends.
struct Reaped(Child);

impl Drop for Reaped {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A port free at bind time. Racy by nature, which is why each spawn
/// waits for its own readiness rather than assuming the port took.
fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("bind an ephemeral port")
        .local_addr()
        .expect("the listener has an address")
        .port()
}

/// The orderbook mock, beside the binary under test.
///
/// `CARGO_BIN_EXE_*` covers only this package's binaries, and the mock
/// belongs to `tools/orderbook-mock`, so it is found as a sibling of the
/// engine rather than named directly.
fn orderbook_mock_bin() -> PathBuf {
    let engine = PathBuf::from(env!("CARGO_BIN_EXE_shepherd"));
    let mock = engine
        .parent()
        .expect("the engine binary sits in a target directory")
        .join("orderbook-mock");
    assert!(
        mock.exists(),
        "{} is missing; run `cargo build -p orderbook-mock` first",
        mock.display(),
    );
    // Cargo does not rebuild another package's binary for this test, and
    // a stale mock fails as a receipt mismatch rather than as a stale
    // build, which is a long way from the cause.
    let source = workspace_root().join("tools/orderbook-mock/src/main.rs");
    if let (Ok(built), Ok(src)) = (
        mock.metadata().and_then(|m| m.modified()),
        source.metadata().and_then(|m| m.modified()),
    ) {
        assert!(
            built >= src,
            "{} is older than its source; run `cargo build -p orderbook-mock`",
            mock.display(),
        );
    }
    mock
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("crates/<pkg> sits two levels under the workspace root")
        .to_owned()
}

/// The endpoint to fork, or `None` when this run cannot reach one.
fn fork_rpc_or_skip() -> Option<String> {
    let rpc = std::env::var("SHEPHERD_FORK_RPC")
        .ok()
        .filter(|s| !s.is_empty())?;
    for tool in ["anvil", "cast"] {
        if Command::new(tool).arg("--version").output().is_err() {
            eprintln!("skipping: {tool} is not on PATH");
            return None;
        }
    }
    Some(rpc)
}

fn cast(args: &[&str]) -> String {
    let out = Command::new("cast")
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("cast {args:?}: {e}"));
    assert!(
        out.status.success(),
        "cast {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr),
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// Fork `rpc` at head and wait until the fork answers.
fn start_anvil(rpc: &str, port: u16) -> Reaped {
    // Owned before the readiness loop, so the panic below still reaps it.
    let child = Reaped(
        Command::new("anvil")
            .args(["--fork-url", rpc, "--port", &port.to_string(), "--silent"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn anvil"),
    );
    let url = format!("http://127.0.0.1:{port}");
    for _ in 0..60 {
        if Command::new("cast")
            .args(["chain-id", "--rpc-url", &url])
            .output()
            .is_ok_and(|o| o.status.success())
        {
            return child;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    panic!("anvil did not answer on {url}");
}

/// Register one TWAP conditional order against the forked registry.
///
/// `t0` is derived from the fork's own clock and backdated, so part 0 is
/// tradeable at once. A pinned `t0` puts the part index far past `n` and
/// every poll then reports the series finished.
fn register_order(anvil: &str, owner: &str) -> String {
    let now: u64 = cast(&[
        "block",
        "latest",
        "--field",
        "timestamp",
        "--rpc-url",
        anvil,
    ])
    .parse()
    .expect("a numeric block timestamp");
    let t0 = now - 60;
    let static_input = cast(&[
        "abi-encode",
        "f((address,address,address,uint256,uint256,uint256,uint256,uint256,uint256,bytes32))",
        &format!(
            "({WETH},{USDC},{owner},1000000000000000,1000000,{t0},2,600,0,\
             0x0000000000000000000000000000000000000000000000000000000000000000)"
        ),
    ]);
    let calldata = cast(&[
        "calldata",
        "create((address,bytes32,bytes),bool)",
        &format!(
            "({HANDLER},0x0000000000000000000000000000000000000000000000000000000000000001,{static_input})"
        ),
        "true",
    ]);
    cast(&[
        "send",
        "--unlocked",
        "--from",
        owner,
        REGISTRY,
        &calldata,
        "--rpc-url",
        anvil,
        "--json",
    ]);
    static_input
}

/// An engine config pointing at this run's anvil and orderbook mock.
fn engine_config(dir: &std::path::Path, anvil: &str, orderbook: &str, manifest: &str) -> PathBuf {
    let root = workspace_root();
    let wasm = root.join("target/wasm32-wasip2/release/ccow_monitor.wasm");
    assert!(
        wasm.exists(),
        "{} is missing; run `just build-modules` first",
        wasm.display(),
    );
    let state = dir.join("state");
    let config = format!(
        r#"
[engine]
state_dir = "{state}"
log_level = "info"
# The wasm is rebuilt for every run, so a pin would have to be
# regenerated each time; the shipped dev configs make the same choice.
require_component_digest = false

[chains.1]
rpc_url = "{anvil}"

[[modules]]
id = "ccow-monitor"
path = "{wasm}"
manifest = "{manifest}"

[extensions.videre.venues.cow]
chain = 1
orderbook_url = "{orderbook}"
owner = "0x0000000000000000000000000000000000000001"
"#,
        wasm = wasm.display(),
        state = state.display(),
    );
    let path = dir.join("engine.anvil.toml");
    std::fs::write(&path, config).expect("write the engine config");
    path
}

/// The shipped manifest with `start_block` moved to the fork block.
///
/// The shipped value is the registry's deployment block, roughly 258000
/// behind head, and the registry holds no logs before the fork anyway.
fn manifest_at(dir: &std::path::Path, from_block: u64) -> PathBuf {
    let shipped = workspace_root().join("modules/ccow-monitor/component.toml");
    let text = std::fs::read_to_string(&shipped).expect("read the shipped manifest");
    let rewritten = text
        .lines()
        .map(|line| {
            if line.trim_start().starts_with("start_block") {
                format!("start_block = {from_block}")
            } else {
                line.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let path = dir.join("component.toml");
    std::fs::write(&path, rewritten).expect("write the test manifest");
    path
}

/// Stream a child's stdout and stderr into one channel so the test can
/// wait on lines without blocking on a pipe that never closes.
///
/// Both, because the engine's tracing subscriber picks its own stream
/// and a test that watched only one would hang for the full timeout with
/// nothing to show.
fn stream_lines(child: &mut Child) -> Receiver<String> {
    let (tx, rx) = channel();
    for pipe in [
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn std::io::Read + Send>),
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn std::io::Read + Send>),
    ]
    .into_iter()
    .flatten()
    {
        let tx = tx.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(pipe).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    return;
                }
            }
        });
    }
    rx
}

/// Wait for a line containing `needle` and return it, echoing what
/// arrived first when it never does.
fn wait_for(rx: &Receiver<String>, needle: &str, seen: &mut Vec<String>) -> String {
    let deadline = std::time::Instant::now() + LOG_TIMEOUT;
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        match rx.recv_timeout(left) {
            Ok(line) => {
                if line.contains(needle) {
                    seen.push(line.clone());
                    return line;
                }
                seen.push(line);
            }
            Err(RecvTimeoutError::Timeout) => {
                panic!(
                    "no engine log line contained {needle:?}; saw:\n  {}",
                    seen.join("\n  ")
                )
            }
            Err(RecvTimeoutError::Disconnected) => {
                panic!(
                    "the engine exited before logging {needle:?}; saw:\n  {}",
                    seen.join("\n  ")
                )
            }
        }
    }
}

/// The whole chain leg: a real registration on the forked registry is
/// indexed, then polled through `getTradeableOrderWithSignature` against
/// the deployed `OwnedTWAP`.
#[test]
fn ccow_monitor_indexes_and_polls_a_real_owned_twap() {
    let Some(rpc) = fork_rpc_or_skip() else {
        return;
    };
    let dir = std::env::temp_dir().join(format!("shepherd-anvil-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create the run directory");

    let anvil_port = free_port();
    let _anvil = start_anvil(&rpc, anvil_port);
    let anvil = format!("http://127.0.0.1:{anvil_port}");

    let ob_port = free_port();
    let _ob = Reaped(
        Command::new(orderbook_mock_bin())
            .args(["--port", &ob_port.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn the orderbook mock"),
    );

    let fork_block: u64 = cast(&["block-number", "--rpc-url", &anvil])
        .parse()
        .expect("a numeric block number");
    let manifest = manifest_at(&dir, fork_block);
    let config = engine_config(
        &dir,
        &anvil,
        &format!("http://127.0.0.1:{ob_port}"),
        &manifest.display().to_string(),
    );

    let mut engine = Command::new(env!("CARGO_BIN_EXE_shepherd"))
        .args(["--engine-config", &config.display().to_string()])
        .current_dir(workspace_root())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn the shepherd binary");
    let lines = stream_lines(&mut engine);
    let _engine = Reaped(engine);

    let mut seen = Vec::new();
    wait_for(&lines, "ccow-monitor", &mut seen);

    // anvil's first unlocked account owns the registration.
    let owner = cast(&["rpc", "eth_accounts", "--rpc-url", &anvil])
        .trim_matches(|c| c == '[' || c == ']' || c == '"')
        .split(',')
        .next()
        .expect("anvil funds at least one account")
        .trim_matches('"')
        .to_owned();
    // EIP-7702 delegation designator: 0xef0100 ++ implementation. Set
    // directly rather than by authorisation tuple, because the test only
    // needs the code to be there, not the signing ceremony that puts it
    // there on a real chain.
    cast(&[
        "rpc",
        "anvil_setCode",
        &owner,
        &format!("0xef0100{}", ACCOUNT_7702.trim_start_matches("0x")),
        "--rpc-url",
        &anvil,
    ]);
    register_order(&anvil, &owner);

    wait_for(&lines, "indexed commitment:", &mut seen);

    // The poll fires on a block dispatch, so the registration needs one
    // after it. Mining is cheap; the engine's own poller drives the rest.
    for _ in 0..6 {
        cast(&["rpc", "evm_mine", "--rpc-url", &anvil]);
        std::thread::sleep(Duration::from_millis(500));
    }

    // `poll {key} -> {outcome}` is the module's own line, so this is the
    // structured generator wire answering over a real eth_call against
    // the deployed `OwnedTWAP`, not a fixture.
    let polled = wait_for(&lines, "poll commitment:", &mut seen);
    for bad in ["did not decode", "eth_call failed"] {
        assert!(
            !seen.iter().any(|l| l.contains(bad)),
            "the poll leg reported {bad:?}:\n  {}",
            seen.join("\n  "),
        );
    }
    assert!(
        polled.contains("-> Post"),
        "the generator posted, so the module must too; got: {polled}",
    );

    // And the post reaches the venue, closing the loop through the
    // borsh body, the journal reservation and the orderbook adapter.
    wait_for(&lines, "submitted", &mut seen);

    let _ = std::fs::remove_dir_all(&dir);
}
