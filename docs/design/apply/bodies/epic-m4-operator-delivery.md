Land the operator-facing delivery that ships alongside the repo cut: green CI and CD, multi-chain provider configuration and deployment docs, container image packaging, and the real Swarm remote-store backend.

## Goal
Give an operator everything needed to run the split system in production. That means continuous integration and deployment that stays green (including the sccache fork-PR fail-open fix), a multi-chain provider map with deployment documentation, ghcr container image packaging, and the real Swarm remote-store backend, which rides this epic purely for delivery convenience.

## Scope
The delivery work happens in step with the physical carve so the freshly split repositories ship a runnable, documented product rather than just source. CI and CD are hardened first, closing the sccache fork-PR failure so pull requests from forks no longer break the pipeline. On top of a green pipeline the operator surface is filled in: a provider map that lets a single deployment target multiple chains, deployment documentation, and packaged ghcr images. The Swarm remote-store backend replaces any placeholder store with the real implementation so operators have a working persistence path from day one.

Milestone: M5: The gated three-repo split.
