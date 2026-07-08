//! Host traits - the seam between strategy logic and the wit-bindgen
//! shims a module generates per-cdylib.
//!
//! Each trait mirrors one nexum host interface ([`ChainHost`] for
//! `nexum:host/chain`, [`LocalStoreHost`] for `nexum:host/local-store`,
//! [`LoggingHost`] for `nexum:host/logging`). A module that wants
//! host-free unit tests writes its strategy logic against the
//! [`Host`] supertrait and lets `nexum-sdk-test` slot in the
//! in-memory mocks. Domain SDKs bound extra host interfaces on top
//! with their own traits over the same [`HostError`].
//!
//! ## Why a separate `HostError`
//!
//! `wit_bindgen::generate!` emits a `HostError` struct into each
//! module's own crate, so its identity is per-module. The SDK
//! exposes [`HostError`] (this module) with the same field shape  -
//! modules wire a one-liner `From` impl between the two so the
//! traits stay world-neutral and the mocks compile without a wasm
//! toolchain. See `nexum-sdk-test`'s crate docs for the adapter
//! pattern.

use strum::IntoStaticStr;
use tracing_core::Level;

/// Coarse categorisation of host failures, mirrored verbatim from
/// `nexum:host/types.host-error-kind` so a module's wit-bindgen
/// `HostErrorKind` can convert one-to-one.
///
/// `IntoStaticStr` exposes each variant as a snake_case `&'static
/// str` so module strategies and the engine can wire structured-log
/// and metric labels straight off the enum without an
/// `error_kind` ladder per call site.
///
/// Marked `#[non_exhaustive]` so the WIT can grow a new kind (e.g.
/// dedicated `WasmTrap`) without breaking downstream `match` sites.
/// Module adapters should provide a wildcard arm when converting
/// SDK -> wit-bindgen `HostErrorKind` (recommended fallback:
/// `_ => HostErrorKind::Internal`, the most conservative remapping
/// for an unrecognised SDK-side variant). See ADR-0009.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
#[non_exhaustive]
pub enum HostErrorKind {
    /// Capability declared but not provisioned by the operator.
    Unsupported,
    /// Capability temporarily unavailable (RPC down, etc).
    Unavailable,
    /// Capability declined the request (auth, allowlist, …).
    Denied,
    /// Rate-limited by an upstream service.
    RateLimited,
    /// Operation took too long.
    Timeout,
    /// Caller-supplied input did not parse / validate.
    InvalidInput,
    /// Catch-all for host-side bugs.
    Internal,
}

/// SDK-side counterpart to wit-bindgen's `HostError`. Same field shape
/// so a module bridges between the two with a trivial `From` impl on
/// each side.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("{domain}: {message} (code={code}, kind={kind:?})")]
pub struct HostError {
    /// Short subsystem identifier (`"chain"`, `"local-store"`,
    /// `"logging"`, or a domain extension's interface name).
    pub domain: String,
    /// See [`HostErrorKind`].
    pub kind: HostErrorKind,
    /// Domain-specific numeric (HTTP status, JSON-RPC code, etc).
    pub code: i32,
    /// Human-readable detail.
    pub message: String,
    /// Optional opaque payload (often JSON-encoded).
    pub data: Option<String>,
}

impl HostError {
    /// Convenience constructor for unsupported / not-yet-implemented
    /// host endpoints. Useful in tests and mock setups.
    pub fn unsupported(domain: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            domain: domain.into(),
            kind: HostErrorKind::Unsupported,
            code: 501,
            message: message.into(),
            data: None,
        }
    }
}

/// The cross-domain failure vocabulary richer host interfaces embed as
/// a case, mirrored from `nexum:host/types.fault`. Typed per-interface
/// errors wrap this shared payload-bearing set so a caller recovers the
/// structured cause without a stringly-typed ladder.
///
/// `#[non_exhaustive]` forces downstream `match` sites to carry a wildcard
/// arm, so the WIT can grow a case without breaking them.
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
    /// Rate-limited by an upstream service; may carry backoff guidance
    /// when the host knows the retry window.
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

/// `nexum:host/chain` - raw JSON-RPC dispatch.
pub trait ChainHost {
    /// Execute a JSON-RPC request against the given chain. The host
    /// routes to its configured provider; the SDK does not care which
    /// transport (HTTP / WebSocket / mock) implements the call.
    fn request(&self, chain_id: u64, method: &str, params: &str) -> Result<String, HostError>;
}

/// `nexum:host/local-store` - per-module key-value persistence.
pub trait LocalStoreHost {
    /// Fetch a value. `Ok(None)` when the key is absent.
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, HostError>;
    /// Insert or overwrite.
    fn set(&self, key: &str, value: &[u8]) -> Result<(), HostError>;
    /// Delete. No-op if the key is absent.
    fn delete(&self, key: &str) -> Result<(), HostError>;
    /// Enumerate keys whose raw form starts with `prefix`.
    fn list_keys(&self, prefix: &str) -> Result<Vec<String>, HostError>;
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
///     ChainHost, Host, HostError, LocalStoreHost, LoggingHost,
/// };
///
/// /// Pure strategy logic - no wit-bindgen calls in here.
/// fn record_block<H: Host>(host: &H, chain_id: u64, key: &str) -> Result<(), HostError> {
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
/// #     fn request(&self, _: u64, _: &str, _: &str) -> Result<String, HostError> {
/// #         Ok("\"0x0\"".into())
/// #     }
/// # }
/// # impl LocalStoreHost for StubHost {
/// #     fn get(&self, _: &str) -> Result<Option<Vec<u8>>, HostError> { Ok(None) }
/// #     fn set(&self, _: &str, _: &[u8]) -> Result<(), HostError> { Ok(()) }
/// #     fn delete(&self, _: &str) -> Result<(), HostError> { Ok(()) }
/// #     fn list_keys(&self, _: &str) -> Result<Vec<String>, HostError> { Ok(vec![]) }
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
    use super::{Fault, HostFault, RateLimit};

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
}
