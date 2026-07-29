Drop the intent import from the host world so the host event stream carries opaque status bytes instead of an intent-status-update variant. Today wit/nexum-host/types.wit line 8 does `use nexum:intent/types@0.1.0.{receipt, intent-status}` and the host event variant carries an intent-status-update, so the L1 host world imports the L2 intent package.

## Why
Until that use is gone, nexum-runtime cannot compile without the L2 intent WIT and an acyclic three-repo split is physically impossible. This is the master gate: it moves first, before any crate moves. Part of milestone M1: Videre contract reshape and host-intent decoupling. See docs/design/videre-split-plan.md and docs/design/issue-milestone-plan.md.

## Scope
- Drop the `use nexum:intent/types` from wit/nexum-host/types.wit so nexum:host becomes a leaf package.
- Redefine the host event stream to carry opaque status bytes instead of the intent-status-update variant.
- Specify the versioned destructuring contract those bytes commit to.
- Regenerate goldens and re-assert the byte-identical tip oracle.

## Done when
- nexum:host WIT no longer uses nexum:intent and is a leaf package.
- The host event carries opaque status bytes with a documented versioned destructuring contract.
- cargo tree -p nexum-runtime reaches no intent crate.
- Goldens and the tip oracle are re-validated.
