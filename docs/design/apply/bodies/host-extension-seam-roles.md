Grow the extension seam so it can register generic worker and provider roles instead of only adding host interfaces. Today `Extension<T> { link, capabilities }` in `host/extension.rs` can hand a worker extra host interfaces, but it cannot register a component kind (`ModuleKind` is a hardcoded enum) or a host service (`PoolRouter` is a privileged field).

## Why
The runtime should know only two generic roles: a worker, which the host pushes events at, and a provider, which the host holds behind a serialized actor. Baking component kinds into an enum and privileging a specific service as a named field is what forces venue and intent shapes into the host layer. `Extension` must instead contribute a namespace, capabilities, a link, a service and a provider, so that anything venue-specific plugs in through registration rather than through core edits. Part of milestone M2: Generic venue-agnostic host. Blocked by: host-r6-decouple. See docs/design/videre-split-plan.md and docs/design/issue-milestone-plan.md.

## Scope
- Extend the `Extension` seam to contribute namespace, capabilities, link, service and provider.
- Add a type-erased `HostService` held on a typed per-namespace `HostState.services` map.
- Add a `ProviderKind<T>` carrying a link plus an async install.
- Use native async-fn-in-trait for the hot static-dispatch guest traits; use `async_trait` only for the one cold dyn path, `ProviderKind::install`; keep `HostService` synchronous so it stays dyn-compatible.
- Preserve the existing worker boot path unchanged.

## Done when
- The `Extension` seam carries namespace, capabilities, link, service and provider.
- `HostService` and `ProviderKind` traits exist.
- `HostState.services` is a typed per-namespace map.
- It compiles on MSRV 1.94 with native async-fn-in-trait for the hot traits and `async_trait` only for `ProviderKind::install`.
- The existing worker boot path is unchanged.
