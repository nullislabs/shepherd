Land the alloy-grade DX polish cluster as one umbrella: a typed venue fault mirror, a typestate order builder, uniform `#[non_exhaustive]`, sealed extension traits, single-source vocabularies, and removal of the hand-copied golden bridges.

## Why
Several DX-polish items have no owner: operator logs still `{0:?}`-format the venue error and the `rate-limited{retry-after-ms}` detail does not survive the fold; the bare 12-field order body literal is error-prone; the fault vocabulary is hand-mirrored in three places and the known table is duplicated; and each venue adapter hand-copies roughly 80 lines of golden bridge boilerplate. Consolidating them removes the last of the copy-paste and makes the surface alloy-grade. Part of milestone M3: Videre SDK, macros and DX. See docs/design/videre-split-plan.md and docs/design/issue-milestone-plan.md.

## Scope
- Mirror `VenueError` to a `VenueFault` with `Display`, an `IntoStaticStr` label and `From<bindings>`, preserving `retry-after-ms`.
- Add an `Order` typestate builder to replace the bare 12-field `OrderBody` literal, plus CoW `SellToken` and `BuyToken` newtypes.
- Apply `#[non_exhaustive]` uniformly across the public error and label enums.
- Seal the extension traits (`Host`, `HostFault`, `RuntimeTypes`, `Runtime`, `IntentPool`) with a private `Sealed` supertrait.
- Derive the mirrored vocabularies from single-source consts, so the fault vocabulary and known table are emitted from one place.
- Remove the roughly 80-line `*_to_golden` bridge boilerplate.

## Done when
- `VenueError` is mirrored to a `Display` and `IntoStaticStr` `VenueFault` that preserves `retry-after-ms`.
- The `Order` typestate builder and the sell and buy token newtypes exist.
- `#[non_exhaustive]` is uniform across the public error and label enums.
- The extension traits are sealed.
- The fault vocabulary and known table are emitted from single-source consts.
- The `*_to_golden` bridges are removed.
