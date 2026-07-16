Puts real enforcement teeth behind http and messaging egress and closes the fidelity gap between mock and host capability grants.

## Goal
Bring http and messaging egress under genuine enforcement, and align the mock capability grant with the real host grant, so that no capability can escape the compile-time world guarantee or slip past a test that the host would reject.

## Scope
This epic hardens the egress capability model on three fronts that must agree with one another. It brings venue http egress under the synthesised-world guarantee (or documents the allowlist-only story at the seam) and settles on a single adapter import-narrowing contract, so undeclared egress fails at build time rather than at runtime. It enforces the declared messaging scope on the query path, matching publish-scope enforcement, before the Waku backend makes the gap live. Finally it reconciles the mock capability grant with the host `CapabilityRegistry` so mock and host give identical grant and deny decisions for every KNOWN capability, ideally derived from one source of truth.

Milestone: M7: Egress guard.
