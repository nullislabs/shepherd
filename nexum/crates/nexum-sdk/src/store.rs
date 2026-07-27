//! Typed local-store helpers over the [`LocalStoreHost`] seam:
//! [`WriteBatch`], [`clear_prefix`], [`TypedCell`], [`TypedMap`], and
//! [`Counter`], so module code hand-rolls neither serialization nor
//! batching. Typed values cross the store as borsh bytes.
//!
//! Batch atomicity follows [`LocalStoreHost::apply`]: all-or-nothing on
//! the real host adapter, per-op on the trait's fallback.

use core::marker::PhantomData;

use borsh::{BorshDeserialize, BorshSerialize};

use crate::host::{Fault, LocalStoreHost, WriteOp};

/// Encode as borsh bytes; a failure folds to [`Fault::Internal`].
fn encode<T: BorshSerialize>(value: &T) -> Result<Vec<u8>, Fault> {
    borsh::to_vec(value).map_err(|e| Fault::Internal(format!("borsh encode failed: {e}")))
}

/// Decode borsh bytes (trailing bytes are malformed); a failure folds
/// to [`Fault::Internal`] naming the key.
fn decode<T: BorshDeserialize>(key: &str, bytes: &[u8]) -> Result<T, Fault> {
    borsh::from_slice(bytes)
        .map_err(|e| Fault::Internal(format!("stored value at `{key}` failed to decode: {e}")))
}

/// Stages set/delete ops, flushed in one [`LocalStoreHost::apply`]
/// call. Dropping an unflushed batch discards it. The host caps a
/// batch at 1024 ops and 4 MiB; a larger batch fails whole.
pub struct WriteBatch<'h, H> {
    host: &'h H,
    ops: Vec<WriteOp>,
}

impl<'h, H: LocalStoreHost> WriteBatch<'h, H> {
    /// Empty batch over the given host.
    pub fn new(host: &'h H) -> Self {
        Self {
            host,
            ops: Vec::new(),
        }
    }

    /// Stage an insert-or-overwrite.
    pub fn set(&mut self, key: impl Into<String>, value: impl Into<Vec<u8>>) -> &mut Self {
        self.ops.push(WriteOp::Set {
            key: key.into(),
            value: value.into(),
        });
        self
    }

    /// Stage a delete.
    pub fn delete(&mut self, key: impl Into<String>) -> &mut Self {
        self.ops.push(WriteOp::Delete { key: key.into() });
        self
    }

    /// Staged op count.
    pub fn len(&self) -> usize {
        self.ops.len()
    }

    /// Whether nothing is staged.
    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    /// Flush every staged op in one [`LocalStoreHost::apply`] call; a
    /// no-op when nothing is staged.
    pub fn flush(self) -> Result<(), Fault> {
        if self.ops.is_empty() {
            return Ok(());
        }
        self.host.apply(&self.ops)
    }
}

/// Delete every key under `prefix` in one [`LocalStoreHost::apply`]
/// call; returns the number of keys deleted. Fails whole past the
/// host's 1024-key batch cap; chunk manually for larger prefixes.
pub fn clear_prefix(host: &impl LocalStoreHost, prefix: &str) -> Result<u64, Fault> {
    let ops: Vec<WriteOp> = host
        .list_keys(prefix)?
        .into_iter()
        .map(|key| WriteOp::Delete { key })
        .collect();
    if ops.is_empty() {
        return Ok(0);
    }
    let count = ops.len() as u64;
    host.apply(&ops)?;
    Ok(count)
}

/// One borsh-typed value under one key.
pub struct TypedCell<'h, H, T> {
    host: &'h H,
    key: String,
    _value: PhantomData<fn() -> T>,
}

impl<'h, H: LocalStoreHost, T: BorshSerialize + BorshDeserialize> TypedCell<'h, H, T> {
    /// Cell over the given key.
    pub fn new(host: &'h H, key: impl Into<String>) -> Self {
        Self {
            host,
            key: key.into(),
            _value: PhantomData,
        }
    }

    /// Decoded value, `Ok(None)` when absent.
    pub fn get(&self) -> Result<Option<T>, Fault> {
        self.host
            .get(&self.key)?
            .map(|bytes| decode(&self.key, &bytes))
            .transpose()
    }

    /// Encode and store `value`.
    pub fn set(&self, value: &T) -> Result<(), Fault> {
        self.host.set(&self.key, &encode(value)?)
    }

    /// Delete the value; a no-op if absent.
    pub fn clear(&self) -> Result<(), Fault> {
        self.host.delete(&self.key)
    }
}

/// Borsh-typed values keyed under a prefix: a keyed collection for
/// sets of things such as watches or orders.
pub struct TypedMap<'h, H, T> {
    host: &'h H,
    prefix: String,
    _value: PhantomData<fn() -> T>,
}

impl<'h, H: LocalStoreHost, T: BorshSerialize + BorshDeserialize> TypedMap<'h, H, T> {
    /// Collection under the given key prefix.
    pub fn new(host: &'h H, prefix: impl Into<String>) -> Self {
        Self {
            host,
            prefix: prefix.into(),
            _value: PhantomData,
        }
    }

    fn full_key(&self, key: &str) -> String {
        format!("{}{key}", self.prefix)
    }

    /// Encode and store `value` under `key`.
    pub fn insert(&self, key: &str, value: &T) -> Result<(), Fault> {
        self.host.set(&self.full_key(key), &encode(value)?)
    }

    /// Decoded value at `key`, `Ok(None)` when absent.
    pub fn get(&self, key: &str) -> Result<Option<T>, Fault> {
        let full = self.full_key(key);
        self.host
            .get(&full)?
            .map(|bytes| decode(&full, &bytes))
            .transpose()
    }

    /// Delete `key`; a no-op if absent.
    pub fn remove(&self, key: &str) -> Result<(), Fault> {
        self.host.delete(&self.full_key(key))
    }

    /// Keys in the collection, prefix stripped.
    pub fn keys(&self) -> Result<Vec<String>, Fault> {
        Ok(self
            .host
            .list_keys(&self.prefix)?
            .into_iter()
            .map(|key| match key.strip_prefix(&self.prefix) {
                Some(stripped) => stripped.to_owned(),
                None => key,
            })
            .collect())
    }

    /// Delete every entry in one [`LocalStoreHost::apply`] call;
    /// returns the number deleted. Fails whole past the 1024-entry cap.
    pub fn clear(&self) -> Result<u64, Fault> {
        clear_prefix(self.host, &self.prefix)
    }
}

/// A `u64` under one key. Read-modify-write, so safe under the
/// runtime's single-actor serialized dispatch; there is no cross-call
/// lock.
pub struct Counter<'h, H> {
    host: &'h H,
    key: String,
}

impl<'h, H: LocalStoreHost> Counter<'h, H> {
    /// Counter over the given key.
    pub fn new(host: &'h H, key: impl Into<String>) -> Self {
        Self {
            host,
            key: key.into(),
        }
    }

    /// Current value; 0 when absent.
    pub fn get(&self) -> Result<u64, Fault> {
        self.host
            .get(&self.key)?
            .map_or(Ok(0), |bytes| decode(&self.key, &bytes))
    }

    /// Add `delta` (saturating) and store; returns the new value.
    pub fn add(&self, delta: u64) -> Result<u64, Fault> {
        let next = self.get()?.saturating_add(delta);
        self.set(next)?;
        Ok(next)
    }

    /// Store `value`.
    pub fn set(&self, value: u64) -> Result<(), Fault> {
        self.host.set(&self.key, &encode(&value)?)
    }
}
