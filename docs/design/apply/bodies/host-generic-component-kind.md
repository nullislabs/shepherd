Extract the generic supervised-component and host-actor machinery out of `AdapterActor` into `nexum-runtime`, and collapse the hardcoded kind dispatch into a generic role loop. The supervisor currently hardcodes component kinds via `enum ModuleKind { EventModule | VenueAdapter }` with a `match kind` dispatch, and `AdapterActor` is a special-cased provider actor.

## Why
Hardcoding component kinds and special-casing the adapter actor bakes venue shapes into the core and leaves provider components outside the recovery machinery. Extracting the reusable primitive (fuel refuel, trap-to-error projection, async-mutex serialization, and restart/poison-sweep membership) lets any provider component join the sweeps and lets the supervisor dispatch generically instead of by named kind. Part of milestone M2: Generic venue-agnostic host. Blocked by: Grow the Extension seam to carry worker/provider roles. See docs/design/videre-split-plan.md and docs/design/issue-milestone-plan.md.

## Scope
- Extract the generic supervised-component and host-actor primitive from `AdapterActor` into `nexum-runtime`: fuel refuel, trap-to-error projection, async-mutex serialization, and sweep membership.
- Collapse the `match kind` dispatch into a generic role loop.
- Fold provider components into the restart and poison sweeps so they expose `adapters_alive`/`providers_alive`.
- Remove any venue-named kind arm from the core.

## Done when
- The generic supervised-component and host-actor primitive is extracted from `AdapterActor` into `nexum-runtime` (fuel, trap projection, serialization, sweeps).
- The supervisor `match kind` is collapsed to a generic role loop.
- Provider components join the restart and poison sweeps and expose a liveness query.
- No venue-named kind arm remains in the core.
