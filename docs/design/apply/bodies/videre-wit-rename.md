Rename the pre-release intent WIT packages and their Rust symbols into the videre namespace as one mechanical fold.

## Why
The pre-release intent contract still carries the nexum:intent branding and a single fused pool face; the videre reshape needs a clean, venue-neutral namespace and separate worker and provider faces before the surface is pinned. Part of milestone M1: Videre contract reshape and host-intent decoupling. Blocked by: Decouple nexum:host from nexum:intent so the host event carries opaque status bytes. See docs/design/videre-split-plan.md and docs/design/issue-milestone-plan.md.

## Scope
- Rename WIT packages: nexum:intent to videre:venue (plus videre:types); nexum:value-flow to videre:value-flow.
- Retire nexum:adapter; its venue-adapter world folds into videre:venue.
- Leave nexum:host and shepherd:cow untouched.
- Split the pool face into a worker face videre:venue/client and a provider face videre:venue/adapter.
- Rename Rust symbols: PoolRouter to VenueRegistry, AdapterActor to VenueActor, GuardPolicy/AllowAllGuard to EgressGuard; introduce a VenueId newtype.
- Regenerate goldens and re-assert the tip oracle.

## Done when
- No nexum:intent, nexum:value-flow, or nexum:adapter package or use reference remains.
- The tree builds.
- echo-venue and echo-client pass against regenerated goldens.
- The byte-identical tip oracle holds across the train.
- The nexum:host and shepherd:cow brands are untouched.
