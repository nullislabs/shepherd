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

use futures::StreamExt;
use futures::stream::{BoxStream, select_all};
use thiserror::Error;
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tracing::{info, warn};

use crate::bindings::nexum;
use crate::host::provider_pool::{ProviderError, ProviderPool};
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

/// Per-chain block subscriptions, one reconnect-aware task per
/// chain id. Tasks are spawned into `tasks` so the caller can drive
/// graceful shutdown (the engine awaits the set after closing its
/// receivers - the tasks exit cleanly when the receiver drops).
pub async fn open_block_streams(
    pool: &ProviderPool,
    chains: &std::collections::BTreeSet<u64>,
    tasks: &mut JoinSet<()>,
) -> Vec<TaggedBlockStream> {
    let mut streams = Vec::new();
    for &chain_id in chains {
        let (tx, rx) = mpsc::channel::<Result<(u64, alloy_rpc_types_eth::Header), StreamError>>(
            RECONNECT_CHANNEL_BUF,
        );
        let pool = pool.clone();
        tasks.spawn(reconnecting_block_task(pool, chain_id, tx));
        let tagged: TaggedBlockStream = Box::pin(receiver_stream(rx));
        streams.push(tagged);
    }
    streams
}

/// Per-module log subscriptions. Each entry gets its own reconnect-
/// aware task tagged with the owning module name + chain id. Tasks
/// are spawned into `tasks` (see [`open_block_streams`]).
pub async fn open_log_streams(
    pool: &ProviderPool,
    subs: Vec<(String, u64, alloy_rpc_types_eth::Filter)>,
    tasks: &mut JoinSet<()>,
) -> Vec<TaggedLogStream> {
    let mut streams = Vec::new();
    for (module, chain_id, filter) in subs {
        let (tx, rx) = mpsc::channel::<Result<(String, u64, alloy_rpc_types_eth::Log), StreamError>>(
            RECONNECT_CHANNEL_BUF,
        );
        let pool = pool.clone();
        tasks.spawn(reconnecting_log_task(pool, module, chain_id, filter, tx));
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

/// Reconnect-aware loop for a single chain's block subscription.
/// Holds `(pool, chain_id)` and re-opens the underlying alloy
/// `eth_subscribe` stream with exponential backoff after every drop
/// or transport error.
async fn reconnecting_block_task(
    pool: ProviderPool,
    chain_id: u64,
    tx: mpsc::Sender<Result<(u64, alloy_rpc_types_eth::Header), StreamError>>,
) {
    let mut attempt: u32 = 0;
    let mut last_event: Option<Instant> = None;
    loop {
        match pool.subscribe_blocks(chain_id).await {
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
                        .map(|header| (chain_id, header))
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
async fn reconnecting_log_task(
    pool: ProviderPool,
    module: String,
    chain_id: u64,
    filter: alloy_rpc_types_eth::Filter,
    tx: mpsc::Sender<Result<(String, u64, alloy_rpc_types_eth::Log), StreamError>>,
) {
    let mut attempt: u32 = 0;
    let mut last_event: Option<Instant> = None;
    loop {
        match pool.subscribe_logs(chain_id, filter.clone()).await {
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
                    let module_name = module.clone();
                    let tagged = item
                        .map(|log| (module_name, chain_id, log))
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
    Box<dyn futures::Stream<Item = Result<(u64, alloy_rpc_types_eth::Header), StreamError>> + Send>,
>;
pub type TaggedLogStream = std::pin::Pin<
    Box<
        dyn futures::Stream<Item = Result<(String, u64, alloy_rpc_types_eth::Log), StreamError>>
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
pub async fn run(
    supervisor: &mut Supervisor,
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
            Log(String, u64, alloy_rpc_types_eth::Log),
            Shutdown,
            StreamPanic(&'static str),
        }
        let next = tokio::select! {
            biased;
            () = &mut shutdown => NextEvent::Shutdown,
            next = blocks.next() => match next {
                Some(Ok((chain_id, header))) => NextEvent::Block(nexum::host::types::Block {
                    chain_id,
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
                Some(Ok((module, chain_id, log))) => NextEvent::Log(module, chain_id, log),
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
            NextEvent::Log(module, chain_id, log) => {
                supervisor.dispatch_log(&module, chain_id, log).await;
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
}
