# Body-version handshake — decision (#373)

Fixes the manifest key and match semantics for the install-time body-schema
handshake. The M2 enforcement wiring references this. Companion to
`videre-wit-pinned-0.1.0.md` (which reserves `adapter.body-versions()`).

## Problem

A keeper serialises an order into an opaque `body: list<u8>` at one schema
version; the venue adapter decodes it. They ship as separate components an
operator upgrades independently, so a keeper built at schema v2 against an
adapter that only decodes v1 fails obscurely at runtime. The handshake refuses
that pair at boot instead.

## Decision

Manifest key: a videre-scoped `[venue]` section, snake_case fields.

```toml
# keeper module.toml            # adapter module.toml
[module]                        [module]
kind = "event-module"           kind = "venue-adapter"
[venue]                         [venue]
body_version = 2                body_versions = [1, 2]
```

- The keeper declares **one** `body_version` (it encodes exactly one layout).
- The adapter declares the **set** `body_versions` it can still decode.
- Install succeeds iff `keeper.body_version` is in `adapter.body_versions`.
  Otherwise `Supervisor::install` refuses the pair, fail-fast and logged.

## Where it lives and who checks

- `nexum-runtime` stays venue-agnostic: it parses `[venue]` opaquely and routes
  it to the extension. It ascribes no meaning to `body_version`.
- `videre-host` supplies the install predicate through the generalized
  `Extension` seam and enforces the membership check.
- The adapter **manifest** `[venue] body_versions` is the install-time
  authority: the check is static, before instantiation, so a mismatch fails
  before any boot cost. The WIT `adapter.body-versions() -> list<u32>` export
  stays for runtime introspection; the venue macro emits both from the same
  `IntentBody` version, so they cannot drift.

## SDK / macros own the plumbing

The body schema is one typed `IntentBody` (with a version constant) in a shared
per-venue crate both sides depend on.

- `#[videre::venue]` emits the decoder, `body-versions()`, and the adapter
  `[venue] body_versions` from that `IntentBody`.
- `#[videre::keeper]` + `VenueClient<V>` take typed bodies; the macro marshals to
  `list<u8>` and stamps the keeper `[venue] body_version` from the same constant.

An author writes typed Rust, never a byte or a version number. Bumping the
shared `IntentBody` moves the encode path and the manifest version together; the
handshake only fires on an independent-upgrade mismatch.

## Scope

M1 decision only. The `Supervisor::install` predicate, the `[venue]` parse, and
the mismatched-pair test are M2.
