Build the `videre-host` crate and its `videre::platform()` entrypoint that registers the venue platform through the generalized runtime seam. The host-side issues grow the seam, extract the service and the generic component-kind, and fold adapters into the sweeps, but no issue yet owns the extension-layer crate assembly or the `videre::platform()` registration.

## Why
Growing the seam is only useful if something registers a real venue platform through it. Nothing currently owns the guard row (`GuardPolicy`/`AllowAll` becoming a videre-owned `EgressGuard`), the venue-adapter and pool-host bindgens, `build_adapter_linker`, or the adapter path of `synthesize_venue`. This crate is that home: it makes videre a single extension registered via `builder.with_extension(videre::platform())`. Part of milestone M2: Generic venue-agnostic host. Blocked by: Grow the Extension seam to carry worker/provider roles; Extract PoolRouter to VenueRegistry as an extension-owned service; Extract a generic supervised-component primitive from AdapterActor. See docs/design/videre-split-plan.md and docs/design/issue-milestone-plan.md.

## Scope
- Create `videre-host` as an extension-layer crate depending on `nexum-runtime` only.
- Register the venue-adapter `ProviderKind` and its install predicate (the body-versions handshake).
- Register the `VenueRegistry` service (the un-privileged former `PoolRouter`).
- Add the `EgressGuard` seam, advisory-only for this milestone.
- Register the `videre:venue/client` interface and land the venue-adapter and pool-host bindgens.
- Expose `videre::platform()` as the single registration entrypoint.

## Done when
- `videre::platform()` registers the provider-kind, the `VenueRegistry` service, the `EgressGuard` seam and `videre:venue/client` via the seam.
- `videre-host` depends on `nexum-runtime` only.
- The echo-venue installs and a worker submits through it with the `HostState.pool_router` field deleted.
