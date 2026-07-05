//! Host traits - the seam between strategy logic and the wit-bindgen
//! shims a module generates per-cdylib.
//!
//! Each trait mirrors one nexum / shepherd host interface
//! ([`ChainHost`] for `nexum:host/chain`, [`LocalStoreHost`] for
//! `nexum:host/local-store`, [`CowApiHost`] for `shepherd:cow/cow-api`,
//! [`LoggingHost`] for `nexum:host/logging`). A module that wants
//! host-free unit tests writes its strategy logic against the
//! [`Host`] supertrait and lets `shepherd-sdk-test` slot in the
//! in-memory mocks.
//!
//! ## Why a separate `HostError`
//!
//! `wit_bindgen::generate!` emits a `HostError` struct into each
//! module's own crate, so its identity is per-module. The SDK
//! exposes [`HostError`] (this module) with the same field shape  -
//! modules wire a one-liner `From` impl between the two so the
//! traits stay world-neutral and the mocks compile without a wasm
//! toolchain. See `shepherd-sdk-test`'s README for the adapter
//! pattern.

use strum::IntoStaticStr;

/// Severity for log messages routed through [`LoggingHost::log`].
/// Mirrors `nexum:host/logging.level`.
///
/// Marked `#[non_exhaustive]` so the WIT can grow a new severity tier
/// (e.g. `Critical`) without breaking downstream code that matches
/// against the enum. Module adapters should provide a wildcard arm
/// when converting SDK -> wit-bindgen `Level` so the new variant
/// degrades gracefully to a safe default. See ADR-0009.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
#[non_exhaustive]
pub enum LogLevel {
    /// Verbose tracing for development.
    Trace,
    /// Detail useful to operators when investigating.
    Debug,
    /// Steady-state events.
    Info,
    /// Recoverable errors - operator notice but no immediate action.
    Warn,
    /// Unrecoverable errors - operator should investigate.
    Error,
}

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
    /// `"cow-api"`, `"logging"`).
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

/// `shepherd:cow/cow-api` - orderbook submission path.
pub trait CowApiHost {
    /// Submit an `OrderCreation` JSON body. The host returns the
    /// canonical order UID on success.
    fn submit_order(&self, chain_id: u64, body: &[u8]) -> Result<String, HostError>;

    /// REST-style request against the CoW Protocol orderbook for the
    /// given chain. The host routes to the correct base URL
    /// (`https://api.cow.fi/<chain>/api/v1/...`). Returns the raw
    /// response body. Strategies that need a typed surface should
    /// wrap this in an SDK helper (see [`crate::cow::resolve_app_data`]).
    ///
    /// `method` is `"GET" | "POST" | "PUT" | "DELETE"`.
    /// `path` is the absolute orderbook path beginning with `/api/v1`.
    /// `body` is an optional JSON request body (only used for POST/PUT).
    ///
    /// Errors carry `code = 404` (and `kind = Unavailable`) on a
    /// missing-resource response, so callers can distinguish
    /// "orderbook does not know this resource" from a genuine upstream
    /// failure by matching on `err.code` rather than introducing a new
    /// `HostErrorKind` variant (which would require a WIT ABI bump).
    fn cow_api_request(
        &self,
        chain_id: u64,
        method: &str,
        path: &str,
        body: Option<&str>,
    ) -> Result<String, HostError>;
}

/// Maps the guest tracing facade's [`Level`](nexum_sdk::tracing::Level)
/// onto [`LogLevel`]. One-to-one: both mirror the host logging enum's
/// five tiers.
impl From<nexum_sdk::tracing::Level> for LogLevel {
    fn from(level: nexum_sdk::tracing::Level) -> Self {
        use nexum_sdk::tracing::Level;
        match level {
            Level::Trace => Self::Trace,
            Level::Debug => Self::Debug,
            Level::Info => Self::Info,
            Level::Warn => Self::Warn,
            Level::Error => Self::Error,
        }
    }
}

/// `nexum:host/logging` - structured runtime logs.
pub trait LoggingHost {
    /// Emit a log line at the given level.
    fn log(&self, level: LogLevel, message: &str);
}

/// Supertrait that bundles the non-cow host interfaces a typical
/// strategy module exercises. Modules that want full host-free
/// integration tests take `&impl Host` (or a generic `<H: Host>`) in
/// their strategy function; `shepherd-sdk-test::MockHost` is the
/// in-memory implementation. Strategies that reach the CoW Protocol
/// orderbook bound [`crate::cow::CowHost`] instead.
///
/// A blanket impl is provided for any type that implements all three
/// component traits, so callers do not have to add a redundant
/// `impl Host for MyHost {}`.
///
/// # Example
///
/// Strategy functions are generic over [`Host`]. Production code plugs
/// the per-module `WitBindgenHost` adapter (see `modules/examples/`);
/// unit tests plug `shepherd_sdk_test::MockHost`.
///
/// ```
/// use shepherd_sdk::host::{
///     ChainHost, Host, HostError, LocalStoreHost, LogLevel, LoggingHost,
/// };
///
/// /// Pure strategy logic - no wit-bindgen calls in here.
/// fn record_block<H: Host>(host: &H, chain_id: u64, key: &str) -> Result<(), HostError> {
///     host.log(LogLevel::Info, "recording block");
///     host.set(key, b"")?;
///     let _block_number = host.request(chain_id, "eth_blockNumber", "[]")?;
///     Ok(())
/// }
///
/// // Minimal hand-rolled host so the doctest is self-contained.
/// // Real modules wire `shepherd_sdk_test::MockHost` here.
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
/// #     fn log(&self, _: LogLevel, _: &str) {}
/// # }
/// record_block(&StubHost, 1, "block:42").unwrap();
/// ```
pub trait Host: ChainHost + LocalStoreHost + LoggingHost {}
impl<T: ChainHost + LocalStoreHost + LoggingHost> Host for T {}

#[cfg(test)]
mod tests {
    use super::LogLevel;

    #[test]
    fn tracing_level_maps_one_to_one() {
        use nexum_sdk::tracing::Level;
        assert_eq!(LogLevel::from(Level::Trace), LogLevel::Trace);
        assert_eq!(LogLevel::from(Level::Debug), LogLevel::Debug);
        assert_eq!(LogLevel::from(Level::Info), LogLevel::Info);
        assert_eq!(LogLevel::from(Level::Warn), LogLevel::Warn);
        assert_eq!(LogLevel::from(Level::Error), LogLevel::Error);
    }
}
