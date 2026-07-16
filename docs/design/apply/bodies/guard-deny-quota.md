Charge the caller's quota when the guard denies a submission, closing the free retry loop.

## Why
When the guard denies a submission, the router does not charge the caller's quota, so a module can retry a denied submission in a tight loop for free: a DoS against the guard and router. It is latent today because only AllowAllGuard ships, but it is cheap to fix now and required the moment a real guard denies. Part of milestone M1: Videre contract reshape and host-intent decoupling. See docs/design/videre-split-plan.md and docs/design/issue-milestone-plan.md.

## Scope
- On a guard-deny verdict, charge the caller's rate and quota exactly as an accepted submit would, before returning the denial.
- Add a test that a repeated denied submit exhausts quota rather than looping for free.

## Done when
- Guard-deny charges quota.
- A repeated-deny loop is rate-limited.
- A regression test is present.
