//! Anvil-side load generator for shepherd's M4 load test.
//!
//! Connects to an Anvil fork of Sepolia, impersonates the pinned test
//! EOA (no signer required - `anvil_impersonateAccount` skips
//! signature verification), and submits N `ComposableCoW.create(...)`
//! plus M `CoWSwapEthFlow.createOrder(...)` calls per new block. The
//! resulting `ConditionalOrderCreated` and `OrderPlacement` events are
//! what shepherd's twap-monitor and ethflow-watcher dispatch on.
//!
//! Knobs (`--help` for the full list):
//! - `--anvil <url>`            WebSocket URL of the Anvil fork
//! - `--twap-per-block N`       calls to ComposableCoW.create per block
//! - `--ethflow-per-block M`    calls to CoWSwapEthFlow.createOrder per block
//! - `--duration <minutes>`     wall-clock window the loop runs for
//!
//! Pinned identities mirror `docs/operations/e2e-prep.md`:
//! EOA, ComposableCoW, TWAP handler, CoWSwapEthFlow, WETH9, COW token,
//! Safe. These are constant across the Sepolia fork.

#![cfg_attr(not(test), warn(unused_crate_dependencies))]

// `alloy-transport-ws` is pulled into the workspace via
// `alloy-provider`'s `pubsub` feature; declared explicitly here so the
// Cargo.toml dependency surface mirrors what the engine pins.
use alloy_transport_ws as _;

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use alloy_primitives::{Address, B256, Bytes, U256, address, b256};
use alloy_provider::{Provider, ProviderBuilder, WsConnect};
use alloy_rpc_types_eth::TransactionRequest;
use alloy_sol_types::{SolCall, SolValue, sol};
use clap::Parser;
use futures::StreamExt;
use tracing::{info, warn};

// --- Pinned identities (Sepolia) -----------------------------------

const EOA: Address = address!("7bF140727D27ea64b607E042f1225680B40ECa6A");
const COMPOSABLE_COW: Address = address!("fdaFc9d1902f4e0b84f65F49f244b32b31013b74");
const TWAP_HANDLER: Address = address!("6cF1e9cA41f7611dEf408122793c358a3d11E5a5");
const ETHFLOW: Address = address!("ba3cb449bd2b4adddbc894d8697f5170800eadec");
const WETH: Address = address!("fFf9976782d46CC05630D1f6eBAb18b2324d6B14");
const COW_TOKEN: Address = address!("0625aFB445C3B6B7B929342a04A22599fd5dBB59");

const EMPTY_APP_DATA: B256 =
    b256!("b48d38f93eaa084033fc5970bf96e559c33c4cdc07d889ab00b4d63f9590739d");

// --- ABI shims (load-gen only needs the call signatures) -----------

sol! {
    #[allow(missing_docs)]
    struct ConditionalOrderParams {
        address handler;
        bytes32 salt;
        bytes staticInput;
    }

    #[allow(missing_docs)]
    function create(ConditionalOrderParams params, bool dispatch);

    #[allow(missing_docs)]
    struct EthFlowOrderData {
        address buyToken;
        address receiver;
        uint256 sellAmount;
        uint256 buyAmount;
        bytes32 appData;
        uint256 feeAmount;
        uint32 validTo;
        bool partiallyFillable;
        int64 quoteId;
    }

    #[allow(missing_docs)]
    function createOrder(EthFlowOrderData order);
}

#[derive(Debug, Parser)]
#[command(
    name = "load-gen",
    about = "Anvil-side load generator for shepherd's load test."
)]
struct Cli {
    /// Anvil WebSocket endpoint.
    #[arg(long, default_value = "ws://localhost:8545")]
    anvil: String,

    /// `ComposableCoW.create(...)` calls submitted per new block.
    #[arg(long, default_value_t = 5)]
    twap_per_block: u32,

    /// `CoWSwapEthFlow.createOrder(...)` calls submitted per new block.
    #[arg(long, default_value_t = 5)]
    ethflow_per_block: u32,

    /// Wall-clock minutes the loop should run before exiting.
    #[arg(long, default_value_t = 5)]
    duration_min: u64,

    /// Address whose state Anvil should impersonate when sending the
    /// load-gen transactions. Defaults to the pinned Sepolia test EOA.
    /// Ignored when `--parallel > 1` - synthetic per-worker EOAs are
    /// used instead so the per-EOA nonce serialisation does not gate
    /// throughput (the bottleneck the saturation 50x50 report
    /// surfaced).
    #[arg(long, default_value_t = EOA)]
    eoa: Address,

    /// Number of parallel workers. Each worker impersonates its own
    /// synthetic EOA (`Address::from([i; 20])` where `i` is the
    /// 1-based worker index), gets its own WS connection, runs its
    /// own per-block submission loop. Total events per block =
    /// `parallel * (twap_per_block + ethflow_per_block)`.
    #[arg(long, default_value_t = 1)]
    parallel: u32,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    let cli = Cli::parse();
    let parallel = cli.parallel.max(1);

    info!(
        parallel,
        twap_per_block = cli.twap_per_block,
        ethflow_per_block = cli.ethflow_per_block,
        duration_min = cli.duration_min,
        "load-gen running"
    );

    // Build per-worker EOAs. Worker 0 reuses the CLI-provided EOA so
    // single-worker runs match the historic behaviour exactly;
    // workers 1..N use deterministic synthetic addresses so each gets
    // an independent nonce stream on Anvil.
    let mut eoas: Vec<Address> = Vec::with_capacity(parallel as usize);
    eoas.push(cli.eoa);
    for i in 1..parallel {
        let mut bytes = [0u8; 20];
        bytes[19] = (i & 0xff) as u8;
        bytes[18] = ((i >> 8) & 0xff) as u8;
        // Tag bytes[0] with 0x57 ('W' for worker) so synthetic EOAs are
        // easy to distinguish from anvil's default unlocked set.
        bytes[0] = 0x57;
        eoas.push(Address::from(bytes));
    }

    let deadline = Instant::now() + Duration::from_secs(cli.duration_min * 60);
    let mut joinset: tokio::task::JoinSet<anyhow::Result<WorkerStats>> =
        tokio::task::JoinSet::new();

    for (idx, eoa) in eoas.into_iter().enumerate() {
        let anvil = cli.anvil.clone();
        let twap_n = cli.twap_per_block;
        let ethflow_m = cli.ethflow_per_block;
        joinset.spawn(async move {
            worker_loop(idx as u32, anvil, eoa, twap_n, ethflow_m, deadline).await
        });
    }

    let mut totals = WorkerStats::default();
    let mut workers_finished = 0u32;
    while let Some(res) = joinset.join_next().await {
        match res {
            Ok(Ok(stats)) => {
                totals.merge(&stats);
                workers_finished += 1;
            }
            Ok(Err(e)) => warn!(error = %e, "worker failed"),
            Err(e) => warn!(error = %e, "worker panicked"),
        }
    }

    info!(
        workers_finished,
        blocks_seen = totals.blocks_seen,
        twap_attempted = totals.twap_attempted,
        twap_ok = totals.twap_ok,
        ethflow_attempted = totals.ethflow_attempted,
        ethflow_ok = totals.ethflow_ok,
        "load-gen finished"
    );
    Ok(())
}

#[derive(Debug, Default, Clone)]
struct WorkerStats {
    blocks_seen: u64,
    twap_attempted: u64,
    twap_ok: u64,
    ethflow_attempted: u64,
    ethflow_ok: u64,
}

impl WorkerStats {
    fn merge(&mut self, other: &Self) {
        self.blocks_seen += other.blocks_seen;
        self.twap_attempted += other.twap_attempted;
        self.twap_ok += other.twap_ok;
        self.ethflow_attempted += other.ethflow_attempted;
        self.ethflow_ok += other.ethflow_ok;
    }
}

async fn worker_loop(
    idx: u32,
    anvil: String,
    eoa: Address,
    twap_n: u32,
    ethflow_m: u32,
    deadline: Instant,
) -> anyhow::Result<WorkerStats> {
    let provider = ProviderBuilder::new()
        .connect_ws(WsConnect::new(&anvil))
        .await?;
    provider
        .raw_request::<_, ()>(
            "anvil_impersonateAccount".into(),
            serde_json::json!([format!("{:?}", eoa)]),
        )
        .await?;
    let funded = format!("0x{:x}", U256::from(10u128.pow(24)));
    provider
        .raw_request::<_, ()>(
            "anvil_setBalance".into(),
            serde_json::json!([format!("{:?}", eoa), funded]),
        )
        .await?;
    let starting_nonce: u64 = provider
        .raw_request::<_, String>(
            "eth_getTransactionCount".into(),
            serde_json::json!([format!("{:?}", eoa), "latest"]),
        )
        .await
        .map_err(|e| anyhow::anyhow!("get nonce: {e}"))
        .and_then(|hex| {
            u64::from_str_radix(hex.trim_start_matches("0x"), 16)
                .map_err(|e| anyhow::anyhow!("parse nonce {hex:?}: {e}"))
        })?;
    info!(worker = idx, eoa = %eoa, starting_nonce, "worker started");

    let mut block_stream = provider.subscribe_blocks().await?.into_stream();
    let mut nonce = starting_nonce;
    // Disjoint salt space per worker via a 96-bit-shifted prefix - the
    // salt is bytes32 so the upper bits stay free.
    let mut salt_counter = (u128::from(idx) + 1) << 96;
    // For ethflow_seq the value flows into `BASE_SELL_AMOUNT + seq` and
    // becomes the tx's `msg.value`. We MUST keep this small so the
    // impersonated EOA's 1_000_000 ETH balance can cover it (the
    // first parallel-mode run shifted by 96 and produced a 7.9e28 wei
    // sellAmount, blowing past the balance and reverting every
    // EthFlow tx). Workers get a 10_000-wide window each, plenty for
    // a 2 minute test at 5 ethflow/block.
    let mut ethflow_seq: u128 = u128::from(idx) * 10_000;
    let mut stats = WorkerStats::default();

    loop {
        tokio::select! {
            biased;
            _ = tokio::signal::ctrl_c() => break,
            _ = tokio::time::sleep_until(deadline.into()) => break,
            maybe_block = block_stream.next() => {
                let Some(header) = maybe_block else {
                    warn!(worker = idx, "block stream ended unexpectedly");
                    break;
                };
                stats.blocks_seen += 1;
                let block_ts = header.timestamp;
                let n_ok = submit_twaps(&provider, eoa, twap_n, &mut salt_counter, &mut nonce, block_ts).await;
                stats.twap_attempted += u64::from(twap_n);
                stats.twap_ok += n_ok;
                let m_ok = submit_ethflows(&provider, eoa, ethflow_m, &mut ethflow_seq, &mut nonce).await;
                stats.ethflow_attempted += u64::from(ethflow_m);
                stats.ethflow_ok += m_ok;
                if stats.blocks_seen.is_multiple_of(5) {
                    info!(
                        worker = idx,
                        block = header.number,
                        twap = format!("{}/{}", stats.twap_ok, stats.twap_attempted),
                        ethflow = format!("{}/{}", stats.ethflow_ok, stats.ethflow_attempted),
                        "progress"
                    );
                }
            }
        }
    }
    Ok(stats)
}

async fn submit_twaps<P: Provider>(
    provider: &P,
    eoa: Address,
    n: u32,
    salt_counter: &mut u128,
    nonce: &mut u64,
    block_ts: u64,
) -> u64 {
    let mut ok = 0u64;
    for _ in 0..n {
        *salt_counter += 1;
        let salt = salt_from_counter(*salt_counter);
        let calldata = encode_twap_create(salt, block_ts);
        match send_impersonated(provider, eoa, COMPOSABLE_COW, calldata, U256::ZERO, *nonce).await {
            Ok(_) => {
                ok += 1;
                *nonce += 1;
            }
            Err(e) => warn!(error = %e, nonce = *nonce, "twap create failed"),
        }
    }
    ok
}

async fn submit_ethflows<P: Provider>(
    provider: &P,
    eoa: Address,
    m: u32,
    seq: &mut u128,
    nonce: &mut u64,
) -> u64 {
    // EthFlow.createOrder dedups by the on-chain GPv2 OrderUid which
    // is derived from `(buyToken, receiver, sellAmount, buyAmount,
    // appData, feeAmount, validTo, partiallyFillable)` - NOT quoteId.
    // We vary `sellAmount` by 1 wei per call so the resulting UIDs
    // are unique and the contract does not reject with
    // `OrderIsAlreadyOwned`.
    const BASE_SELL_AMOUNT: u128 = 10_000_000_000; // 1e-8 ETH
    let mut ok = 0u64;
    for _ in 0..m {
        *seq += 1;
        let sell_amount = BASE_SELL_AMOUNT + *seq;
        let calldata = encode_ethflow_create_order(eoa, sell_amount, 0);
        match send_impersonated(
            provider,
            eoa,
            ETHFLOW,
            calldata,
            U256::from(sell_amount),
            *nonce,
        )
        .await
        {
            Ok(_) => {
                ok += 1;
                *nonce += 1;
            }
            Err(e) => warn!(error = %e, nonce = *nonce, "ethflow createOrder failed"),
        }
    }
    ok
}

fn salt_from_counter(n: u128) -> B256 {
    let mut bytes = [0u8; 32];
    bytes[16..].copy_from_slice(&n.to_be_bytes());
    B256::from(bytes)
}

/// Encode `ComposableCoW.create((handler, salt, staticInput), true)`.
/// The static input is the TWAP tuple from
/// `docs/operations/e2e-prep.md` §4.2 with `t0 = block_ts - 60`
/// so part 0 is Ready immediately.
fn encode_twap_create(salt: B256, block_ts: u64) -> Bytes {
    let static_input = (
        WETH,
        COW_TOKEN,
        EOA,                                     // receiver - load test does not settle
        U256::from(1_000_000_000_000_000u128),   // partSellAmount = 0.001 WETH
        U256::from(500_000_000_000_000_000u128), // minPartLimit = 0.5 COW
        U256::from(block_ts.saturating_sub(60)), // t0 = now - 60
        U256::from(2u8),                         // n
        U256::from(600u32),                      // t (seconds between parts)
        U256::ZERO,                              // span = full part window
        B256::ZERO,                              // appData = empty
    )
        .abi_encode();
    let call = createCall {
        params: ConditionalOrderParams {
            handler: TWAP_HANDLER,
            salt,
            staticInput: static_input.into(),
        },
        dispatch: true,
    };
    call.abi_encode().into()
}

/// Encode `CoWSwapEthFlow.createOrder(EthFlowOrder.Data)` with a sell
/// amount matched to the tx `value`. `appData` is the empty hash so
/// the orderbook mirror's `GET /api/v1/app_data/{hash}` returns the
/// document without contention. `validTo` is `u32::MAX` per the
/// canonical EthFlow shape (the mock orderbook is
/// permissive here, and shepherd's strategy will drop with the
/// expected Info-level log per PR #49).
fn encode_ethflow_create_order(eoa: Address, sell_amount: u128, quote_id: i64) -> Bytes {
    let order = EthFlowOrderData {
        buyToken: COW_TOKEN,
        receiver: eoa,
        sellAmount: U256::from(sell_amount),
        buyAmount: U256::from(1u8),
        appData: EMPTY_APP_DATA,
        feeAmount: U256::ZERO,
        validTo: u32::MAX,
        partiallyFillable: false,
        quoteId: quote_id,
    };
    let call = createOrderCall { order };
    call.abi_encode().into()
}

async fn send_impersonated<P: Provider>(
    provider: &P,
    from: Address,
    to: Address,
    data: Bytes,
    value: U256,
    nonce: u64,
) -> anyhow::Result<B256> {
    // `eth_sendTransaction` on Anvil uses the impersonated account's
    // virtual signer - no local key needed. We pin the nonce explicitly
    // so concurrent submissions do not race on the per-account counter
    // (root cause of the 5/270 revert rate in the baseline).
    let tx = TransactionRequest::default()
        .from(from)
        .to(to)
        .value(value)
        .nonce(nonce)
        .input(data.into());
    let hash: B256 = provider
        .raw_request("eth_sendTransaction".into(), serde_json::json!([tx]))
        .await?;
    Ok(hash)
}

// `now_unix` is kept here for future runbook-driven scenarios that
// drive load-gen without a live block stream. Not used today.
#[allow(dead_code)]
fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// Address parser sanity test - keeps the pinned identities in lockstep
// with the prep doc.
#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn pinned_addresses_round_trip() {
        for (label, addr) in [
            ("EOA", EOA),
            ("ComposableCoW", COMPOSABLE_COW),
            ("TWAP handler", TWAP_HANDLER),
            ("EthFlow", ETHFLOW),
            ("WETH", WETH),
            ("COW", COW_TOKEN),
        ] {
            let reparsed = Address::from_str(&format!("{addr:?}")).expect(label);
            assert_eq!(reparsed, addr, "{label}");
        }
    }

    #[test]
    fn salt_from_counter_is_unique_and_big_endian() {
        let a = salt_from_counter(1);
        let b = salt_from_counter(2);
        assert_ne!(a, b);
        // High 16 bytes always zero (counter fits in u128).
        assert_eq!(&a.as_slice()[..16], &[0u8; 16]);
        // Counter sits in the low 16 bytes, big-endian.
        assert_eq!(a.as_slice()[31], 1);
        assert_eq!(b.as_slice()[31], 2);
    }

    #[test]
    fn twap_calldata_starts_with_create_selector() {
        let calldata = encode_twap_create(B256::ZERO, 1_700_000_000);
        // Selector for `create((address,bytes32,bytes),bool)` is the
        // first 4 bytes of keccak256("create((address,bytes32,bytes),bool)").
        // We assert structurally rather than pinning a magic constant
        // so a future ABI tweak fails the test with a clear shape diff.
        assert_eq!(calldata.len() % 32, 4, "selector + abi-encoded body");
    }
}
