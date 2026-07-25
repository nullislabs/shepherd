---
status: accepted
---

# Local store durability is per-call, not per-event atomic

## Context

The `nexum:host/local-store` seam is call-by-call (get, set, delete, list-keys, contains, len, count) and the redb backend commits each set/delete in its own fsynced transaction. No transaction spans an `on_event` or `init` call; the dispatch path holds none. A handler that writes then traps (typed fault, panic, OutOfFuel, deadline, process crash) keeps every write already returned. Earlier docs claimed an implicit per-event write transaction that rolls back on trap; it never existed.

The keeper Journal (#583-#587) reserves a marker before a venue submit and commits after, and a top-of-sweep reconcile re-posts stranded reservations. Its correctness requires the `RESERVED` marker, committed before the submit, to survive a trap during or after it. Per-event rollback would erase that marker while the venue may already hold the order: silent divergence or double-submit, the failure #574 closed.

## Decision

Per-call committed durability is the contract. Every set/delete/apply is fsync-durable when `Ok` returns, program-order, read-your-writes; a trap freezes state at the last completed call and never rewinds it. Single-actor serialised dispatch means no observer sees a torn mid-event state.

Per-event atomicity is rejected in every form:

- A held redb `WriteTransaction` across `on_event` locks the single shared write head across handler awaits (wasi:http, chain RPC) for up to the dispatch deadline, and the deadline drop leaks the open transaction while `HostState` survives, write-freezing every module through the poison-backoff window.
- A host-side staged overlay defers the `RESERVED` marker's durability past the wire send, so any durable-now escape hatch carries all load-bearing writes and the transaction protects nothing new.
- A snapshot or undo log is dominated by both.

Cross-boundary atomicity is impossible: no store transaction undoes an order the venue holds.

The sanctioned atomicity scope is one opt-in batch verb, `apply(ops: list<write-op>)`, committed in a single synchronous host call (#609). Multi-key state invariants use `apply`; any write sequence crossing an await or an external effect uses the Journal, whose reliance on no-rollback is load-bearing, not a missing feature. A logical change that spans keys without `apply` orders its writes so every prefix is a valid state (the recoverable key last).

## Consequences

The store is a write-ahead intent log with key-value convenience, not a transactional database: traps freeze state, they never rewind it. At-least-once effects composed with the per-venue idempotency key (#574) are effectively exactly-once at the venue, the strongest guarantee attainable across the boundary. `apply` is an additive `nexum:host@0.1.0` verb landed pre-cleave. Whole-event staging is rejected rather than parked; reopen only on a measured need for cursor-joined exactly-once that redelivery plus idempotence demonstrably fails to cover.
