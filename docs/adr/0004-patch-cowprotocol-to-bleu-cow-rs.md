---
status: accepted
---

# Patch the `cowprotocol` crate to a maintained fork

## Context

The workspace needs `cowprotocol` changes ahead of any published release: the `OrderCreationAppData` hash-only submission shape (`OrderCreation::new_app_data_hash_only`, watch-tower parity for conditional-order submission) and a WASI clock fix that keeps `js_sys` out of non-browser wasm builds. The latest crates.io release (`0.2.0-alpha.1`) carries neither.

## Decision

A single workspace-level `[patch.crates-io]` redirects `cowprotocol` to `https://github.com/nullislabs/cow-rs` at a pinned rev. Every crate declaring `cowprotocol` picks up the patched build with no change at the dependent site. Bumping the fork is a one-line rev edit.

Rather than vendor the missing types locally (reuse over reimplement) or pin each dependent to a git source, the `[patch.crates-io]` override centralises the redirect.

## Consequences

- `cargo update` re-resolves to the same rev; the lock pins it.
- Drop the patch once a published `cowprotocol` release carries the hash-only constructor. The comment above `[patch.crates-io]` in the root `Cargo.toml` states the current drop condition.
- Modules built standalone against crates.io see the unpatched release and may hit the bugs the patch closes.
