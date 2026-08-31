# shepherd

The CoW Protocol composition root over the [nexum](https://github.com/nullislabs/nexum-runtime) WASM Component Model runtime and the [videre](https://github.com/nullislabs/videre-nexum-module) intent/venue layer: the `shepherd` engine binary, the CoW venue adapter, the ComposableCoW keeper machinery, and the production keeper modules.

## Layout

- `crates/shepherd-engine` - the `shepherd` engine binary: nexum runtime + videre host wired as the cow composition root.
- `crates/cow-venue` - CoW venue slices, orderbook-only. Default `body` slice carries the venue-neutral order intent body types; the `venue` feature carries the native venue the composition root registers.
- `crates/composable-cow` - ComposableCoW keeper machinery: the conditional-order body, the structured poll Verdict, and the run composition over the venue client.
- `modules/twap-monitor`, `modules/ethflow-watcher` - the production keeper modules (wasm32-wasip2 components).
- `tools/orderbook-mock` - orderbook REST mock for load tests.
- `tools/baseline-latency` - latency baseline capture/analysis (Python).
- `wit/shepherd-cow` - this repo's WIT package; `wit/deps/` vendors the cross-repo packages (sources pinned in `wit/deps.toml`).

## Building

Use the flake devshell (`nix develop`, or `direnv allow`), then:

```
just build        # engine + module wasms + cow venue adapter
just test         # nextest + doctests
just ci           # the full CI series locally
```

Cross-repo dependencies are pinned by git rev in the crate manifests: `nexum-*` to [nexum-runtime](https://github.com/nullislabs/nexum-runtime) and `videre-*` to [videre-nexum-module](https://github.com/nullislabs/videre-nexum-module). `extensions.toml` at the repo root is the client-capability registry the module world synthesis reads; keep it next to `Cargo.toml`.

## Scenarios

- `just run-m2` - M2 smoke / round-trip on Sepolia (both keeper modules + the cow venue adapter).
- `just e2e` / `just load` / `scripts/soak-snapshot.sh` - the managed e2e / load / soak drivers (see `scripts/README.md`).
- Docker: `docker compose up` with `engine.docker.toml`; the soak profile is `docker-compose.soak.yml`.

Note: the e2e and soak scenario configs also reference the nexum example modules (`price-alert`, `balance-tracker`), which live in the nexum-runtime repo since the carve. Build their wasms from a sibling checkout at `../nexum-runtime` into this repo's `target/` first; the same applies to the `load-gen` driver used by `scripts/load-run.sh`.

## History

This repository was carved out of the `nullislabs` runtime monorepo with full history (`git filter-repo`); the pre-carve state is preserved under the pre-carve tag.

## Licence

AGPL-3.0. See `LICENSE`.
