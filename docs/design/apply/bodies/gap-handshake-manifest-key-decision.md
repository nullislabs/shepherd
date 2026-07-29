Decide the install-time handshake manifest key name (body_version versus a version-set field) and the supported-set match semantics.

## Why
This is one of only two remaining open decisions: the precise manifest key name and the supported-set match semantics for the install-time handshake. The handshake implementation presumes this decision; the schema is videre's while Supervisor::install, which asserts agreement, lives in nexum-runtime and must stay venue-agnostic, so the videre-host install predicate supplies it. Part of milestone M1: Videre contract reshape and host-intent decoupling. See docs/design/videre-split-plan.md and docs/design/issue-milestone-plan.md.

## Scope
- Pin the manifest key name and the module-version-in-adapter-supported-set match semantics (exact-set versus range).
- Note where it slots: module and adapter manifests, asserted at install, fail-fast and logged.

## Done when
- A one-page decision fixes the manifest key name and the supported-set match semantics.
- The install-time handshake implementation references it.
