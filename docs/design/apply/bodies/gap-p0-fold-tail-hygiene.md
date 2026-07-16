Bundle the contract-hygiene items that must ride the reshape fold: the codec version discriminator, migration-cruft deletion, and the denied() doc caveat.

## Why
The reshape enumerates several contract-hygiene items that must ride the fold but are not covered by any other issue: the surface pin sets the shape and the conformance kit lands too late for the fold. These are cheap now (echo-only pinning) and must precede the true 0.1.0 cut. Part of milestone M1: Videre contract reshape and host-intent decoupling. Blocked by: Pin the videre WIT surface. See docs/design/videre-split-plan.md and docs/design/issue-milestone-plan.md.

## Scope
- Add a codec version discriminator plus reject-unknown and a non-empty-vector assertion on the cross-language codec goldens (#297).
- Delete the migration cruft: docs/migration/0.1-to-0.2.md and the "Migration from 0.1" prose in docs/08-platform-generalisation.md.
- Add a MUST-NOT-retry doc caveat on venue-error.denied().
- Verify the valid-until to valid-until-ms rename and the named-ERC-record lift actually land in the fold.

## Done when
- Codec goldens carry a version discriminator, reject unknown versions, and assert a non-empty vector.
- docs/migration/0.1-to-0.2.md and the docs/08 migration prose are deleted.
- denied() has a MUST-NOT-retry doc.
- valid-until-ms and named ERC records are confirmed in-tree.
