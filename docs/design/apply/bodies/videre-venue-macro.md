Make `#[videre::venue]` the single blessed venue authoring path, fixed to emit `impl VenueAdapter`. Two authoring paths currently fork the one arrangement.

## Why
Today `#[venue]` emits `impl Guest` over raw bindgen and bypasses the typed `VenueAdapter` trait, while `export_venue_adapter!` routes through it on a differently-named world that imports chain and messaging unconditionally. Two paths for one job is a fork; collapsing to one blessed macro that always emits `impl VenueAdapter` removes the ambiguity and the hand-copied bridge boilerplate. Part of milestone M3: Videre SDK, macros and DX. Blocked by: videre-sdk: rename nexum-venue-sdk + add Keeper::sweep assembler + VenueClient. See docs/design/videre-split-plan.md and docs/design/issue-milestone-plan.md.

## Scope
- Fix `#[videre::venue]` to emit `impl VenueAdapter`, the `videre:venue/adapter` export and the manifest kind, not a raw `Guest` impl.
- Demote `export_venue_adapter!` to the internal codegen the macro expands to, so there is no public second path.
- Narrow imports by construction via the synthesized venue world, rather than dead-import elision.
- Remove the roughly 80-line `*_to_golden` bridges; port echo-venue to the macro.
- Land the macro in videre-macros after the macro split.

## Done when
- `#[videre::venue]` emits `impl VenueAdapter` plus the `videre:venue/adapter` export and the manifest kind, not a raw `Guest` impl.
- `export_venue_adapter!` is demoted to the internal codegen the macro expands to, with no public second path.
- Import narrowing is by construction, with no dead-import elision.
- echo-venue uses the macro and the `*_to_golden` bridge boilerplate is gone.
