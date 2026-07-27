//! Open live chain event sources and dispatch their events to the supervisor
//! until shutdown. Blocks come from `eth_subscribe(newHeads)` (WS); chain-logs
//! from an `eth_getLogs` block-range poller (HTTP or WS) that recovers events
//! across a reconnect by re-querying the gap rather than dropping them.
//!
//! `open_block_streams` and `open_chain_log_streams` each spawn one
//! reconnect-aware task per subscription: it opens the stream, pumps items to
//! an mpsc channel, and on drop waits `restart_policy::backoff_for` before
//! reopening, resetting the backoff once the stream has been healthy for
//! `HEALTHY_WINDOW`. The tasks exit with [`TaskExit::ReceiverGone`] when `run`
//! drops the receivers; their handles collect into a [`TaskSet`] the loop
//! drains on shutdown.

use std::sync::Arc;
use std::time::{Duration, Instant};

use alloy_chains::Chain;
use futures::StreamExt;
use futures::stream::{BoxStream, select_all};
use thiserror::Error;
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::bindings::nexum;
use crate::host::component::{ChainProvider, RuntimeTypes};
use crate::host::extension::{ExtensionEvent, ExtensionEventStream};
use crate::host::provider_pool::ProviderError;
use crate::runtime::restart_policy::backoff_for;
use crate::supervisor::{ChainLogSub, Supervisor};
use nexum_tasks::{TaskExecutor, TaskExit, TaskSet};

/// Errors carried by the tagged block and chain-log streams.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum StreamError {
    /// Provider or transport failure opening or pumping the subscription.
    #[error(transparent)]
    Provider(#[from] ProviderError),
}

/// Uninterrupted-event duration before the backoff counter resets to 0.
const HEALTHY_WINDOW: Duration = Duration::from_secs(60);

/// Silence between block events beyond which the next event logs a gap-closed
/// line, surfacing an alloy-internal transport reconnect that produced no
/// `stream ended` event.
const BLOCK_GAP_LOG_THRESHOLD: Duration = Duration::from_secs(60);

/// Channel buffer for each reconnect task.
const RECONNECT_CHANNEL_BUF: usize = 64;

/// Block-gap size at or above which a re-open logs a large-backfill notice.
const LARGE_GAP_LOG_THRESHOLD: u64 = 1_000;

/// Open one reconnect-aware block-subscription task per chain, spawned via
/// `executor` with handles pushed into `tasks` for graceful shutdown.
pub fn open_block_streams<C>(
    pool: &C,
    chains: &[Chain],
    executor: &TaskExecutor,
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
        tasks.push(executor.spawn(reconnecting_block_task(pool, chain, tx)));
        let tagged: TaggedBlockStream = Box::pin(receiver_stream(rx));
        streams.push(tagged);
    }
    streams
}

/// Open one reconnect-aware chain-log task per subscription; see
/// [`open_block_streams`].
pub fn open_chain_log_streams<C>(
    pool: &C,
    subs: Vec<ChainLogSub>,
    executor: &TaskExecutor,
    tasks: &mut TaskSet,
) -> Vec<TaggedChainLogStream>
where
    C: ChainProvider + Clone + Send + Sync + 'static,
{
    let mut streams = Vec::new();
    for sub in subs {
        let (tx, rx) = mpsc::channel::<TaggedChainLog>(RECONNECT_CHANNEL_BUF);
        let pool = pool.clone();
        let resume = ChainLogResume {
            // The cursor key is constant per subscription and cloned onto every
            // log; `Arc` keeps that clone cheap.
            cursor_key: sub.cursor_key.map(Arc::from),
            initial_cursor: sub.initial_cursor,
            max_lookback: sub.max_lookback,
        };
        tasks.push(executor.spawn(reconnecting_chain_log_task(
            pool, sub.module, sub.chain, sub.filter, resume, tx,
        )));
        let tagged: TaggedChainLogStream = Box::pin(receiver_stream(rx));
        streams.push(tagged);
    }
    streams
}

/// Wrap an `mpsc::Receiver<T>` as a `Stream<Item = T>`.
fn receiver_stream<T: Send + 'static>(
    rx: mpsc::Receiver<T>,
) -> impl futures::Stream<Item = T> + Send {
    futures::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|item| (item, rx))
    })
}

/// Reconnect-aware loop for one chain's block subscription: re-opens the
/// `eth_subscribe` stream with exponential backoff after every drop or error.
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

/// Per-subscription resume and backfill knobs for a chain-log task.
struct ChainLogResume {
    /// Durable cursor key; `Some` for a `resume` subscription.
    cursor_key: Option<Arc<str>>,
    /// Persisted resume block read at boot; the first open starts here.
    initial_cursor: Option<u64>,
    /// Opt-in cap in blocks on backfill depth; `None` backfills the whole gap.
    max_lookback: Option<u64>,
}

/// Poller-backed loop for one (module, chain) chain-log subscription. Drives
/// the `eth_getLogs` block-range poller, which reconciles reorgs and re-queries
/// gaps internally. On a terminal poller error it re-opens from the block after
/// the last delivered one and backfills the whole missed range, bounded only by
/// `max_lookback` if set.
async fn reconnecting_chain_log_task<C>(
    pool: C,
    module: String,
    chain: Chain,
    filter: alloy_rpc_types_eth::Filter,
    resume: ChainLogResume,
    tx: mpsc::Sender<TaggedChainLog>,
) -> TaskExit
where
    C: ChainProvider + Send + Sync + 'static,
{
    let ChainLogResume {
        cursor_key,
        initial_cursor,
        max_lookback,
    } = resume;
    let chain_id = chain.id();
    let mut attempt: u32 = 0;
    let mut last_event: Option<Instant> = None;
    // Highest block whose logs we have delivered; the resume point after a
    // poller re-open, so the missed range is synced back rather than skipped.
    let mut last_seen_block: Option<u64> = None;
    // Persisted resume cursor, consumed on the first open: for a `resume`
    // subscription the poller starts here (replaying the block in full)
    // rather than at head. `None` for a fresh or non-resume subscription.
    let mut boot_resume: Option<u64> = initial_cursor;
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
        // Choosing the poller start block:
        // - `boot_resume` (persisted cursor, first open only): resume AT the
        //   cursor block, replaying it in full so a mid-block crash before
        //   the restart loses nothing. Never past head: a reorg that left
        //   the cursor ahead of head starts at head and lets the poller
        //   catch up.
        // - otherwise a re-open resumes just after the last delivered block
        //   (within-process gap-free), or at head on the very first open.
        // Either way the whole gap is backfilled with no lower floor, so
        // nothing is skipped unless the subscription set `max_lookback`.
        let mut start_block = match boot_resume.take() {
            Some(resume) => resume.min(head),
            None => poller_resume_block(last_seen_block, head),
        };
        // Opt-in bound: `max_lookback` caps how far back a resume
        // subscription backfills. The default (`None`) backfills fully; a
        // set cap clamps the start up to `head - cap` and surfaces the
        // dropped oldest blocks.
        if let Some(cap) = max_lookback {
            let floor = head.saturating_sub(cap);
            if start_block < floor {
                warn!(
                    module = %module,
                    chain_id,
                    skipped_from = start_block,
                    skipped_to = floor,
                    "chain-log gap exceeds max_lookback - skipping the oldest missed blocks",
                );
                start_block = floor;
            }
        }
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
                                let tagged = Ok((module.clone(), chain, log, cursor_key.clone()));
                                if tx.send(tagged).await.is_err() {
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
/// One tagged chain-log item: `(module, chain, log, cursor_key)` or a stream
/// error. `cursor_key` is `Some` for a `resume` subscription and threads the
/// durable cursor key to the dispatch site.
pub type TaggedChainLog =
    Result<(String, Chain, alloy_rpc_types_eth::Log, Option<Arc<str>>), StreamError>;
pub type TaggedChainLogStream =
    std::pin::Pin<Box<dyn futures::Stream<Item = TaggedChainLog> + Send>>;
/// Drive the supervisor with events until `shutdown` resolves.
///
/// `shutdown` is observed only between dispatches, never mid-`call_on_event`,
/// so an in-flight wasmtime call finishes before the loop exits; the guard it
/// yields is held until return, so the drain covers the final dispatch and
/// cursor commit. Returns the `(blocks, chain_logs)` dispatch tally.
pub async fn run<T: RuntimeTypes, G>(
    supervisor: &mut Supervisor<T>,
    block_streams: Vec<TaggedBlockStream>,
    chain_log_streams: Vec<TaggedChainLogStream>,
    extension_streams: Vec<ExtensionEventStream>,
    tasks: TaskSet,
    shutdown: impl std::future::Future<Output = G> + Send,
) -> (u64, u64) {
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
    let mut extension_events: BoxStream<'_, _> = if extension_streams.is_empty() {
        futures::stream::pending().boxed()
    } else {
        select_all(extension_streams).boxed()
    };
    let mut shutdown = Box::pin(shutdown);
    let mut dispatched_blocks: u64 = 0;
    let mut dispatched_chain_logs: u64 = 0;
    let mut dispatched_extension_events: u64 = 0;
    let started = Instant::now();
    loop {
        // Phase 1: pick the next event OR observe shutdown. The
        // dispatch itself happens in phase 2 (outside the select)
        // so an in-flight wasmtime call never gets cancelled by a
        // shutdown signal arriving mid-dispatch.
        enum NextEvent<G> {
            Block(nexum::host::types::Block),
            // The alloy `Log` is boxed so the `Chain` tag does not push
            // the enum past the large-variant lint threshold.
            ChainLog(
                String,
                Chain,
                Box<alloy_rpc_types_eth::Log>,
                Option<Arc<str>>,
            ),
            Extension(ExtensionEvent),
            // Carries the drain guard `shutdown` yielded.
            Shutdown(G),
            StreamPanic(&'static str),
        }
        let next = tokio::select! {
            biased;
            guard = &mut shutdown => NextEvent::Shutdown(guard),
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
                Some(Ok((module, chain, log, cursor_key))) => {
                    NextEvent::ChainLog(module, chain, Box::new(log), cursor_key)
                }
                Some(Err(err)) => {
                    warn!(error = %err, "chain-log stream error - continuing");
                    continue;
                }
                None => NextEvent::StreamPanic("chain-log"),
            },
            next = extension_events.next() => match next {
                Some(event) => NextEvent::Extension(event),
                // Extension source tasks loop forever; `None` means one exited.
                None => NextEvent::StreamPanic("extension-event"),
            },
        };

        match next {
            NextEvent::Block(block) => {
                supervisor.dispatch_block(block).await;
                dispatched_blocks += 1;
            }
            NextEvent::ChainLog(module, chain, log, cursor_key) => {
                supervisor
                    .dispatch_chain_log(&module, chain, *log, cursor_key.as_deref())
                    .await;
                dispatched_chain_logs += 1;
            }
            NextEvent::Extension(event) => {
                supervisor.dispatch_extension_event(event).await;
                dispatched_extension_events += 1;
            }
            NextEvent::Shutdown(guard) => {
                // Drop the stream-end receivers so the reconnect
                // tasks observe a closed channel and exit. Then drain
                // the task set so the engine genuinely sees the tasks
                // finish before returning.
                drop(blocks);
                drop(chain_logs);
                drop(extension_events);
                tasks.shutdown().await;
                info!(
                    dispatched_blocks,
                    dispatched_chain_logs,
                    dispatched_extension_events,
                    uptime_secs = started.elapsed().as_secs(),
                    "graceful shutdown complete",
                );
                drop(guard);
                return (dispatched_blocks, dispatched_chain_logs);
            }
            NextEvent::StreamPanic(kind) => {
                // Reconnect tasks should loop forever.
                // Hitting `None` from `select_all` means the task
                // exited (panic or channel closed). Bail loudly.
                drop(blocks);
                drop(chain_logs);
                drop(extension_events);
                tasks.shutdown().await;
                warn!(
                    kind,
                    "reconnect task ended unexpectedly - shutting down for engine restart"
                );
                return (dispatched_blocks, dispatched_chain_logs);
            }
        }
    }
}

/// Start block for a re-opened log poller: `None` (first open) starts at head;
/// otherwise just after the last delivered block, backfilling the whole gap.
fn poller_resume_block(last_seen_block: Option<u64>, head: u64) -> u64 {
    match last_seen_block {
        None => head,
        Some(last) => last.saturating_add(1),
    }
}

/// `Some(gap)` when `now` is at least `threshold` past the last event; `None`
/// on the first event or when events arrive within `threshold`.
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

    // ── Structural tests: per-stream task allocation (#56) ──────────────────

    /// `open_block_streams` spawns one independent reconnect task per chain.
    #[tokio::test]
    async fn open_block_streams_opens_one_task_per_chain() {
        use crate::test_utils::MockChainProvider;
        use nexum_tasks::TaskManager;

        let pool = MockChainProvider::new();
        let manager = TaskManager::new();
        let executor = manager.executor();
        let mut tasks = TaskSet::new();
        let chains = vec![
            alloy_chains::Chain::mainnet(),
            alloy_chains::Chain::from_id(100),
        ];
        let streams = open_block_streams(&pool, &chains, &executor, &mut tasks);
        assert_eq!(streams.len(), 2, "one stream per chain");
        tasks.shutdown().await;
    }

    /// `open_chain_log_streams` spawns one reconnect task per subscription.
    #[tokio::test]
    async fn open_chain_log_streams_opens_one_task_per_subscription() {
        use crate::test_utils::MockChainProvider;
        use nexum_tasks::TaskManager;

        let pool = MockChainProvider::new();
        let manager = TaskManager::new();
        let executor = manager.executor();
        let mut tasks = TaskSet::new();
        let subs = vec![
            ChainLogSub {
                module: "mod-a".to_string(),
                chain: alloy_chains::Chain::mainnet(),
                filter: alloy_rpc_types_eth::Filter::default(),
                cursor_key: None,
                initial_cursor: None,
                max_lookback: None,
            },
            ChainLogSub {
                module: "mod-b".to_string(),
                chain: alloy_chains::Chain::mainnet(),
                filter: alloy_rpc_types_eth::Filter::default(),
                cursor_key: None,
                initial_cursor: None,
                max_lookback: None,
            },
        ];
        let streams = open_chain_log_streams(&pool, subs, &executor, &mut tasks);
        assert_eq!(streams.len(), 2, "one stream per subscription");
        tasks.shutdown().await;
    }

    /// A reconnect task whose receiver drops exits on its own with
    /// [`TaskExit::ReceiverGone`], not via abort.
    #[tokio::test]
    async fn reconnect_task_exits_receiver_gone_when_receiver_drops() {
        use crate::test_utils::MockChainProvider;
        use nexum_tasks::TaskManager;

        let pool = MockChainProvider::new();
        // Buffer one header so the task has an item to forward - the
        // failing `tx.send` against the dropped receiver is the exit path
        // under test.
        pool.push_block(alloy_rpc_types_eth::Header::default());

        let manager = TaskManager::new();
        let executor = manager.executor();
        let (tx, rx) = mpsc::channel(1);
        let handle = executor.spawn(reconnecting_block_task(
            pool.clone(),
            alloy_chains::Chain::mainnet(),
            tx,
        ));
        drop(rx);

        let exit = tokio::time::timeout(Duration::from_secs(5), handle.join())
            .await
            .expect("task must exit promptly once the receiver is gone");
        assert_eq!(
            exit,
            Some(TaskExit::ReceiverGone),
            "the task must exit naturally, not via abort (abort yields None)",
        );
    }

    // ── block_stream_gap_to_log unit tests ──────────────────────────────────

    /// No prior event yields `None`.
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
