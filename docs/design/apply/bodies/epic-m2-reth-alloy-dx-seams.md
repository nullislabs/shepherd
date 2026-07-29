Complete the guest-facing developer surface with the remaining host seams, an alloy provider over the chain host, and a cluster of alloy-grade DX polish.

## Goal
Bring the guest SDK up to an alloy-grade standard. Fill in the identity, messaging and remote-store guest traits with mocks so modules can unit-test host-free, add richer local-store queries, put an alloy Provider seam over the raw chain request path, and land the polish cluster (typed fault mirror, typestate builders, sealed traits, single-source vocabularies) that removes the last of the hand-copied boilerplate.

## Scope
Three of the six host interfaces still have no guest seam; this epic adds their traits and mocks and widens the host supertrait to cover them. It replaces the stringly chain request surface with an alloy Provider shim so authors call typed methods instead of hand-building JSON-RPC, and carries the typed chain method surface through to the guest. The remaining work is a polish cluster that mirrors the venue error into a typed fault, introduces a typestate order builder with typed token newtypes, seals the extension traits, and derives the mirrored vocabularies from single-source constants so the fault list and known table are no longer hand-maintained in several places.

Milestone: M3: Videre SDK, macros and DX.
