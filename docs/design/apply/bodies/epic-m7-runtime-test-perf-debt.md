Pays down host-internal debt across the test harness, supervisor clock seam, lock performance, and state-seam batching.

## Goal
Clear the host-internal runtime debt: a multi-module test harness, a supervisor clock seam, lock performance, and state-seam batching.

## Scope
The runtime carries several pieces of internal debt that do not surface to venues but slow development and hurt performance. A multi-module test harness lets scenarios exercise more than one module at once, a supervisor clock seam makes lifecycle timing testable, and lock and state-seam batching work reduce contention and round-trips. Together these harden the host internals ahead of further venue work.

Milestone: M8: Post-v1 hardening and debt.
