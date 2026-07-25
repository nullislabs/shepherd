//! Host traits, the seam between module logic and the wit-bindgen
//! shims a module generates per-cdylib. Each trait mirrors one nexum
//! host interface ([`ChainHost`], [`IdentityHost`], [`LocalStoreHost`],
//! [`RemoteStoreHost`], [`MessagingHost`], [`LoggingHost`]); [`Host`]
//! bundles all six.
//!
//! Module logic written against these traits runs host-free against
//! the `nexum-sdk-test` mocks. The traits are world-neutral over this
//! module's [`Fault`], mirroring the per-module `Fault` that
//! `wit_bindgen::generate!` emits, so modules wire a one-line converter
//! between the two.

use alloy_primitives::{Address, B256, Bytes, Signature};
use strum::IntoStaticStr;
use tracing_core::Level;

/// Shared cross-domain failure vocabulary, mirrored from
/// `nexum:host/types.fault`. Typed per-interface errors embed it as a
/// case so a caller recovers the structured cause. `#[non_exhaustive]`:
/// the WIT can grow a case.
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
    #[error("rate limited{}", .0.retry_after_ms.map_or_else(String::new, |ms| format!(", retry after {ms} ms")))]
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

/// Sealing markers for [`Host`] and [`HostFault`]: implement alongside
/// the trait.
#[doc(hidden)]
pub mod sealed {
    pub trait SealedHost {}
    pub trait SealedHostFault {}
}

impl<T> sealed::SealedHost for T where
    T: ChainHost + IdentityHost + LocalStoreHost + RemoteStoreHost + MessagingHost + LoggingHost
{
}

impl sealed::SealedHostFault for Fault {}
impl sealed::SealedHostFault for ChainError {}

/// Recovers the shared [`Fault`] and a stable snake_case label from a
/// richer per-interface error. Sealed.
pub trait HostFault: sealed::SealedHostFault {
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
/// `nexum:host/chain.rpc-error`. `data` holds the host-decoded
/// `error.data` revert bytes, ready for a revert decoder.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("rpc error {code}: {message}")]
pub struct RpcError {
    /// JSON-RPC error code from the node.
    pub code: i32,
    /// Human-readable detail.
    pub message: String,
    /// Decoded `error.data` bytes, when the node returned a hex payload.
    pub data: Option<Bytes>,
}

/// Failure of a `nexum:host/chain` call, mirrored from
/// `nexum:host/chain.chain-error`: a shared host [`Fault`] or a
/// structured JSON-RPC [`RpcError`]. [`HostFault`] recovers the
/// embedded [`Fault`], present only on the `Fault` case.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[non_exhaustive]
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

/// Fold a [`ChainError`] into the shared [`Fault`]: the `Fault` case
/// passes through; an [`RpcError`] becomes [`Fault::Internal`] carrying
/// the code, message, and any revert bytes as a `0x` hex suffix.
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
    /// Execute a JSON-RPC request against the given chain.
    fn request(&self, chain_id: u64, method: &str, params: &str) -> Result<String, ChainError>;
}

/// `nexum:host/local-store` - per-module key-value persistence.
pub trait LocalStoreHost {
    /// Fetch a value. `Ok(None)` when the key is absent.
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, Fault>;
    /// Insert or overwrite.
    fn set(&self, key: &str, value: &[u8]) -> Result<(), Fault>;
    /// Delete. No-op if the key is absent.
    fn delete(&self, key: &str) -> Result<(), Fault>;
    /// Enumerate keys whose raw form starts with `prefix`.
    fn list_keys(&self, prefix: &str) -> Result<Vec<String>, Fault>;
    /// Whether `key` exists.
    fn contains(&self, key: &str) -> Result<bool, Fault> {
        Ok(self.get(key)?.is_some())
    }
    /// Value byte length, `Ok(None)` when absent.
    fn len(&self, key: &str) -> Result<Option<u64>, Fault> {
        Ok(self.get(key)?.map(|v| v.len() as u64))
    }
    /// Number of keys starting with `prefix`.
    fn count(&self, prefix: &str) -> Result<u64, Fault> {
        Ok(self.list_keys(prefix)?.len() as u64)
    }
}

/// `nexum:host/logging` - structured runtime logs.
pub trait LoggingHost {
    /// Emit a log line at the given [`Level`].
    fn log(&self, level: Level, message: &str);
}

/// `nexum:host/identity` - host-held accounts and signing.
pub trait IdentityHost {
    /// Accounts the host is willing to sign for. Empty means no
    /// signing capability.
    fn accounts(&self) -> Result<Vec<Address>, Fault>;
    /// Sign `message` with `personal_sign` semantics (the host
    /// prepends the `"\x19Ethereum Signed Message:\n"` prefix).
    fn sign(&self, account: Address, message: &[u8]) -> Result<Signature, Fault>;
    /// Sign a JSON-encoded EIP-712 payload.
    fn sign_typed_data(&self, account: Address, typed_data: &str) -> Result<Signature, Fault>;
}

/// One delivered message, mirrored from `nexum:host/types.message` so
/// the [`MessagingHost`] seam stays mockable without naming bindgen
/// types.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Message {
    /// Content topic the message arrived on.
    pub content_topic: String,
    /// Opaque payload bytes.
    pub payload: Vec<u8>,
    /// Delivery timestamp, ms since the Unix epoch, UTC.
    pub timestamp: u64,
    /// Optional sender identity (protocol-dependent).
    pub sender: Option<Vec<u8>>,
}

/// `nexum:host/messaging` - publish to and query content topics,
/// confined to the component's `messaging_topics` grant; an off-scope
/// topic fails as [`Fault::Denied`].
pub trait MessagingHost {
    /// Publish a payload to a content topic
    /// (`/<app>/<version>/<topic>/<encoding>`).
    fn publish(&self, content_topic: &str, payload: &[u8]) -> Result<(), Fault>;
    /// Query historical messages on a topic, window bounded by the
    /// optional `start_time` / `end_time` (ms since the Unix epoch,
    /// UTC) and `limit`.
    fn query(
        &self,
        content_topic: &str,
        start_time: Option<u64>,
        end_time: Option<u64>,
        limit: Option<u32>,
    ) -> Result<Vec<Message>, Fault>;
}

/// `nexum:host/remote-store` - content-addressed blobs and mutable
/// feeds on the decentralized store.
pub trait RemoteStoreHost {
    /// Upload raw data; returns its 32-byte content reference.
    fn upload(&self, data: &[u8]) -> Result<B256, Fault>;
    /// Download the data behind a content reference.
    fn download(&self, reference: B256) -> Result<Vec<u8>, Fault>;
    /// Latest value of the `(owner, topic)` mutable feed, when set.
    fn read_feed(&self, owner: Address, topic: B256) -> Result<Option<Vec<u8>>, Fault>;
    /// Update the host-owned feed at `topic` (the host signs with its
    /// configured identity); returns the new chunk's reference.
    fn write_feed(&self, topic: B256, data: &[u8]) -> Result<B256, Fault>;
}

/// Lift a host-returned account into an [`Address`]; any length but 20
/// folds to [`Fault::Internal`].
pub fn account_from_wire(raw: &[u8]) -> Result<Address, Fault> {
    Address::try_from(raw).map_err(|_| {
        Fault::Internal(format!(
            "identity returned a {}-byte account, expected 20",
            raw.len()
        ))
    })
}

/// Lift a host-returned 65-byte `r || s || v` signature into a
/// [`Signature`]; a malformed buffer folds to [`Fault::Internal`].
pub fn signature_from_wire(raw: &[u8]) -> Result<Signature, Fault> {
    Signature::from_raw(raw)
        .map_err(|e| Fault::Internal(format!("identity returned a malformed signature: {e}")))
}

/// Lift a host-returned content reference into a [`B256`]; any length
/// but 32 folds to [`Fault::Internal`].
pub fn reference_from_wire(raw: &[u8]) -> Result<B256, Fault> {
    B256::try_from(raw).map_err(|_| {
        Fault::Internal(format!(
            "remote-store returned a {}-byte reference, expected 32",
            raw.len()
        ))
    })
}

/// Supertrait bundling all six core host interfaces. Module functions
/// take `<H: Host>` (or bound exactly the interfaces they exercise) and
/// run against `nexum_sdk_test::MockHost` in tests. Blanket-implemented
/// for any type carrying all six; sealed, so that impl is the only one.
pub trait Host:
    sealed::SealedHost
    + ChainHost
    + IdentityHost
    + LocalStoreHost
    + RemoteStoreHost
    + MessagingHost
    + LoggingHost
{
}
impl<T> Host for T where
    T: ChainHost + IdentityHost + LocalStoreHost + RemoteStoreHost + MessagingHost + LoggingHost
{
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{Address, B256, U256};

    use super::{
        ChainError, Fault, HostFault, RateLimit, RpcError, account_from_wire, reference_from_wire,
        signature_from_wire,
    };

    #[test]
    fn local_store_metadata_defaults_derive_from_required_methods() {
        use super::LocalStoreHost;

        /// Two fixed rows; only the four required methods are written.
        struct TwoRows;
        impl LocalStoreHost for TwoRows {
            fn get(&self, key: &str) -> Result<Option<Vec<u8>>, Fault> {
                Ok(match key {
                    "a" => Some(b"abc".to_vec()),
                    "b" => Some(Vec::new()),
                    _ => None,
                })
            }
            fn set(&self, _: &str, _: &[u8]) -> Result<(), Fault> {
                Ok(())
            }
            fn delete(&self, _: &str) -> Result<(), Fault> {
                Ok(())
            }
            fn list_keys(&self, prefix: &str) -> Result<Vec<String>, Fault> {
                Ok(["a", "b"]
                    .iter()
                    .filter(|k| k.starts_with(prefix))
                    .map(|k| (*k).to_owned())
                    .collect())
            }
        }

        assert!(TwoRows.contains("a").unwrap());
        assert!(!TwoRows.contains("missing").unwrap());
        assert_eq!(TwoRows.len("a").unwrap(), Some(3));
        assert_eq!(TwoRows.len("b").unwrap(), Some(0));
        assert_eq!(TwoRows.len("missing").unwrap(), None);
        assert_eq!(TwoRows.count("").unwrap(), 2);
        assert_eq!(TwoRows.count("a").unwrap(), 1);
        assert_eq!(TwoRows.count("z").unwrap(), 0);
    }

    #[test]
    fn wire_lifts_accept_exact_lengths() {
        let account = account_from_wire(&[0x11; 20]).unwrap();
        assert_eq!(account, Address::from([0x11; 20]));

        let reference = reference_from_wire(&[0x22; 32]).unwrap();
        assert_eq!(reference, B256::from([0x22; 32]));

        let raw = alloy_primitives::Signature::new(U256::from(1), U256::from(2), true).as_bytes();
        let signature = signature_from_wire(&raw).unwrap();
        assert_eq!(signature.r(), U256::from(1));
        assert_eq!(signature.s(), U256::from(2));
        assert!(signature.v());
    }

    #[test]
    fn wire_lifts_fold_malformed_buffers_to_internal() {
        for fault in [
            account_from_wire(&[0u8; 19]).unwrap_err(),
            signature_from_wire(&[0u8; 64]).unwrap_err(),
            reference_from_wire(&[0u8; 31]).unwrap_err(),
        ] {
            assert!(matches!(fault, Fault::Internal(_)), "got {fault:?}");
        }
    }

    #[test]
    fn fault_labels_match_the_single_source_vocabulary() {
        use nexum_world::FaultLabel as Label;
        let cases: [(Fault, &str); 7] = [
            (Fault::Unsupported(String::new()), Label::Unsupported.into()),
            (Fault::Unavailable(String::new()), Label::Unavailable.into()),
            (Fault::Denied(String::new()), Label::Denied.into()),
            (
                Fault::RateLimited(RateLimit::default()),
                Label::RateLimited.into(),
            ),
            (Fault::Timeout, Label::Timeout.into()),
            (
                Fault::InvalidInput(String::new()),
                Label::InvalidInput.into(),
            ),
            (Fault::Internal(String::new()), Label::Internal.into()),
        ];
        for (fault, label) in cases {
            assert_eq!(fault.label(), label);
            assert_eq!(fault.fault(), Some(&fault));
        }
    }

    #[test]
    fn rate_limit_display_carries_the_retry_hint() {
        let hinted = Fault::RateLimited(RateLimit {
            retry_after_ms: Some(250),
        });
        assert_eq!(hinted.to_string(), "rate limited, retry after 250 ms");
        assert_eq!(
            Fault::RateLimited(RateLimit::default()).to_string(),
            "rate limited"
        );
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
            data: Some(vec![0xde, 0xad].into()),
        });
        assert_eq!(rpc.fault(), None);
        assert_eq!(rpc.label(), "rpc");
    }

    #[test]
    fn chain_error_rpc_folds_to_internal_fault_with_hex_data() {
        let fault = Fault::from(ChainError::Rpc(RpcError {
            code: -32000,
            message: "execution reverted".into(),
            data: Some(vec![0x08, 0xc3, 0x79, 0xa0].into()),
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
