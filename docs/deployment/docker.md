# Docker deployment

This guide shows how to run `shepherd` from the container image with Docker Compose.
The engine config reference is in [Deploying shepherd](../deployment.md).
The chain layout is in [Multi-chain deployment](multi-chain.md).
The engine-level topics are in the nexum-runtime operator handbook: <https://github.com/nullislabs/nexum-runtime/blob/main/docs/production.md>.

## The image

`.github/workflows/docker.yml` builds the image and publishes it to `ghcr.io/nullislabs/shepherd`.

| Tag | Published on |
|---|---|
| `latest` | Each push to `main`. |
| `sha-<short>` | Each published build. Use it to pin an exact build. |
| `v*` | Each push of a `v*` git tag. The image tag is the git tag. |
| `manual-<run id>` | Each manual run of the workflow. |

The image is for `linux/amd64` only.

The `Dockerfile` puts these items in the image:

| Path | Content |
|---|---|
| `/usr/local/bin/shepherd` | The engine binary, as a release build. |
| `/opt/shepherd/modules/ccow_monitor.wasm` | The `ccow-monitor` module. |
| `/opt/shepherd/modules/ethflow_watcher.wasm` | The `ethflow-watcher` module. |
| `/opt/shepherd/manifests/ccow-monitor.toml` | The manifest of `ccow-monitor`, copied from `modules/ccow-monitor/component.toml`. |
| `/opt/shepherd/manifests/ethflow-watcher.toml` | The manifest of `ethflow-watcher`, copied from `modules/ethflow-watcher/component.toml`. |
| `/var/lib/shepherd` | The state directory. The `shepherd` user owns it. |
| `/etc/shepherd` | The directory where you mount the engine config. It is empty in the image. |

The base is `debian:bookworm-slim`.
`tini` is PID 1, and it forwards signals to the engine.
The process runs as the non-root user `shepherd`.
The default command is `shepherd --engine-config /etc/shepherd/engine.toml`.
The image contains no engine config, so you must mount one.

## Minimum configuration for a fresh server

The server needs Docker Engine with the Compose plugin, on `linux/amd64`.
It needs outbound network access to your RPC endpoints and to `api.cow.fi`.
It needs no inbound port.

You supply three things.

1. The image reference.
   The default is `ghcr.io/nullislabs/shepherd:latest`.
   To pin a build, set `SHEPHERD_IMAGE=ghcr.io/nullislabs/shepherd:sha-<short>` in `.env`.
2. Three files from this repository, in one directory:

   | File | Purpose |
   |---|---|
   | `docker-compose.yml` | The service definition. |
   | `engine.docker.toml` | The engine config. Compose mounts it at `/etc/shepherd/engine.toml`, read-only. |
   | `.env.example`, saved as `.env` | The environment variables that Compose forwards to the container. |

3. Five environment variables in `.env`, each with the RPC URL of one chain:

   | Variable | Chain |
   |---|---|
   | `MAINNET_RPC_URL` | 1 (Ethereum Mainnet) |
   | `GNOSIS_RPC_URL` | 100 (Gnosis Chain) |
   | `SEPOLIA_RPC_URL` | 11155111 (Sepolia) |
   | `ARBITRUM_RPC_URL` | 42161 (Arbitrum One) |
   | `BASE_RPC_URL` | 8453 (Base) |

`engine.docker.toml` has one `[chains.<id>]` table for each of these variables.
The engine stops at boot if one of the five is not set, and the error gives the name.
Use paid endpoints: public endpoints limit `eth_subscribe` and `eth_getLogs`.

You do not need a clone of the repository.
These commands get the three files and start the engine:

```bash
mkdir shepherd && cd shepherd
base=https://raw.githubusercontent.com/nullislabs/shepherd/main
curl -fsSLO "$base/docker-compose.yml"
curl -fsSLO "$base/engine.docker.toml"
curl -fsSL -o .env "$base/.env.example"
${EDITOR:-vi} .env

docker compose pull shepherd
docker compose up -d --no-build
docker compose logs -f shepherd
```

`--no-build` makes Compose use the image that you pulled.
Without it, Compose tries to build from a `Dockerfile` when the image is not present.

Compose creates the named volume `shepherd-state` for the state directory.
It publishes the metrics listener on `127.0.0.1:9100` of the host.

### What the default config runs

`engine.docker.toml` loads both shipped modules and sets the venue to chain 1.

- `ccow-monitor` reads Ethereum Mainnet and submits ready orders to the mainnet CoW orderbook.
- `ethflow-watcher` reads Sepolia, as its shipped manifest says.
  The venue asks the mainnet orderbook for the status of each order, and that orderbook does not hold Sepolia orders.

[Multi-chain deployment](multi-chain.md) explains this layout and shows how to make each engine consistent.

### Fewer RPC endpoints

The two shipped modules use chains 1 and 11155111 only.
To supply only those two endpoints, use your own copy of the config:

```bash
cp engine.docker.toml engine.local.toml
${EDITOR:-vi} engine.local.toml   # delete the [chains.100], [chains.42161] and [chains.8453] tables
echo 'SHEPHERD_ENGINE_CONFIG=./engine.local.toml' >> .env
docker compose up -d --no-build
```

`SHEPHERD_ENGINE_CONFIG` is the host path that Compose mounts in place of `engine.docker.toml`.
Git ignores `*.local.toml` files.

## Verify the deployment

```bash
# Liveness: the answer is "ok".
curl -s http://127.0.0.1:9100/healthz

# Readiness: "ready: true", then one "name: state" line for each module.
curl -s http://127.0.0.1:9100/readyz

# One series for each module must have the state "alive" and the value 1.
curl -s http://127.0.0.1:9100/metrics | grep '^nexum_runtime_module_state'

# The boot is complete when this line is present.
docker compose logs shepherd | grep '"supervisor ready"'

# Warnings and errors.
docker compose logs shepherd | grep -E '"level":"(ERROR|WARN)"'

# The local store.
docker compose exec shepherd ls -la /var/lib/shepherd/
```

A correct deployment gives these results:

- `/readyz` answers `ready: true`, `ccow-monitor: alive`, and `ethflow-watcher: alive`.
- The `supervisor ready` line has `"modules":2` and `"alive":2`.
- `/var/lib/shepherd/` contains `local-store.redb`.
- `docker compose ps` shows the container as `healthy`.

With the default config, the log has two `loading unverified` warnings, one for each module.
"Pin the module digests" below removes them.

The series `nexum_runtime_module_poisoned` appears only after a module is quarantined.
Do not use its absence at boot as a test.

## Engine config in the container

Compose mounts the host file `${SHEPHERD_ENGINE_CONFIG:-./engine.docker.toml}` at `/etc/shepherd/engine.toml`.
The engine replaces each `${NAME}` token in that file with the value of the environment variable `NAME`.
Compose forwards only the five RPC variables of the table above.
A config that uses a different variable name stops at boot, unless you also add the name to `environment:` in a Compose override file.

`engine.docker.toml` sets `bind_addr = "0.0.0.0:9100"`.
The handbook tells you to bind the loopback address.
The container is the exception: the address `0.0.0.0` is inside the container network namespace, and Compose publishes the port only on the loopback address of the host.
Do not change the port mapping to `9100:9100` unless a proxy with authentication is in front of it.

### Pin the module digests

`engine.docker.toml` sets `require_component_digest = false`, so the engine does not check the module bytes.
For a production run, pin each module.
Get the pins from the image that you run:

```bash
image=ghcr.io/nullislabs/shepherd:sha-<short>
docker run --rm "$image" digest /opt/shepherd/modules/ccow_monitor.wasm
docker run --rm "$image" digest /opt/shepherd/modules/ethflow_watcher.wasm
```

Each command prints one `sha256:<64 hex chars>` line.
In your copy of the config, add that line as `digest = "sha256:..."` to the related `[[modules]]` entry, and delete the `require_component_digest = false` line.
Set `SHEPHERD_IMAGE` to the same `sha-<short>` tag.
A pin belongs to one build, so a different image needs new pins.
After the change, the `supervisor ready` line has `"verified":2`.

## Observability profile

The `observability` profile adds a Prometheus container that scrapes the engine.

```bash
docker compose --profile observability up -d --no-build
```

This profile needs one more file from the repository, at the same relative path: `docs/deployment/prometheus.yml`.
Compose mounts it into the `prometheus` service.
The Prometheus UI is at `http://127.0.0.1:9090`.
Prometheus scrapes `shepherd:9100` over the Compose network, with the job name `shepherd`.

The file loads no alert rules.
The handbook, section 7, has the rules.
The handbook rule `NexumDown` selects `up{job="nexum"}`, so change that selector to `job="shepherd"` when you use the rules with this profile.
The handbook, section 6, has the metric table.
Every metric name starts with `nexum_runtime_`.

## Stop and shutdown

`docker-compose.yml` sets `stop_signal: SIGINT` and `stop_grace_period: 30s`.
On SIGINT, the engine stops dispatch, lets the one call in flight complete, saves its cursor, and exits with code 0.

Docker sends SIGKILL 30 seconds after SIGINT.
The engine allows a longer time for the call in flight: the `supervisor ready` line gives it as `shutdown_drain_secs`, and the default is 150 seconds.
If a call in flight takes more than 30 seconds, Docker kills the engine before that call completes.
The cursor then stays at the last dispatch that was saved, and a trigger with `resume = true` reads the missed logs again at the next start.

To give the engine its full time, put a larger value in a `docker-compose.override.yml` file beside `docker-compose.yml`:

```yaml
services:
  shepherd:
    stop_grace_period: 180s
```

Compose reads that file automatically.
The handbook, section 2, explains the shutdown sequence.

## Upgrade and rollback

```bash
# Move to the newest build of main.
docker compose pull shepherd && docker compose up -d --no-build
```

To pin a build or to go back to an earlier one, set the tag in `.env` and start again:

```bash
# In .env:
#   SHEPHERD_IMAGE=ghcr.io/nullislabs/shepherd:sha-<short>
docker compose pull shepherd && docker compose up -d --no-build
```

The `shepherd-state` volume stays when Compose replaces the container, so the local store and its cursors continue across an upgrade.
`docker compose down -v` deletes the volume.
Do not use `-v` unless you want an empty state.

Before an upgrade, read the handbook, sections 10 and 11, and make a backup of the state.

## State backup

The state is one file, `/var/lib/shepherd/local-store.redb`, on the `shepherd-state` volume.
Stop the engine before you copy it:

```bash
docker compose stop shepherd
docker compose cp shepherd:/var/lib/shepherd/local-store.redb \
  "./local-store-$(date -u +%Y%m%dT%H%M%SZ).redb"
docker compose start shepherd
```

To restore a snapshot, copy it back and make the `shepherd` user its owner:

```bash
docker compose stop shepherd
docker compose run --rm --no-deps --user root --entrypoint sh \
  -v "$PWD/snapshot.redb:/restore/local-store.redb:ro" shepherd \
  -c 'cp /restore/local-store.redb /var/lib/shepherd/local-store.redb \
      && chown shepherd:shepherd /var/lib/shepherd/local-store.redb'
docker compose start shepherd
```

The handbook, section 3, explains what the file contains and when a copy is safe.

## Build the image locally

Use a local build to test a change that is not in a published image.
The build compiles the engine and both modules, so it needs a clone of the repository and some time.

```bash
git clone https://github.com/nullislabs/shepherd && cd shepherd
cp .env.example .env
${EDITOR:-vi} .env                 # set the RPC URLs and SHEPHERD_IMAGE=shepherd:local
docker compose build shepherd
docker compose up -d
```

`docker compose build` gives the image the name in `SHEPHERD_IMAGE`.
`just docker-build` builds the same `Dockerfile` with the tag `ghcr.io/nullislabs/shepherd:dev`.

## See also

- [Deploying shepherd](../deployment.md): the engine config reference and the troubleshooting table.
- [Multi-chain deployment](multi-chain.md): chains, the venue, and manifest overrides.
- [Soak runbook](../operations/soak-runbook.md): the 7-day run with `docker-compose.soak.yml`.
- The nexum-runtime operator handbook: <https://github.com/nullislabs/nexum-runtime/blob/main/docs/production.md>.
