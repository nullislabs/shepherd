# Pinned videre WIT surface — 0.1.0 (frozen fold target)

The byte-exact target the M1 contract fold (#366) rewrites to, and the oracle
checks against. Supersedes `videre-split-plan.md` §7.4 where they differ.

Decisions locked 2026-07-16:

1. Thin to §7.4: single-asset `gives`/`wants`, `auth-scheme {eip1271, eip712}`,
   EVM-only assets, plain-enum `intent-status`. Dropped cases return as
   additive 0.2+ variants.
2. Amounts and addresses are big-endian, minimal-length, variable width (zero =
   empty list). §7.4's "32-byte LE" is void. The `u256` alias is renamed `uint`.
3. `intent-status` is a plain enum; settlement proof and failure reason ride the
   #360 opaque-status body, not the WIT (see §4).
4. `erc20 {token: address}` carries no chain id; assets share `settlement.chain`.

Bundled consequence of (1), flagged: `intent-header` drops `valid-until`. Header
expiry is gone in 0.1; expiry survives on `quote.valid-until-ms`. Re-add as an
additive `option<u64>` in 0.2 if a keeper needs pre-decode expiry.

## 1. `videre:value-flow@0.1.0`

```wit
package videre:value-flow@0.1.0;

/// Egress-neutral vocabulary for value in motion. Carries no dependency so it
/// outlives any contract built on it. EVM-only in 0.1.
interface types {
    /// 20-byte EVM address, big-endian.
    type address = list<u8>;

    /// Unsigned integer, big-endian, minimal-length: no leading zero bytes,
    /// zero is the empty list. Decoders MUST compare by integer value, not by
    /// byte equality.
    type uint = list<u8>;

    /// An ERC-20 token on the intent's settlement chain.
    record erc20 {
        token: address,
    }

    /// A kind of value that can move. erc721/erc1155/service/offchain are 0.2+.
    variant asset {
        /// The settlement chain's gas token.
        native,
        erc20(erc20),
    }

    /// An amount of one asset. Never negative; direction lives in the field
    /// that holds the pair (`gives` vs `wants`).
    record asset-amount {
        asset: asset,
        amount: uint,
    }
}
```

## 2. `videre:types@0.1.0`

```wit
package videre:types@0.1.0;

/// The venue-neutral intent ontology. Depends only on value-flow; never on
/// nexum:host, so the venue-error transport cases are its own.
interface types {
    use videre:value-flow/types.{asset-amount};

    /// How an intent is authorised at its venue. Non-EVM schemes are 0.2+.
    variant auth-scheme {
        eip1271,
        eip712,
    }

    /// Where a deal settles. EVM-only in 0.1.
    record settlement {
        chain: u64,
    }

    /// Adapter-derived description of an intent body: the ontology guard policy
    /// runs on. Policy has teeth on `gives`; `wants` is display-grade.
    record intent-header {
        gives: asset-amount,
        wants: asset-amount,
        settlement: settlement,
        authorisation: auth-scheme,
    }

    /// Venue-scoped stable id for a submitted intent. Opaque to host and policy.
    type receipt = list<u8>;

    /// An EVM call the host must sign and send. The adapter only describes it;
    /// the host fills gas/fee and signs, so adapters cannot move value. Always
    /// a call to existing code.
    record unsigned-tx {
        chain: u64,
        /// 20-byte contract address.
        to: list<u8>,
        /// Native value, big-endian minimal; empty is zero.
        value: list<u8>,
        /// ABI-encoded calldata.
        data: list<u8>,
    }

    /// What a successful submit produced.
    variant submit-outcome {
        accepted(receipt),
        requires-signing(unsigned-tx),
    }

    /// Lifecycle state. Coarse and portable; proof and failure reason ride the
    /// opaque status body (see docs/design/videre-wit-pinned-0.1.0.md §4).
    enum intent-status {
        pending,
        open,
        fulfilled,
        cancelled,
        expired,
    }

    /// Failure of a client or adapter call. `denied` and `rate-limited` are the
    /// only guard/transport shapes; `denied` MUST NOT be retried.
    variant venue-error {
        unknown-venue,
        invalid-body(string),
        unsupported,
        denied(string),
        rate-limited(rate-limit),
        unavailable(string),
        timeout,
    }

    record rate-limit {
        retry-after-ms: option<u64>,
    }

    /// An indicative quotation for a body. Firm/RFQ maker-side offers are #355.
    record quotation {
        gives: asset-amount,
        wants: asset-amount,
        fee: asset-amount,
        valid-until-ms: u64,
    }
}
```

## 3. `videre:venue@0.1.0`

Two mirrored faces: the worker `client` face the keeper imports, the provider
`adapter` face one venue exports. One adapter is one venue.

```wit
package videre:venue@0.1.0;

/// Worker (keeper) face. The host holds the venue registry; the keeper names a
/// venue by string.
interface client {
    use videre:types/types.{quotation, receipt, intent-status, submit-outcome, venue-error};

    quote:  func(venue: string, body: list<u8>) -> result<quotation, venue-error>;
    submit: func(venue: string, body: list<u8>) -> result<submit-outcome, venue-error>;
    status: func(venue: string, receipt: receipt) -> result<intent-status, venue-error>;
    cancel: func(venue: string, receipt: receipt) -> result<_, venue-error>;
}

/// Provider (venue) face. Mirrors `client` without the venue selector.
interface adapter {
    use videre:types/types.{intent-header, quotation, receipt, intent-status, submit-outcome, venue-error};

    /// Supported body schema versions, for the install-time handshake (#373).
    body-versions: func() -> list<u32>;
    /// Pure: derive the guard-facing header from a body. No I/O.
    derive-header: func(body: list<u8>) -> result<intent-header, venue-error>;
    quote:  func(body: list<u8>) -> result<quotation, venue-error>;
    submit: func(body: list<u8>) -> result<submit-outcome, venue-error>;
    status: func(receipt: receipt) -> result<intent-status, venue-error>;
    cancel: func(receipt: receipt) -> result<_, venue-error>;
}
```

## 4. `#360` opaque status body (host decouple)

R6 (#361) stops `nexum:host` importing the intent contract. The host event
carries status as opaque `list<u8>`; the keeper decodes it. This body is not
WIT; it is a versioned codec the adapter emits and the keeper reads.

`v1` layout, borsh, leading `u8` version tag = 1:

```
status-body-v1 := 0x01
              ++ status:   intent-status            // enum discriminant
              ++ proof:    option<list<u8>>         // settlement proof, venue bytes
              ++ reason:   option<fail-reason>      // set only on a terminal failure

fail-reason := { code: string, detail: string }
```

Contract:

- The version tag leads. An unknown tag is a decode error, fail-closed
  (reject-unknown).
- The body is never empty: at minimum tag + status.
- `proof` is display-grade venue bytes (for an EVM venue, typically the settle
  tx hash). The host never inspects it.
- `reason` is present iff `status` decodes to a terminal-failure lifecycle the
  venue reports; the enum itself has no `failed` case, so a keeper reads a
  non-`fulfilled` terminal state plus a `reason`.
- `code` is a venue-scoped machine string a keeper may match on; `detail` is for
  logs and the consent surface.

Codec goldens carry the version discriminator, a reject-unknown case, and a
non-empty-vector assertion.

### host `types.wit` after R6

```wit
// was: use nexum:intent/types@0.1.0.{receipt, intent-status};   // removed

record intent-status-update {
    venue: string,
    /// Venue receipt, opaque to the host.
    receipt: list<u8>,
    /// Opaque status body; see §4.
    status: list<u8>,
}
```

## 5. Rename + normalize map (the fold)

| From (current) | To |
|---|---|
| `nexum:intent` (types, pool, adapter) | `videre:types` + `videre:venue` (client + adapter faces) |
| `nexum:value-flow` | `videre:value-flow` |
| `nexum:adapter` (venue-adapter world) | folded into `videre:venue` |
| `nexum:host@0.2.0`, `shepherd:cow@0.2.0`, all `videre:*` | `@0.1.0` (single baseline) |
| `PoolRouter` | `VenueRegistry` |
| `AdapterActor` | `VenueActor` |
| `GuardPolicy` / `AllowAllGuard` | `EgressGuard` |
| (new) | `VenueId` newtype |

Dropped vs current tree (0.2+ additive re-adds): multi-asset `gives`/`wants`
lists; `auth-scheme` presign/offchain-sig/unsigned; `asset`
erc721/erc1155/service/offchain and `service-desc`/`offchain-desc`; off-chain
`settlement`; `intent-header.valid-until`; `venue-error` invalid-receipt /
rejected / internal-error.
