# Videre reshape fold — runbook + tip oracle (#366)

The recipe #366 executes: replay the whole reshape across the M1 train as one
`git-filter-repo` pass, rebase the stack with jj + mergiraf, and gate on a
byte-identical tip oracle. Validated by a dry-run of the version-normalize slice
(#371) on 2026-07-16.

## Tooling

`git-filter-repo`, `jj` 0.42, `mergiraf` 0.17, `wasm-tools` 1.252 — all run
ephemerally via `nix shell nixpkgs#<pkg>`.

## The harness

Work in an isolated clone; never fold the live checkout.

```sh
git clone --no-local /code/nxm/runtime clone && cd clone   # no shared objects
git filter-repo --replace-text rules.txt --force           # content transform
```

`rules.txt` is package-scoped regex, one rule per transform. Content-based, so
it hits every file type (wit, rs, toml, md, goldens) uniformly.

## The tip oracle

Two rebuild paths must produce a byte-identical tip tree:

- **B (fold):** transform replayed across the train, then `git rev-parse <tip>^{tree}`.
- **A (direct):** the same rules applied once to the original tip tree
  (`git commit-tree <tip-tree>` into an orphan, filter-repo, read its tree).

`A == B` proves the fold is a pure, history-independent blob transform. A
divergence means the fold did not cover every touched blob (see the finding).

## Hazard: scope every rule to the package

A blanket `@0.2.0 -> @0.1.0` corrupts WASI, which carries its own `@0.2.0`
(`wasi:io/streams@0.2.0`, `wasi:sockets/tcp@0.2.0`, ...). Rules MUST bind the
package prefix:

```
regex:(nexum:host[/a-z0-9-]*)@0\.2\.0==>\1@0.1.0
regex:(shepherd:cow[/a-z0-9-]*)@0\.2\.0==>\1@0.1.0
```

Dry-run confirmed: 5 WASI `@0.2.0` refs left intact, nexum/shepherd normalised.

## Finding: base-owned blobs do not ride a train-range fold

Range-limiting the fold to the train (`develop..dev/m1`) leaves 4 `@0.2.0`
survivors: `wit/nexum-host/logging.wit` and `wit/shepherd-cow/{cow-ext,shepherd}.wit`
(plus one ADR). Those files were last touched by base commits (`7ab804a`,
`70ec505`), so their tip blob arrives through the develop side of the tip merge
and is never in the train's blob stream. The oracle caught it (A != B).

Implication for the split:

- **Train-owned reshape folds cleanly** — the `nexum:intent`/`nexum:value-flow`/
  `nexum:adapter` -> `videre:*` rename and the surface thin touch packages
  authored in the train (#247/#248), so every touched blob is in range.
- **Base-owned normalise does not** — `nexum:host` and `shepherd:cow` are L1/base
  packages. Their `@0.2.0 -> @0.1.0` normalise must land as a base commit on
  develop, under the train, not inside the L2 videre fold. R6 (#361) already
  edits `nexum-host/types.wit`, so that file is train-touched and normalises with
  the edit; the untouched host/cow WIT files do not.

Rule: a fold rule only reaches files the fold range owns. Split base-package
normalise (develop) from the train-owned videre reshape (the fold).

## Dry-run result (version-normalize slice)

- Range fold: oracle red (4 base-owned survivors) — correct signal.
- Full-coverage fold: oracle green, 0 nexum/shepherd `@0.2.0` left, WASI intact.
- Harness, oracle, and scope-guard all validated. No push.
