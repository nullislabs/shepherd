//! Open live chain event sources and dispatch their events to the
//! supervisor until a shutdown signal arrives. Blocks come from
//! `eth_subscribe(newHeads)` (WS); chain-logs come from alloy's
//! canonical `eth_getLogs` block-range poller (HTTP or WS), which
//! recovers events across a reconnect by re-querying the gap instead
//! of dropping them.
//!
//! ## Per-stream reconnect with exponential backoff
//!
//! `open_block_streams` / `open_chain_log_streams` no longer return a
//! `Vec<Stream>` that ends on the first drop. They each spawn one
//! reconnect-aware task per `(chain_id)` or `(module, chain_id,
//! filter)` tuple. The task:
//!
//! 1. Opens the block subscription / log poller via the provider pool.
//! 2. Pumps items to an mpsc channel until the underlying stream ends
//!    (a WebSocket drop for blocks, or a terminal poller error for
//!    logs - a hard RPC failure or a reorg past retained history).
//! 3. Logs the end + waits `restart_policy::backoff_for(attempt)`
//!    (1s -> 2s -> ... cap 5min).
//! 4. Reopens. On the first event after a reopen, attempt resets
//!    if the stream has been healthy for `HEALTHY_WINDOW`.
//!
//! The event loop reads the receiver as a regular `Stream`. The
//! reconnect tasks live for the lifetime of the engine; they exit
//! cleanly with [`TaskExit::ReceiverGone`] when their channel receiver
//! is dropped (which happens when `run` returns). They are spawned via
//! an injectable [`TaskExecutor`] and their handles collected into a
//! [`TaskSet`] the loop drains on shutdown.

use std::time::{Duration, Instant};

use alloy_chains::Chain;
use futures::StreamExt;
use futures::stream::{BoxStream, select_all};
use thiserror::Error;
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::bindings::nexum;
use crate::host::component::{ChainProvider, RuntimeTypes};
use crate::host::provider_pool::ProviderError;
use crate::runtime::restart_policy::backoff_for;
use crate::runtime::task::{TaskExecutor, TaskExit, TaskSet};
use crate::supervisor::Supervisor;

/// Errors carried by the tagged block / chain-log streams that the
/// supervisor consumes. Library-side code keeps `anyhow::Error` out
/// of long-lived stream item types per the rust idiomatic rubric.
#[derive(Debug, Error)]
pub enum StreamError {
    /// Underlying provider / transport failure while opening or
    /// pumping the subscription.
    #[error(transparent)]
    Provider(#[from] ProviderError),
}

/// Time the wrapper stream must observe uninterrupted events before
/// the backoff counter resets to 0. Long enough that a brief but
/// real connection blip does not silently undo the doubling, short
/// enough that a healthy node reverts to fast retries on the next
/// drop.
const HEALTHY_WINDOW: Duration = Duration::from_secs(60);

/// Time without any block event that we treat as a gap worth a
/// positive recovery log line. Sepolia and Ethereum
/// mainnet both produce blocks reliably every ~12 s, so a silence
/// longer than this is either a transport-layer reconnect that alloy
/// handled internally (no `stream ended` reached the engine, hence
/// no `subscription reopened` log fires) or an upstream RPC stall.
/// Either way, the soak operator wants a positive log line when
/// blocks resume - otherwise an `alloy_transport_ws::native` ERROR
/// followed by silence looks identical to a hung engine.
const BLOCK_GAP_LOG_THRESHOLD: Duration = Duration::from_secs(60);

/// Channel buffer for the reconnect tasks. Each chain / module
/// subscription gets its own task -> channel pair; buffer is small
/// because the event loop drains in real time.
const RECONNECT_CHANNEL_BUF: usize = 64;

/// Gap size (blocks) at or above which a re-open logs a large-backfill
/// notice. Purely informational - nothing is ever skipped.
const LARGE_GAP_LOG_THRESHOLD: u64 = 1_000;

/// Per-chain block subscriptions, one reconnect-aware task per
/// chain id. Tasks are spawned via `executor` and their handles pushed
/// into `tasks` so the caller can drive graceful shutdown (the engine
/// drains the set after closing its receivers - the tasks exit cleanly
/// when the receiver drops).
///
/// Not `async`: the openers only spawn, they never await, so the caller
/// gets the tagged streams synchronously.
pub fn open_block_streams<C>(
    pool: &C,
    chains: &[Chain],
    executor: &dyn TaskExecutor,
    tasks: &mut TaskSet,
) -> Vec<TaggedBlockStream>
where
    C: ChainProvider + Clone + Send + Sync + 'static,
{
    let mut streams = Vec::new();
    for &chain in chains {
        let (tx, rx) = mpsc::channel::<Result<(Chain, alloy_rpc_types_eth::Header), StreamError>>(
            RECONNECT_CHANNEL_BUF,
        );
        let pool = pool.clone();
        tasks.push(executor.spawn(Box::pin(reconnecting_block_task(pool, chain, tx))));
        let tagged: TaggedBlockStream = Box::pin(receiver_stream(rx));
        streams.push(tagged);
    }
    streams
}

/// Per-module chain-log subscriptions. Each entry gets its own reconnect-
/// aware task tagged with the owning module name + chain id. Tasks
/// are spawned via `executor` and pushed into `tasks` (see
/// [`open_block_streams`]).
pub fn open_chain_log_streams<C>(
    pool: &C,
    subs: Vec<(String, Chain, alloy_rpc_types_eth::Filter)>,
    executor: &dyn TaskExecutor,
    tasks: &mut TaskSet,
) -> Vec<TaggedChainLogStream>
where
    C: ChainProvider + Clone + Send + Sync + 'static,
{
    let mut streams = Vec::new();
    for (module, chain, filter) in subs {
        let (tx, rx) = mpsc::channel::<
            Result<(String, Chain, alloy_rpc_types_eth::Log), StreamError>,
        >(RECONNECT_CHANNEL_BUF);
        let pool = pool.clone();
        tasks.push(executor.spawn(Box::pin(reconnecting_chain_log_task(
            pool, module, chain, filter, tx,
        ))));
        let tagged: TaggedChainLogStream = Box::pin(receiver_stream(rx));
        streams.push(tagged);
    }
    streams
}

/// Wrap an `mpsc::Receiver<T>` as a `Stream<Item = T>` using
/// `futures::stream::unfold`. Avoids pulling in `tokio-stream` just
/// for `ReceiverStream`.
fn receiver_stream<T: Send + 'static>(
    rx: mpsc::Receiver<T>,
) -> impl futures::Stream<Item = T> + Send {
    futures::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|item| (item, rx))
    })
}

/// Reconnect-aware loop for a single chain's block subscription.
/// Holds `(pool, chain_id)` and re-opens the underlying alloy
/// `eth_subscribe` stream with exponential backoff after every drop
/// or transport error.
async fn reconnecting_block_task<C>(
    pool: C,
    chain: Chain,
    tx: mpsc::Sender<Result<(Chain, alloy_rpc_types_eth::Header), StreamError>>,
) -> TaskExit
where
    C: ChainProvider + Send + Sync + 'static,
{
    let chain_id = chain.id();
    let mut attempt: u32 = 0;
    let mut last_event: Option<Instant> = None;
    loop {
        match pool.subscribe_blocks(chain).await {
            Ok(mut inner) => {
                if attempt == 0 {
                    info!(chain_id, "block subscription open");
                } else {
                    info!(chain_id, attempt, "block subscription reopened");
                    metrics::counter!(
                        "shepherd_stream_reconnects_total",
                        "kind" => "block",
                        "chain_id" => chain_id.to_string(),
                    )
                    .increment(1);
                }
                while let Some(item) = inner.next().await {
                    let now = Instant::now();
                    if attempt > 0
                        && last_event.is_some_and(|t| now.duration_since(t) >= HEALTHY_WINDOW)
                    {
                        info!(chain_id, "block stream healthy - resetting backoff");
                        attempt = 0;
                    }
                    // Detect transport-layer reconnects that
                    // alloy handled internally - `inner.next().await`
                    // keeps yielding events but with a long gap. The
                    // engine's reconnect path (`stream ended` -> wait
                    // backoff -> `subscription reopened`) does not fire
                    // for these, so without this log a soak operator
                    // sees an `alloy_transport_ws::native` ERROR
                    // followed by silence indistinguishable from a
                    // hung engine.
                    if let Some(gap) =
                        block_stream_gap_to_log(now, last_event, BLOCK_GAP_LOG_THRESHOLD)
                    {
                        let gap_s = gap.as_secs();
                        info!(
                            chain_id,
                            gap_s,
                            kind = "block",
                            "stream gap closed - first event after silence \
                             (likely an alloy-internal transport reconnect)"
                        );
                    }
                    last_event = Some(now);
                    let tagged = item
                        .map(|header| (chain, header))
                        .map_err(StreamError::from);
                    if tx.send(tagged).await.is_err() {
                        // Receiver dropped -> engine shutting down.
                        return TaskExit::ReceiverGone;
                    }
                }
                warn!(chain_id, "block stream ended (WebSocket dropped?)");
                attempt = attempt.saturating_add(1);
            }
            Err(err) => {
                warn!(chain_id, error = %err, "block subscription failed");
                attempt = attempt.saturating_add(1);
            }
        }
        let backoff = backoff_for(attempt);
        warn!(
            chain_id,
            attempt,
            backoff_ms = backoff.as_millis() as u64,
            "reconnecting block subscription after backoff",
        );
        tokio::time::sleep(backoff).await;
    }
}

/// Poller-backed loop for a single (module, chain) chain-log
/// subscription. Instead of `eth_subscribe(logs)` - which silently
/// drops events emitted during a WebSocket reconnect - it drives
/// alloy's canonical `eth_getLogs` block-range poller. The poller
/// reconciles reorgs and re-queries any gap internally, so no manual
/// backfill or dedup is needed here. A hard RPC error (after the
/// transport's own retries), or a reorg deeper than the poller's
/// retained history, ends the poller stream; this loop then re-opens
/// from the block after the last one it delivered and backfills the
/// entire missed range (no lookback cap; nothing is skipped), with
/// exponential backoff.
async fn reconnecting_chain_log_task<C>(
    pool: C,
    module: String,
    chain: Chain,
    filter: alloy_rpc_types_eth::Filter,
    tx: mpsc::Sender<Result<(String, Chain, alloy_rpc_types_eth::Log), StreamError>>,
) -> TaskExit
where
    C: ChainProvider + Send + Sync + 'static,
{
    let chain_id = chain.id();
    let mut attempt: u32 = 0;
    let mut last_event: Option<Instant> = None;
    // Highest block whose logs we have delivered; the resume point after a
    // poller re-open, so the missed range is synced back rather than skipped.
    let mut last_seen_block: Option<u64> = None;
    loop {
        let head = match pool.block_number(chain).await {
            Ok(head) => head,
            Err(err) => {
                attempt = attempt.saturating_add(1);
                let backoff = backoff_for(attempt);
                warn!(
                    module = %module,
                    chain_id,
                    error = %err,
                    attempt,
                    backoff_ms = backoff.as_millis() as u64,
                    "chain-log head fetch failed - retrying after backoff",
                );
                tokio::time::sleep(backoff).await;
                continue;
            }
        };
        // First open starts at the head - no history replay on boot, and
        // the across-restart gap (logs mined before the engine started) is
        // out of scope since nothing persists a cursor. A re-open resumes
        // just after the last delivered block and backfills the entire gap
        // so no event is ever skipped.
        let start_block = poller_resume_block(last_seen_block, head);
        // A large gap is backfilled in full (never skipped); surface it so a long
        // catch-up is visible rather than looking like a stall.
        if head.saturating_sub(start_block) >= LARGE_GAP_LOG_THRESHOLD {
            info!(
                module = %module,
                chain_id,
                from = start_block,
                to = head,
                blocks = head.saturating_sub(start_block),
                "chain-log poller backfilling a large gap"
            );
        }
        match pool.watch_chain_logs(chain, filter.clone(), start_block) {
            Ok(mut inner) => {
                if attempt == 0 {
                    info!(module = %module, chain_id, start_block, "chain-log poller open");
                } else {
                    info!(
                        module = %module,
                        chain_id,
                        attempt,
                        start_block,
                        "chain-log poller reopened"
                    );
                    metrics::counter!(
                        "shepherd_stream_reconnects_total",
                        "kind" => "chain-log",
                        "chain_id" => chain_id.to_string(),
                        "module" => module.clone(),
                    )
                    .increment(1);
                }
                while let Some(item) = inner.next().await {
                    let now = Instant::now();
                    if attempt > 0
                        && last_event.is_some_and(|t| now.duration_since(t) >= HEALTHY_WINDOW)
                    {
                        info!(
                            module = %module,
                            chain_id,
                            "chain-log stream healthy - resetting backoff"
                        );
                        attempt = 0;
                    }
                    last_event = Some(now);
                    match item {
                        // One canonical block's matching logs; fan the
                        // batch out into the existing per-log dispatch
                        // path. Each log already carries its `removed`
                        // flag from the poller.
                        Ok(logs) => {
                            for log in logs {
                                if let Some(block) = log.block_number {
                                    last_seen_block =
                                        Some(last_seen_block.map_or(block, |seen| seen.max(block)));
                                }
                                if tx.send(Ok((module.clone(), chain, log))).await.is_err() {
                                    return TaskExit::ReceiverGone;
                                }
                            }
                        }
                        // A poller error is terminal for the alloy stream;
                        // break to re-open from a fresh head rather than
                        // pumping a dead stream.
                        Err(err) => {
                            warn!(
                                module = %module,
                                chain_id,
                                error = %err,
                                "chain-log poller error - reopening"
                            );
                            break;
                        }
                    }
                }
                warn!(module = %module, chain_id, "chain-log poller stream ended - reopening");
                attempt = attempt.saturating_add(1);
            }
            Err(err) => {
                warn!(
                    module = %module,
                    chain_id,
                    error = %err,
                    "chain-log poller open failed"
                );
                attempt = attempt.saturating_add(1);
            }
        }
        let backoff = backoff_for(attempt);
        warn!(
            module = %module,
            chain_id,
            attempt,
            backoff_ms = backoff.as_millis() as u64,
            "reconnecting chain-log poller after backoff",
        );
        tokio::time::sleep(backoff).await;
    }
}

pub type TaggedBlockStream = std::pin::Pin<
    Box<
        dyn futures::Stream<Item = Result<(Chain, alloy_rpc_types_eth::Header), StreamError>>
            + Send,
    >,
>;
pub type TaggedChainLogStream = std::pin::Pin<
    Box<
        dyn futures::Stream<Item = Result<(String, Chain, alloy_rpc_types_eth::Log), StreamError>>
            + Send,
    >,
>;

/// Drive the supervisor with events until `shutdown` resolves.
///
/// Graceful shutdown: the dispatch path is structured so
/// that `shutdown` is only observed *between* dispatches, never
/// mid-`call_on_event`. Each select fork either yields a fresh event
/// to dispatch or signals shutdown - the in-flight wasmtime call
/// finishes naturally before the loop exits.
pub async fn run<T: RuntimeTypes>(
    supervisor: &mut Supervisor<T>,
    block_streams: Vec<TaggedBlockStream>,
    chain_log_streams: Vec<TaggedChainLogStream>,
    tasks: TaskSet,
    shutdown: impl std::future::Future<Output = ()> + Send,
) {
    // `select_all` over an empty Vec yields `None` immediately, which
    // would trip the "stream ended -> shut down" arm below before the
    // first block / chain-log ever flows. Engine configs that subscribe to
    // only one event kind (e.g. all modules use `[[subscription]] kind
    // = "block"`) are valid and must not be punished. Replace each
    // empty side with `stream::pending()` so the corresponding select
    // arm is never selected; the bail-on-None semantic still fires
    // when a *non-empty* stream actually closes.
    let mut blocks: BoxStream<'_, _> = if block_streams.is_empty() {
        futures::stream::pending().boxed()
    } else {
        select_all(block_streams).boxed()
    };
    let mut chain_logs: BoxStream<'_, _> = if chain_log_streams.is_empty() {
        futures::stream::pending().boxed()
    } else {
        select_all(chain_log_streams).boxed()
    };
    let mut shutdown = Box::pin(shutdown);
    let mut dispatched_blocks: u64 = 0;
    let mut dispatched_chain_logs: u64 = 0;
    let started = Instant::now();
    loop {
        // Phase 1: pick the next event OR observe shutdown. The
        // dispatch itself happens in phase 2 (outside the select)
        // so an in-flight wasmtime call never gets cancelled by a
        // shutdown signal arriving mid-dispatch.
        enum NextEvent {
            Block(nexum::host::types::Block),
            // The alloy `Log` is boxed so the `Chain` tag does not push
            // the enum past the large-variant lint threshold.
            ChainLog(String, Chain, Box<alloy_rpc_types_eth::Log>),
            Shutdown,
            StreamPanic(&'static str),
        }
        let next = tokio::select! {
            biased;
            () = &mut shutdown => NextEvent::Shutdown,
            next = blocks.next() => match next {
                Some(Ok((chain, header))) => NextEvent::Block(nexum::host::types::Block {
                    chain_id: chain.id(),
                    number: header.number,
                    hash: header.hash.as_slice().to_vec(),
                    timestamp: header.timestamp.saturating_mul(1000),
                }),
                Some(Err(err)) => {
                    warn!(error = %err, "block stream error - continuing");
                    continue;
                }
                None => NextEvent::StreamPanic("block"),
            },
            next = chain_logs.next() => match next {
                Some(Ok((module, chain, log))) => NextEvent::ChainLog(module, chain, Box::new(log)),
                Some(Err(err)) => {
                    warn!(error = %err, "chain-log stream error - continuing");
                    continue;
                }
                None => NextEvent::StreamPanic("chain-log"),
            },
        };

        match next {
            NextEvent::Block(block) => {
                supervisor.dispatch_block(block).await;
                dispatched_blocks += 1;
            }
            NextEvent::ChainLog(module, chain, log) => {
                supervisor.dispatch_chain_log(&module, chain, *log).await;
                dispatched_chain_logs += 1;
            }
            NextEvent::Shutdown => {
                // Drop the stream-end receivers so the reconnect
                // tasks observe a closed channel and exit. Then drain
                // the task set so the engine genuinely sees the tasks
                // finish before returning.
                drop(blocks);
                drop(chain_logs);
                tasks.shutdown().await;
                info!(
                    dispatched_blocks,
                    dispatched_chain_logs,
                    uptime_secs = started.elapsed().as_secs(),
                    "graceful shutdown complete",
                );
                return;
            }
            NextEvent::StreamPanic(kind) => {
                // Reconnect tasks should loop forever.
                // Hitting `None` from `select_all` means the task
                // exited (panic or channel closed). Bail loudly.
                drop(blocks);
                drop(chain_logs);
                tasks.shutdown().await;
                warn!(
                    kind,
                    "reconnect task ended unexpectedly - shutting down for engine restart"
                );
                return;
            }
        }
    }
}

/// The block a re-opened log poller should start from. `None` (the
/// first open) starts at the head, so no history is replayed on boot.
/// Otherwise resume just after the last delivered block and backfill
/// the whole gap - there is no lookback cap, so nothing is ever
/// skipped. This is reorg-safe: the old blocks are final, and the
/// poller fetches one `eth_getLogs` per block (immune to a provider's
/// block-range limit).
fn poller_resume_block(last_seen_block: Option<u64>, head: u64) -> u64 {
    match last_seen_block {
        None => head,
        Some(last) => last.saturating_add(1),
    }
}

/// Returns `Some(gap)` when the time between the last observed event
/// and `now` meets or exceeds `threshold` - the caller should emit a
/// positive-recovery log line at this point. `None` covers
/// both the first-event case (no `last_event` yet) and the normal
/// "events are arriving at expected cadence" case.
fn block_stream_gap_to_log(
    now: Instant,
    last_event: Option<Instant>,
    threshold: Duration,
) -> Option<Duration> {
    let last = last_event?;
    let gap = now.duration_since(last);
    (gap >= threshold).then_some(gap)
}

/// Wait for SIGINT or (on Unix) SIGTERM, whichever arrives first.
pub async fn wait_for_shutdown_signal() -> anyhow::Result<&'static str> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut sigterm = signal(SignalKind::terminate())?;
        let mut sigint = signal(SignalKind::interrupt())?;
        tokio::select! {
            _ = sigterm.recv() => Ok("SIGTERM"),
            _ = sigint.recv()  => Ok("SIGINT"),
        }
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await?;
        Ok("ctrl-c")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The helper that decides whether to emit a
    /// "stream gap closed" line on the next block event.
    #[test]
    fn block_stream_gap_to_log_returns_none_when_no_prior_event() {
        let now = Instant::now();
        assert_eq!(
            block_stream_gap_to_log(now, None, Duration::from_secs(60)),
            None,
        );
    }

    #[test]
    fn block_stream_gap_to_log_returns_none_when_under_threshold() {
        let earlier = Instant::now();
        let now = earlier + Duration::from_secs(30);
        assert_eq!(
            block_stream_gap_to_log(now, Some(earlier), Duration::from_secs(60)),
            None,
            "30s < 60s threshold -> do not log",
        );
    }

    #[test]
    fn block_stream_gap_to_log_returns_some_at_threshold_boundary() {
        let earlier = Instant::now();
        let now = earlier + Duration::from_secs(60);
        assert_eq!(
            block_stream_gap_to_log(now, Some(earlier), Duration::from_secs(60)),
            Some(Duration::from_secs(60)),
            "boundary is inclusive - exactly the threshold counts as a gap",
        );
    }

    #[test]
    fn block_stream_gap_to_log_returns_some_when_well_over_threshold() {
        let earlier = Instant::now();
        let now = earlier + Duration::from_secs(3600);
        // The 2026-06-23 soak observation: a 1h gap between the
        // `alloy_transport_ws::native` ERROR at 09:05 and the next
        // block at 10:05. This is the exact case the log line was
        // added for.
        let gap = block_stream_gap_to_log(now, Some(earlier), Duration::from_secs(60))
            .expect("1h gap is well over the 60s threshold");
        assert_eq!(gap.as_secs(), 3600);
    }

    #[test]
    fn poller_resume_block_first_open_starts_at_head() {
        assert_eq!(
            poller_resume_block(None, 100),
            100,
            "first open starts at head, no history replay",
        );
    }

    #[test]
    fn poller_resume_block_resumes_after_last_delivered() {
        assert_eq!(
            poller_resume_block(Some(90), 100),
            91,
            "a re-open resumes just after the last delivered block",
        );
    }

    #[test]
    fn poller_resume_block_backfills_the_full_gap() {
        assert_eq!(
            poller_resume_block(Some(10), 1_000_000),
            11,
            "no lookback cap; resume just after the last delivered block and backfill the whole gap",
        );
    }
}
