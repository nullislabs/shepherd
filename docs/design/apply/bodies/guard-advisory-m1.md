Keep the egress guard advisory-only for this milestone: keep AllowAllGuard as the default, feature-gate the pool import, and document the checkpoint as not yet enforcing.

## Why
The egress guard is AllowAllGuard, a no-op (pool_router.rs lines 104-110), and the real guard is deferred wholly to the egress-guard epic. This milestone must not advertise a boundary it does not enforce. Part of milestone M1: Videre contract reshape and host-intent decoupling. See docs/design/videre-split-plan.md and docs/design/issue-milestone-plan.md.

## Scope
- Keep AllowAllGuard as the default guard.
- Feature-gate the nexum:intent/pool import so the advertised derive to guard to submit checkpoint is not shipped as enforcing in the default build.
- Document at the router seam and in venue docs that the checkpoint is advisory-only and not yet enforcing, with a forward pointer to the egress-guard epic.

## Done when
- AllowAll is kept as the default.
- The pool import is feature-gated.
- The router seam and venue docs mark the checkpoint advisory-only for this milestone with a link to the guard epic.
