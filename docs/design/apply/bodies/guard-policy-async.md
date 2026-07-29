Convert `GuardPolicy::check` (and the guard seam it fronts) from synchronous to async so the real guard can perform I/O without blocking the supervisor.

## Why
`GuardPolicy::check` is synchronous, but the real guard performs I/O: simulate over provider-pool state, fact assembly, and possibly a remote analyzer or policy backend. None of that can be expressed by a sync trait without blocking the supervisor loop. The conversion must follow the project async-dispatch strategy: native async-fn-in-trait for static-dispatch guest traits, `async_trait` only for cold dyn boot paths, and keeping the dyn-required guard and service traits object-safe. Part of milestone M7: Egress guard. Blocked by: egress-guard-hardening-epic. See docs/design/videre-split-plan.md and docs/design/issue-milestone-plan.md.

## Scope
- Make `GuardPolicy::check` and the guard seam it fronts async.
- Apply the async-dispatch split: native async-fn-in-trait for static-dispatch guest traits, `async_trait` only on cold dyn boot paths, guard and service traits kept object-safe.
- Update `AllowAllGuard` and all call sites to await the new signature.

## Done when
- `GuardPolicy::check` is async and awaited without blocking the loop.
- The async-dispatch split matches the documented strategy.
- Green on MSRV 1.94.
