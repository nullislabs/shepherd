Add an install-time handshake so a keeper and its adapter agree on a body schema version before booting together. Bodies are opaque `list<u8>` with a guest-side borsh version tag, so schema agreement is never a checked property today.

## Why
Because schema agreement is never checked, a keeper and adapter can install with mismatched body encodings and fail obscurely at runtime. An install-time capability handshake catches the mismatch at boot instead: the venue host contributes an install predicate through the generalized `Extension` seam, and the supervisor refuses any pair whose supported versions do not intersect. This is chosen over freezing the WIT surface. Part of milestone M2: Generic venue-agnostic host. Blocked by: videre-wit-surface; Grow the Extension seam to carry worker/provider roles. See docs/design/videre-split-plan.md and docs/design/issue-milestone-plan.md.

## Scope
- Wire `videre:venue/adapter.body-versions() -> list<u32>` (reserved in the surface pin) into the adapter interface.
- Add a `body_version` field to the module and adapter `module.toml` manifests.
- Have the venue host contribute an install predicate via the generalized `Extension` seam.
- Make `Supervisor::install` assert version intersection and refuse mismatched pairs, failing fast with a logged error.

## Done when
- `videre:venue/adapter.body-versions() -> list<u32>` exists.
- The module and adapter manifests declare a supported `body_version` or set.
- `Supervisor::install` refuses to boot a keeper/adapter pair whose versions do not intersect, failing fast with a logged error.
- A mismatched-pair test asserts the refusal.
