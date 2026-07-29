Reconcile the mock capability grant with the host `CapabilityRegistry` grant so a capability the host would deny is also denied under mock, ideally deriving both from the KNOWN capability table.

## Why
The mock capability grant diverges from the host's real grant, so tests can pass while real enforcement differs; this fidelity gap hides capability regressions. As the real guard replaces the shims, the mock and host grant must be canonicalised to one behaviour, ideally sourced from a single source of truth: the KNOWN capability table. Part of milestone M7: Egress guard. Blocked by: egress-guard-hardening-epic. See docs/design/videre-split-plan.md and docs/design/issue-milestone-plan.md.

## Scope
- Reconcile the mock capability-grant behaviour with the host `CapabilityRegistry` grant.
- Derive both grant decisions from one source of truth, the KNOWN capability table, where practical.
- Add a skew-guard test that fails if mock and host diverge on any KNOWN capability.

## Done when
- Mock and host agree on grant or deny for every KNOWN capability.
- A skew-guard test exists.
- No mock-only pass survives that the host would reject.
