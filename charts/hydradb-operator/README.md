# hydradb-operator

HydraDB Kubernetes operator (kube-rs) reconciling GraphCluster, GraphNodePool, and GraphIndexer resources

The operator reconciles three CRDs in group `hydradb.helmforge.dev/v1alpha1`:
`GraphCluster` (deployment lifecycle), `GraphNodePool` (worker scaling),
`GraphIndexer` (indexing jobs). The controller builds every child object in
Rust — it never shells out to `helm template` at runtime.

## Install

```sh
helm install hydradb-operator oci://harbor.kubexa.tech/oss/hydradb-operator \
  --version <version> --namespace hydradb-system --create-namespace
```

## Watch scope

`installScope: namespaced` (default) watches `OPERATOR_NAMESPACE` only and
renders `Role`/`RoleBinding`. `installScope: cluster` watches all namespaces
and renders `ClusterRole`/`ClusterRoleBinding`. See `docs/install-scope.md`.

## Monitoring

A Prometheus `ServiceMonitor` is rendered only when
`monitoring.serviceMonitor.enabled: true` (default `false`).
See `docs/monitoring.md`.

## Examples

- `examples/values-development.yaml` — minimal local install.
- `examples/values-production.yaml` — HA install with monitoring and guards.

## Testing

```sh
helm unittest charts/hydradb-operator
helm template release charts/hydradb-operator -f charts/hydradb-operator/ci/default-values.yaml
```

Suites live in `tests/` (one file per template, `documentSelector` keyed,
both enabled and disabled states asserted for every optional resource).

## Security scan

No image is published with the `:latest` tag; the Deployment test asserts a
`v<semver>` tag. Chart signing and SBOM are tracked separately
(see `docs/signing.md` in a later change).

## Requirements

Kubernetes: `>=1.26.0-0`

## Values

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| affinity | object | `{}` | Affinity rules for operator pods. |
| crds | object | `{"install":true}` | Custom Resource Definition handling. |
| crds.install | bool | `true` | Install CRDs from charts/hydradb-operator/crds on install/upgrade. |
| image | object | `{"digest":"","pullPolicy":"IfNotPresent","pullSecrets":[],"repository":"harbor.kubexa.tech/oss/hydradb-operator","tag":"v0.1.0"}` | Operator container image repository (registry + path). |
| image.digest | string | `""` | Pinned image digest (sha256 hex without the "sha256:" prefix); empty means tag only. |
| image.pullPolicy | string | `"IfNotPresent"` | Image pull policy (IfNotPresent, Always, or Never). |
| image.pullSecrets | list | `[]` | Image pull secrets for private registries. |
| image.repository | string | `"harbor.kubexa.tech/oss/hydradb-operator"` | Operator container image repository (registry + path). |
| image.tag | string | `"v0.1.0"` | Operator container image tag (pinned semver; never "latest"). |
| installScope | string | `"namespaced"` | Operator watch scope: "namespaced" watches OPERATOR_NAMESPACE only, "cluster" watches all namespaces. |
| log | object | `{"level":"info"}` | Log verbosity settings. |
| log.level | string | `"info"` | Tracing/logging filter directive (RUST_LOG format). |
| monitoring | object | `{"serviceMonitor":{"enabled":false}}` | Monitoring integration settings. |
| monitoring.serviceMonitor | object | `{"enabled":false}` | Prometheus ServiceMonitor settings. |
| monitoring.serviceMonitor.enabled | bool | `false` | Enable creation of a ServiceMonitor for the operator metrics endpoint. |
| networkPolicy | object | `{"enabled":true}` | NetworkPolicy settings for the operator pods. |
| networkPolicy.enabled | bool | `true` | Enable creation of a NetworkPolicy (healthz ingress, DNS/apiserver egress). |
| nodeSelector | object | `{}` | Node selector constraints for operator pods. |
| pdb | object | `{"enabled":true}` | PodDisruptionBudget settings for the operator Deployment. |
| pdb.enabled | bool | `true` | Enable creation of a PodDisruptionBudget (minAvailable: 1). |
| replicaCount | int | `1` | Number of operator Deployment replicas. |
| resources | object | `{"limits":{"cpu":"500m","memory":"512Mi"},"requests":{"cpu":"100m","memory":"128Mi"}}` | Operator container resource requests and limits. |
| resources.limits | object | `{"cpu":"500m","memory":"512Mi"}` | CPU resource limits. |
| resources.limits.cpu | string | `"500m"` | CPU limit. |
| resources.limits.memory | string | `"512Mi"` | Memory limit. |
| resources.requests | object | `{"cpu":"100m","memory":"128Mi"}` | CPU resource requests. |
| resources.requests.cpu | string | `"100m"` | CPU request. |
| resources.requests.memory | string | `"128Mi"` | Memory request. |
| tolerations | list | `[]` | Toleration rules for operator pods. |
