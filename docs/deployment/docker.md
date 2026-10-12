# Docker deployment

This guide shows how to run `shepherd` from the container image with Docker Compose.
The engine config reference is in [Deploying shepherd](../deployment.md).
The engine-level topics are in the nexum-runtime operator handbook: <https://github.com/nullislabs/nexum-runtime/blob/main/docs/production.md>.

## The image

The `Dockerfile` builds the engine and both modules, and puts these items in the image:

| Path | Content |
|---|---|
| `/usr/local/bin/shepherd` | The engine binary, as a release build. |
| `/opt/shepherd/modules/ccow_monitor.wasm` | The `ccow-monitor` module. |
| `/opt/shepherd/modules/ethflow_watcher.wasm` | The `ethflow-watcher` module. |
| `/opt/shepherd/manifests/ccow-monitor.toml` | The manifest of `ccow-monitor`. |
| `/opt/shepherd/manifests/ethflow-watcher.toml` | The manifest of `ethflow-watcher`. |
| `/var/lib/shepherd` | The state directory. |

The process runs as the non-root user `shepherd`.
The default command is `shepherd --engine-config /etc/shepherd/engine.toml`.
The image contains no engine config, so you must mount one.

`.github/workflows/docker.yml` publishes the image to `ghcr.io/nullislabs/shepherd`, for `linux/amd64` only.

| Tag | Published on |
|---|---|
| `latest` | Each push to `main`. |
| `sha-<short>` | Each published build. Use it to pin an exact build. |
| `v*` | Each push of a `v*` git tag. |

## Run

You need Docker Engine with the Compose plugin, a clone of this repository, and one Ethereum Mainnet RPC endpoint.
The host needs outbound access to that endpoint and to `api.cow.fi`.
It needs no inbound port.

```bash
git clone https://github.com/nullislabs/shepherd && cd shepherd
cp .env.example .env
${EDITOR:-vi} .env          # set MAINNET_RPC_URL
docker compose pull shepherd
docker compose up -d
```

`docker compose pull` gets the published image.
The package is public, so the pull needs no login.
Without `SHEPHERD_IMAGE` in `.env`, Compose uses the `latest` tag, which is the build of the last commit on `main`.
To pin an exact build, set `SHEPHERD_IMAGE` in `.env` to a `sha-<short>` tag or to a `v*` tag.
Use a clone and an image of the same commit, because the clone has `engine.docker.toml` and the image has the module manifests.

To build the image from the clone in place of the pull, use `docker compose build` in place of `docker compose pull shepherd`.
The build gives the image the name in `SHEPHERD_IMAGE`.

Compose mounts `engine.docker.toml` at `/etc/shepherd/engine.toml`, read-only.
That config loads both shipped modules on Ethereum Mainnet and sets the venue to chain 1.
`ccow-monitor` submits each ready order to the production mainnet orderbook.

Compose creates the named volume `shepherd-state` for the state directory.
It publishes the metrics listener on `127.0.0.1:9100` of the host only, because the listener has no authentication.

To use your own engine config, set `SHEPHERD_ENGINE_CONFIG` in `.env` to its path.
Compose forwards only `MAINNET_RPC_URL` to the container.
A config that reads a different variable needs that name under `environment:` in a `docker-compose.override.yml` file.

## Verify

```bash
# Liveness: the answer is "ok".
curl -s http://127.0.0.1:9100/healthz

# Readiness: "ready: true", then one "name: state" line for each module.
curl -s http://127.0.0.1:9100/readyz

# The boot is complete when this line is present.
docker compose logs shepherd | grep '"supervisor ready"'

# Warnings and errors.
docker compose logs shepherd | grep -E '"level":"(ERROR|WARN)"'
```

A correct deployment gives these results:

- `/readyz` answers `ready: true`, `ccow-monitor: alive`, and `ethflow-watcher: alive`.
- The `supervisor ready` line has `"modules":2` and `"alive":2`.
- `docker compose ps` shows the container as `healthy`.

With the committed config, the log has one `loading unverified` warning for each module.
The next section removes them.

## Pin the module digests

`engine.docker.toml` sets `require_component_digest = false`, so the engine does not check the module bytes.
For a production run, pin each module.
Get the pins from the image that you run:

```bash
docker compose run --rm --no-deps shepherd digest /opt/shepherd/modules/ccow_monitor.wasm
docker compose run --rm --no-deps shepherd digest /opt/shepherd/modules/ethflow_watcher.wasm
```

Each command prints one `sha256:<64 hex chars>` line.
The `loading unverified` warning in the engine log gives the same value for each module.
Copy `engine.docker.toml` to `engine.local.toml`, which git ignores.
In the copy, add each line as `digest = "sha256:..."` to its `[[modules]]` entry, and delete the `require_component_digest = false` line.
Set `SHEPHERD_ENGINE_CONFIG=./engine.local.toml` in `.env`, and start the engine again.
The `supervisor ready` line then has `"verified":2`.
A pin belongs to one build, so a different image needs new pins.

## Prometheus

The `observability` profile adds a Prometheus container that scrapes the engine each 15 seconds and keeps 30 days of data.

```bash
docker compose --profile observability up -d
```

The Prometheus UI is at `http://127.0.0.1:9090`.
The job name is `shepherd`.
The scrape config is `docs/deployment/prometheus.yml`, and it loads no alert rules.
The handbook, section 6, has the metric table, and section 7 has the alert rules.
The handbook rule `NexumDown` selects `up{job="nexum"}`, so change that selector to `job="shepherd"` when you use the rules here.

## Stop

```bash
docker compose stop
```

Compose sends SIGINT.
The engine stops dispatch, lets the call in flight complete, saves its cursors, and writes `graceful shutdown complete`.
Compose waits 180 seconds before it kills the container, which is longer than the 150 seconds that the engine allows for the call in flight.

## Upgrade

```bash
git pull
docker compose pull shepherd
docker compose up -d
```

If you pin `SHEPHERD_IMAGE`, set it to the tag of the new commit first.
If you build the image from the clone, use `docker compose build` in place of the pull.

The `shepherd-state` volume stays when Compose replaces the container, so the cursors continue across an upgrade.
`docker compose down -v` deletes the volume.
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

The handbook, section 3, explains what the file contains and how to restore it.
