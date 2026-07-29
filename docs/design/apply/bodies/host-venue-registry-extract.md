Turn the privileged `HostState.pool_router` field into an extension-owned `HostService` carried through the generic services map, and delete the named supervisor field. The router is currently a privileged field `HostState.pool_router` in `host/state.rs:54`, built and cloned through `supervisor.rs`.

## Why
Once the seam can carry a service, the router no longer needs to be a privileged field: videre can register it as the renamed, un-privileged `VenueRegistry` behind `HostState.services[ns]`. Deleting `HostState.pool_router` and still booting the echo-venue is the forcing function that proves the host layer is intent-free. This issue covers the host half, holding the router only through the generic services map and removing the named field; the `VenueRegistry` implementation itself belongs to the extension. Part of milestone M2: Generic venue-agnostic host. Blocked by: Grow the Extension seam to carry worker/provider roles. See docs/design/videre-split-plan.md and docs/design/issue-milestone-plan.md.

## Scope
- Delete the `HostState.pool_router` field.
- Carry the router only via `HostState.services` as a type-erased `HostService`, renamed `VenueRegistry`.
- Remove every `PoolRouter` symbol from `nexum-runtime/src`.
- Keep the echo-venue booting and routing a submit through the services map.

## Done when
- The `HostState.pool_router` field is deleted.
- The router is carried only via `HostState.services` as a type-erased `HostService` renamed `VenueRegistry`.
- No `PoolRouter` symbol remains in `nexum-runtime/src`.
- The echo-venue still boots and routes a submit.
