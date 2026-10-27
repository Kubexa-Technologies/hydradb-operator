//! Opt-in children that integrate the operator with the wider Kubernetes
//! ecosystem: cert-manager `Certificate`, prometheus-operator `PrometheusRule`
//! and `autoscaling/v2` `HorizontalPodAutoscaler`.
//!
//! None of these groups ship `k8s-openapi` types, so every one is driven as a
//! [`DynamicObject`] with an [`ApiResource`] pinned here. The pin matters: it is
//! what `cargo xtask gen-rbac` reads to derive the matching RBAC rule, so a
//! typo cannot silently produce a controller that is forbidden at runtime.
//!
//! All builders are pure: they take the CR and return the object, so the whole
//! surface is unit-testable without an apiserver.

use std::collections::BTreeMap;

use hydradb_operator_api::crd::{GraphCluster, GraphNodePool};
use hydradb_operator_api::spec::{NodeRole, PhaseLabel, TuningSpec};
use k8s_openapi::api::core::v1::{EnvVar, EnvVarSource, ObjectFieldSelector};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use kube::core::{ApiResource, DynamicObject, GroupVersionKind};
use kube::{Resource as _, ResourceExt as _};

use crate::apply::{cluster_child_name, owner_ref_for, standard_labels};
use crate::error::Error;

/// Child role for the cert-manager `Certificate`.
pub const ROLE_CERTIFICATE: &str = "tls";
/// Child role for the `PrometheusRule`.
pub const ROLE_PROMETHEUSRULE: &str = "rules";
/// Child role for a pool `HorizontalPodAutoscaler`.
pub const ROLE_HPA: &str = "hpa";

/// Define a minimal typed handle for a `DynamicObject`-backed child kind.
///
/// [`Controller::owns`] needs `Resource<DynamicType = ()>`, which a plain
/// [`DynamicObject`] does not satisfy; this macro generates the smallest shape
/// that does. Unknown fields decode past it and the apply path keeps using
/// [`DynamicObject`].
macro_rules! dynamic_child {
    ($ty:ident, $group:literal, $version:literal, $kind:literal, $plural:literal, $doc:literal) => {
        #[doc = $doc]
        #[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
        pub struct $ty {
            metadata: ObjectMeta,
            #[serde(default)]
            spec: serde_json::Value,
        }

        impl kube::Resource for $ty {
            type DynamicType = ();
            type Scope = kube::core::NamespaceResourceScope;

            fn meta(&self) -> &ObjectMeta {
                &self.metadata
            }

            fn meta_mut(&mut self) -> &mut ObjectMeta {
                &mut self.metadata
            }

            fn group(_: &()) -> std::borrow::Cow<'_, str> {
                std::borrow::Cow::Borrowed($group)
            }

            fn version(_: &()) -> std::borrow::Cow<'_, str> {
                std::borrow::Cow::Borrowed($version)
            }

            fn kind(_: &()) -> std::borrow::Cow<'_, str> {
                std::borrow::Cow::Borrowed($kind)
            }

            fn plural(_: &()) -> std::borrow::Cow<'_, str> {
                std::borrow::Cow::Borrowed($plural)
            }
        }
    };
}

dynamic_child!(
    CertificateObject,
    "cert-manager.io",
    "v1",
    "Certificate",
    "certificates",
    "Typed watch handle for a cert-manager `Certificate`."
);
dynamic_child!(
    PrometheusRuleObject,
    "monitoring.coreos.com",
    "v1",
    "PrometheusRule",
    "prometheusrules",
    "Typed watch handle for a prometheus-operator `PrometheusRule`."
);
dynamic_child!(
    HpaObject,
    "autoscaling",
    "v2",
    "HorizontalPodAutoscaler",
    "horizontalpodautoscalers",
    "Typed watch handle for an `autoscaling/v2` `HorizontalPodAutoscaler`."
);

/// `ApiResource` for the cert-manager `Certificate` kind.
#[must_use]
pub fn certificate_api_resource() -> ApiResource {
    ApiResource::from_gvk_with_plural(
        &GroupVersionKind::gvk("cert-manager.io", "v1", "Certificate"),
        "certificates",
    )
}

/// `ApiResource` for the prometheus-operator `PrometheusRule` kind.
#[must_use]
pub fn prometheus_rule_api_resource() -> ApiResource {
    ApiResource::from_gvk_with_plural(
        &GroupVersionKind::gvk("monitoring.coreos.com", "v1", "PrometheusRule"),
        "prometheusrules",
    )
}

/// `ApiResource` for the `autoscaling/v2` `HorizontalPodAutoscaler` kind.
#[must_use]
pub fn hpa_api_resource() -> ApiResource {
    ApiResource::from_gvk_with_plural(
        &GroupVersionKind::gvk("autoscaling", "v2", "HorizontalPodAutoscaler"),
        "horizontalpodautoscalers",
    )
}

/// Treat "the CRD for this kind is not installed" as "no object exists".
///
/// `get_opt` only swallows a 404 for a missing *object*; when the whole CRD is
/// absent the apiserver also answers 404, and an operator that hard-fails on
/// that cannot run on a cluster without the optional ecosystem operators.
#[must_use]
pub fn missing_kind_or_object(error: &kube::Error) -> bool {
    matches!(error, kube::Error::Api(response) if response.code == 404)
}

/// `get_opt` that maps a missing CRD to `None` as well as a missing object.
///
/// # Errors
/// Returns [`Error::Api`] for every failure other than a 404.
pub async fn get_opt_tolerant<K>(api: &kube::Api<K>, name: &str) -> Result<Option<K>, Error>
where
    K: kube::Resource + serde::de::DeserializeOwned + Clone + std::fmt::Debug,
{
    match api.get_opt(name).await {
        Ok(object) => Ok(object),
        Err(error) if missing_kind_or_object(&error) => Ok(None),
        Err(error) => Err(Error::from_kube_error(error)),
    }
}

// --- cert-manager ----------------------------------------------------------

/// Whether the cluster asks the operator to obtain its serving certificate.
#[must_use]
pub fn certificate_enabled(cluster: &GraphCluster) -> bool {
    cluster.spec.tls.enabled && cluster.spec.tls.issuer_ref.is_some()
}

/// Name of the operator-owned `Certificate`/serving Secret: `<cluster>-tls`.
#[must_use]
pub fn certificate_secret_name(cluster: &GraphCluster) -> String {
    cluster
        .spec
        .tls
        .secret_name
        .clone()
        .unwrap_or_else(|| cluster_child_name(&cluster.name_any(), ROLE_CERTIFICATE))
}

/// DNS names the serving certificate must cover.
///
/// The three Service names are always included so in-cluster drivers can verify
/// the certificate; `spec.tls.additionalDnsNames` adds external names.
#[must_use]
pub fn certificate_dns_names(cluster: &GraphCluster) -> Vec<String> {
    let name = cluster.name_any();
    let namespace = cluster.namespace().unwrap_or_default();
    let mut names = vec![
        crate::reconcile::cluster::headless_service_name(&name),
        cluster_child_name(&name, crate::apply::ROLE_BOLT),
        cluster_child_name(&name, crate::apply::ROLE_HTTPS),
        format!("{name}.{namespace}"),
        format!("{name}.{namespace}.svc",),
        format!("{name}.{namespace}.svc.cluster.local"),
    ];
    names.extend(cluster.spec.tls.additional_dns_names.iter().cloned());
    names.sort();
    names.dedup();
    names
}

/// Desired cert-manager `Certificate`.
///
/// Callers must gate on [`certificate_enabled`]: building without an issuer is a
/// bug, not a default.
///
/// # Errors
/// Returns [`Error::MissingField`] when the owner has no name/uid yet.
pub fn desired_certificate(cluster: &GraphCluster) -> Result<DynamicObject, Error> {
    let issuer = cluster
        .spec
        .tls
        .issuer_ref
        .clone()
        .ok_or_else(|| Error::MissingField("tls.issuerRef is unset".to_owned()))?;
    let name = cluster.name_any();
    let api = certificate_api_resource();
    let mut obj = DynamicObject::new(&cluster_child_name(&name, ROLE_CERTIFICATE), &api);
    obj.metadata.labels = Some(standard_labels(&name, ROLE_CERTIFICATE));
    obj.metadata.owner_references = Some(vec![owner_ref_for(cluster)?]);
    obj.metadata.namespace = cluster.namespace();
    obj.data = serde_json::json!({
        "spec": {
            "secretName": certificate_secret_name(cluster),
            "issuerRef": {
                "name": issuer.name,
                "kind": issuer.kind,
                "group": issuer.group,
            },
            "dnsNames": certificate_dns_names(cluster),
            "privateKey": {"rotationPolicy": "Always"},
        }
    });
    Ok(obj)
}

// --- prometheus-operator ---------------------------------------------------

/// Whether the cluster opts into the operator's alert set.
#[must_use]
pub fn prometheus_rule_enabled(cluster: &GraphCluster) -> bool {
    cluster.spec.monitoring.prometheus_rule.enabled
}

/// Name of the operator-owned `PrometheusRule`: `<cluster>-rules`.
#[must_use]
pub fn prometheus_rule_name(cluster_name: &str) -> String {
    cluster_child_name(cluster_name, ROLE_PROMETHEUSRULE)
}

/// Alert expressions shipped with every enabled `PrometheusRule`.
///
/// Only metrics that exist are referenced: the operator's own gauges
/// (`hydradb_operator_*`, defined in `metrics`) and the operand counters that
/// were verified against the operand source (`graph_indexer_*`). Standard
/// `kube-state-metrics` series are used for workload readiness.
#[must_use]
pub fn prometheus_alert_rules() -> serde_json::Value {
    serde_json::json!([
        {
            "alert": "HydraDBIndexStalled",
            "expr": "hydradb_operator_index_stale_seconds > 900",
            "for": "10m",
            "labels": {"severity": "warning"},
            "annotations": {
                "summary": "HydraDB index has not advanced in over 15 minutes",
                "description": "Cluster {{ $labels.instance }} has not published a new index \
                                generation for {{ $value | humanizeDuration }}. Reads stay correct \
                                (WAL overlay) but traversal falls back to canonical snapshots."
            }
        },
        {
            "alert": "HydraDBIndexGenerationFailing",
            "expr": "increase(graph_indexer_generation_failures[15m]) > 0",
            "for": "5m",
            "labels": {"severity": "warning"},
            "annotations": {
                "summary": "HydraDB indexer is failing to build generations",
                "description": "{{ $value }} generation builds failed in the last 15 minutes."
            }
        },
        {
            "alert": "HydraDBIndexIncrementalBypassed",
            "expr": "increase(graph_indexer_incremental_fallbacks[1h]) > 3",
            "for": "15m",
            "labels": {"severity": "info"},
            "annotations": {
                "summary": "HydraDB incremental indexing keeps falling back to full builds",
                "description": "{{ $value }} fallbacks in the last hour: WAL tails exceed the \
                                configured cap or the graph is below the edge threshold."
            }
        },
        {
            "alert": "HydraDBNodesNotReady",
            "expr": "kube_statefulset_status_replicas_ready{statefulset=~\".+-nodes\"} < kube_statefulset_status_replicas{statefulset=~\".+-nodes\"}",
            "for": "15m",
            "labels": {"severity": "warning"},
            "annotations": {
                "summary": "HydraDB node pool has unready replicas",
                "description": "{{ $labels.namespace }}/{{ $labels.statefulset }} is missing \
                                {{ $value }} ready replicas."
            }
        },
        {
            "alert": "HydraDBBackupNotSucceeding",
            "expr": "time() - hydradb_operator_backup_last_success_timestamp_seconds > 93600",
            "for": "30m",
            "labels": {"severity": "critical"},
            "annotations": {
                "summary": "HydraDB backup has not succeeded in 26 hours",
                "description": "Last successful backup for {{ $labels.name }} was \
                                {{ $value | humanizeDuration }} ago."
            }
        },
        {
            "alert": "HydraDBRestoreFailed",
            "expr": "hydradb_operator_restore_failed{namespace=~\".+\"} > 0",
            "for": "5m",
            "labels": {"severity": "critical"},
            "annotations": {
                "summary": "HydraDB restore job failed",
                "description": "Cluster {{ $labels.name }} could not restore its data; nodes stay \
                                scaled down until it succeeds."
            }
        }
    ])
}

/// Desired prometheus-operator `PrometheusRule`.
///
/// Callers must gate on [`prometheus_rule_enabled`].
///
/// # Errors
/// Returns [`Error::MissingField`] when the owner has no name/uid yet.
pub fn desired_prometheus_rule(cluster: &GraphCluster) -> Result<DynamicObject, Error> {
    let name = cluster.name_any();
    let mut obj = DynamicObject::new(
        &prometheus_rule_name(&name),
        &prometheus_rule_api_resource(),
    );
    let mut labels = standard_labels(&name, ROLE_PROMETHEUSRULE);
    labels.extend(cluster.spec.monitoring.prometheus_rule.labels.clone());
    obj.metadata.labels = Some(labels);
    obj.metadata.owner_references = Some(vec![owner_ref_for(cluster)?]);
    obj.metadata.namespace = cluster.namespace();
    obj.data = serde_json::json!({
        "spec": {
            "groups": [{
                "name": format!("hydradb.{name}"),
                "rules": prometheus_alert_rules(),
            }]
        }
    });
    Ok(obj)
}

// --- horizontal pod autoscaler ---------------------------------------------

/// Whether the pool opts into autoscaling.
#[must_use]
pub fn hpa_enabled(pool: &GraphNodePool) -> bool {
    pool.spec.autoscaling.is_some()
}

/// Name of the pool-owned autoscaler: `<pool>-hpa`.
#[must_use]
pub fn hpa_name(pool_name: &str) -> String {
    format!("{pool_name}-{ROLE_HPA}")
}

/// Desired `autoscaling/v2` `HorizontalPodAutoscaler` for a pool.
///
/// The scale target is the pool's own StatefulSet, which carries the same name
/// as the pool.
///
/// # Errors
/// Returns [`Error::MissingField`] when the pool has no name/uid, or
/// [`Error::InvalidSpec`] when the bounds are incoherent.
pub fn desired_hpa(pool: &GraphNodePool) -> Result<DynamicObject, Error> {
    let auto = pool
        .spec
        .autoscaling
        .as_ref()
        .ok_or_else(|| Error::MissingField("spec.autoscaling is unset".to_owned()))?;
    if auto.max_replicas < auto.min_replicas {
        return Err(Error::InvalidSpec(format!(
            "autoscaling.maxReplicas ({}) is below autoscaling.minReplicas ({})",
            auto.max_replicas, auto.min_replicas
        )));
    }
    let pool_name = pool.name_any();
    if pool.spec.cluster_ref.name.is_empty() {
        return Err(Error::MissingField(
            "spec.clusterRef.name is empty".to_owned(),
        ));
    }
    let cluster_name = pool.spec.cluster_ref.name.clone();
    let mut obj = DynamicObject::new(&hpa_name(&pool_name), &hpa_api_resource());
    obj.metadata.labels = Some(standard_labels(&cluster_name, ROLE_HPA));
    obj.metadata.owner_references = Some(vec![pool_owner_ref(pool)?]);
    obj.metadata.namespace = pool.namespace();

    let mut metrics = Vec::new();
    if let Some(target) = auto.target_cpu_utilization_percentage {
        metrics.push(serde_json::json!({
            "type": "Resource",
            "resource": {
                "name": "cpu",
                "target": {"type": "Utilization", "averageUtilization": target}
            }
        }));
    }
    if metrics.is_empty() {
        // An HPA without a metric is rejected by the apiserver; fall back to
        // the conventional CPU target rather than emitting an invalid object.
        metrics.push(serde_json::json!({
            "type": "Resource",
            "resource": {
                "name": "cpu",
                "target": {"type": "Utilization", "averageUtilization": 70}
            }
        }));
    }

    let mut spec = serde_json::json!({
        "scaleTargetRef": {
            "apiVersion": "apps/v1",
            "kind": "StatefulSet",
            "name": pool_name,
        },
        "minReplicas": auto.min_replicas,
        "maxReplicas": auto.max_replicas,
        "metrics": metrics,
    });
    if let Some(behavior) = &auto.behavior {
        // Typed behaviour serialises straight into the HPA body; a failure here
        // would mean the type and the k8s schema disagree, so it is surfaced
        // rather than silently dropped.
        let rendered = serde_json::to_value(behavior)
            .map_err(|error| Error::InvalidSpec(format!("autoscaling.behavior: {error}")))?;
        spec["behavior"] = rendered;
    }
    obj.data = serde_json::json!({ "spec": spec });
    Ok(obj)
}

/// `OwnerReference` pointing at the owning [`GraphNodePool`].
///
/// # Errors
/// Returns [`Error::MissingField`] when the pool has no name or uid yet.
fn pool_owner_ref(
    pool: &GraphNodePool,
) -> Result<k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference, Error> {
    let meta = pool.meta();
    let name = meta
        .name
        .clone()
        .ok_or_else(|| Error::MissingField("GraphNodePool has no metadata.name".to_owned()))?;
    let uid = meta
        .uid
        .clone()
        .ok_or_else(|| Error::MissingField("GraphNodePool has no metadata.uid".to_owned()))?;
    Ok(
        k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference {
            api_version: GraphNodePool::api_version(&()).into_owned(),
            kind: GraphNodePool::kind(&()).into_owned(),
            name,
            uid,
            controller: Some(true),
            block_owner_deletion: Some(false),
        },
    )
}

// --- typed tuning -> env ---------------------------------------------------

/// Flatten [`TuningSpec`] into operand env pairs, skipping every unset leaf.
///
/// Ordering is deterministic (the `BTreeMap` sorts by key) so two builds of the
/// same spec are byte-identical and never churn the ConfigMap.
#[must_use]
pub fn tuning_env(tuning: &TuningSpec) -> BTreeMap<String, String> {
    use hydradb_operator_api::constants as c;
    let mut out = BTreeMap::new();
    let mut set = |key: &str, value: Option<String>| {
        if let Some(value) = value {
            out.insert(key.to_owned(), value);
        }
    };

    let g = &tuning.graphblas;
    set(
        c::GRAPH_MAX_GRAPHBLAS_BYTES,
        g.max_bytes.map(|v| v.to_string()),
    );
    set(
        c::GRAPH_MAX_GRAPHBLAS_MATRICES,
        g.max_matrices.map(|v| v.to_string()),
    );
    set(
        c::GRAPH_MAX_MATRIX_ADJACENCIES,
        g.max_adjacencies.map(|v| v.to_string()),
    );
    set(
        c::GRAPH_MAX_MATRIX_ADJACENCY_BYTES,
        g.max_adjacency_bytes.map(|v| v.to_string()),
    );
    set(
        c::GRAPH_MAX_CONCURRENT_MATRIX_COMPILATIONS,
        g.max_concurrent_compilations.map(|v| v.to_string()),
    );

    let q = &tuning.query;
    set(
        c::GRAPH_MAX_QUERY_RUNTIME_MS,
        q.max_runtime_ms.map(|v| v.to_string()),
    );
    set(
        c::GRAPH_DEFAULT_PAGE_SIZE,
        q.default_page_size.map(|v| v.to_string()),
    );
    set(
        c::GRAPH_MAX_SERVER_CURSORS,
        q.max_server_cursors.map(|v| v.to_string()),
    );
    set(
        c::GRAPH_MAX_CURSOR_BUFFER_BYTES,
        q.max_cursor_buffer_bytes.map(|v| v.to_string()),
    );
    set(
        c::GRAPH_CURSOR_TTL_MS,
        q.cursor_ttl_ms.map(|v| v.to_string()),
    );
    set(
        c::GRAPH_SLOW_QUERY_MS,
        q.slow_query_ms.map(|v| v.to_string()),
    );
    set(
        c::GRAPH_MAX_CONCURRENT_HYDRATIONS,
        q.max_concurrent_hydrations.map(|v| v.to_string()),
    );
    set(
        c::GRAPH_MAX_BOOKMARK_WAIT_MS,
        q.max_bookmark_wait_ms.map(|v| v.to_string()),
    );
    set(
        c::GRAPH_TXN_MAX_RETRIES,
        q.txn_max_retries.map(|v| v.to_string()),
    );

    let n = &tuning.connection;
    set(
        c::GRAPH_MAX_BOLT_CONNECTIONS,
        n.max_connections.map(|v| v.to_string()),
    );
    set(
        c::GRAPH_BOLT_IDLE_TIMEOUT_MS,
        n.idle_timeout_ms.map(|v| v.to_string()),
    );
    set(
        c::GRAPH_BOLT_MAX_CONNECTION_AGE_MS,
        n.max_age_ms.map(|v| v.to_string()),
    );
    set(
        c::GRAPH_BOLT_AUTHENTICATION_TIMEOUT_MS,
        n.authentication_timeout_ms.map(|v| v.to_string()),
    );
    set(
        c::GRAPH_GRACEFUL_SHUTDOWN_MS,
        n.graceful_shutdown_ms.map(|v| v.to_string()),
    );

    let s = &tuning.storage;
    set(
        c::GRAPH_DATA_CACHE_PART_BYTES,
        s.cache_part_bytes.map(|v| v.to_string()),
    );
    set(
        c::GRAPH_L0_FLUSH_PARALLELISM,
        s.l0_flush_parallelism.map(|v| v.to_string()),
    );
    set(
        c::GRAPH_L0_SST_SIZE_BYTES,
        s.l0_sst_size_bytes.map(|v| v.to_string()),
    );
    set(
        c::GRAPH_MAINTENANCE_BATCH_KEYS,
        s.maintenance_batch_keys.map(|v| v.to_string()),
    );
    set(
        c::GRAPH_ARTIFACT_GC_BATCH_KEYS,
        s.artifact_gc_batch_keys.map(|v| v.to_string()),
    );
    set(
        c::GRAPH_ARTIFACT_WRITE_BATCH_KEYS,
        s.artifact_write_batch_keys.map(|v| v.to_string()),
    );
    set(
        c::GRAPH_WAL_FLUSH_INTERVAL_MS,
        s.wal_flush_interval_ms.map(|v| v.to_string()),
    );
    set(
        c::GRAPH_TRIM_MEMORY_AFTER_HYDRATION,
        s.trim_memory_after_hydration.map(|v| v.to_string()),
    );

    let e = &tuning.engine;
    set(c::GRAPH_CYPHER_ENGINE, e.cypher_engine.clone());
    set(c::GRAPH_COMPILED_KERNEL, e.compiled_kernel.clone());
    set(c::GRAPH_SPARSE_KERNEL, e.sparse_kernel.clone());
    set(
        c::GRAPH_PROFILE_MATRIX,
        e.profile_matrix.map(|v| v.to_string()),
    );

    let p = &tuning.placement;
    set(
        c::GRAPH_HEARTBEAT_INTERVAL_MS,
        p.heartbeat_interval_ms.map(|v| v.to_string()),
    );
    set(
        c::GRAPH_HEARTBEAT_TIMEOUT_MS,
        p.heartbeat_timeout_ms.map(|v| v.to_string()),
    );
    set(
        c::GRAPH_WRITER_LEASE_MS,
        p.writer_lease_ms.map(|v| v.to_string()),
    );
    set(c::GRAPH_WRITE_LANES, p.write_lanes.map(|v| v.to_string()));
    set(c::GRAPH_READ_ROUTING, p.read_routing.clone());
    set(
        c::GRAPH_READER_MANIFEST_POLL_INTERVAL,
        p.reader_manifest_poll_interval_ms.map(|v| v.to_string()),
    );
    set(
        c::GRAPH_INDEX_DISCOVERY_INTERVAL_MS,
        p.index_discovery_interval_ms.map(|v| v.to_string()),
    );

    out
}

// --- role + identity env ---------------------------------------------------

/// Env vars that encode a pool's role and stable node identity.
///
/// `GRAPH_NODE_ID` comes from the downward API (`metadata.name`), so placement
/// heartbeats stay stable across restarts without the operator reading pods.
#[must_use]
pub fn role_env(role: &NodeRole) -> Vec<EnvVar> {
    use hydradb_operator_api::constants as c;
    vec![
        EnvVar {
            name: c::GRAPH_NODE_ROLE.to_owned(),
            value: Some(role.label().to_owned()),
            ..EnvVar::default()
        },
        EnvVar {
            name: c::GRAPH_NODE_ID.to_owned(),
            value_from: Some(EnvVarSource {
                field_ref: Some(ObjectFieldSelector {
                    api_version: Some("v1".to_owned()),
                    field_path: "metadata.name".to_owned(),
                }),
                ..EnvVarSource::default()
            }),
            ..EnvVar::default()
        },
    ]
}

/// Read routing value implied by a role: readers are told to follow, writers to
/// stay authoritative. `All` emits nothing so the operand default applies.
#[must_use]
pub fn read_routing_for(role: &NodeRole) -> Option<&'static str> {
    match role {
        NodeRole::Reader => Some("follower"),
        NodeRole::Writer | NodeRole::All => None,
        NodeRole::Unknown(_) => None,
    }
}

/// Insert `var` into `env`, replacing an entry with the same name in place.
///
/// Replacement in place (rather than a rebuild) preserves the documented order
/// of the surrounding entries, which the pod-spec tests assert on.
pub fn push_or_replace(env: &mut Vec<EnvVar>, var: EnvVar) {
    if let Some(slot) = env.iter_mut().find(|existing| existing.name == var.name) {
        *slot = var;
    } else {
        env.push(var);
    }
}

/// `secretRef` for an `envFrom` entry pulling a Secret's keys as variables.
///
/// Built here (not inline in the reconcilers) so the fully-qualified path keeps
/// the literal unambiguous and the shape is unit-testable.
#[must_use]
pub fn secret_env_source(name: &str) -> Option<k8s_openapi::api::core::v1::SecretEnvSource> {
    Some(k8s_openapi::api::core::v1::SecretEnvSource {
        name: name.to_owned(),
        optional: Some(false),
    })
}

/// Read-only `Volume` projecting a Secret's files at a mount path.
///
/// Built through `serde_json` so the k8s-openapi shape stays authoritative and
/// no field has to be repeated by hand.
///
/// # Errors
/// Returns [`Error::InvalidSpec`] if the shape stops matching `Volume`.
pub fn secret_volume(
    name: &str,
    secret_name: &str,
) -> Result<k8s_openapi::api::core::v1::Volume, Error> {
    serde_json::from_value(serde_json::json!({
        "name": name,
        "secret": {"secretName": secret_name, "optional": false}
    }))
    .map_err(|error| Error::InvalidSpec(format!("volume {name}: {error}")))
}

/// Merge `extra` into `base` by variable name, `extra` winning.
///
/// The operator's own variables are never passed through here: validation
/// rejects a user value for any of them, so a collision cannot occur.
#[must_use]
pub fn merge_env(base: Vec<EnvVar>, extra: &[EnvVar]) -> Vec<EnvVar> {
    if extra.is_empty() {
        return base;
    }
    let mut merged: BTreeMap<String, EnvVar> = base
        .into_iter()
        .map(|var| (var.name.clone(), var))
        .collect();
    for var in extra {
        merged.insert(var.name.clone(), var.clone());
    }
    merged.into_values().collect()
}
