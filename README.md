# HydraDB Operator

A [kube-rs](https://kube.rs/) operator that runs [HydraDB](https://github.com/hydra-db/hydradb) — an
object-storage-native distributed graph database — on Kubernetes.

The operator builds every child object in Rust and server-side-applies it. It never shells out to
`helm`, never resolves `:latest` at runtime, and never holds a credential: credentials stay in the
Secrets you point it at.

## Custom resources

API group `hydradb.kubexa.tech`, version `v1alpha1`, all namespaced:

| Kind | Short | Purpose |
|------|-------|---------|
| **GraphCluster** | `hgc` | One graph: image, object store, auth, TLS, ports, tuning, optional restore. |
| **GraphNodePool** | `hgnp` | A pool of graph nodes: size, resources, cache, role (writer/reader), optional HPA. |
| **GraphIndexer** | `hgi` | Index builder: build mode, concurrency, WAL-tail caps, retention of generations. |
| **GraphBackup** | `hgb` | Scheduled or one-shot copies of the graph's object-store data. |
| **GraphIngestSource** | `hgis` | Scheduled or webhook-triggered ingestion runs from a git repo or URL. |

## Quickstart

```sh
# CRDs only, if you manage the operator yourself
kubectl apply -f deploy/crds/

# or the whole thing from the Harbor oss project
helm install hydradb-operator oci://harbor.kubexa.tech/oss/hydradb-operator --version <version>
```

Then a minimal cluster — `image`, `graph` and `objectStore` are required; there is no hidden default
for any of them:

```yaml
apiVersion: hydradb.kubexa.tech/v1alpha1
kind: GraphCluster
metadata:
  name: example
  namespace: default
spec:
  image:
    repository: harbor.kubexa.tech/oss/hydradb
    tag: v0.1.0
  graph:
    namespace: tenant
    id: default
    database: default
    cellId: cell-0
  objectStore:
    explicit:
      bucket: hydradb-data
      credentialsSecretRef:
        name: hydradb-s3
---
apiVersion: hydradb.kubexa.tech/v1alpha1
kind: GraphNodePool
metadata:
  name: example-nodes
  namespace: default
spec:
  clusterRef:
    name: example
  replicas: 3
  cache:
    storage:
      emptyDir: {}
---
apiVersion: hydradb.kubexa.tech/v1alpha1
kind: GraphIndexer
metadata:
  name: example-indexer
  namespace: default
spec:
  clusterRef:
    name: example
  enabled: true
  replicas: 1
```

`kubectl hydradb status example`, `endpoints`, `logs`, `doctor`, `scale`, `backup` and `restore`
ship in the same image as the controller.

## Scope: what this operator is, and is not

The operator manages **the HydraDB storage engine** — the part that is published upstream under
AGPL-3.0. Concretely, that engine provides: durable graph storage on SlateDB over S3-compatible
object storage, an OpenCypher query engine, GraphBLAS traversal with immutable index generations, and
Bolt (7687) plus an HTTPS query API.

It does **not** provide a semantic or retrieval layer. HydraDB's hosted product additionally markets
entity/relationship extraction, a retrieval orchestrator, hybrid semantic + BM25 retrieval and
reranking; **none of that is in the open-source repository** (there is no embedding, BM25, rerank or
entity-extraction code in it — its only "vector" type is the GraphBLAS FFI). This operator therefore
exposes engine knobs only, and its ingestion contract writes Cypher into the graph — there is no
vector or full-text store to write into.

Practical consequence: this project gives you a **self-hostable graph database**. If you need the
semantic retrieval layer, that is HydraDB's hosted service or SDK. Do not read any field in
`tuning` or `GraphIngestSource` as reaching past the engine — none of them do.

## Docs

- `docs/backup.md` — backup, restore and ingest contracts (the `HYDRADB_*` variables, safety
  ordering, webhook authentication).
- `docs/crd-*.md` — generated CRD reference.
- `docs/decisions/` — ADRs, including the proven dual-registry image push.
- `docs/backup.md` §4 — why there is no vector/BM25 store to ingest into.
- `CONTRIBUTING.md` — build, test, submit.
- `SECURITY.md` — how to report vulnerabilities.

## License

**This operator is Apache-2.0** (see [LICENSE](LICENSE)). It links nothing from HydraDB and contains
no HydraDB code: it deploys the operand image you name at runtime, so the two licences never mix in
one artefact.

HydraDB itself is **AGPL-3.0**. That distinction is deliberate and load-bearing:

- running **unmodified** HydraDB is unrestricted; AGPL's source-offer obligation (§13) attaches to a
  *modified* version that users interact with over a network;
- a controller that talks to HydraDB over Bolt, and manages its Kubernetes objects, is a separate
  program at arm's length — not a derivative work;
- if you publish a **patched** operand image, you owe the corresponding source to the people who use
  it. This repo never does that: operand images are pulled by the cluster, never copied into the
  operator image.

Contributions are accepted under Apache-2.0 (inbound = outbound).

<!-- The GitHub copy is written by the mirror pipeline in this repository. -->
