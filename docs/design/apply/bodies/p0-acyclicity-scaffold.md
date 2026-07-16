Add a CI job and a local command that assert nexum-runtime (L1) is venue-agnostic and knows nothing about intents, venues, or CoW. It ships advisory (non-blocking) now, with a tracked promotion to a blocking gate later.

## Why
The entire split rests on one invariant: nexum-runtime knows nothing about intents, venues, or CoW. Deleting the HostState.pool_router field is the forcing-function acceptance test for that invariant, and the invariant needs to be continuously checkable long before the physical cut. Part of milestone M1: Videre contract reshape and host-intent decoupling. See docs/design/videre-split-plan.md and docs/design/issue-milestone-plan.md.

## Scope
- Add a CI job plus a just/cargo xtask entrypoint that asserts L1 is venue-agnostic.
- cargo tree -p nexum-runtime must not reach videre-*, intent, or cow crates.
- An rg symbol scan must return empty.
- Assert the WIT DAG resolves with nexum:host as a leaf.
- Land it advisory (non-blocking) now, with a tracked flip to a blocking gate.

## Done when
- CI and a local command assert the cargo tree and rg symbol checks on nexum-runtime and the leaf-ness of nexum:host.
- The checks are wired advisory now with a tracked flip-to-blocking recorded.
