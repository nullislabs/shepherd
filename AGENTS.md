# AGENTS.md

`CLAUDE.md` is a symlink to this file.

## What shepherd is

shepherd is the CoW Protocol composition root over the nexum WASM Component Model runtime and the videre intent and venue layer.
nexum-runtime supplies the venue-agnostic host: the Component Model runtime, the module supervisor, and the capability tables.
videre-nexum-module supplies the intent and venue platform on top of it.
This repository adds the CoW-specific parts: the `shepherd` engine binary, the CoW venue adapter, the ComposableCoW keeper machinery, and the production keeper modules.
Every module is a wasm32-wasip2 component, and the host grants each one only the capabilities its manifest declares.

## Layout

- `crates/shepherd-engine` builds the `shepherd` binary: the nexum runtime and the videre host wired as the CoW composition root.
- `crates/cow-venue` holds the CoW venue slices, and it is orderbook-only.
  The default `body` slice carries the venue-neutral order intent body types and their borsh codec; the `client`, `assembly`, and `adapter` features layer the typed client, the chain-edge order assembly, and the wasm32-wasip2 venue adapter component on top.
- `crates/composable-cow` holds the ComposableCoW keeper machinery: the conditional-order body, the structured poll `Verdict`, and the `run` composition over the venue client.
- `modules/ccow-monitor` and `modules/ethflow-watcher` are the production keeper modules.
- `tools/orderbook-mock` is the orderbook REST mock for load tests, and `tools/baseline-latency` is the Python latency baseline tooling.
- `wit/shepherd-cow` is this repository's WIT package.
  `wit/deps/` vendors the cross-repo WIT packages, and `wit/deps.toml` pins their sources.
- `extensions.toml` is the client-capability registry that the module world synthesis reads.
  It names the WIT import each `[capabilities]` declaration becomes, so it must stay next to `Cargo.toml`.
- The `engine*.toml` files are engine-side runtime configs, one per scenario.
  `engine.example.toml` is the annotated template, `engine.m2.toml` and `engine.m3.toml` drive the smoke runbooks, `engine.e2e.toml` and `engine.load.toml` drive the e2e and load scenarios, `engine.soak.toml` and `engine.soak.docker.toml` drive the soak run, and `engine.docker.toml` matches the layout the `Dockerfile` bakes.
  A real `engine.toml` carries paid RPC keys and is git-ignored; the committed files are placeholder templates that read secrets from `${VAR_NAME}` environment tokens.

## Dependency pins

This repository was carved out of the runtime monorepo, so the siblings are now external dependencies.
Each crate manifest pins `nexum-*` to a git rev of nullislabs/nexum-runtime and `videre-*` to a git rev of nullislabs/videre-nexum-module.
`wit/deps.toml` pins the same two revs for the vendored WIT packages.
To move to a newer sibling rev, change every occurrence of the old rev together: the crate manifests, `wit/deps.toml`, and the vendored `wit/deps/` copies.
A partial bump splits the graph and breaks the WIT resolve.
`Cargo.toml` also patches `cowprotocol` to a git rev of nullislabs/cow-rs until an upstream release carries the hash-only constructor.

## Build, test, lint

The workspace is edition 2024 on a pinned Rust 1.94 toolchain.
The flake devshell, the CI setup action, and the `Dockerfile` all pin 1.94; bump them in lockstep.
Enter the devshell with `nix develop`, or let `direnv allow` do it.
Every external dependency is hoisted into the `[workspace.dependencies]` table, and the core crates inherit with `dep.workspace = true`.
Guest modules under `modules/` do not inherit that table: they declare their own external dependencies, because a real module author has no access to it.

Use the justfile recipes:

```
just build      # build-modules + build-engine
just test       # cargo nextest run, then cargo test --doc
just fmt        # cargo fmt --all
just lint       # cargo clippy --workspace --all-targets --all-features -- -D warnings
just ci         # the full CI series locally
```

Run tests with `cargo nextest run` and doctests with `cargo test --doc`, because nextest does not run doctests.
Run `just ci` before you push: it mirrors `.github/workflows/ci.yml` one-to-one.
`cargo fmt --all --check` and the clippy `-D warnings` gate are the pre-commit gate, and both are blocking CI jobs.
CI also runs `cargo doc --workspace --no-deps` under `-D warnings` and the blocking `scripts/check-cow-orderbook-only.sh` gate, which holds `crates/cow-venue` to orderbook-only.
`just docker-build` builds the image, and `.github/workflows/docker.yml` publishes it to ghcr.io.

The `.claude/hooks/` scripts support this loop.
`rustfmt-on-edit.sh` formats each edited `.rs` file with rustfmt.
`nextest-on-stop.sh` runs nextest for the crates with uncommitted `.rs` changes at the end of a turn.
Each hook exits without an error when its tool is absent, so nothing runs outside the dev shell.

## House rules

Do not use em-dashes (U+2014) anywhere.
Use ASCII hyphens, a colon, or split the sentence.
`.claude/hooks/content-lint.sh` blocks an edit that adds one to a `.rs` or `.md` file.
Write commit messages as Conventional Commits with an imperative subject.
Disclose AI assistance with an honest `AI Assistance: <tool> used for <what>` line in the commit message and the PR body.
Never add the `Co-Authored-By: Claude Code` or `Generated with Claude Code` boilerplate.
In a PR or issue body, keep one logical line per paragraph.

## Documentation

Write all documentation in ASD-STE100 Simplified Technical English.
Use short sentences, the active voice, and one idea per sentence.
In markdown files, put each sentence on its own line and do not wrap within a sentence; GitHub reflows the file when it displays it.
This keeps a diff to one changed line per changed sentence.
In PR and issue bodies, keep one line per paragraph, because GitHub renders a single newline in a comment as a line break.
