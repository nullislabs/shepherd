//! Strategy-keeper stores: the persistent-state conventions shared by
//! conditional-commitment modules, expressed over [`LocalStoreHost`]
//! alone so they compile for any world and test against the in-memory
//! mocks.
//!
//! Three stores cover the machinery watcher modules hand-roll:
//!
//! - [`WatchSet`] - the watch-set registry, one `watch:{owner}:{hash}`
//!   row per conditional commitment.
//! - [`Gates`] - `next_block:` / `next_epoch:` gate keys holding a
//!   u64 little-endian threshold, with an
//!   [`is_ready`](Gates::is_ready) predicate the poll loop consults.
//! - [`Journal`] - the receipt-keyed idempotency journal of
//!   `submitted:` / `observed:` presence markers.
//!
//! Two pieces drive the stores from the poll loop:
//!
//! - [`ConditionalSource`] - the world-neutral poll seam: one watch in,
//!   one outcome out, at a given [`Tick`]. Implementations own the
//!   transport and the outcome shape.
//! - [`Retrier`] - runs a [`RetryAction`]'s effect through the
//!   stores after a failed run attempt.
//!
//! [`WatchRef`] ties the first two together: gate keys are derived
//! from the exact hex substrings of the stored watch key, and
//! [`WatchSet::remove`] drops a watch together with all of its gate
//! keys so no failure path can orphan a gate.
//!
//! ```
//! use nexum_sdk::keeper::{Gates, Journal, WatchRef, WatchSet};
//! use nexum_sdk::host::{Fault, LocalStoreHost};
//! use nexum_sdk::prelude::*;
//!
//! # use std::cell::RefCell;
//! # use std::collections::BTreeMap;
//! # #[derive(Default)]
//! # struct StubStore(RefCell<BTreeMap<String, Vec<u8>>>);
//! # impl LocalStoreHost for StubStore {
//! #     fn get(&self, key: &str) -> Result<Option<Vec<u8>>, Fault> {
//! #         Ok(self.0.borrow().get(key).cloned())
//! #     }
//! #     fn set(&self, key: &str, value: &[u8]) -> Result<(), Fault> {
//! #         self.0.borrow_mut().insert(key.into(), value.into());
//! #         Ok(())
//! #     }
//! #     fn delete(&self, key: &str) -> Result<(), Fault> {
//! #         self.0.borrow_mut().remove(key);
//! #         Ok(())
//! #     }
//! #     fn list_keys(&self, prefix: &str) -> Result<Vec<String>, Fault> {
//! #         Ok(self
//! #             .0
//! #             .borrow()
//! #             .keys()
//! #             .filter(|k| k.starts_with(prefix))
//! #             .cloned()
//! #             .collect())
//! #     }
//! # }
//! let host = StubStore::default();
//! let watches = WatchSet::new(&host);
//! let key = watches.put(&Address::ZERO, &B256::ZERO, b"params")?;
//! let watch = WatchRef::parse(&key).expect("well-formed key");
//!
//! let gates = Gates::new(&host);
//! gates.set_next_block(watch, 100)?;
//! assert!(!gates.is_ready(watch, 99, 0)?);
//! assert!(gates.is_ready(watch, 100, 0)?);
//!
//! let journal = Journal::submitted(&host);
//! journal.record("0xuid")?;
//! assert!(journal.contains("0xuid")?);
//!
//! watches.remove(watch)?;
//! assert!(watches.list()?.is_empty());
//! # Ok::<(), Fault>(())
//! ```

use alloy_primitives::{Address, B256};
use strum::IntoStaticStr;

use crate::host::{Fault, LocalStoreHost};

/// Prefix of every watch-set row.
pub const WATCH_PREFIX: &str = "watch:";
/// Prefix of the block-height gate row paired with a watch.
pub const NEXT_BLOCK_PREFIX: &str = "next_block:";
/// Prefix of the Unix-seconds gate row paired with a watch.
pub const NEXT_EPOCH_PREFIX: &str = "next_epoch:";
/// Journal prefix for receipts the module posted upstream itself: the
/// submit path recorded that it has sent an order on.
pub const SUBMITTED_PREFIX: &str = "submitted:";
/// Journal prefix for receipts the module confirmed but did not post:
/// the observe-and-verify path (e.g. ethflow) recorded an existing
/// upstream order as seen.
pub const OBSERVED_PREFIX: &str = "observed:";

/// Borrowed view of a watch key's two hex halves, parsed from a
/// `watch:{owner}:{hash}` row. Gate keys are derived from the exact
/// substrings of the stored key, so a parse-then-derive round trip is
/// byte-stable regardless of how the original writer cased the hex.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WatchRef<'k> {
    owner_hex: &'k str,
    hash_hex: &'k str,
}

impl<'k> WatchRef<'k> {
    /// Parse a `watch:{owner}:{hash}` key. `None` when the prefix or
    /// the separating colon is missing, or when either half is empty
    /// (an empty half would derive a degenerate gate key like
    /// `next_block::`).
    pub fn parse(key: &'k str) -> Option<Self> {
        let rest = key.strip_prefix(WATCH_PREFIX)?;
        let (owner_hex, hash_hex) = rest.split_once(':')?;
        if owner_hex.is_empty() || hash_hex.is_empty() {
            return None;
        }
        Some(Self {
            owner_hex,
            hash_hex,
        })
    }

    /// The owner half, verbatim from the key.
    pub fn owner_hex(&self) -> &'k str {
        self.owner_hex
    }

    /// The commitment-hash half, verbatim from the key.
    pub fn hash_hex(&self) -> &'k str {
        self.hash_hex
    }

    /// Rebuild the full watch key.
    pub fn key(&self) -> String {
        format!("{WATCH_PREFIX}{}:{}", self.owner_hex, self.hash_hex)
    }

    /// The `next_block:` gate key paired with this watch.
    pub fn next_block_key(&self) -> String {
        format!("{NEXT_BLOCK_PREFIX}{}:{}", self.owner_hex, self.hash_hex)
    }

    /// The `next_epoch:` gate key paired with this watch.
    pub fn next_epoch_key(&self) -> String {
        format!("{NEXT_EPOCH_PREFIX}{}:{}", self.owner_hex, self.hash_hex)
    }
}

/// Watch-set registry: one row per conditional commitment, keyed
/// `watch:{owner}:{hash}` with the encoded commitment parameters as
/// the value.
pub struct WatchSet<'h, H> {
    host: &'h H,
}

impl<'h, H: LocalStoreHost> WatchSet<'h, H> {
    /// Registry view over the given host.
    pub fn new(host: &'h H) -> Self {
        Self { host }
    }

    /// Canonical key for an owner / commitment-hash pair (lowercase
    /// `0x`-prefixed hex on both halves).
    pub fn key(owner: &Address, hash: &B256) -> String {
        format!("{WATCH_PREFIX}{owner:#x}:{hash:#x}")
    }

    /// Insert or overwrite the watch row; returns the key written.
    /// Overwriting in place makes re-indexing a replayed log a no-op.
    pub fn put(&self, owner: &Address, hash: &B256, value: &[u8]) -> Result<String, Fault> {
        let key = Self::key(owner, hash);
        self.host.set(&key, value)?;
        Ok(key)
    }

    /// The stored value. `Ok(None)` when the watch is absent.
    pub fn get(&self, watch: WatchRef<'_>) -> Result<Option<Vec<u8>>, Fault> {
        self.host.get(&watch.key())
    }

    /// Every watch key currently registered.
    pub fn list(&self) -> Result<Vec<String>, Fault> {
        self.host.list_keys(WATCH_PREFIX)
    }

    /// Drop the watch together with both of its gate keys. Gates go
    /// first: a fault part-way leaves the watch row behind so a retry
    /// re-drops it, and a gate key can never outlive its watch.
    pub fn remove(&self, watch: WatchRef<'_>) -> Result<(), Fault> {
        Gates::new(self.host).clear(watch)?;
        self.host.delete(&watch.key())
    }
}

/// Gate-key discipline: `next_block:{owner}:{hash}` and
/// `next_epoch:{owner}:{hash}` rows holding a u64 little-endian
/// threshold. A malformed or absent row reads as "no gate", so a
/// corrupt value can only make a watch poll sooner, never wedge it.
pub struct Gates<'h, H> {
    host: &'h H,
}

impl<'h, H: LocalStoreHost> Gates<'h, H> {
    /// Gate view over the given host.
    pub fn new(host: &'h H) -> Self {
        Self { host }
    }

    /// Skip polls until the chain reaches `block`.
    pub fn set_next_block(&self, watch: WatchRef<'_>, block: u64) -> Result<(), Fault> {
        self.host.set(&watch.next_block_key(), &block.to_le_bytes())
    }

    /// Skip polls until the Unix-seconds clock reaches `epoch_s`.
    pub fn set_next_epoch(&self, watch: WatchRef<'_>, epoch_s: u64) -> Result<(), Fault> {
        self.host
            .set(&watch.next_epoch_key(), &epoch_s.to_le_bytes())
    }

    /// Whether the watch is clear to poll at the given block height
    /// and Unix-seconds timestamp. Both gates must pass; each is
    /// inclusive at its threshold.
    #[must_use = "the readiness verdict gates the poll; `?` alone drops the inner bool"]
    pub fn is_ready(&self, watch: WatchRef<'_>, block: u64, epoch_s: u64) -> Result<bool, Fault> {
        if let Some(next) = self.read_u64(&watch.next_block_key())?
            && block < next
        {
            return Ok(false);
        }
        if let Some(next) = self.read_u64(&watch.next_epoch_key())?
            && epoch_s < next
        {
            return Ok(false);
        }
        Ok(true)
    }

    /// Delete both gate keys. No-op for gates never set.
    pub fn clear(&self, watch: WatchRef<'_>) -> Result<(), Fault> {
        self.host.delete(&watch.next_block_key())?;
        self.host.delete(&watch.next_epoch_key())
    }

    fn read_u64(&self, key: &str) -> Result<Option<u64>, Fault> {
        // Absent key: silently no gate. Present but wrong length: the
        // value is corrupt, so warn before falling open to no gate -
        // fail-open is deliberate (a corrupt value can only make the
        // watch poll sooner), but it must not pass unobserved.
        let Some(b) = self.host.get(key)? else {
            return Ok(None);
        };
        match <[u8; 8]>::try_from(b.as_slice()) {
            Ok(bytes) => Ok(Some(u64::from_le_bytes(bytes))),
            Err(_) => {
                tracing::warn!(%key, len = b.len(), "gate value corrupt; treating as absent");
                Ok(None)
            }
        }
    }
}

/// Receipt-keyed idempotency journal: presence markers under a fixed
/// prefix. The marker value is empty - presence of the key is the
/// receipt - so re-recording is idempotent by construction.
pub struct Journal<'h, H> {
    host: &'h H,
    prefix: &'static str,
}

impl<'h, H: LocalStoreHost> Journal<'h, H> {
    /// Journal of receipts this module has submitted upstream
    /// (`submitted:` markers).
    pub fn submitted(host: &'h H) -> Self {
        Self {
            host,
            prefix: SUBMITTED_PREFIX,
        }
    }

    /// Journal of receipts this module has observed upstream
    /// (`observed:` markers).
    pub fn observed(host: &'h H) -> Self {
        Self {
            host,
            prefix: OBSERVED_PREFIX,
        }
    }

    /// Record the receipt.
    pub fn record(&self, receipt: &str) -> Result<(), Fault> {
        self.host.set(&format!("{}{receipt}", self.prefix), b"")
    }

    /// Whether the receipt is already journalled.
    pub fn contains(&self, receipt: &str) -> Result<bool, Fault> {
        Ok(self
            .host
            .get(&format!("{}{receipt}", self.prefix))?
            .is_some())
    }
}

/// One poll dispatch's world view: chain, block height, and the block
/// clock in Unix seconds. Gate checks and backoff arithmetic read the
/// same instant a source is polled at, so a watch can never gate
/// itself against a clock it was not judged by.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Tick {
    /// Chain the dispatch targets.
    pub chain_id: u64,
    /// Block height at the tick.
    pub block: u64,
    /// Block timestamp, Unix seconds.
    pub epoch_s: u64,
}

/// A source of conditional commitments: poll one watch, produce one
/// outcome. Generic over the host so implementations stay mock-
/// testable; deliberately no venue-transport abstraction - the source
/// owns its own wire (an `eth_call`, an HTTP probe, a stub).
///
/// A transient failure should surface as a retry-flavoured outcome,
/// not tear down the caller's sweep: `poll` is infallible by contract.
pub trait ConditionalSource<H> {
    /// What one poll produces.
    type Outcome;

    /// Poll the source for `watch` at `tick`. `params` is the stored
    /// watch value (the encoded commitment parameters), passed
    /// verbatim so the source owns the decode.
    fn poll(&self, host: &H, watch: WatchRef<'_>, params: &[u8], tick: &Tick) -> Self::Outcome;
}

/// What the retry ledger should do to a watch after a failed
/// run attempt.
///
/// `IntoStaticStr` exposes each variant as a snake_case `&'static
/// str` for log and metric labels. `#[non_exhaustive]` so the
/// contract can grow a variant; downstream dispatch should treat an
/// unknown variant as "leave the watch in place" (the conservative
/// choice).
#[derive(Clone, Copy, Debug, Eq, PartialEq, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
#[non_exhaustive]
pub enum RetryAction {
    /// Leave the watch untouched; the next tick re-attempts.
    TryNextBlock,
    /// Gate the watch until `now + seconds` on the epoch clock.
    Backoff {
        /// Seconds to wait before retrying.
        seconds: u64,
    },
    /// Remove the watch and its gates; no retry can succeed.
    Drop,
}

/// Retry ledger: runs a [`RetryAction`]'s effect through the keeper
/// stores. `Backoff` saturates at `u64::MAX` on the epoch clock;
/// `Drop` delegates to [`WatchSet::remove`], so gates go first and no
/// failure path can orphan one.
pub struct Retrier<'h, H> {
    host: &'h H,
}

impl<'h, H: LocalStoreHost> Retrier<'h, H> {
    /// Ledger view over the given host.
    pub fn new(host: &'h H) -> Self {
        Self { host }
    }

    /// Apply `action` to the watch, with `now_epoch_s` as the backoff
    /// origin.
    pub fn apply(
        &self,
        watch: WatchRef<'_>,
        action: RetryAction,
        now_epoch_s: u64,
    ) -> Result<(), Fault> {
        match action {
            RetryAction::TryNextBlock => Ok(()),
            RetryAction::Backoff { seconds } => {
                Gates::new(self.host).set_next_epoch(watch, now_epoch_s.saturating_add(seconds))
            }
            RetryAction::Drop => WatchSet::new(self.host).remove(watch),
        }
    }
}
