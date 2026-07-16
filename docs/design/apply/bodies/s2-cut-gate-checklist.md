A single tracking and checklist issue, plus a short pre-carve runbook, that must be closed before the three carves may start, asserting that the two cut gates are green.

## Why
The physical repo cut is gated (refactor now, cut later) on two conditions only: the runtime is venue-agnostic (zero-leak gate blocking and green, pool_router deleted); and a real shepherd cow-venue cdylib exists with the keeper ported off CowApiHost onto videre:venue/client. Per the 2026-07-15 decision the cut is not gated on a second venue: the genuine non-cow second-protocol venue is de-gated from the cut and becomes a post-cut acceptance milestone built against the already-split videre-sdk, so it can no longer freeze the carve behind an unbuilt venue. The risk this carries is that a wrong videre abstraction discovered by the post-cut second venue becomes a cross-repo change rather than an in-monorepo fold, so videre:* must stay additively extensible through the cut and the videre:value-flow 1.0 freeze must hold until the second venue proves the abstraction. Part of milestone M5: The gated three-repo split. Blocked by: s1-gate-runtime-venue-agnostic; s1b-gate-cow-on-generic-seam. See docs/design/videre-split-plan.md and docs/design/issue-milestone-plan.md.

## Scope
- Track the runtime-venue-agnostic gate: zero-leak gate blocking and green, pool_router deleted.
- Track the cow-on-generic-seam gate: a real shepherd cow-venue cdylib with the keeper ported off CowApiHost onto videre:venue/client.
- Confirm all planned WIT reshapes are complete.
- Write and sign off a short pre-carve runbook.
- Confirm videre:* is additively extensible so a post-cut abstraction fix is a non-breaking cross-repo change.

## Done when
- Both cut gates (runtime venue-agnostic; cow on the generic seam) are closed green.
- All planned WIT reshapes are confirmed complete.
- The pre-carve runbook is signed off.
- The second-venue gate is explicitly not required pre-carve; it is post-cut acceptance under this milestone.
- videre:* is confirmed additively extensible so a post-cut abstraction fix is a non-breaking cross-repo change.
