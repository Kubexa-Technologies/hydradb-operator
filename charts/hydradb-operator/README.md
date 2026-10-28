# hydradb-operator

HydraDB Kubernetes operator (kube-rs) reconciling GraphCluster, GraphNodePool, and GraphIndexer resources

The operator reconciles three CRDs in group `hydradb.kubexa.tech/v1alpha1`:
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
| affinity | object | `{}` | Affinity for operator pods. |
| crds | object | `{"install":true}` | Install the HydraDB CRDs (GraphCluster, GraphNodePool, GraphIndexer, GraphBackup, GraphIngestSource) with the chart. |
| extraArgs | list | `[]` | Extra command-line arguments for the operator binary. |
| extraEnv | list | `[]` | Extra environment variables for the operator container.  This is the supported way to configure the controller process:   OPERATOR_SCOPE, OPERATOR_NAMESPACE, RUST_LOG, RUST_BACKTRACE,   OTEL_EXPORTER_OTLP_ENDPOINT, OPERATOR_WEBHOOK_ADDR,   OPERATOR_INGEST_ALLOWED_SERVICE_ACCOUNTS (comma-separated list; empty means   only `default`, and the operator's own SA is always refused). |
| image | object | `{"digest":"","pullPolicy":"IfNotPresent","pullSecrets":[],"repository":"harbor.kubexa.tech/oss/hydradb-operator","tag":"v0.1.0"}` | Operator container image repository (registry + path). |
| image.digest | string | `""` | Pinned image digest (sha256 hex without the "sha256:" prefix); empty means tag only. |
| image.pullPolicy | string | `"IfNotPresent"` | Image pull policy (IfNotPresent, Always, or Never). |
| image.pullSecrets | list | `[]` | Image pull secrets for private registries. |
| image.repository | string | `"harbor.kubexa.tech/oss/hydradb-operator"` | Operator container image repository (registry + path). |
| image.tag | string | `"v0.1.0"` | Operator container image tag (pinned semver; never "latest"). |
| installScope | string | `"namespaced"` | Operator watch scope: "namespaced" watches OPERATOR_NAMESPACE only, "cluster" watches all namespaces. Use "cluster" when the GraphCluster CRs live in namespaces other than the release namespace. |
| log | object | `{"level":"info"}` | Operator log configuration. |
| log.level | string | `"info"` | RUST_LOG filter directive for the controller. |
| monitoring | object | `{"serviceMonitor":{"enabled":false,"interval":"30s","labels":{}}}` | Monitoring integration for the operator. |
| monitoring.serviceMonitor.enabled | bool | `false` | Create a ServiceMonitor scraping the operator's /metrics. |
| monitoring.serviceMonitor.interval | string | `"30s"` | Scrape interval. |
| monitoring.serviceMonitor.labels | object | `{}` | Extra labels for the ServiceMonitor (e.g. your Prometheus release selector). |
| networkPolicy | object | `{"enabled":true}` | Operator NetworkPolicy (ingress to the health/metrics port, DNS + API + S3 egress). |
| nodeSelector | object | `{}` | Node selector for operator pods (e.g. pin to an architecture). |
| pdb | object | `{"enabled":true,"minAvailable":1}` | Operator PodDisruptionBudget. |
| pdb.enabled | bool | `true` | Create a PDB for the operator pods. |
| pdb.minAvailable | int | `1` | Pods that must remain available during voluntary disruption. |
| podAnnotations | object | `{}` | Extra annotations for operator pods. |
| podLabels | object | `{}` | Extra labels for operator pods. |
| priorityClassName | string | `""` | Priority class ensuring the controller survives node pressure. |
| replicaCount | int | `2` | Number of operator Deployment replicas.  >1 is safe: the leader-election lease elects exactly one active reconciler and the others stay ready but idle (they serve /readyz as 503 until promoted). |
| resources | object | `{"limits":{"cpu":"500m","memory":"512Mi"},"requests":{"cpu":"100m","memory":"128Mi"}}` | Operator container resource requests and limits. |
| revisionHistoryLimit | int | `3` | Number of old ReplicaSets retained for rollback. |
| serviceAccount | object | `{"annotations":{},"automount":true,"create":true,"name":""}` | ServiceAccount for the operator Deployment. |
| serviceAccount.annotations | object | `{}` | Annotations, e.g. eks.amazonaws.com/role-arn for IRSA. |
| serviceAccount.automount | bool | `true` | Mount the API token into the pod (required for in-cluster operation). |
| serviceAccount.create | bool | `true` | Create the ServiceAccount (set false to use a pre-existing one). |
| serviceAccount.name | string | `""` | Override the ServiceAccount name (empty uses the chart fullname). |
| terminationGracePeriodSeconds | int | `30` | Grace period for operator pod termination, in seconds. |
| tolerations | list | `[]` | Tolerations for operator pods. |
| topologySpreadConstraints | list | `[]` | Topology spread constraints for operator pods (HA scheduling). |
| webhook | object | `{"enabled":false,"port":8081,"service":{"annotations":{},"type":"ClusterIP"}}` | Ingest webhook receiver: the controller's authenticated HTTP endpoint that starts GraphIngestSource runs on push. It configures the RECEIVER, not the ingest job (which is defined on the GraphIngestSource CR). |
| webhook.enabled | bool | `false` | Expose the receiver and its Service. |
| webhook.port | int | `8081` | Container/Service port the receiver listens on. |
| webhook.service.annotations | object | `{}` | Annotations for the receiver Service (e.g. MetalLB address pinning). |
| webhook.service.type | string | `"ClusterIP"` | Service type for the receiver (ClusterIP, NodePort, LoadBalancer). |
