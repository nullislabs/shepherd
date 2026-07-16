Pin the shape of the videre contract surface across videre:types, videre:venue, and videre:value-flow. The 0.1 surface is EVM-only.

## Why
The rename moves the namespace; this issue pins the shape so every later venue and keeper compiles against a stable contract. Both ride one fold. Part of milestone M1: Videre contract reshape and host-intent decoupling. Blocked by: Rename the nexum:intent WIT packages and symbols to videre. See docs/design/videre-split-plan.md and docs/design/issue-milestone-plan.md.

## Scope
- Pin videre:types: intent-header, auth-scheme {eip1271, eip712}, settlement {chain:u64}, receipt = list<u8>, submit-outcome {accepted, requires-signing}, unsigned-tx, intent-status, venue-error with rate-limit.
- Pin videre:venue: the worker client face and the provider adapter face mirror each other.
- Pin videre:value-flow: asset-amount, asset {native, erc20}, named records with no anonymous tuples.
- Add codec goldens with a version discriminator, reject-unknown, and a non-empty assertion.
- Document caveats: valid-until renamed to valid-until-ms, denied() MUST-NOT-retry, derive-header purity, gives is adapter-attested not host-verified.

## Done when
- The three packages match the pinned shapes in docs/design/videre-split-plan.md.
- The worker client face and the provider adapter face mirror.
- venue-error carries rate-limited{retry-after-ms} and denied.
- value-flow uses named records with no anonymous ERC tuples.
- Codec goldens carry a version discriminator, reject-unknown, and a non-empty-vector assertion.
- echo-venue implements the pinned adapter face and passes.
