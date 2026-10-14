# HydraDB Operator

A [kube-rs](https://kube.rs/) operator that reconciles HydraDB, an object-storage-native distributed graph database, onto Kubernetes.

## Custom resources

API group `hydradb.helmforge.dev`, version `v1alpha1`:

| Kind | Purpose |
|------|---------|
| **GraphCluster** | Top-level cluster: desired topology and version of a HydraDB deployment. |
| **GraphNodePool** | A pool of graph worker nodes belonging to a cluster: size, resources, storage. |
| **GraphIndexer** | Indexer jobs for a cluster: what to index and on what schedule. |

## Quickstart

Install the operator from the Harbor `oss` project:

```sh
helm install hydradb-operator oci://harbor.kubexa.tech/oss/hydradb-operator --version <version>
kubectl apply -f config/samples/
```

Then create a cluster:

```yaml
apiVersion: hydradb.helmforge.dev/v1alpha1
kind: GraphCluster
metadata:
  name: example
  namespace: default
spec: {}
```

## Docs

- `docs/` — design notes, CRD reference, operations guides.
- `CONTRIBUTING.md` — how to build, test, and submit changes.
- `SECURITY.md` — how to report vulnerabilities.

## License

Apache License 2.0. See [LICENSE](LICENSE).
