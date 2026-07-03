//! Open live `eth_subscribe` streams and dispatch their events to the
//! supervisor until a shutdown signal arrives.
//!
//! ## Per-stream reconnect with exponential backoff
//!
//! `open_block_streams` / `open_log_streams` no longer return a
//! `Vec<Stream>` that ends on the first WebSocket drop. They each
//! spawn one reconnect-aware task per `(chain_id)` or `(module,
//! chain_id, filter)` tuple. The task:
//!
//! 1. Opens the subscription via the provider pool.
//! 2. Pumps items to an mpsc channel until the underlying stream
//!    yields `None` (WS drop) or `Err` (transport-level error).
//! 3. Logs the drop + waits `restart_policy::backoff_for(attempt)`
//!    (1s -> 2s -> ... cap 5min).
//! 4. Reopens. On the first event after a reopen, attempt resets
//!    if the stream has been healthy for `HEALTHY_WINDOW`.
//!
//! The event loop reads the receiver as a regular `Stream`. The
//! reconnect tasks live for the lifetime of the engine; they exit
//! cleanly when their channel receiver is dropped (which happens
//! when `run` returns).

use std::time::{Duration, Instant};

use alloy_chains::Chain;
use futures::StreamExt;
use futures::stream::{BoxStream, select_all};
use thiserror::Error;
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tracing::{info, warn};

use crate::bindings::nexum;
use crate::host::component::{ChainProvider, RuntimeTypes};
use crate::host::provider_pool::ProviderError;
use crate::runtime::restart_policy::backoff_for;
use crate::supervisor::Supervisor;

/// Errors carried by the tagged block / log streams that the
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

/// Maximum number of blocks to backfill via `eth_getLogs` after a
/// reconnect. Most RPC nodes cap `eth_getLogs` to 2k-10k blocks per
/// request; asking for more fails every time, turning the backfill
/// into an infinite warning source. When the gap exceeds this limit,
/// `from_block` is clamped and a warning is emitted about the partial
/// backfill.
const MAX_BACKFILL_BLOCKS: u64 = 10_000;

/// Per-chain block subscriptions, one reconnect-aware task per
/// chain id. Tasks are spawned into `tasks` so the caller can drive
/// graceful shutdown (the engine awaits the set after closing its
/// receivers - the tasks exit cleanly when the receiver drops).
pub async fn open_block_streams<C>(
    pool: &C,
    chains: &[Chain],
    tasks: &mut JoinSet<()>,
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
        tasks.spawn(reconnecting_block_task(pool, chain, tx));
        let tagged: TaggedBlockStream = Box::pin(receiver_stream(rx));
        streams.push(tagged);
    }
    streams
}

/// Per-module log subscriptions. Each entry gets its own reconnect-
/// aware task tagged with the owning module name + chain id. Tasks
/// are spawned into `tasks` (see [`open_block_streams`]).
pub async fn open_log_streams<C>(
    pool: &C,
    subs: Vec<(String, Chain, alloy_rpc_types_eth::Filter)>,
    tasks: &mut JoinSet<()>,
) -> Vec<TaggedLogStream>
where
    C: ChainProvider + Clone + Send + Sync + 'static,
{
    let mut streams = Vec::new();
    for (module, chain, filter) in subs {
        let (tx, rx) = mpsc::channel::<
            Result<(String, Chain, alloy_rpc_types_eth::Log), StreamError>,
        >(RECONNECT_CHANNEL_BUF);
        let pool = pool.clone();
        tasks.spawn(reconnecting_log_task(pool, module, chain, filter, tx));
        let tagged: TaggedLogStream = Box::pin(receiver_stream(rx));
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

/// Log the block-number gap (if any) between `last_seen` and the
/// current chain head. Pure side-effect (tracing output only).
async fn log_block_gap<C: ChainProvider + Sync>(
    pool: &C,
    chain: Chain,
    chain_id: u64,
    last_seen: u64,
) {
    match pool.get_block_number(chain).await {
        Ok(current) if current > last_seen.saturating_add(1) => {
            let missed = current.saturating_sub(last_seen).saturating_sub(1);
            if missed > MAX_BACKFILL_BLOCKS {
                warn!(
                    chain_id,
                    from = last_seen.saturating_add(1),
                    to = current,
                    missed,
                    max = MAX_BACKFILL_BLOCKS,
                    "large block gap during disconnect exceeds \
                     backfill cap (modules will poll current state)"
                );
            } else {
                info!(
                    chain_id,
                    from = last_seen.saturating_add(1),
                    to = current,
                    missed,
                    "block gap during disconnect \
                     (modules will poll current state)"
                );
            }
        }
        Ok(_) => {} // no gap or only 1 block ahead
        Err(err) => {
            warn!(
                chain_id,
                error = %err,
                "failed to fetch current block number for gap detection"
            );
        }
    }
}

/// Attempt to backfill missed log events for the gap
/// `[last_seen+1, current_block]`. Returns `Some((logs, current_block))`
/// when events were fetched successfully; the caller should dispatch
/// the logs and set `dedup_until_block = current_block`. Returns `None`
/// when there is no gap or the backfill failed (warnings already emitted).
async fn try_backfill_logs<C: ChainProvider + Sync>(
    pool: &C,
    chain: Chain,
    chain_id: u64,
    module: &str,
    filter: &alloy_rpc_types_eth::Filter,
    last_seen: u64,
) -> Option<(Vec<alloy_rpc_types_eth::Log>, u64)> {
    let current = match pool.get_block_number(chain).await {
        Ok(current) if current > last_seen => current,
        Ok(_) => return None,
        Err(err) => {
            warn!(
                module = %module,
                chain_id,
                error = %err,
                "failed to fetch current block number for backfill"
            );
            return None;
        }
    };

    let gap = current.saturating_sub(last_seen);
    let from = if gap > MAX_BACKFILL_BLOCKS {
        let clamped = current
            .saturating_sub(MAX_BACKFILL_BLOCKS)
            .saturating_add(1);
        warn!(
            module = %module,
            chain_id,
            full_gap = gap,
            clamped_from = clamped,
            to = current,
            max = MAX_BACKFILL_BLOCKS,
            "backfill gap exceeds MAX_BACKFILL_BLOCKS - \
             clamping from_block (oldest events in gap \
             will be missed)"
        );
        clamped
    } else {
        last_seen.saturating_add(1)
    };

    let backfill_filter = filter.clone().from_block(from).to_block(current);
    match pool.get_logs(chain, backfill_filter).await {
        Ok(logs) => {
            info!(
                module = %module,
                chain_id,
                from,
                to = current,
                count = logs.len(),
                "backfilled missed log events"
            );
            Some((logs, current))
        }
        Err(err) => {
            warn!(
                module = %module,
                chain_id,
                from,
                to = current,
                error = %err,
                "backfill eth_getLogs failed \
                 - continuing with live stream"
            );
            None
        }
    }
}

/// Reconnect-aware loop for a single chain's block subscription.
/// Holds `(pool, chain_id)` and re-opens the underlying alloy
/// `eth_subscribe` stream with exponential backoff after every drop
/// or transport error.
///
/// On reconnect, logs the block-number gap (if any) so operators can
/// see exactly which block range was missed. Modules handle missed
/// blocks gracefully via polling, so individual block headers are not
/// backfilled.
async fn reconnecting_block_task<C>(
    pool: C,
    chain: Chain,
    tx: mpsc::Sender<Result<(Chain, alloy_rpc_types_eth::Header), StreamError>>,
) where
    C: ChainProvider + Send + Sync + 'static,
{
    let chain_id = chain.id();
    let mut attempt: u32 = 0;
    let mut last_event: Option<Instant> = None;
    let mut last_seen_block: Option<u64> = None;
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

                    if let Some(last) = last_seen_block {
                        log_block_gap(&pool, chain, chain_id, last).await;
                    }
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
                    // Track the latest block number for gap detection on
                    // reconnect.
                    if let Ok(ref header) = item {
                        last_seen_block = Some(header.number);
                    }
                    let tagged = item
                        .map(|header| (chain, header))
                        .map_err(StreamError::from);
                    if tx.send(tagged).await.is_err() {
                        // Receiver dropped -> engine shutting down.
                        return;
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

/// Reconnect-aware loop for a single (module, chain) log subscription.
///
/// On reconnect, queries `eth_getLogs` for the block range
/// `[last_seen + 1, current_block]` to backfill any events emitted
/// during the disconnect window. This is the critical path for
/// preventing silent event loss (the scenario observed in soak
/// testing where a `ConditionalOrderCreated` event was permanently
/// missed).
async fn reconnecting_log_task<C>(
    pool: C,
    module: String,
    chain: Chain,
    filter: alloy_rpc_types_eth::Filter,
    tx: mpsc::Sender<Result<(String, Chain, alloy_rpc_types_eth::Log), StreamError>>,
) where
    C: ChainProvider + Send + Sync + 'static,
{
    let chain_id = chain.id();
    let mut attempt: u32 = 0;
    let mut last_event: Option<Instant> = None;
    let mut last_seen_block: Option<u64> = None;
    let mut dedup_until_block: Option<u64> = None;
    loop {
        match pool.subscribe_logs(chain, filter.clone()).await {
            Ok(mut inner) => {
                if attempt == 0 {
                    info!(module = %module, chain_id, "log subscription open");
                } else {
                    info!(module = %module, chain_id, attempt, "log subscription reopened");
                    metrics::counter!(
                        "shepherd_stream_reconnects_total",
                        "kind" => "log",
                        "chain_id" => chain_id.to_string(),
                        "module" => module.clone(),
                    )
                    .increment(1);

                    if let Some(last) = last_seen_block
                        && let Some((logs, current)) =
                            try_backfill_logs(&pool, chain, chain_id, &module, &filter, last).await
                    {
                        for log in logs {
                            let tagged = Ok((module.clone(), chain, log));
                            if tx.send(tagged).await.is_err() {
                                return;
                            }
                        }
                        last_seen_block = Some(current);
                        dedup_until_block = Some(current);
                    }
                }
                while let Some(item) = inner.next().await {
                    let now = Instant::now();
                    if attempt > 0
                        && last_event.is_some_and(|t| now.duration_since(t) >= HEALTHY_WINDOW)
                    {
                        info!(
                            module = %module,
                            chain_id,
                            "log stream healthy - resetting backoff"
                        );
                        attempt = 0;
                    }
                    last_event = Some(now);

                    // Dedup: skip events from blocks already covered by
                    // the backfill. The live subscription was opened
                    // *before* the backfill ran, so its internal buffer
                    // may contain events from blocks <=
                    // dedup_until_block. The backfill already dispatched
                    // everything up to that block, so we skip until we
                    // see a block_number past the boundary.
                    if let Some(dedup_until) = dedup_until_block {
                        let dominated = match &item {
                            Ok(log) => log.block_number.is_some_and(|n| n <= dedup_until),
                            Err(_) => false, // never suppress errors
                        };
                        if dominated {
                            continue;
                        }
                        // First event past the dedup window — clear the
                        // guard. All subsequent events are live-only.
                        dedup_until_block = None;
                    }

                    // Track the latest block number for backfill range
                    // calculation on reconnect.
                    if let Ok(ref log) = item
                        && let Some(block_num) = log.block_number
                    {
                        last_seen_block = Some(block_num);
                    }
                    let module_name = module.clone();
                    let tagged = item
                        .map(|log| (module_name, chain, log))
                        .map_err(StreamError::from);
                    if tx.send(tagged).await.is_err() {
                        return;
                    }
                }
                warn!(module = %module, chain_id, "log stream ended (WebSocket dropped?)");
                attempt = attempt.saturating_add(1);
            }
            Err(err) => {
                warn!(
                    module = %module,
                    chain_id,
                    error = %err,
                    "log subscription failed"
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
            "reconnecting log subscription after backoff",
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
pub type TaggedLogStream = std::pin::Pin<
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
    log_streams: Vec<TaggedLogStream>,
    mut tasks: JoinSet<()>,
    shutdown: impl std::future::Future<Output = ()> + Send,
) {
    // `select_all` over an empty Vec yields `None` immediately, which
    // would trip the "stream ended -> shut down" arm below before the
    // first block / log ever flows. Engine configs that subscribe to
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
    let mut logs: BoxStream<'_, _> = if log_streams.is_empty() {
        futures::stream::pending().boxed()
    } else {
        select_all(log_streams).boxed()
    };
    let mut shutdown = Box::pin(shutdown);
    let mut dispatched_blocks: u64 = 0;
    let mut dispatched_logs: u64 = 0;
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
            Log(String, Chain, Box<alloy_rpc_types_eth::Log>),
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
            next = logs.next() => match next {
                Some(Ok((module, chain, log))) => NextEvent::Log(module, chain, Box::new(log)),
                Some(Err(err)) => {
                    warn!(error = %err, "log stream error - continuing");
                    continue;
                }
                None => NextEvent::StreamPanic("log"),
            },
        };

        match next {
            NextEvent::Block(block) => {
                supervisor.dispatch_block(block).await;
                dispatched_blocks += 1;
            }
            NextEvent::Log(module, chain, log) => {
                supervisor.dispatch_log(&module, chain, *log).await;
                dispatched_logs += 1;
            }
            NextEvent::Shutdown => {
                // Drop the stream-end receivers so the reconnect
                // tasks observe a closed channel and exit. Then drain
                // the JoinSet so the engine genuinely sees the tasks
                // finish before returning.
                drop(blocks);
                drop(logs);
                tasks.shutdown().await;
                info!(
                    dispatched_blocks,
                    dispatched_logs,
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
                drop(logs);
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

    // ------------------------------------------------------------------
    // MockChainProvider + reconnect-task integration tests
    // ------------------------------------------------------------------

    use std::future::Future;
    use std::sync::{Arc, Mutex};

    use crate::host::component::{ChainMethod, ChainProvider};
    use crate::host::provider_pool::{BlockStream, LogStream, ProviderError};
    use alloy_rpc_types_eth::{Filter, Log};
    use tokio::sync::mpsc as tokio_mpsc;

    /// Controllable mock for [`ChainProvider`]. Each method returns
    /// values pre-loaded by the test via the builder. Call history is
    /// recorded for assertions.
    #[derive(Clone)]
    struct MockChainProvider {
        inner: Arc<MockInner>,
    }

    struct MockInner {
        block_streams:
            Mutex<Vec<tokio_mpsc::Receiver<Result<alloy_rpc_types_eth::Header, ProviderError>>>>,
        log_streams: Mutex<Vec<tokio_mpsc::Receiver<Result<Log, ProviderError>>>>,
        block_numbers: Mutex<Vec<Result<u64, ProviderError>>>,
        logs_responses: Mutex<Vec<Result<Vec<Log>, ProviderError>>>,
        get_logs_calls: Mutex<Vec<Filter>>,
    }

    impl ChainProvider for MockChainProvider {
        fn subscribe_blocks(
            &self,
            _chain: Chain,
        ) -> impl Future<Output = Result<BlockStream, ProviderError>> + Send {
            let rx = self.inner.block_streams.lock().unwrap().remove(0);
            async move {
                let stream = receiver_stream(rx).map(|item| item);
                Ok(Box::pin(stream) as BlockStream)
            }
        }

        fn subscribe_logs(
            &self,
            _chain: Chain,
            _filter: Filter,
        ) -> impl Future<Output = Result<LogStream, ProviderError>> + Send {
            let rx = self.inner.log_streams.lock().unwrap().remove(0);
            async move {
                let stream = receiver_stream(rx);
                Ok(Box::pin(stream) as LogStream)
            }
        }

        fn get_block_number(
            &self,
            _chain: Chain,
        ) -> impl Future<Output = Result<u64, ProviderError>> + Send {
            let result = self.inner.block_numbers.lock().unwrap().remove(0);
            async move { result }
        }

        fn get_logs(
            &self,
            _chain: Chain,
            filter: Filter,
        ) -> impl Future<Output = Result<Vec<Log>, ProviderError>> + Send {
            self.inner.get_logs_calls.lock().unwrap().push(filter);
            let result = self.inner.logs_responses.lock().unwrap().remove(0);
            async move { result }
        }

        async fn request(
            &self,
            _chain: Chain,
            _method: ChainMethod,
            _params_json: String,
        ) -> Result<String, ProviderError> {
            Err(ProviderError::UnknownChain(Chain::from_id(0)))
        }
    }

    struct MockBuilder {
        block_streams:
            Vec<tokio_mpsc::Receiver<Result<alloy_rpc_types_eth::Header, ProviderError>>>,
        log_streams: Vec<tokio_mpsc::Receiver<Result<Log, ProviderError>>>,
        block_numbers: Vec<Result<u64, ProviderError>>,
        logs_responses: Vec<Result<Vec<Log>, ProviderError>>,
    }

    impl MockChainProvider {
        fn builder() -> MockBuilder {
            MockBuilder {
                block_streams: Vec::new(),
                log_streams: Vec::new(),
                block_numbers: Vec::new(),
                logs_responses: Vec::new(),
            }
        }
    }

    impl MockBuilder {
        fn add_log_stream(&mut self) -> tokio_mpsc::Sender<Result<Log, ProviderError>> {
            let (tx, rx) = tokio_mpsc::channel(64);
            self.log_streams.push(rx);
            tx
        }

        fn add_block_stream(
            &mut self,
        ) -> tokio_mpsc::Sender<Result<alloy_rpc_types_eth::Header, ProviderError>> {
            let (tx, rx) = tokio_mpsc::channel(64);
            self.block_streams.push(rx);
            tx
        }

        fn block_number(mut self, n: u64) -> Self {
            self.block_numbers.push(Ok(n));
            self
        }

        fn logs_response(mut self, logs: Vec<Log>) -> Self {
            self.logs_responses.push(Ok(logs));
            self
        }

        fn logs_err(mut self) -> Self {
            self.logs_responses
                .push(Err(ProviderError::UnknownChain(Chain::from_id(0))));
            self
        }

        fn build(self) -> MockChainProvider {
            MockChainProvider {
                inner: Arc::new(MockInner {
                    block_streams: Mutex::new(self.block_streams),
                    log_streams: Mutex::new(self.log_streams),
                    block_numbers: Mutex::new(self.block_numbers),
                    logs_responses: Mutex::new(self.logs_responses),
                    get_logs_calls: Mutex::new(Vec::new()),
                }),
            }
        }
    }

    fn log_at_block(n: u64) -> Log {
        Log {
            block_number: Some(n),
            ..Log::default()
        }
    }

    #[tokio::test(start_paused = true)]
    async fn log_task_backfills_on_reconnect() {
        let (tx, mut output_rx) = mpsc::channel(64);
        let mut builder = MockChainProvider::builder();

        // First subscription: we feed one event, then drop to
        // simulate disconnect.
        let stream1_tx = builder.add_log_stream();
        // Second subscription: resumed after reconnect + backfill.
        let stream2_tx = builder.add_log_stream();

        let mock = builder
            .block_number(103) // get_block_number on reconnect
            .logs_response(vec![
                log_at_block(101),
                log_at_block(102),
                log_at_block(103),
            ])
            .build();

        let chain = Chain::from_id(1);
        let filter = Filter::new();

        tokio::spawn(reconnecting_log_task(
            mock.clone(),
            "test_mod".into(),
            chain,
            filter,
            tx,
        ));

        // Feed first stream: one event at block 100.
        stream1_tx.send(Ok(log_at_block(100))).await.unwrap();
        let item = output_rx.recv().await.unwrap().unwrap();
        assert_eq!(item.2.block_number, Some(100));

        // Drop stream 1 -> simulates WS disconnect.
        drop(stream1_tx);

        // Backfill should dispatch 3 events (blocks 101, 102, 103).
        for expected in [101, 102, 103] {
            let item = output_rx.recv().await.unwrap().unwrap();
            assert_eq!(item.2.block_number, Some(expected));
        }

        // Live stream resumes with block 105.
        stream2_tx.send(Ok(log_at_block(105))).await.unwrap();
        let item = output_rx.recv().await.unwrap().unwrap();
        assert_eq!(item.2.block_number, Some(105));

        // Shut down cleanly.
        drop(output_rx);
    }

    #[tokio::test(start_paused = true)]
    async fn log_task_deduplicates_overlap_after_backfill() {
        let (tx, mut output_rx) = mpsc::channel(64);
        let mut builder = MockChainProvider::builder();

        let stream1_tx = builder.add_log_stream();
        let stream2_tx = builder.add_log_stream();

        let mock = builder
            .block_number(102)
            .logs_response(vec![log_at_block(101), log_at_block(102)])
            .build();

        let chain = Chain::from_id(1);
        let filter = Filter::new();

        tokio::spawn(reconnecting_log_task(
            mock.clone(),
            "test_mod".into(),
            chain,
            filter,
            tx,
        ));

        // First stream: one event at block 100.
        stream1_tx.send(Ok(log_at_block(100))).await.unwrap();
        output_rx.recv().await.unwrap().unwrap();
        drop(stream1_tx); // disconnect

        // Backfill dispatches blocks 101, 102.
        output_rx.recv().await.unwrap().unwrap(); // 101
        output_rx.recv().await.unwrap().unwrap(); // 102

        // Live stream sends overlapping events then a new one.
        stream2_tx.send(Ok(log_at_block(101))).await.unwrap();
        stream2_tx.send(Ok(log_at_block(102))).await.unwrap();
        stream2_tx.send(Ok(log_at_block(103))).await.unwrap();

        // Only block 103 should arrive (101, 102 are deduped).
        let item = output_rx.recv().await.unwrap().unwrap();
        assert_eq!(item.2.block_number, Some(103));

        drop(output_rx);
    }

    #[tokio::test(start_paused = true)]
    async fn log_task_continues_after_backfill_failure() {
        let (tx, mut output_rx) = mpsc::channel(64);
        let mut builder = MockChainProvider::builder();

        let stream1_tx = builder.add_log_stream();
        let stream2_tx = builder.add_log_stream();

        let mock = builder
            .block_number(102) // get_block_number succeeds
            .logs_err() // get_logs fails
            .build();

        let chain = Chain::from_id(1);
        let filter = Filter::new();

        tokio::spawn(reconnecting_log_task(
            mock.clone(),
            "test_mod".into(),
            chain,
            filter,
            tx,
        ));

        stream1_tx.send(Ok(log_at_block(100))).await.unwrap();
        output_rx.recv().await.unwrap().unwrap();
        drop(stream1_tx);

        // After backfill failure, live stream should still work and
        // no dedup guard should be set.
        stream2_tx.send(Ok(log_at_block(103))).await.unwrap();
        let item = output_rx.recv().await.unwrap().unwrap();
        assert_eq!(item.2.block_number, Some(103));

        stream2_tx.send(Ok(log_at_block(104))).await.unwrap();
        let item = output_rx.recv().await.unwrap().unwrap();
        assert_eq!(item.2.block_number, Some(104));

        drop(output_rx);
    }

    #[tokio::test(start_paused = true)]
    async fn log_task_clamps_large_backfill_gap() {
        let (tx, mut output_rx) = mpsc::channel(64);
        let mut builder = MockChainProvider::builder();

        let stream1_tx = builder.add_log_stream();
        let stream2_tx = builder.add_log_stream();

        let mock = builder
            .block_number(100_000) // gap of 99_900 blocks
            .logs_response(vec![log_at_block(95_000)])
            .build();

        let chain = Chain::from_id(1);
        let filter = Filter::new();

        tokio::spawn(reconnecting_log_task(
            mock.clone(),
            "test_mod".into(),
            chain,
            filter,
            tx,
        ));

        stream1_tx.send(Ok(log_at_block(100))).await.unwrap();
        output_rx.recv().await.unwrap().unwrap();
        drop(stream1_tx);

        // Backfill event should still arrive.
        let item = output_rx.recv().await.unwrap().unwrap();
        assert_eq!(item.2.block_number, Some(95_000));

        // Verify the filter used a clamped from_block.
        {
            let calls = mock.inner.get_logs_calls.lock().unwrap();
            assert_eq!(calls.len(), 1);
            let recorded = &calls[0];
            // from_block should be 100_000 - 10_000 + 1 = 90_001, not 101.
            let from = recorded.get_from_block().expect("from_block should be set");
            assert_eq!(from, 90_001);
        }

        // Live stream should resume.
        stream2_tx.send(Ok(log_at_block(100_001))).await.unwrap();
        let item = output_rx.recv().await.unwrap().unwrap();
        assert_eq!(item.2.block_number, Some(100_001));

        drop(output_rx);
    }

    #[tokio::test(start_paused = true)]
    async fn block_task_reconnects_and_dispatches() {
        let (tx, mut output_rx) = mpsc::channel(64);
        let mut builder = MockChainProvider::builder();

        let stream1_tx = builder.add_block_stream();
        let stream2_tx = builder.add_block_stream();

        let mock = builder.block_number(105).build();

        let chain = Chain::from_id(1);

        tokio::spawn(reconnecting_block_task(mock, chain, tx));

        // Feed a default header with block number set via Deref.
        let mut h1: alloy_rpc_types_eth::Header = Default::default();
        h1.inner.number = 100;
        stream1_tx.send(Ok(h1)).await.unwrap();
        let item = output_rx.recv().await.unwrap().unwrap();
        assert_eq!(item.1.number, 100);

        // Drop stream 1 -> reconnect.
        drop(stream1_tx);

        // Stream 2 delivers a new block.
        let mut h2: alloy_rpc_types_eth::Header = Default::default();
        h2.inner.number = 106;
        stream2_tx.send(Ok(h2)).await.unwrap();
        let item = output_rx.recv().await.unwrap().unwrap();
        assert_eq!(item.1.number, 106);

        drop(output_rx);
    }
}
