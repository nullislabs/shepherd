Write a short design note or ADR that pins how the host event's opaque status bytes destructure, so the host-intent decouple can land correctly.

## Why
The host-intent decouple drops `use nexum:intent/types.{receipt, intent-status}` from wit/nexum-host/types.wit and has the host event stream carry opaque status bytes, but how those bytes destructure is still an open decision: the exact wording and versioning scheme of the documented opaque-status destructuring contract is unresolved and ranks as a top risk. No other issue owns this design decision; the decouple is the implementation and cannot land correctly until the contract shape exists. Part of milestone M1: Videre contract reshape and host-intent decoupling. See docs/design/videre-split-plan.md and docs/design/issue-milestone-plan.md.

## Scope
- Define the wire form: a version discriminator plus a destructuring rule.
- Decide schema ownership: the bytes are host-emitted but their meaning is videre-owned.
- Land it as a short design note or ADR under docs/design/.

## Done when
- A committed design note or ADR pins the opaque-status byte format: version discriminator, destructuring rule, and schema ownership.
- The host-intent decouple issue lists it as a dependency and cites it.
- The corresponding open item is closed.
