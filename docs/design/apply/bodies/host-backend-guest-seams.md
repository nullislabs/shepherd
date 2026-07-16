Add guest SDK seams and mocks for the identity, messaging and remote-store host interfaces, and wire them to the stub backends. These three of the six host interfaces currently have no guest seam.

## Why
The identity, messaging and remote-store interfaces carry `adapter:None` in the macro known table, have no `*Host` trait and no `Mock*`, so the promise that each interface becomes a trait plus a mock is unfulfilled for all three. Without them, modules cannot unit-test host-free against these interfaces. Part of milestone M3: Videre SDK, macros and DX. See docs/design/videre-split-plan.md and docs/design/issue-milestone-plan.md.

## Scope
- Add `IdentityHost`, `MessagingHost` and `RemoteStoreHost` guest traits plus a `Mock*` for each.
- Make the bind macros recognise the three new traits and wire the corresponding bind-macro slices.
- Wire the three seams to the existing stub backends, with no change to backend liveness scope.
- Widen the `Host` supertrait to cover all six interfaces, or document opt-in subset supertraits.

## Done when
- `IdentityHost`, `MessagingHost` and `RemoteStoreHost` guest traits plus their `Mock*` exist and are recognised by the bind macros.
- The three seams are wired to the stub backends.
- The `Host` supertrait covers all six interfaces, or the subset supertraits are documented.
- Modules can host-free unit-test against all three interfaces.
