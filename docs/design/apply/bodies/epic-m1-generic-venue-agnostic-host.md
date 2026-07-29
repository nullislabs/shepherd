Make the runtime host fully generic so that nothing venue, intent or cow shaped lives in the host layer.

## Goal
Grow the extension seam so it can carry both worker and provider roles, pull the venue registry and the generic supervised-component primitive out of the adapter actor, and remove every hardcoded venue assumption from the host. Once this lands, the host holds venues, intents and cow logic only through generic registration points, never as privileged fields or baked-in table rows.

## Scope
This epic extends `Extension<T>` to contribute host services and provider kinds, turns the privileged `HostState.pool_router` field into an extension-owned `VenueRegistry` service, and extracts the fuel/trap/serialization/sweep machinery from `AdapterActor` into a reusable supervised-component primitive. It de-hardcodes the KNOWN capability table so per-namespace rows come from registered extensions, and factors world synthesis into a plain `nexum-world` library. It then lands a generic launcher library plus a bare `Ext=()` engine binary, and assembles the `videre-host` crate whose `videre::platform()` registers the venue provider-kind, the registry service, the egress guard seam and the client interface through the generalized seam. Together these pieces let the host boot with zero cow or venue dependencies while venues plug in as ordinary extensions.

Milestone: M2: Generic venue-agnostic host.
