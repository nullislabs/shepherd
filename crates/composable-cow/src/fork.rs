//! Structured ComposableCoW poll wire and its mapping onto [`Verdict`].
//!
//! The fork answers `getTradeableOrderWithSignature` with a value
//! instead of a revert, so a poll outcome is data. The auth and
//! interface reverts it still raises go to [`classify_revert`].

use alloy_primitives::{Bytes, Selector};
use alloy_sol_types::{SolValue, sol};
use cowprotocol::GPv2OrderData;
use nexum_sdk::host::ChainError;

use super::{NextPoll, ParkReason, Verdict};

sol! {
    /// Mirror of the deployed fork's poll surface, verified at
    /// `0xf9ba6F64c9b41Df1cEe76A50e2039D3847064232` on mainnet.
    #[derive(Debug)]
    enum GeneratorResultCode {
        /// A discrete order is ready to post.
        POST,
        /// Wait until `waitUntil`, a Unix timestamp.
        WAIT_TIMESTAMP,
        /// Wait until `waitUntil`, a block number.
        WAIT_BLOCK,
        /// Transient; retry next block.
        TRY_NEXT_BLOCK,
        /// Permanently invalid; stop polling.
        INVALID,
        /// Requires non-empty `offchainInput`.
        NEEDS_INPUT
    }

    /// Per orderUid, not per commitment: a filled TWAP tranche reports
    /// `FILLED` while later tranches are still owed.
    #[derive(Debug)]
    enum FillStatus {
        /// No fill observed.
        NONE,
        /// Some but not all of the order filled.
        PARTIALLY_FILLED,
        /// Fully filled.
        FILLED,
        /// Cancelled via `invalidateOrder`.
        INVALIDATED
    }

    #[derive(Debug)]
    enum Restriction {
        /// No registry-level restriction.
        NONE,
        /// The owner's swap guard rejected the order.
        SWAP_GUARD
    }

    #[derive(Debug)]
    struct GeneratorResult {
        /// Which arm the generator took.
        GeneratorResultCode code;
        /// The discrete order, meaningful only on `POST`.
        GPv2OrderData order;
        /// Advisory next poll, meaningful only on `POST`. `0` means
        /// `order.validTo + 1`; `u256::MAX` means no next order.
        uint256 nextPollTimestamp;
        /// Gate for the two `WAIT_` codes; a timestamp or a block
        /// number as the code says.
        uint256 waitUntil;
        /// Diagnostic selector, log only.
        bytes4 reasonCode;
    }

    #[derive(Debug)]
    struct PollResult {
        /// The handler's own outcome.
        GeneratorResult generator;
        /// Registry-observed fill state for this uid.
        FillStatus fill;
        /// Amount filled so far, in sell-token units for a sell order
        /// and buy-token units for a buy order. `u256::MAX` is the
        /// invalidated sentinel, not an amount.
        uint256 filledAmount;
        /// Registry-level restriction, if any.
        Restriction restriction;
    }
}

/// Why a structured poll produced no order to post.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Suppressed {
    /// The chain reports this uid filled, partially filled, or
    /// invalidated, so posting it again is at best a duplicate.
    Fill,
    /// The owner's swap guard rejected the order.
    SwapGuard,
}

/// A structured poll outcome.
#[derive(Debug)]
pub enum Mapped {
    /// Act on this verdict.
    Verdict(Verdict),
    /// Do not post; keep the commitment and schedule per `next_poll`.
    Suppress {
        /// Why the post was withheld.
        why: Suppressed,
        /// Schedule derived from the generator's hint.
        next_poll: NextPoll,
    },
}

/// Decode a `getTradeableOrderWithSignature` return.
///
/// A failure here is permanent, never transient: the fork's return is
/// fixed, so a shape mismatch means the wrong contract or the wrong ABI,
/// which no retry fixes.
///
/// `sol!` widens a Solidity enum to its full `u8` range and decodes an
/// out-of-range value to a sentinel instead of rejecting it, so that
/// case is rejected here. It cannot arise from the chain: the registry
/// assigns the handler's return into a typed `GeneratorResult`, so
/// Solidity panics with `Panic(0x21)` and reverts before returning.
///
/// # Errors
/// [`DecodeError`] when the bytes are not a `(PollResult, bytes)` tuple,
/// or carry a code outside the declared range.
pub fn decode_poll_return(data: &[u8]) -> Result<(PollResult, Bytes), DecodeError> {
    let (result, signature) =
        <(PollResult, Bytes)>::abi_decode_params(data).map_err(|source| DecodeError {
            detail: source.to_string(),
        })?;
    if matches!(result.generator.code, GeneratorResultCode::__Invalid) {
        return Err(DecodeError {
            detail: "generator code outside the declared range".to_owned(),
        });
    }
    Ok((result, signature))
}

/// A `PollResult` return that did not match the fork's ABI.
#[derive(Debug, thiserror::Error)]
#[error("poll return does not match the fork ABI: {detail}")]
pub struct DecodeError {
    /// Decoder detail.
    pub detail: String,
}

/// Map a decoded `PollResult` onto a [`Mapped`]. Pure: no I/O, no clock.
///
/// Teardown is deliberately narrow. Only `INVALID` drops a commitment;
/// a fill never does, because `FillStatus` is per orderUid while a
/// commitment spans many.
#[must_use]
pub fn map_verdict(result: &PollResult, signature: &Bytes) -> Mapped {
    let generator = &result.generator;
    let reason = generator.reasonCode;
    let next_poll = NextPoll::from_wire(generator.nextPollTimestamp);

    match generator.code {
        GeneratorResultCode::POST => {
            // Any observed fill suppresses this post: the uid is already
            // on the book, so re-posting it is a duplicate. The
            // commitment survives and reschedules.
            if matches!(
                result.fill,
                FillStatus::PARTIALLY_FILLED | FillStatus::FILLED | FillStatus::INVALIDATED
            ) {
                return Mapped::Suppress {
                    why: Suppressed::Fill,
                    next_poll,
                };
            }
            if matches!(result.restriction, Restriction::SWAP_GUARD) {
                return Mapped::Suppress {
                    why: Suppressed::SwapGuard,
                    next_poll,
                };
            }
            Mapped::Verdict(Verdict::Post {
                order: Box::new(generator.order.clone()),
                signature: signature.clone(),
                next_poll: Some(next_poll),
            })
        }
        GeneratorResultCode::WAIT_TIMESTAMP => Mapped::Verdict(Verdict::WaitTimestamp {
            wait_until: saturating_u64(generator.waitUntil),
            reason,
        }),
        GeneratorResultCode::WAIT_BLOCK => Mapped::Verdict(Verdict::WaitBlock {
            wait_until: saturating_u64(generator.waitUntil),
            reason,
        }),
        GeneratorResultCode::TRY_NEXT_BLOCK => Mapped::Verdict(Verdict::TryNextBlock { reason }),
        GeneratorResultCode::INVALID => Mapped::Verdict(Verdict::Invalid { reason }),
        GeneratorResultCode::NEEDS_INPUT => Mapped::Verdict(Verdict::Park {
            why: ParkReason::NeedsInput,
            reason,
        }),
        // Unreachable from the wire: `decode_poll_return` rejects it.
        GeneratorResultCode::__Invalid => Mapped::Verdict(Verdict::Invalid { reason }),
    }
}

sol! {
    /// The three errors a poll can revert with; the other five are
    /// registration or settlement paths.
    #[derive(Debug)]
    interface IComposableCowResidual {
        /// Not registered: removed, or never created.
        error SingleOrderNotAuthed();
        /// The merkle proof does not verify against the owner's root.
        error ProofNotAuthed();
        /// Handler fails the ERC-165 check.
        error InterfaceNotSupported();
    }
}

/// Classify a failed poll `eth_call`. Every reachable revert is
/// deterministic on-chain state, so all are terminal; a re-`create`
/// re-indexes through its own event. A payload-free failure is the
/// transport, not the contract, so it stays retryable.
#[must_use]
pub fn classify_revert(err: &ChainError) -> Verdict {
    let ChainError::Rpc(rpc) = err else {
        // `ChainError` is `#[non_exhaustive]`: transport faults and any
        // future case are payload-free, so they stay retryable.
        return Verdict::TryNextBlock {
            reason: Selector::ZERO,
        };
    };
    let Some(data) = rpc.data.as_deref() else {
        return Verdict::TryNextBlock {
            reason: Selector::ZERO,
        };
    };
    let Some(reason) = data.get(..4).map(Selector::from_slice) else {
        return Verdict::TryNextBlock {
            reason: Selector::ZERO,
        };
    };
    // Unrecognised still means the contract refused; a handler `Panic`
    // lands here.
    Verdict::Invalid { reason }
}

/// Selectors the classifier recognises, for logging and tests.
#[must_use]
pub fn is_residual_selector(selector: Selector) -> bool {
    use alloy_sol_types::SolError;
    [
        IComposableCowResidual::SingleOrderNotAuthed::SELECTOR,
        IComposableCowResidual::ProofNotAuthed::SELECTOR,
        IComposableCowResidual::InterfaceNotSupported::SELECTOR,
    ]
    .map(Selector::from)
    .contains(&selector)
}

/// Fold a [`Mapped`] into a [`Verdict`] the run loop handles.
///
/// A suppression keeps the commitment and schedules from the hint,
/// clamped by [`schedule_at`]. `Never` is the exception: the generator
/// has said there is no successor, so the commitment is spent.
#[must_use]
pub fn to_verdict(mapped: Mapped, order_valid_to: u32) -> Verdict {
    match mapped {
        Mapped::Verdict(verdict) => verdict,
        Mapped::Suppress {
            next_poll: NextPoll::Never,
            ..
        } => Verdict::Complete,
        Mapped::Suppress { why: _, next_poll } => Verdict::WaitTimestamp {
            wait_until: schedule_at(next_poll, order_valid_to),
            reason: Selector::ZERO,
        },
    }
}

/// Clamp a generator hint to `validTo + 1`.
///
/// The hint is advisory and handler code is arbitrary, so a generator
/// must not hold a commitment past the point its order can settle.
/// There is no floor here: the run loop is block-driven and polls each
/// commitment at most once per block, so one block is already the
/// shortest possible interval on any chain. A hint at or before the
/// current tick becomes a next-block gate, which the caller applies.
///
/// `Never` has no schedule; callers handle it before reaching here.
#[must_use]
pub fn schedule_at(next_poll: NextPoll, order_valid_to: u32) -> u64 {
    let ceiling = u64::from(order_valid_to).saturating_add(1);
    match next_poll {
        NextPoll::At(ts) => ts.min(ceiling),
        NextPoll::AtValidToPlus1 | NextPoll::Never => ceiling,
    }
}

/// Clamp rather than wrap: a gate far in the future must not become a
/// gate in the past.
fn saturating_u64(value: alloy_primitives::U256) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{Address, U256};

    use super::*;

    fn order() -> GPv2OrderData {
        GPv2OrderData {
            sellToken: Address::repeat_byte(0x11),
            buyToken: Address::repeat_byte(0x22),
            receiver: Address::ZERO,
            sellAmount: U256::from(1_000u64),
            buyAmount: U256::from(900u64),
            validTo: 1_800_000_000,
            appData: [0u8; 32].into(),
            feeAmount: U256::ZERO,
            kind: [0u8; 32].into(),
            partiallyFillable: false,
            sellTokenBalance: [0u8; 32].into(),
            buyTokenBalance: [0u8; 32].into(),
        }
    }

    fn result(
        code: GeneratorResultCode,
        fill: FillStatus,
        restriction: Restriction,
        next_poll: U256,
        wait_until: U256,
    ) -> PollResult {
        PollResult {
            generator: GeneratorResult {
                code,
                order: order(),
                nextPollTimestamp: next_poll,
                waitUntil: wait_until,
                reasonCode: Selector::new([1, 2, 3, 4]),
            },
            fill,
            filledAmount: U256::ZERO,
            restriction,
        }
    }

    fn post(fill: FillStatus, restriction: Restriction) -> PollResult {
        result(
            GeneratorResultCode::POST,
            fill,
            restriction,
            U256::from(42u64),
            U256::ZERO,
        )
    }

    fn map(r: &PollResult) -> Mapped {
        map_verdict(r, &Bytes::from_static(b"sig"))
    }

    #[test]
    fn next_poll_sentinels_are_not_timestamps() {
        assert_eq!(NextPoll::from_wire(U256::ZERO), NextPoll::AtValidToPlus1);
        assert_eq!(NextPoll::from_wire(U256::MAX), NextPoll::Never);
        assert_eq!(NextPoll::from_wire(U256::from(7u64)), NextPoll::At(7));
    }

    /// A hint above `u64::MAX` must not wrap into a near-term poll.
    #[test]
    fn an_oversized_hint_saturates() {
        let raw = U256::from(u64::MAX) + U256::from(1u64);
        assert_eq!(NextPoll::from_wire(raw), NextPoll::At(u64::MAX));
    }

    #[test]
    fn post_without_fill_or_restriction_posts_with_its_hint() {
        let mapped = map(&post(FillStatus::NONE, Restriction::NONE));
        let Mapped::Verdict(Verdict::Post {
            next_poll,
            signature,
            ..
        }) = mapped
        else {
            panic!("expected Post, got {mapped:?}");
        };
        assert_eq!(next_poll, Some(NextPoll::At(42)));
        assert_eq!(signature, Bytes::from_static(b"sig"));
    }

    /// Every observed fill suppresses the post for this uid, and none of
    /// them touches the commitment: `FillStatus` is per orderUid while a
    /// commitment spans many.
    #[test]
    fn every_fill_state_suppresses_the_post_and_keeps_the_commitment() {
        for fill in [
            FillStatus::PARTIALLY_FILLED,
            FillStatus::FILLED,
            FillStatus::INVALIDATED,
        ] {
            let mapped = map(&post(fill, Restriction::NONE));
            assert!(
                matches!(
                    mapped,
                    Mapped::Suppress {
                        why: Suppressed::Fill,
                        next_poll: NextPoll::At(42)
                    }
                ),
                "{fill:?} produced {mapped:?}",
            );
        }
    }

    #[test]
    fn a_swap_guard_suppresses_the_post_and_keeps_the_commitment() {
        let mapped = map(&post(FillStatus::NONE, Restriction::SWAP_GUARD));
        assert!(
            matches!(
                mapped,
                Mapped::Suppress {
                    why: Suppressed::SwapGuard,
                    ..
                }
            ),
            "{mapped:?}",
        );
    }

    /// A fill outranks a guard, so the reason logged is the one that
    /// would still hold if the guard were lifted.
    #[test]
    fn a_fill_outranks_a_swap_guard() {
        let mapped = map(&post(FillStatus::FILLED, Restriction::SWAP_GUARD));
        assert!(
            matches!(
                mapped,
                Mapped::Suppress {
                    why: Suppressed::Fill,
                    ..
                }
            ),
            "{mapped:?}",
        );
    }

    #[test]
    fn wait_codes_gate_on_wait_until_not_on_the_poll_hint() {
        let ts = result(
            GeneratorResultCode::WAIT_TIMESTAMP,
            FillStatus::NONE,
            Restriction::NONE,
            U256::from(42u64),
            U256::from(1_700u64),
        );
        assert!(
            matches!(
                map(&ts),
                Mapped::Verdict(Verdict::WaitTimestamp {
                    wait_until: 1_700,
                    ..
                })
            ),
            "{:?}",
            map(&ts),
        );

        let block = result(
            GeneratorResultCode::WAIT_BLOCK,
            FillStatus::NONE,
            Restriction::NONE,
            U256::from(42u64),
            U256::from(99u64),
        );
        assert!(
            matches!(
                map(&block),
                Mapped::Verdict(Verdict::WaitBlock { wait_until: 99, .. })
            ),
            "{:?}",
            map(&block),
        );
    }

    /// An unreachable gate must not wrap into a gate in the past.
    #[test]
    fn an_oversized_wait_saturates() {
        let huge = result(
            GeneratorResultCode::WAIT_TIMESTAMP,
            FillStatus::NONE,
            Restriction::NONE,
            U256::ZERO,
            U256::MAX,
        );
        assert!(matches!(
            map(&huge),
            Mapped::Verdict(Verdict::WaitTimestamp {
                wait_until: u64::MAX,
                ..
            })
        ));
    }

    #[test]
    fn the_remaining_codes_map_one_to_one() {
        let cases = [
            (GeneratorResultCode::TRY_NEXT_BLOCK, "TryNextBlock"),
            (GeneratorResultCode::INVALID, "Invalid"),
            (GeneratorResultCode::NEEDS_INPUT, "Park"),
        ];
        for (code, want) in cases {
            let r = result(
                code,
                FillStatus::NONE,
                Restriction::NONE,
                U256::ZERO,
                U256::ZERO,
            );
            let mapped = map(&r);
            let got = match mapped {
                Mapped::Verdict(Verdict::TryNextBlock { .. }) => "TryNextBlock",
                Mapped::Verdict(Verdict::Invalid { .. }) => "Invalid",
                Mapped::Verdict(Verdict::Park { .. }) => "Park",
                other => panic!("{code:?} produced {other:?}"),
            };
            assert_eq!(got, want, "{code:?}");
        }
    }

    /// A fill never tears a commitment down, whatever the code: only
    /// INVALID does.
    #[test]
    fn only_invalid_tears_down_a_commitment() {
        for code in [
            GeneratorResultCode::POST,
            GeneratorResultCode::WAIT_TIMESTAMP,
            GeneratorResultCode::WAIT_BLOCK,
            GeneratorResultCode::TRY_NEXT_BLOCK,
            GeneratorResultCode::NEEDS_INPUT,
        ] {
            for fill in [
                FillStatus::NONE,
                FillStatus::PARTIALLY_FILLED,
                FillStatus::FILLED,
                FillStatus::INVALIDATED,
            ] {
                let r = result(code, fill, Restriction::NONE, U256::ZERO, U256::ZERO);
                assert!(
                    !matches!(map(&r), Mapped::Verdict(Verdict::Invalid { .. })),
                    "{code:?} with {fill:?} tore the commitment down",
                );
            }
        }
    }

    /// `sol!` decodes an out-of-range code to a sentinel rather than
    /// rejecting it, so the decoder rejects it instead. The chain cannot
    /// produce one: the registry assigns the handler's return into a
    /// typed struct, so Solidity panics and reverts first.
    #[test]
    fn an_out_of_range_code_is_a_decode_error() {
        let mut r = post(FillStatus::NONE, Restriction::NONE);
        r.generator.code = GeneratorResultCode::__Invalid;
        let encoded = (r, Bytes::from_static(b"sig")).abi_encode_params();
        let err = decode_poll_return(&encoded).expect_err("an out-of-range code refuses");
        assert!(
            err.to_string().contains("outside the declared range"),
            "{err}"
        );
    }

    #[test]
    fn a_shape_mismatch_is_a_decode_error() {
        assert!(decode_poll_return(&[0u8; 7]).is_err());
    }

    #[test]
    fn a_poll_return_round_trips() {
        let encoded = (
            post(FillStatus::NONE, Restriction::NONE),
            Bytes::from_static(b"sig"),
        )
            .abi_encode_params();
        let (decoded, signature) = decode_poll_return(&encoded).expect("round trip");
        assert_eq!(signature, Bytes::from_static(b"sig"));
        assert!(matches!(
            map_verdict(&decoded, &signature),
            Mapped::Verdict(Verdict::Post { .. })
        ));
    }
}

#[cfg(test)]
mod residual_tests {
    use alloy_primitives::fixed_bytes;
    use alloy_sol_types::SolError;
    use nexum_sdk::host::RpcError;

    use super::*;

    fn reverted(data: Option<Vec<u8>>) -> ChainError {
        ChainError::Rpc(RpcError {
            code: 3,
            message: "execution reverted".into(),
            data: data.map(Into::into),
        })
    }

    /// All three are terminal: a backoff would wait out state only a
    /// re-`create` changes, and that re-indexes anyway.
    #[test]
    fn every_reachable_residual_error_is_terminal() {
        for selector in [
            IComposableCowResidual::SingleOrderNotAuthed::SELECTOR,
            IComposableCowResidual::ProofNotAuthed::SELECTOR,
            IComposableCowResidual::InterfaceNotSupported::SELECTOR,
        ]
        .map(Selector::from)
        {
            let verdict = classify_revert(&reverted(Some(selector.to_vec())));
            assert!(
                matches!(verdict, Verdict::Invalid { reason } if reason == selector),
                "{selector:?} produced {verdict:?}",
            );
            assert!(is_residual_selector(selector));
        }
    }

    /// A handler `Panic` is deployed code misbehaving; no retry fixes it.
    #[test]
    fn an_unrecognised_selector_is_terminal() {
        let panic_selector = Selector::new([0x4e, 0x48, 0x7b, 0x71]);
        assert!(!is_residual_selector(panic_selector));
        assert!(matches!(
            classify_revert(&reverted(Some(panic_selector.to_vec()))),
            Verdict::Invalid { .. }
        ));
    }

    #[test]
    fn a_payload_free_failure_stays_retryable() {
        assert!(matches!(
            classify_revert(&reverted(None)),
            Verdict::TryNextBlock { .. }
        ));
        assert!(matches!(
            classify_revert(&reverted(Some(vec![1, 2]))),
            Verdict::TryNextBlock { .. }
        ));
    }

    /// A rename upstream must fail here, not silently reclassify a
    /// removed order as retryable.
    #[test]
    fn residual_selectors_match_the_deployed_abi() {
        assert_eq!(
            Selector::from(IComposableCowResidual::SingleOrderNotAuthed::SELECTOR),
            fixed_bytes!("7a933234"),
        );
        assert_eq!(
            Selector::from(IComposableCowResidual::ProofNotAuthed::SELECTOR),
            fixed_bytes!("4a821464"),
        );
        assert_eq!(
            Selector::from(IComposableCowResidual::InterfaceNotSupported::SELECTOR),
            fixed_bytes!("2c7ca6d7"),
        );
    }
}

#[cfg(test)]
mod fold_tests {
    use super::*;

    const VALID_TO: u32 = 2_000_000;

    fn suppressed(next_poll: NextPoll) -> Mapped {
        Mapped::Suppress {
            why: Suppressed::Fill,
            next_poll,
        }
    }

    fn wait_until(mapped: Mapped) -> u64 {
        match to_verdict(mapped, VALID_TO) {
            Verdict::WaitTimestamp { wait_until, .. } => wait_until,
            other => panic!("expected WaitTimestamp, got {other:?}"),
        }
    }

    #[test]
    fn a_suppression_schedules_rather_than_dropping() {
        assert_eq!(wait_until(suppressed(NextPoll::At(1_500_000))), 1_500_000);
        assert_eq!(
            wait_until(suppressed(NextPoll::AtValidToPlus1)),
            u64::from(VALID_TO) + 1,
        );
    }

    /// A generator must not hold a commitment past the point its order
    /// can settle.
    #[test]
    fn a_hint_beyond_valid_to_is_lowered_to_it() {
        assert_eq!(
            wait_until(suppressed(NextPoll::At(u64::MAX - 1))),
            u64::from(VALID_TO) + 1,
        );
    }

    /// There is no seconds floor: the run loop polls each commitment at
    /// most once per block, so one block is already the shortest
    /// interval on any chain. A hint in the past passes through here and
    /// the run loop turns it into a next-block gate.
    #[test]
    fn a_hint_in_the_past_is_left_for_the_run_loop() {
        assert_eq!(wait_until(suppressed(NextPoll::At(0))), 0);
        assert_eq!(wait_until(suppressed(NextPoll::At(7))), 7);
    }

    /// An order already past `validTo` clamps to a ceiling in the past,
    /// which the run loop schedules as the next block; the poll then
    /// retires it.
    #[test]
    fn an_expired_order_clamps_into_the_past() {
        assert_eq!(schedule_at(NextPoll::At(u64::MAX), 10), 11);
    }

    /// `Never` is the generator saying there is no successor, so the
    /// commitment is spent rather than gated out of reach.
    #[test]
    fn never_completes_the_commitment() {
        assert!(matches!(
            to_verdict(suppressed(NextPoll::Never), VALID_TO),
            Verdict::Complete
        ));
    }

    #[test]
    fn a_verdict_passes_through_untouched() {
        let mapped = Mapped::Verdict(Verdict::TryNextBlock {
            reason: Selector::repeat_byte(9),
        });
        assert!(matches!(
            to_verdict(mapped, VALID_TO),
            Verdict::TryNextBlock { reason } if reason == Selector::repeat_byte(9)
        ));
    }
}
