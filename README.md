# shepherd

The CoW Protocol composition root over the [nexum](https://github.com/nxm-rs/nexum-runtime) WASM Component Model runtime and the [videre](https://github.com/nxm-rs/videre) intent/venue layer: the `shepherd` engine binary, the CoW venue adapter, the ComposableCoW keeper machinery, and the production keeper modules.

## Layout

- `crates/shepherd-engine` - the `shepherd` engine binary: nexum runtime + videre host wired as the cow composition root.
- `crates/cow-venue` - CoW venue slices, orderbook-only. Default `body` slice carries the venue-neutral order intent body types; the `venue` feature carries the native venue the composition root registers.
- `crates/composable-cow` - ComposableCoW keeper machinery: the conditional-order body, the structured poll Verdict, and the run composition over the venue client.
- `modules/ccow-monitor`, `modules/ethflow-watcher` - the production keeper modules (wasm32-wasip2 components).
- `tools/orderbook-mock` - orderbook REST mock that the forked-mainnet engine test drives.
- `tools/baseline-latency` - latency baseline capture/analysis (Python).
- `wit/deps/` - the vendored cross-repo WIT packages (sources named in `wit/deps.toml`). This repo has no WIT package of its own.
- `deploy/kubernetes` - a Kustomize base that runs the engine on Kubernetes.
- `docs/` - the operator documentation (see "Documentation" below).

## Building

Use the flake devshell (`nix develop`, or `direnv allow`), then:

```
just build        # engine + module wasms
just test         # nextest + doctests
just ci           # the full CI series locally
```

Cross-repo dependencies are pinned by git rev in the crate manifests: `nexum-*` to [nexum-runtime](https://github.com/nxm-rs/nexum-runtime) and `videre-*` to [videre](https://github.com/nxm-rs/videre). `extensions.toml` at the repo root is the client-capability registry the module world synthesis reads; keep it next to `Cargo.toml`.

## Running

- From source: copy `engine.example.toml` to `engine.toml`, then `shepherd --engine-config engine.toml` (see [docs/deployment.md](docs/deployment.md)).
- Docker: `docker compose up` with `engine.docker.toml` (see [docs/deployment/docker.md](docs/deployment/docker.md)).

## Documentation

Operator documentation is in this repository:

- [Deploying shepherd](docs/deployment.md): the engine config, the shipped modules, and the troubleshooting table.
- [Docker deployment](docs/deployment/docker.md): the image, Docker Compose, Prometheus, upgrade, and backup.
- [Kubernetes deployment](docs/deployment/kubernetes.md): the Kustomize base, the pod security settings, metrics, and upgrade.
- [Multi-chain deployment](docs/deployment.md#multi-chain-deployment): the two patterns, the supported chains, and what is tested.
- [Soak runbook](docs/operations/soak-runbook.md): a run of many days, the Prometheus queries, and the evidence.

Engine and module documentation is in [nexum-runtime](https://github.com/nxm-rs/nexum-runtime):

- [Operator handbook](https://github.com/nxm-rs/nexum-runtime/blob/main/docs/production.md): metrics, alerts, RPC selection, and the runbook.
- [Module tutorial](https://github.com/nxm-rs/nexum-runtime/blob/main/docs/tutorial-first-module.md): write, build, and run a first module.
- [SDK guide](https://github.com/nxm-rs/nexum-runtime/blob/main/docs/sdk.md): the SDK crates and the API reference.
- [Testing guide](https://github.com/nxm-rs/nexum-runtime/blob/main/docs/testing.md): test a module without a chain.
- [Manifest reference](https://github.com/nxm-rs/nexum-runtime/blob/main/docs/02-modules-triggers-packaging.md): triggers, dependencies, and packaging.

## History

This repository was carved out of the `nullislabs` runtime monorepo with full history (`git filter-repo`); the pre-carve state is preserved under the pre-carve tag.

## Licence

AGPL-3.0-or-later. See `LICENSE`.
