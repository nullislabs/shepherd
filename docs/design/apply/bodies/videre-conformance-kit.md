Rename the venue conformance kit to videre-test and harden it so a venue's cargo test fails whenever its wire shape drifts. The kit exists as nexum-venue-test but is mis-named and its codec golden passes vacuously on an empty vector.

## Why
The conformance kit is the gate that holds every venue to portable codec vectors and header goldens, but it is mis-named for the split and its codec golden currently passes on an empty vector, so drift can slip through. Renaming and hardening it makes wire-shape drift a hard cargo-test failure. Part of milestone M3: Videre SDK, macros and DX. Blocked by: videre-sdk: rename nexum-venue-sdk + add Keeper::sweep assembler + VenueClient. See docs/design/videre-split-plan.md and docs/design/issue-milestone-plan.md.

## Scope
- Rename nexum-venue-test to videre-test; keep `CodecVectors`, `HeaderGoldens` and `MockTransport`.
- Harden the goldens with a version discriminator, reject-unknown handling and a non-empty-vector assertion.
- Regenerate the goldens under the `videre:*` namespace and re-assert the tip oracle.
- Align mock-grant fidelity to the host to close the known divergence.

## Done when
- nexum-venue-test is renamed to videre-test.
- The kit ships `CodecVectors`, `HeaderGoldens` and `MockTransport`.
- A venue's cargo test fails on any wire-shape drift.
- The goldens carry the version discriminator, reject-unknown and a non-empty-vector assertion.
- The goldens regenerate under the `videre:*` namespace and the tip oracle holds.
