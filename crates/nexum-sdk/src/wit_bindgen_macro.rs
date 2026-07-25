//! Declarative macro generating the `WitBindgenHost` adapter a module
//! ships in `lib.rs`: `struct WitBindgenHost;` plus the core trait
//! impls and the fault, chain-error, and level conversions.
//!
//! Capability-selected: `caps: [...]` emits only the pieces backed by
//! the listed capabilities (how `#[nexum_sdk::module]` invokes it); the
//! zero-argument form emits the full six-interface set. Either way the
//! wit-bindgen output for the world must already be in scope, so
//! selecting a capability the world does not import is a compile error.
//! A domain SDK layers its own interfaces on the same `WitBindgenHost`.
//!
//! ```ignore
//! wit_bindgen::generate!({ /* ... */ });
//! nexum_sdk::bind_host_via_wit_bindgen!();
//! // or capability-selected:
//! nexum_sdk::bind_host_via_wit_bindgen!(caps: [chain, logging]);
//! // Call `install_tracing()` once at the top of `Guest::init`.
//! ```

/// Generate `WitBindgenHost`, the `*Host` trait impls, and the error /
/// level `From` impls for the selected capabilities. See module docs.
///
/// The generated names `WitBindgenHost`, `convert_chain_err`,
/// `HostLogSink`, and `install_tracing` are visible in the caller's
/// scope (`macro_rules!` is not hygienic for items).
#[macro_export]
macro_rules! bind_host_via_wit_bindgen {
    // Blanket-world form: every core interface is in scope, emit the
    // full adapter.
    () => {
        $crate::bind_host_via_wit_bindgen!(
            caps: [chain, identity, local_store, remote_store, messaging, logging]
        );
    };
    // Capability-selected form: the base pieces (which need only the
    // always-present `nexum:host/types`) plus one block per listed
    // capability.
    (caps: [$($cap:ident),* $(,)?]) => {
        /// Wraps the module's per-cdylib wit-bindgen imports so a
        /// module can hold a `&impl Host`.
        struct WitBindgenHost;

        /// Lift the wit-bindgen `types.fault` into the SDK's `Fault`.
        impl ::core::convert::From<nexum::host::types::Fault> for $crate::host::Fault {
            fn from(f: nexum::host::types::Fault) -> Self {
                match f {
                    nexum::host::types::Fault::Unsupported(s) => Self::Unsupported(s),
                    nexum::host::types::Fault::Unavailable(s) => Self::Unavailable(s),
                    nexum::host::types::Fault::Denied(s) => Self::Denied(s),
                    nexum::host::types::Fault::RateLimited(rl) => {
                        Self::RateLimited($crate::host::RateLimit {
                            retry_after_ms: rl.retry_after_ms,
                        })
                    }
                    nexum::host::types::Fault::Timeout => Self::Timeout,
                    nexum::host::types::Fault::InvalidInput(s) => Self::InvalidInput(s),
                    nexum::host::types::Fault::Internal(s) => Self::Internal(s),
                }
            }
        }

        /// Lower the SDK `Fault` back into the wit-bindgen `Fault` for
        /// the export signature. A future `#[non_exhaustive]` SDK case
        /// falls back to `internal` carrying the `Display` detail.
        impl ::core::convert::From<$crate::host::Fault> for nexum::host::types::Fault {
            fn from(f: $crate::host::Fault) -> Self {
                match f {
                    $crate::host::Fault::Unsupported(s) => Self::Unsupported(s),
                    $crate::host::Fault::Unavailable(s) => Self::Unavailable(s),
                    $crate::host::Fault::Denied(s) => Self::Denied(s),
                    $crate::host::Fault::RateLimited(rl) => {
                        Self::RateLimited(nexum::host::types::RateLimit {
                            retry_after_ms: rl.retry_after_ms,
                        })
                    }
                    $crate::host::Fault::Timeout => Self::Timeout,
                    $crate::host::Fault::InvalidInput(s) => Self::InvalidInput(s),
                    $crate::host::Fault::Internal(s) => Self::Internal(s),
                    // `$crate::host::Fault` is `#[non_exhaustive]`; a
                    // future SDK case lands here as `internal`.
                    other => Self::Internal(::std::string::ToString::to_string(&other)),
                }
            }
        }

        /// Rebuild the native alloy log from the wit-bindgen `chain-log`
        /// record; assembly lives in `nexum_sdk::events`.
        impl ::core::convert::From<nexum::host::types::ChainLog> for $crate::events::Log {
            fn from(log: nexum::host::types::ChainLog) -> Self {
                $crate::events::ChainLogParts {
                    address: &log.address,
                    topics: &log.topics,
                    data: &log.data,
                    block_hash: log.block_hash.as_deref(),
                    block_number: log.block_number,
                    block_timestamp: log.block_timestamp,
                    transaction_hash: log.transaction_hash.as_deref(),
                    transaction_index: log.transaction_index,
                    log_index: log.log_index,
                    removed: log.removed,
                }
                .into()
            }
        }

        /// Rebuild the SDK `Message` from the wit-bindgen `message`
        /// record.
        impl ::core::convert::From<nexum::host::types::Message> for $crate::host::Message {
            fn from(message: nexum::host::types::Message) -> Self {
                Self {
                    content_topic: message.content_topic,
                    payload: message.payload,
                    timestamp: message.timestamp,
                    sender: message.sender,
                }
            }
        }

        $($crate::__bind_host_cap_via_wit_bindgen!($cap);)*
    };
}

/// One capability's slice of the `WitBindgenHost` adapter. Invoked by
/// [`bind_host_via_wit_bindgen!`]; not part of the public surface.
#[doc(hidden)]
#[macro_export]
macro_rules! __bind_host_cap_via_wit_bindgen {
    (chain) => {
        impl $crate::host::ChainHost for WitBindgenHost {
            fn request(
                &self,
                chain_id: u64,
                method: &str,
                params: &str,
            ) -> ::core::result::Result<::std::string::String, $crate::host::ChainError> {
                nexum::host::chain::request(chain_id, method, params).map_err(convert_chain_err)
            }
        }

        /// Lift the wit-bindgen `chain.chain-error` into the SDK's
        /// host-neutral `ChainError`.
        fn convert_chain_err(e: nexum::host::chain::ChainError) -> $crate::host::ChainError {
            match e {
                nexum::host::chain::ChainError::Fault(f) => {
                    $crate::host::ChainError::Fault(::core::convert::Into::into(f))
                }
                nexum::host::chain::ChainError::Rpc(r) => {
                    $crate::host::ChainError::Rpc($crate::host::RpcError {
                        code: r.code,
                        message: r.message,
                        data: r.data.map(::core::convert::Into::into),
                    })
                }
            }
        }
    };
    (identity) => {
        impl $crate::host::IdentityHost for WitBindgenHost {
            fn accounts(
                &self,
            ) -> ::core::result::Result<
                ::std::vec::Vec<$crate::prelude::Address>,
                $crate::host::Fault,
            > {
                nexum::host::identity::accounts()
                    .map_err($crate::host::Fault::from)?
                    .iter()
                    .map(|account| $crate::host::account_from_wire(account))
                    .collect()
            }
            fn sign(
                &self,
                account: $crate::prelude::Address,
                message: &[u8],
            ) -> ::core::result::Result<$crate::prelude::Signature, $crate::host::Fault> {
                let raw = nexum::host::identity::sign(account.as_slice(), message)
                    .map_err($crate::host::Fault::from)?;
                $crate::host::signature_from_wire(&raw)
            }
            fn sign_typed_data(
                &self,
                account: $crate::prelude::Address,
                typed_data: &str,
            ) -> ::core::result::Result<$crate::prelude::Signature, $crate::host::Fault> {
                let raw = nexum::host::identity::sign_typed_data(account.as_slice(), typed_data)
                    .map_err($crate::host::Fault::from)?;
                $crate::host::signature_from_wire(&raw)
            }
        }
    };
    (local_store) => {
        impl $crate::host::LocalStoreHost for WitBindgenHost {
            fn get(
                &self,
                key: &str,
            ) -> ::core::result::Result<
                ::core::option::Option<::std::vec::Vec<u8>>,
                $crate::host::Fault,
            > {
                nexum::host::local_store::get(key).map_err($crate::host::Fault::from)
            }
            fn set(
                &self,
                key: &str,
                value: &[u8],
            ) -> ::core::result::Result<(), $crate::host::Fault> {
                nexum::host::local_store::set(key, value).map_err($crate::host::Fault::from)
            }
            fn delete(&self, key: &str) -> ::core::result::Result<(), $crate::host::Fault> {
                nexum::host::local_store::delete(key).map_err($crate::host::Fault::from)
            }
            fn list_keys(
                &self,
                prefix: &str,
            ) -> ::core::result::Result<::std::vec::Vec<::std::string::String>, $crate::host::Fault>
            {
                nexum::host::local_store::list_keys(prefix).map_err($crate::host::Fault::from)
            }
            fn contains(&self, key: &str) -> ::core::result::Result<bool, $crate::host::Fault> {
                nexum::host::local_store::contains(key).map_err($crate::host::Fault::from)
            }
            fn len(
                &self,
                key: &str,
            ) -> ::core::result::Result<::core::option::Option<u64>, $crate::host::Fault> {
                nexum::host::local_store::len(key).map_err($crate::host::Fault::from)
            }
            fn count(&self, prefix: &str) -> ::core::result::Result<u64, $crate::host::Fault> {
                nexum::host::local_store::count(prefix).map_err($crate::host::Fault::from)
            }
        }
    };
    (remote_store) => {
        impl $crate::host::RemoteStoreHost for WitBindgenHost {
            fn upload(
                &self,
                data: &[u8],
            ) -> ::core::result::Result<$crate::prelude::B256, $crate::host::Fault> {
                let raw =
                    nexum::host::remote_store::upload(data).map_err($crate::host::Fault::from)?;
                $crate::host::reference_from_wire(&raw)
            }
            fn download(
                &self,
                reference: $crate::prelude::B256,
            ) -> ::core::result::Result<::std::vec::Vec<u8>, $crate::host::Fault> {
                nexum::host::remote_store::download(reference.as_slice())
                    .map_err($crate::host::Fault::from)
            }
            fn read_feed(
                &self,
                owner: $crate::prelude::Address,
                topic: $crate::prelude::B256,
            ) -> ::core::result::Result<
                ::core::option::Option<::std::vec::Vec<u8>>,
                $crate::host::Fault,
            > {
                nexum::host::remote_store::read_feed(owner.as_slice(), topic.as_slice())
                    .map_err($crate::host::Fault::from)
            }
            fn write_feed(
                &self,
                topic: $crate::prelude::B256,
                data: &[u8],
            ) -> ::core::result::Result<$crate::prelude::B256, $crate::host::Fault> {
                let raw = nexum::host::remote_store::write_feed(topic.as_slice(), data)
                    .map_err($crate::host::Fault::from)?;
                $crate::host::reference_from_wire(&raw)
            }
        }
    };
    (messaging) => {
        impl $crate::host::MessagingHost for WitBindgenHost {
            fn publish(
                &self,
                content_topic: &str,
                payload: &[u8],
            ) -> ::core::result::Result<(), $crate::host::Fault> {
                nexum::host::messaging::publish(content_topic, payload)
                    .map_err($crate::host::Fault::from)
            }
            fn query(
                &self,
                content_topic: &str,
                start_time: ::core::option::Option<u64>,
                end_time: ::core::option::Option<u64>,
                limit: ::core::option::Option<u32>,
            ) -> ::core::result::Result<::std::vec::Vec<$crate::host::Message>, $crate::host::Fault>
            {
                let messages =
                    nexum::host::messaging::query(content_topic, start_time, end_time, limit)
                        .map_err($crate::host::Fault::from)?;
                ::core::result::Result::Ok(
                    messages
                        .into_iter()
                        .map(::core::convert::Into::into)
                        .collect(),
                )
            }
        }
    };
    (logging) => {
        impl $crate::host::LoggingHost for WitBindgenHost {
            fn log(&self, level: $crate::Level, message: &str) {
                nexum::host::logging::log(nexum::host::logging::Level::from(level), message);
            }
        }

        /// Translate a `tracing_core::Level` into the wit-bindgen
        /// `logging::Level` wire enum.
        impl ::core::convert::From<$crate::Level> for nexum::host::logging::Level {
            fn from(level: $crate::Level) -> Self {
                if level == $crate::Level::ERROR {
                    Self::Error
                } else if level == $crate::Level::WARN {
                    Self::Warn
                } else if level == $crate::Level::INFO {
                    Self::Info
                } else if level == $crate::Level::DEBUG {
                    Self::Debug
                } else {
                    Self::Trace
                }
            }
        }

        /// Routes guest `tracing` events to the bound host logging call.
        struct HostLogSink;

        impl $crate::tracing::LogSink for HostLogSink {
            fn log(&self, level: $crate::Level, message: &str) {
                <WitBindgenHost as $crate::host::LoggingHost>::log(&WitBindgenHost, level, message);
            }
        }

        /// Install the guest tracing facade and panic hook over the
        /// bound host logging call. Call once at the top of `Guest::init`.
        fn install_tracing() {
            $crate::tracing::init(HostLogSink);
        }
    };
}
