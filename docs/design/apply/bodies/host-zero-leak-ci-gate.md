Add a permanent CI check that fails if the host regains any intent, venue or cow knowledge, and wire it as a required check. Once the host is generic, the venue-agnostic invariant must be enforced permanently rather than left to review.

## Why
A one-time refactor can silently regress: a later change could reintroduce an intent or venue symbol or a forbidden crate edge, quietly re-coupling the host. A grep-plus-dependency-graph check turns the venue-agnostic invariant into an enforced, permanent property. Part of milestone M2: Generic venue-agnostic host. Blocked by: Extract PoolRouter to VenueRegistry as an extension-owned service; De-hardcode the KNOWN capability table and extract world synthesis to nexum-world; Extract a generic supervised-component primitive from AdapterActor. See docs/design/videre-split-plan.md and docs/design/issue-milestone-plan.md.

## Scope
- Add a CI check that greps `crates/nexum-runtime/src` for `nexum:intent|value-flow|VenueAdapter|synthesize_venue|nexum:adapter|PoolRouter` and fails on any hit.
- Run `cargo tree -p nexum-runtime` and fail if it reaches any videre, intent or cow crate.
- Wire it as a required check.
- Document the invariant in the crate charter.

## Done when
- CI has a required check that greps `nexum-runtime/src` for intent, venue and cow symbols and runs `cargo tree -p nexum-runtime` for forbidden crate edges, failing on any hit.
- The check is green at the generalized tip.
- The invariant is documented in the crate charter.
