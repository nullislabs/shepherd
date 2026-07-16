Land the entire contract reshape as one oracle-validated git-filter-repo/jj fold across the milestone train, rather than as per-car edits.

## Why
Every reshape content change (the host-intent decouple, the videre rename, the version normalize, quote, the surface pin, plus the fold-tail hygiene) must land as a single train-wide fold. A WIT type touched in an early car is imported by all downstream cars, so editing car by car desyncs the stack. No other issue owns the mechanical fold execution: the range-limited git-filter-repo pass replayed across the stack, the jj-driven per-car rebases, mergiraf conflict resolution, videre-test golden regeneration, byte-identical tip-oracle re-assertion, and the single force-push. This is the proven keeper-rename template and it is load-bearing. Part of milestone M1: Videre contract reshape and host-intent decoupling. Blocked by: Decouple nexum:host from nexum:intent so the host event carries opaque status bytes; Spec the opaque-status destructuring contract; Rename the nexum:intent WIT packages and symbols to videre; Normalize all WIT packages to a single @0.1.0; Add quote to videre:venue and the IntentClient typestate; Pin the videre WIT surface; Fold-tail contract hygiene. See docs/design/videre-split-plan.md and docs/design/issue-milestone-plan.md.

## Scope
- Assemble all reshape content changes on refactor/intent-contract-reshape.
- Run the fold across the milestone stack (#239 to #260 plus #334/#335).
- Regenerate goldens.
- Re-assert the tip oracle.
- Single force-push.

## Done when
- The full reshape lands as one force-pushed fold.
- The tip oracle is byte-identical across the two rebuild paths.
- videre-test goldens are regenerated and green.
- All stack branches are MERGEABLE.
