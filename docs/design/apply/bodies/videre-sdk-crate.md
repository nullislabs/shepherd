Rename the venue-author SDK to videre-sdk and add its missing keeper sweep assembler and typed venue client. The persona crate is shipped as nexum-venue-sdk/nexum-venue-test but is mis-named for the split and lacks its assembler.

## Why
The venue-author SDK exists but is mis-named and incomplete: it has no generic sweep assembler, so keeper authors have nothing to assemble a sweep from. Renaming to videre-sdk and giving it the assembler lets a keeper compile against one crate with no cow or host dependency. Part of milestone M3: Videre SDK, macros and DX. Blocked by: videre-wit-surface, host-extension-seam-roles. See docs/design/videre-split-plan.md and docs/design/issue-milestone-plan.md.

## Scope
- Rename nexum-venue-sdk to videre-sdk; own `VenueAdapter`, the `IntentBody` codec plus `BodyError`, `IntentClient<P>` and `VenueId`.
- Add the generic `Keeper::sweep` assembler wiring `WatchSet` to `Gates` to `source.poll` to `Retrier` to `Journal`, plus a shared `Sweep` outcome that resolves the dangling `ConditionalSource::Outcome`.
- Keep the world-neutral keeper primitives (`WatchSet`, `Gates`, `Journal`, `Retrier`, `ConditionalSource`) in nexum-sdk.
- Apply DX polish: `VenueFault` with `Display` and `IntoStaticStr`, `#[non_exhaustive]`, and sealed extension traits.

## Done when
- nexum-venue-sdk is renamed to videre-sdk.
- The crate exports `VenueAdapter`, `IntentBody`, `IntentClient<P>`, `VenueId` and a generic `Keeper::sweep` assembler over a `Sweep` outcome that resolves the dangling `ConditionalSource::Outcome`.
- The world-neutral keeper primitives stay in nexum-sdk.
- A keeper compiles against videre-sdk alone with no cow or host dependency.
