Extract a generic launcher library plus a bare `Ext=()` engine binary, and remove the backwards dependency from the CLI onto the cow host. The host charter calls for a generic launcher library (`nexum-launch`) and a bare engine binary (`nexum`), but the CLI composition root wires cow in directly (`launch.rs:16,47` via `shepherd_cow_host::extension` / `with_extensions`), making `nexum-cli` depend on `shepherd-cow-host`.

## Why
`nexum-cli` depending on `shepherd-cow-host` is a backwards crate edge: a generic host binary should not reach into a specific downstream extension. Extracting `nexum-launch` and shipping a bare `Ext=()` binary gives a clean generic composition path, and moving the cow wiring into its own `shepherd` binary keeps the downstream composition root where it belongs. Part of milestone M2: Generic venue-agnostic host. Blocked by: Grow the Extension seam to carry worker/provider roles. See docs/design/videre-split-plan.md and docs/design/issue-milestone-plan.md.

## Scope
- Extract a generic `nexum-launch` library that composes a runtime from a supplied extension list.
- Add a bare `nexum` binary with `Ext=()`.
- Remove the cow wiring from the generic path.
- Move the cow composition root into a separate `shepherd` binary.

## Done when
- The generic `nexum-launch` library composes a runtime from a supplied extension list.
- A bare `Ext=()` `nexum` binary boots with zero cow or venue dependencies.
- No host-layer crate depends on `shepherd-cow-host`.
- The cow composition root is moved out to a `shepherd` binary.
