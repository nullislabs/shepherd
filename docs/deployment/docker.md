# Docker deployment

Running Shepherd from the published container image. The hardening surface (systemd, backup, RPC selection, alerting) is in [`docs/production.md`](../production.md); the `engine.toml` reference is in [`docs/deployment.md`](../deployment.md).

The image is published to `ghcr.io/nullislabs/shepherd` on every push to `main` and every `v*` tag:

```
ghcr.io/nullislabs/shepherd:latest         # main HEAD
ghcr.io/nullislabs/shepherd:sha-<short>    # exact-build pin
ghcr.io/nullislabs/shepherd:v0.2.0         # release tag
```

The image is a multi-stage build: a `debian:bookworm-slim` runtime, `tini` as PID 1 (forwards SIGINT/SIGTERM), a non-root `shepherd` user, the `shepherd` binary, and the four production modules plus the `cow-venue` adapter baked under `/opt/shepherd/`. `linux/amd64` only.

## 1. First boot

```bash
git clone https://github.com/nullislabs/shepherd /opt/shepherd
cd /opt/shepherd

# Operator RPC URLs. `.env` is gitignored; `.env.example` lists every
# variable the engine substitutes into engine.docker.toml via ${VAR}.
cp .env.example .env
${EDITOR:-vi} .env

docker compose pull
docker compose up -d
docker compose logs -f shepherd
```

The observability profile adds Prometheus on the same host:

```bash
docker compose --profile observability up -d   # Prometheus UI: http://127.0.0.1:9090
```

The metrics endpoint binds the host loopback (`127.0.0.1:9100`); the Prometheus container scrapes via the compose DNS name `shepherd:9100`. Never expose `:9100` publicly without authn (see [`docs/production.md`](../production.md)).

## 2. Configuring the engine

The image bind-mounts the committed `engine.docker.toml` at `/etc/shepherd/engine.toml` read-only. It uses `${VAR}` placeholders for every RPC URL, substituted at load time from the environment; a missing variable fails the boot with the exact name. To run a custom config, set `SHEPHERD_ENGINE_CONFIG=./engine.local.toml` in `.env` (the `*.local.toml` pattern is gitignored).

Inside the container the metrics exporter binds `0.0.0.0:9100` so the compose port mapping reaches it; the mapping keeps it on the host loopback. Public RPCs throttle `eth_subscribe` and `eth_getLogs` under load, so use paid endpoints.

## 3. Upgrade and rollback

```bash
# Roll forward to the latest main build. Graceful shutdown drains the
# in-flight dispatch before the new container takes over.
docker compose pull && docker compose up -d

# Pin a specific build.
export SHEPHERD_IMAGE=ghcr.io/nullislabs/shepherd:sha-abc1234
docker compose up -d
```

The `shepherd-state` named volume survives container recreation, so the redb file and its idempotency markers persist across upgrades.

## 4. Building locally

For testing unmerged changes:

```bash
docker compose build              # repo-root Dockerfile
export SHEPHERD_IMAGE=shepherd:local
docker build -t "$SHEPHERD_IMAGE" .
docker compose up -d
```

## 5. Verifying

```bash
# Engine up, no module or adapter quarantined.
curl -s http://127.0.0.1:9100/metrics \
  | grep -E '^shepherd_(module_poisoned|adapter_poisoned|stream_reconnects_total)'

docker compose logs -f shepherd | grep -E '"level":("ERROR"|"WARN")'
docker compose exec shepherd ls -la /var/lib/shepherd/
```

Green: poisoned gauges `0`, no ERROR/WARN beyond boot, and a non-empty `local-store.redb` under `/var/lib/shepherd/`.

## 6. See also

- [`docs/production.md`](../production.md): systemd, backup, RPC selection, alerting.
- [`docs/06-production-hardening.md`](../06-production-hardening.md): resource-limit design, restart policy, RPC resilience.
- `engine.example.toml`: annotated engine config reference.
