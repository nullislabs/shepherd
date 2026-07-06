//! Host traits - the seam between strategy logic and the wit-bindgen
//! shims a module generates per-cdylib.
//!
//! Each trait mirrors one nexum host interface ([`ChainHost`] for
//! `nexum:host/chain`, [`LocalStoreHost`] for `nexum:host/local-store`,
//! [`LoggingHost`] for `nexum:host/logging`). A module that wants
//! host-free unit tests writes its strategy logic against the
//! [`Host`] supertrait and lets `nexum-sdk-test` slot in the
//! in-memory mocks. Domain SDKs bound extra host interfaces on top
//! with their own traits over the same [`Fault`].
//!
//! ## Why a separate `Fault`
//!
//! `wit_bindgen::generate!` emits a `Fault` type into each module's
//! own crate, so its identity is per-module. The SDK exposes [`Fault`]
//! (this module) with the same case shape, so modules wire a one-liner
//! converter between the two and the traits stay world-neutral, letting
//! the mocks compile without a wasm toolchain. See `nexum-sdk-test`'s
//! crate docs for the adapter pattern.

use strum::IntoStaticStr;
use tracing_core::Level;

/// The cross-domain failure vocabulary richer host interfaces embed as
/// a case, mirrored from `nexum:host/types.fault`. Typed per-interface
/// errors wrap this shared payload-bearing set so a caller recovers the
/// structured cause without a stringly-typed ladder.
///
/// `IntoStaticStr` exposes each case as a snake_case `&'static str` for
/// structured-log and metric labels. Marked `#[non_exhaustive]` so the
/// WIT can grow a case without breaking downstream `match` sites.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
#[non_exhaustive]
pub enum Fault {
    /// Capability declared but not provisioned by the operator.
    #[error("unsupported: {0}")]
    Unsupported(String),
    /// Capability temporarily unavailable (RPC down, etc).
    #[error("unavailable: {0}")]
    Unavailable(String),
    /// Capability declined the request (auth, allowlist, …).
    #[error("denied: {0}")]
    Denied(String),
    /// Rate-limited by an upstream service; carries backoff guidance.
    #[error("rate limited")]
    RateLimited(RateLimit),
    /// Operation took too long.
    #[error("timeout")]
    Timeout,
    /// Caller-supplied input did not parse / validate.
    #[error("invalid input: {0}")]
    InvalidInput(String),
    /// Catch-all for host-side bugs.
    #[error("internal: {0}")]
    Internal(String),
}

/// Backoff guidance carried by [`Fault::RateLimited`], mirrored from
/// `nexum:host/types.rate-limit`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub struct RateLimit {
    /// Host's suggested wait before retrying, in milliseconds, when known.
    pub retry_after_ms: Option<u64>,
}

/// Recovers the shared [`Fault`] from a richer, per-interface error.
///
/// Typed interface errors that embed a fault case implement this so a
/// caller can dispatch on the structured cause and pull a stable
/// snake_case [`label`](HostFault::label) for logs and metrics without
/// matching the outer type.
pub trait HostFault {
    /// The embedded fault, when this value represents one.
    fn fault(&self) -> Option<&Fault>;
    /// Stable snake_case label for logs and metrics.
    fn label(&self) -> &'static str;
}

impl HostFault for Fault {
    fn fault(&self) -> Option<&Fault> {
        Some(self)
    }

    fn label(&self) -> &'static str {
        self.into()
    }
}

/// A structured JSON-RPC error response, mirrored from
/// `nexum:host/chain.rpc-error`. `code` is the node-reported numeric
/// (typically `-32000` for an `eth_call` revert). `data` is the decoded
/// `error.data` payload: the host hex-decodes the upstream JSON string
/// once, so a strategy receives the raw abi-encoded revert bytes and
/// can hand them straight to a revert decoder.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("rpc error {code}: {message}")]
pub struct RpcError {
    /// JSON-RPC error code from the node.
    pub code: i32,
    /// Human-readable detail.
    pub message: String,
    /// Decoded `error.data` bytes, when the node returned a hex payload.
    pub data: Option<Vec<u8>>,
}

/// Failure of a `nexum:host/chain` call, mirrored from
/// `nexum:host/chain.chain-error`: either a shared host [`Fault`]
/// (transport down, timed out, denied, ...) or a structured JSON-RPC
/// [`RpcError`] carrying the node code and any decoded revert payload.
///
/// [`HostFault`] recovers the embedded [`Fault`] (present only on the
/// `Fault` case) and a stable snake_case label for logs and metrics.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ChainError {
    /// A shared host fault.
    #[error(transparent)]
    Fault(#[from] Fault),
    /// A structured JSON-RPC error response.
    #[error(transparent)]
    Rpc(#[from] RpcError),
}

impl HostFault for ChainError {
    fn fault(&self) -> Option<&Fault> {
        match self {
            ChainError::Fault(f) => Some(f),
            ChainError::Rpc(_) => None,
        }
    }

    fn label(&self) -> &'static str {
        match self {
            ChainError::Fault(f) => f.label(),
            ChainError::Rpc(_) => "rpc",
        }
    }
}

/// Fold a [`ChainError`] into the shared [`Fault`] a module returns
/// from `init` / `on_event`. The `fault` case passes through; a
/// structured JSON-RPC [`RpcError`] has no shared-vocabulary case, so
/// it becomes an [`Fault::Internal`] carrying the node code, message,
/// and any decoded revert bytes as a `0x` hex suffix.
impl From<ChainError> for Fault {
    fn from(err: ChainError) -> Self {
        match err {
            ChainError::Fault(fault) => fault,
            ChainError::Rpc(rpc) => {
                let mut message = format!("rpc error {}: {}", rpc.code, rpc.message);
                if let Some(data) = rpc.data {
                    message.push_str(" (");
                    message.push_str(&alloy_primitives::hex::encode_prefixed(data));
                    message.push(')');
                }
                Fault::Internal(message)
            }
        }
    }
}

/// `nexum:host/chain` - raw JSON-RPC dispatch.
pub trait ChainHost {
    /// Execute a JSON-RPC request against the given chain. The host
    /// routes to its configured provider; the SDK does not care which
    /// transport (HTTP / WebSocket / mock) implements the call. A
    /// failure is a [`ChainError`]: a shared [`Fault`] or a structured
    /// JSON-RPC [`RpcError`] carrying any decoded revert bytes.
    fn request(&self, chain_id: u64, method: &str, params: &str) -> Result<String, ChainError>;
}

/// `nexum:host/local-store` - per-module key-value persistence.
///
/// The interface reports failures as a [`Fault`]: the interface is the
/// failure domain, so the case vocabulary alone carries the cause. A
/// strategy that aggregates store and chain calls into one [`Fault`]
/// return relies on the `From<ChainError>` fold for `?`.
pub trait LocalStoreHost {
    /// Fetch a value. `Ok(None)` when the key is absent.
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, Fault>;
    /// Insert or overwrite.
    fn set(&self, key: &str, value: &[u8]) -> Result<(), Fault>;
    /// Delete. No-op if the key is absent.
    fn delete(&self, key: &str) -> Result<(), Fault>;
    /// Enumerate keys whose raw form starts with `prefix`.
    fn list_keys(&self, prefix: &str) -> Result<Vec<String>, Fault>;
}

/// `nexum:host/logging` - structured runtime logs.
pub trait LoggingHost {
    /// Emit a log line at the given [`Level`]. The bind macro maps it
    /// onto the generated wire enum; the WIT edge is the only place a
    /// non-`Level` severity type appears.
    fn log(&self, level: Level, message: &str);
}

/// Supertrait that bundles the core host interfaces a typical
/// strategy module exercises. Modules that want full host-free
/// integration tests take `&impl Host` (or a generic `<H: Host>`) in
/// their strategy function; `nexum-sdk-test::MockHost` is the
/// in-memory implementation. Strategies that reach a domain extension
/// bound its host trait as well (the CoW SDK's `CowHost`, say).
///
/// A blanket impl is provided for any type that implements all three
/// component traits, so callers do not have to add a redundant
/// `impl Host for MyHost {}`.
///
/// # Example
///
/// Strategy functions are generic over [`Host`]. Production code plugs
/// the per-module `WitBindgenHost` adapter (see `modules/examples/`);
/// unit tests plug `nexum_sdk_test::MockHost`.
///
/// ```
/// use nexum_sdk::Level;
/// use nexum_sdk::host::{
///     ChainError, ChainHost, Fault, Host, LocalStoreHost, LoggingHost,
/// };
///
/// /// Pure strategy logic - no wit-bindgen calls in here.
/// fn record_block<H: Host>(host: &H, chain_id: u64, key: &str) -> Result<(), Fault> {
///     host.log(Level::INFO, "recording block");
///     host.set(key, b"")?;
///     let _block_number = host.request(chain_id, "eth_blockNumber", "[]")?;
///     Ok(())
/// }
///
/// // Minimal hand-rolled host so the doctest is self-contained.
/// // Real modules wire `nexum_sdk_test::MockHost` here.
/// # struct StubHost;
/// # impl ChainHost for StubHost {
/// #     fn request(&self, _: u64, _: &str, _: &str) -> Result<String, ChainError> {
/// #         Ok("\"0x0\"".into())
/// #     }
/// # }
/// # impl LocalStoreHost for StubHost {
/// #     fn get(&self, _: &str) -> Result<Option<Vec<u8>>, Fault> { Ok(None) }
/// #     fn set(&self, _: &str, _: &[u8]) -> Result<(), Fault> { Ok(()) }
/// #     fn delete(&self, _: &str) -> Result<(), Fault> { Ok(()) }
/// #     fn list_keys(&self, _: &str) -> Result<Vec<String>, Fault> { Ok(vec![]) }
/// # }
/// # impl LoggingHost for StubHost {
/// #     fn log(&self, _: Level, _: &str) {}
/// # }
/// record_block(&StubHost, 1, "block:42").unwrap();
/// ```
pub trait Host: ChainHost + LocalStoreHost + LoggingHost {}
impl<T: ChainHost + LocalStoreHost + LoggingHost> Host for T {}

#[cfg(test)]
mod tests {
    use super::{ChainError, Fault, HostFault, RateLimit, RpcError};

    #[test]
    fn fault_labels_are_stable_snake_case() {
        let cases: [(Fault, &str); 7] = [
            (Fault::Unsupported(String::new()), "unsupported"),
            (Fault::Unavailable(String::new()), "unavailable"),
            (Fault::Denied(String::new()), "denied"),
            (Fault::RateLimited(RateLimit::default()), "rate_limited"),
            (Fault::Timeout, "timeout"),
            (Fault::InvalidInput(String::new()), "invalid_input"),
            (Fault::Internal(String::new()), "internal"),
        ];
        for (fault, label) in cases {
            assert_eq!(fault.label(), label);
            assert_eq!(fault.fault(), Some(&fault));
        }
    }

    #[test]
    fn host_fault_is_object_safe() {
        let boxed: Box<dyn HostFault> = Box::new(Fault::Timeout);
        assert_eq!(boxed.label(), "timeout");
    }

    #[test]
    fn chain_error_recovers_embedded_fault() {
        let fault = ChainError::Fault(Fault::Timeout);
        assert_eq!(fault.fault(), Some(&Fault::Timeout));
        assert_eq!(fault.label(), "timeout");

        let rpc = ChainError::Rpc(RpcError {
            code: -32000,
            message: "execution reverted".into(),
            data: Some(vec![0xde, 0xad]),
        });
        assert_eq!(rpc.fault(), None);
        assert_eq!(rpc.label(), "rpc");
    }

    #[test]
    fn chain_error_rpc_folds_to_internal_fault_with_hex_data() {
        let fault = Fault::from(ChainError::Rpc(RpcError {
            code: -32000,
            message: "execution reverted".into(),
            data: Some(vec![0x08, 0xc3, 0x79, 0xa0]),
        }));
        let Fault::Internal(message) = fault else {
            panic!("rpc folds to internal, got {fault:?}");
        };
        assert!(message.contains("-32000"));
        assert!(message.contains("0x08c379a0"));
    }

    #[test]
    fn chain_error_fault_folds_through_unchanged() {
        let fault = Fault::from(ChainError::Fault(Fault::Unavailable("rpc down".into())));
        assert_eq!(fault, Fault::Unavailable("rpc down".into()));
    }
}
