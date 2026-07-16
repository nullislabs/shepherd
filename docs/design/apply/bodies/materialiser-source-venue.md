Generalize the keeper sweep assembler into a fully source-agnostic and venue-agnostic Materialiser<Source, Venue> that materialises a source's outcomes onto any venue. Today the generic Keeper::sweep assembler resolves the outcome and gives strategy authors an assembler over the parts, but it is not yet venue-neutral.

## Why
The venue-neutral Materialiser<Source, Venue> is the explicit destination past the first stable runtime: a single assembler that drives any source's outcomes onto any target venue with no venue-specific branches. It needs a second real venue and a settled keeper-to-pool port before it can be generalized safely, so it stays deferred until those exist. Part of milestone M8: Post-v1 hardening and debt. See docs/design/videre-split-plan.md and docs/design/issue-milestone-plan.md.

## Scope
- Generalize the Keeper::sweep assembler to a Materialiser<Source, Venue> parameterized over both source and target venue.
- Share the common Sweep outcome across the parameterizations.
- Prove venue-neutrality against at least the CoW keeper and one second venue.

## Done when
- Materialiser<Source, Venue> drives two distinct (Source, Venue) pairs through one assembler.
- The materialiser contains no venue-specific branches.
