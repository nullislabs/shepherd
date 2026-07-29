Prove the host is venue-agnostic by landing a permanent zero-leak CI check and making it a blocking, required gate.

## Goal
Enforce, forever, that the host layer never regains intent, venue or cow knowledge. The check greps the runtime crate for forbidden symbols and inspects its dependency graph for forbidden crate edges, and an echo-venue boot test acts as the oracle that the generic seam actually works.

## Scope
This epic adds a permanent zero-leak and acyclicity CI check over `nexum-runtime`, then flips it from advisory to blocking once the `HostState.pool_router` field is deleted and the router is carried only through the generic services map. The forcing function is that an echo-venue must still install and route a submission with no intent or cow crate anywhere in the graph, which demonstrates the host is genuinely venue-agnostic rather than merely refactored.

Milestone: M2: Generic venue-agnostic host.
