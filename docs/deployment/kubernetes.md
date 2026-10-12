# Kubernetes deployment

This guide shows how to run `shepherd` on a Kubernetes cluster from the published image.
The engine config reference is in [Deploying shepherd](../deployment.md).
The engine-level topics are in the nexum-runtime operator handbook: <https://github.com/nullislabs/nexum-runtime/blob/main/docs/production.md>.

## The manifests

`deploy/kubernetes/` is a Kustomize base with two objects:

| Object | Content |
|---|---|
| StatefulSet `shepherd` | One engine pod, and one 2 GiB volume for the state directory. |
| Service `shepherd` | A headless Service for the metrics listener on port 9100. |

The base does not create these two objects, so you must create them:

| Object | Content |
|---|---|
| ConfigMap `shepherd-config` | The engine config, in the key `engine.toml`. |
| Secret `shepherd-rpc` | The RPC URL, in the key `MAINNET_RPC_URL`. |

The pod runs as the user `shepherd` of the image (user id 999), with a read-only root file system and no capabilities.
It agrees with the `restricted` Pod Security level.
The pod does not call the Kubernetes API, and it needs no service account permission.

Keep one replica.
A second engine with the same modules does the same work a second time.

## Deploy

You need `kubectl`, a clone of this repository, and one Ethereum Mainnet RPC endpoint.
The pod needs outbound access to that endpoint and to `api.cow.fi`.
It needs no inbound access, except from the metrics collector.

```bash
git clone https://github.com/nullislabs/shepherd && cd shepherd
kubectl create namespace shepherd
kubectl -n shepherd create configmap shepherd-config \
  --from-file=engine.toml=engine.docker.toml
kubectl -n shepherd create secret generic shepherd-rpc \
  --from-literal=MAINNET_RPC_URL='wss://<your mainnet endpoint>'
kubectl -n shepherd apply -k deploy/kubernetes
kubectl -n shepherd rollout status statefulset/shepherd
```

`engine.docker.toml` loads both shipped modules on Ethereum Mainnet and sets the venue to chain 1.
`ccow-monitor` submits each ready order to the production mainnet orderbook.

The base uses the `latest` image tag and the default StorageClass of the cluster.
For a real deployment, pin the image and, if necessary, name the StorageClass in an overlay:

```yaml
apiVersion: kustomize.config.k8s.io/v1beta1
kind: Kustomization
namespace: shepherd
resources:
  - https://github.com/nullislabs/shepherd//deploy/kubernetes?ref=<commit>
images:
  - name: ghcr.io/nullislabs/shepherd
    newTag: sha-<short>
patches:
  - target:
      kind: StatefulSet
      name: shepherd
    patch: |-
      - op: add
        path: /spec/volumeClaimTemplates/0/spec/storageClassName
        value: <storage class>
```

Use a base, an image, and an engine config of the same commit, because the image has the module manifests.

## Verify

```bash
# The pod must be "1/1 Running".
kubectl -n shepherd get pod shepherd-0

# Readiness: "ready: true", then one "name: state" line for each module.
kubectl -n shepherd port-forward shepherd-0 9100 &
curl -s http://127.0.0.1:9100/readyz

# Warnings and errors. This command needs jq.
kubectl -n shepherd logs shepherd-0 \
  | jq -R -r 'fromjson? | select(.level == "WARN" or .level == "ERROR") | .message' \
  | sort | uniq -c | sort -rn
```

A `loading unverified` warning for each module is normal with `engine.docker.toml`.
To remove it, pin the module digests as [Docker deployment](docker.md) shows.

## Metrics

The pod has the `prometheus.io/scrape`, `prometheus.io/port`, and `prometheus.io/path` annotations.
A collector that finds its targets by annotation scrapes the pod without more config.
For a different collector, add a scrape target for port 9100 of the pod.

If the cluster denies ingress by default, allow the collector to connect to port 9100 of the pod.
The probes of the kubelet come from the node.

The queries of the [soak runbook](../operations/soak-runbook.md) apply without change.
The engine has no metric for the memory of its process or for the restarts of its pod.
Read these from the metrics of the cluster, for example `container_memory_working_set_bytes` and `kube_pod_container_status_restarts_total`.

## Stop and upgrade

Kubernetes sends `SIGTERM` to the engine, and the engine stops cleanly.
The pod has a termination grace period of 180 seconds, which is longer than the 150 seconds that the engine allows for the call in flight.

The state volume stays when the pod stops or moves, so each event trigger continues from its cursor.
If you delete the PersistentVolumeClaim `state-shepherd-0`, you delete the cursors.

To upgrade, change the image tag and apply the overlay again.
A pod does not read a changed ConfigMap, so restart it after a config change:

```bash
kubectl -n shepherd rollout restart statefulset/shepherd
```

Before an upgrade, read the handbook, sections 10 and 11.

## What is tested

- The base was run on a local `kind` cluster (Kubernetes 1.36) in a namespace that enforces the `restricted` Pod Security level, with an Ethereum Mainnet node.
  Both modules became `alive`, and the health and metrics endpoints answered.
- After a pod deletion, the engine wrote `graceful shutdown complete`, and the new pod opened each event trigger at its cursor.
- That cluster had no network policy and a local-path volume.
  A network volume and a default-deny network policy were not tested there.
