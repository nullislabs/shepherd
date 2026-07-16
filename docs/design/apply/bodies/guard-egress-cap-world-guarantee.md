Bring venue http egress under the compile-time synthesised-world guarantee and canonicalise a single adapter import-narrowing contract, retiring the blanket shim path.

## Why
Two coupled gaps let egress escape the capability model. First, http escapes the compile-time guarantee: in the KNOWN table http has `import: None` (`world.rs:88-92`), so `wasi:http` is linked out-of-band and gated only by the `engine.toml` allowlist; the undeclared-capability-is-a-compile-error guarantee therefore covers chain, messaging, and logging but not http. Second, there are two adapter contracts: `export_venue_adapter!` imports chain and messaging unconditionally and leans on `wasm-tools` dead-import elision, while `synthesize_venue` narrows by construction. Either bring http under the synthesised-world guarantee or document loudly at the seam that http egress is allowlist-gated, and settle on one import-narrowing contract. Part of milestone M7: Egress guard. Blocked by: egress-guard-hardening-epic. See docs/design/videre-split-plan.md and docs/design/issue-milestone-plan.md.

## Scope
- Bring http under the synthesised-world guarantee, or document at the seam that http egress is allowlist-gated only.
- Canonicalise one adapter contract that narrows imports by construction.
- Retire the blanket shim path that imports chain and messaging unconditionally and relies on dead-import elision.

## Done when
- Undeclared http egress is a build error, or the allowlist-only story is documented at the seam.
- Exactly one adapter contract exists, with import narrowing by construction.
