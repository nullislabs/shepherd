# Pre-carve runbook

Go/no-go checklist for the physical three-repo carve:
`nullislabs/nexum-runtime` (L1), `nullislabs/videre-nexum-module` (L2), and
`nullislabs/shepherd` staying as the L3 bundle. `nexum-world` carves with
L1. Every box in sections 1 to 4 is asserted on
the current train tip; section 5 records the de-gate; section 6 is completed
at carve time, on the exact carve base commit, before any `git-filter-repo`
extraction runs. Plan: `docs/design/videre-split-plan.md` (design-notes
branch), section 5 Phase S2 and section 8.

## 1. Cut gate: runtime venue-agnostic

Closed by the M2 stack; blocking in CI as the `venue-agnostic` job.

- [x] Host-layer crate graphs (`nexum-runtime`, `nexum-launch`, `nexum-cli`)
  reach no videre/intent/venue/cow crate (`scripts/check-venue-agnostic.sh`,
  section 1).
- [x] No charter symbol in `crates/nexum-runtime/src`; symbol set aligned in
  PR #449.
- [x] Privileged router field deleted: the venue registry rides the extension
  services map (PR #443); the script refuses any return of
  `pool_router`/`venue_registry` to the runtime.
- [x] `nexum:host` is a leaf WIT package and resolves standalone; the
  host-to-intent decouple landed as opaque status bytes (PR #426).
- [x] Gate lifecycle: advisory in PR #432, blocking with the echo-venue boot
  oracle in PR #450.

Re-assert locally: `just check-venue-agnostic`.

## 2. Cut gate: cow on the generic seam

Closed by the M4 stack; blocking in CI as the `cow-orderbook-only` job.

- [x] A real `cow-venue` cdylib adapter targets `videre:venue/venue-adapter`
  (PR #467; `crates/cow-venue` builds `["lib", "cdylib"]`).
- [x] The composable-cow keeper runs on the typed `videre:venue/client`
  (PR #468); twap-monitor submits through the adapter (PR #469);
  ethflow-watcher observes through the client (PR #470).
- [x] The legacy `cow-api` host cone is retired; no `CowApiHost` symbol
  remains under `crates/` (PR #471).
- [x] The venue is orderbook-only in code: no composable symbol in the
  `crates/cow-venue` sources, enforced by
  `scripts/check-cow-orderbook-only.sh` since the cleave (PR #462).
- [ ] Known trip, clear before sign-off: the scan flags a
  `classification.toml` comment that names ComposableCoW in the
  one-block-grace rationale (PR #473); reword the comment.

Re-assert locally: `just check-cow-orderbook-only`.

## 3. WIT reshape completeness

All planned reshapes landed in the M1 stack; the free-reshape window closes
at the carve.

- [x] Host-to-intent decouple: opaque status bytes (PR #426).
- [x] Videre rename: the intent contract is `videre:{types,venue,value-flow}`
  (PR #428).
- [x] Every package normalized to `@0.1.0` (PR #429).
- [x] `quote` on both venue faces plus the client typestate (PR #431).
- [x] Install-time `body-versions` schema handshake (PR #448).
- [x] Cow event ABIs owned at the bundle layer in `wit/shepherd-cow`
  (PR #463).
- [x] The WIT tree is exactly `nexum-host` (leaf), `videre-types`,
  `videre-value-flow`, `videre-venue`, `shepherd-cow`, and each package
  resolves under `wasm-tools component wit`.

## 4. Videre additive extensibility

A post-cut abstraction fix discovered by the second venue must land as an
additive cross-repo minor, never a wire break.

- [x] Open variants leave room for additive cases: `asset` documents
  erc721/erc1155/service/offchain as 0.2+; `venue-error` and
  `submit-outcome` are variants, not enums.
- [x] New funcs and interfaces are additive on the `videre:venue` faces; the
  provider face mirrors the client face so both grow together.
- [x] `videre:value-flow` stays frozen until the second venue proves the
  abstraction (held under M6).
- [x] Cross-repo pins are exact git tags with checked-in lockfiles, so a fix
  is an additive tag on videre plus a repin in shepherd; no package is
  edited in place across repos after the cut.

## 5. Second venue: explicitly de-gated

Per the 2026-07-15 decision the carve is not gated on a second venue. The
genuine second-protocol venue is post-cut acceptance under milestone M6,
built against the already-split `videre-sdk` via published deps only. The
accepted risk, a wrong videre abstraction surfacing as a cross-repo change,
is bounded by section 4.

## 6. Sign-off (complete at carve time)

- [ ] `venue-agnostic` and `cow-orderbook-only` CI jobs green on the carve
  base commit.
- [ ] Full workspace gate green on the carve base commit (fmt, check,
  clippy `-D warnings`, rustdoc, nextest, doc tests).
- [ ] Carve base commit recorded: `git rev-parse HEAD` = ____
- [ ] Maintainer sign-off recorded; the carve may start.
