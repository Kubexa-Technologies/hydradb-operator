# Backup, restore and ingest

Three things live here: the object-store copy contract the operator schedules,
the built-in copier, and the ingest trigger contract.

## 1. Backup / restore contract

A backup is a **physical copy** of the graph's object-store prefix:

```
namespaces/<tenant>[/subnamespaces/<child>...]/graphs/<graph-id>/
```

The operator never talks to S3 itself. It schedules a `Job`/`CronJob` from
`spec.image` and injects this environment. Any program that honours it works;
`hydradb backup` / `hydradb restore` are the built-in implementation.

| variable | meaning |
| --- | --- |
| `HYDRADB_BACKUP_DIRECTION` | `backup` or `restore` |
| `HYDRADB_BACKUP_SOURCE_BUCKET` / `_PREFIX` | where data is read from |
| `HYDRADB_BACKUP_TARGET_BUCKET` / `_PREFIX` | where the copy goes |
| `HYDRADB_BACKUP_VERIFY` | `true` when the run must verify the copy |
| `HYDRADB_BACKUP_RETENTION` | how many copies to keep (the copier prunes) |

`AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, `AWS_ENDPOINT`,
`AWS_DEFAULT_REGION` and `AWS_ALLOW_HTTP` come from the Secret named in
`target.store.credentialsSecretRef` (mounted read-only) or from the cluster's
own configuration.

### Built-in copier

```yaml
spec:
  image:
    repository: harbor.kubexa.tech/oss/hydradb-operator
    tag: v0.1.0
  command: ["/usr/local/bin/hydradb"]
  args: ["backup"]        # or "restore"
```

`hydradb backup --dry-run` prints the exact object plan without copying
anything, which is the fastest way to validate a prefix before a real run.

Safety properties, in order of importance:

1. **The manifest is copied last.** An interrupted run leaves a target that
   HydraDB will not adopt, instead of one pointing at objects that were never
   written.
2. **An empty source is an error**, never a successful empty backup.
3. **Verification compares key sets and sizes** (not just counts), so a
   truncated object fails the run.
4. **Nothing is deleted by the operator.** `spec.retention` bounds the operator's
   own run records; the copier removes old copies because only it can enumerate
   them.

## 2. Restore into a new cluster

```yaml
spec:
  restoreFrom:
    store:
      bucket: hydradb-backups
      endpoint: http://rook-ceph-rgw-ceph-bucket.rook-ceph.svc:80
      credentialsSecretRef:
        name: backup-creds
    prefix: clusters/prod          # the backup's target prefix
    image:
      repository: harbor.kubexa.tech/oss/hydradb-operator
      tag: v0.1.0
    verify: true
```

The operator runs the restore job **once**, publishes a `Restored` condition,
and **node pools refuse to scale up until that condition is true** — no writer
ever opens a half-copied store. A failed restore reports
`hydradb_operator_restore_failed=1` and leaves pools at zero replicas; delete
the job to retry.

The restore target must not be the cluster's own object store: validation
rejects it, because copying a prefix onto itself provides no isolation.

## 3. Ingest

HydraDB indexes only what is written into its WAL, so an external source does
nothing until an ingester writes it. `GraphIngestSource` owns the triggers:

- `spec.schedule` → a `CronJob` running your ingester;
- `spec.webhook.enabled` → `POST /webhooks/v1alpha1/ingest/<namespace>/<path>`
  starts a one-off `Job`.

The receiver is authenticated and replay-resistant:

```
X-HydraDB-Timestamp: <unix seconds>
X-HydraDB-Signature: sha256=<hex hmac_sha256(key, "<timestamp>.<body>")>
```

Requests outside a 300 s skew are rejected, the signature covers the timestamp
so a captured request cannot be replayed, bodies are capped at 1 MiB, and the
revision is taken from the **signed body** (`after`, `checkout_sha`,
`head_commit.id`, `commits[0].id`) so a header cannot smuggle one in.

The ingester receives:

| variable | meaning |
| --- | --- |
| `HYDRADB_INGEST_NAMESPACE` / `_GRAPH_ID` | target scope |
| `HYDRADB_INGEST_BOLT_URL` | `host:port` to write through |
| `HYDRADB_INGEST_SOURCE_URL` | git repository or document URL |
| `HYDRADB_INGEST_REVISION` | commit/tag resolved for this run |
| `HYDRADB_INGEST_PATHS` | comma-joined path globs |

Contract: exit `0` on success. The operator owns scheduling, concurrency policy,
retries, TTL and status; the ingester owns parsing.

An ingest pod must **not** run as the operator's ServiceAccount — the
reconciler refuses `serviceAccount: hydradb-operator`, because the pod runs
user-chosen code.

## 4. What is *not* indexable

HydraDB's open-source engine indexes its own WAL into GraphBLAS adjacency generations. It has **no
vector index, no BM25/lexical index, and no entity extraction** — those live in HydraDB's hosted
product, not in the AGPL repository.

So an ingester's job is exactly one thing: turn a source into Cypher writes (`MERGE`/`CREATE` with
properties). Writing "embeddings" is meaningless against this engine; if you need semantic recall,
that layer is upstream's service, not something this operator can schedule for you.
