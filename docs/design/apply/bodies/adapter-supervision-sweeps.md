Fold venue adapters into the restart and poison-recovery sweeps and expose an `adapters_alive` liveness signal. Adapters currently boot once and install but are not in the sweeps (`supervisor.rs:61-66`), so a trapped adapter stays dead until the process restarts and the router only projects the trap to an internal-error.

## Why
Because adapters are outside the recovery sweeps, a strategy cannot tell an unknown venue (never installed) from a venue that is temporarily dead (trapped but recoverable), and a trapped adapter never comes back on its own. Folding adapters into the sweeps recovers them automatically, and a liveness signal lets strategies distinguish the two cases. This falls out naturally from extracting the generic supervised-component primitive from `AdapterActor`. Part of milestone M2: Generic venue-agnostic host. Blocked by: Extract a generic supervised-component primitive from AdapterActor. See docs/design/videre-split-plan.md and docs/design/issue-milestone-plan.md.

## Scope
- Fold venue adapters, as the generalized provider/component kind, into the restart and poison-recovery sweeps.
- Expose `adapters_alive` or an equivalent liveness signal.
- Distinguish unknown-venue from venue-temporarily-dead.

## Done when
- Trapped adapters are swept and restarted.
- A liveness signal distinguishes unknown-venue from venue-temporarily-dead.
- A trap-to-recovery test is present.
