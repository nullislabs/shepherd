Add `#[videre::keeper]`, the keeper-author mirror of the venue macro, letting authors drive a venue through a typed `VenueClient<V>` instead of hand-writing `list<u8>` marshalling.

## Why
The venue author gets `#[videre::venue]`; the keeper author has no equivalent and must hand-write byte marshalling to drive a venue over `videre:venue/client`. A macro plus a typed, alloy-style client closes that gap and keeps the hot path free of boxing. Part of milestone M3: Videre SDK, macros and DX. Blocked by: #[videre::venue]: single blessed authoring path emitting impl VenueAdapter, videre-quote. See docs/design/videre-split-plan.md and docs/design/issue-milestone-plan.md.

## Scope
- Add `#[videre::keeper]`: the author writes logic against a typed `VenueClient<V>`, and the macro wires the event subscriptions and the `videre:venue/client` import.
- Provide `VenueClient<V>` as an alloy-style typed wrapper exposing quote, submit, status and cancel, with typed bodies via the venue's `IntentBody`, keyed by `VenueId`.
- Use native AFIT for the hot traits under MSRV 1.94 so there is zero boxing on the hot path.
- Prove the path by driving echo-venue from a keeper.

## Done when
- `#[videre::keeper]` emits a worker that drives a venue via a typed `VenueClient<V>` (alloy-style, typed rather than `list<u8>`) wrapping `videre:venue/client` and wiring the event subscriptions.
- A keeper written against `VenueClient<CowVenue>` compiles and calls quote, submit, status and cancel with typed bodies.
- Dispatch is static via native AFIT, with zero boxing on the hot path.
