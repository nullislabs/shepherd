Close two coupled defects in `pool_router.rs`: derivation running before the guard checkpoint (letting side effects escape policy) and a double-decode between the guard and submit. Decode the body once and feed the guard-vetted header straight into submit.

## Why
Today the router runs the adapter's derive-header before `guard.check`, so any side effect performed during derivation escapes policy entirely; the honest fix is a guarded sub-world or moving derivation behind the checkpoint. Separately, the guard inspects the adapter's own derive-header output while submit re-decodes the body independently, a time-of-check-to-time-of-use gap: a buggy or hostile adapter can present a benign `gives` to the guard and settle something else. Passing the derived header into submit collapses this to a single decode. Part of milestone M7: Egress guard. Blocked by: egress-guard-hardening-epic. See docs/design/videre-split-plan.md and docs/design/issue-milestone-plan.md.

## Scope
- Reorder or sandbox derivation so it cannot run side effects before `guard.check`.
- Decode the body once and thread the guard-vetted header through to submit, removing the independent re-decode.
- Add a divergent-re-decode test that proves an adapter cannot show one value to the guard and settle another.

## Done when
- Derivation cannot side-effect before `guard.check`.
- The body is decoded once and submit consumes the guard-vetted header.
- A divergent-re-decode test proves no bypass.
