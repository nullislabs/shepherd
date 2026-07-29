Umbrella gate: drive the milestone to a single green linear dev/m1 tip before any repository carve begins.

## Why
The plan has an explicit, un-owned gate: land the milestone's Rust amends (#249, #250, #251, #296), the #334 verdict-seam fixes, the install-time handshake, and the approved cars, to a single green linear dev/m1 tip. The carve must not begin until this tip exists, because carving mid-train triples the fold surgery across three repos. Several required cars are otherwise un-referenced: #249 supervisor missing-manifest error, #251 RateLimited fold test, #296 wit-bindgen 0.59 bump, and the #334 fixes (model Verdict::Post.next_poll_timestamp as an Option or NextPoll rather than a 0-sentinel, and add a NeedsInput dispatch test). Part of milestone M1: Videre contract reshape and host-intent decoupling. Blocked by: Execute the reshape as one oracle-validated fold; Charge quota on guard-deny to close the busy-loop DoS. See docs/design/videre-split-plan.md and docs/design/issue-milestone-plan.md.

## Scope
- Track landing of #249, #251, #296 plus the two #334 fixes plus the approved cars to one green linear tip (amend in place, no fold).

## Done when
- dev/m1 is a green linear tip with #249, #251, #296 plus both #334 fixes plus approved cars merged.
- CI is green.
- The tip is explicitly signed off as the precondition for beginning the carve.
