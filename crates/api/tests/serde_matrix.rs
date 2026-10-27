//! Todo 12: serde round-trip matrix, extensible-enum matrix, documented
//! defaults, and nested-schema `deny_unknown_fields` coverage for the api
//! crate.
//!
//! JSON stands in for YAML throughout (`serde_json` is already an api
//! dependency; JSON is a valid YAML 1.2 subset, so the wire contract tested
//! is identical). Complements (never duplicates) `tests/crd_types.rs`,
//! `tests/validation.rs`, and `src/spec/tests.rs`: those own the minimal
//! fixtures and single-enum spot checks, this file owns the fully-populated
//! round-trips, the complete five-enum matrix, and the exhaustive default
//! assertions.

use hydradb_operator_api::crd::{DeletionPolicy, GraphCluster, GraphIndexer, GraphNodePool};
use hydradb_operator_api::spec::{BuildMode, CacheType, ConditionReason, PullPolicy, ServiceType};
use serde_json::json;

// --- fully-populated fixtures ---------------------------------------------

fn full_cluster_value() -> serde_json::Value {
    json!({
        "apiVersion": "hydradb.kubexa.tech/v1alpha1",
        "kind": "GraphCluster",
        "metadata": { "name": "full", "namespace": "default" },
        "spec": {
            "image": {
                "repository": "example.com/org/hydradb",
                "tag": "v0.1.0",
                "digest": "sha256:abc123",
                "pullPolicy": "Always",
                "pullSecrets": [{ "name": "regcred" }]
            },
            "graph": {
                "namespace": "production",
                "id": "default",
                "database": "default",
                "cellId": "cell-0"
            },
            "objectStore": {
                "explicit": {
                    "bucket": "my-bucket",
                    "endpoint": "https://s3.example.com",
                    "region": "us-east-1",
                    "allowHttp": false,
                    "credentialsSecretRef": { "name": "store-creds" }
                }
            },
            "auth": {
                "existingSecret": "hydra-auth",
                "secretKey": "token",
                "generate": true,
                "token": "s3cr3t"
            },
            "tls": {
                "enabled": true,
                "secretName": "hydra-tls",
                "additionalDnsNames": ["extra.example.com"]
            },
            "deletionPolicy": "Delete",
            "service": { "boltPort": 7687, "httpsPort": 443, "adminPort": 9090 },
            "runtime": {
                "rustLog": "debug",
                "maxOpenScopes": 8,
                "slatedbCacheBytes": 671088640,
                "dataCacheMaxOpenFiles": 512,
                "readerWalReplayConcurrency": 16,
                "maxConcurrentQueries": 256,
                "maxQueryScanEdges": 1000000
            }
        }
    })
}

fn full_pool_value() -> serde_json::Value {
    json!({
        "apiVersion": "hydradb.kubexa.tech/v1alpha1",
        "kind": "GraphNodePool",
        "metadata": { "name": "pool-0", "namespace": "default" },
        "spec": {
            "clusterRef": { "name": "example" },
            "replicas": 3,
            "image": { "repository": "example.com/org/hydradb", "tag": "v0.1.0" },
            "resources": {
                "requests": { "cpu": "2", "memory": "4Gi" },
                "limits": { "cpu": "8", "memory": "16Gi" }
            },
            "cache": {
                "storage": {
                    "persistentVolume": {
                        "size": "40Gi",
                        "storageClassName": "ceph-block",
                        "accessModes": ["ReadWriteOnce"]
                    }
                },
                "memoryLimitBytes": 1073741824
            },
            "tmpSizeLimit": "1Gi",
            "serviceAccount": "hydra-worker",
            "priorityClassName": "high-priority",
            "terminationGracePeriodSeconds": 60,
            "updateStrategy": { "type": "RollingUpdate", "rollingUpdatePartition": 1 },
            "affinity": {},
            "topologySpreadConstraints": [
                { "maxSkew": 1, "topologyKey": "zone", "whenUnsatisfiable": "ScheduleAnyway" }
            ],
            "tolerations": [
                { "key": "dedicated", "operator": "Equal", "value": "hydra", "effect": "NoSchedule" }
            ],
            "nodeSelector": { "disktype": "ssd" },
            "extraEnv": [{ "name": "EXTRA", "value": "1" }],
            "podAnnotations": { "annotation.example.com/key": "value" },
            "podLabels": { "label.example.com/key": "value" },
            "service": {
                "advertisedBoltAddress": "bolt.example.com:7687",
                "boltType": "ClusterIP"
            }
        }
    })
}

fn full_indexer_value() -> serde_json::Value {
    json!({
        "apiVersion": "hydradb.kubexa.tech/v1alpha1",
        "kind": "GraphIndexer",
        "metadata": { "name": "example-indexer", "namespace": "default" },
        "spec": {
            "clusterRef": { "name": "example" },
            "enabled": true,
            "replicas": 2,
            "image": { "repository": "example.com/org/hydradb", "tag": "v0.1.0" },
            "intervalMs": 7000,
            "dirtyPollIntervalMs": 300,
            "retainPreviousGenerations": 2,
            "buildMode": "incremental",
            "incrementalMinEdges": 100000,
            "scopeConcurrency": 4,
            "scopesPerCycle": 128,
            "maxOpenScopes": 64,
            "readinessFailureThreshold": 5,
            "maxWalTailFiles": 2048,
            "walTailFetchConcurrency": 8,
            "resources": {
                "requests": { "cpu": "1", "memory": "2Gi" },
                "limits": { "cpu": "4", "memory": "8Gi" }
            },
            "serviceAccount": "hydra-indexer",
            "nodeSelector": { "disktype": "ssd" },
            "tolerations": [
                { "key": "dedicated", "operator": "Equal", "value": "hydra", "effect": "NoSchedule" }
            ],
            "affinity": {},
            "podAnnotations": { "annotation.example.com/key": "value" },
            "podLabels": { "label.example.com/key": "value" },
            "extraEnv": [{ "name": "EXTRA", "value": "1" }]
        }
    })
}

// --- 1. serde round-trips for all three kinds ------------------------------

#[test]
fn fully_populated_graphcluster_round_trips_byte_identically() {
    let cluster: GraphCluster = serde_json::from_value(full_cluster_value())
        .expect("fully-populated GraphCluster must parse");
    // Every optional leaf survives as `Some(..)`, never silently dropped.
    assert_eq!(
        cluster.spec.image.tag.as_deref(),
        Some("v0.1.0"),
        "image.tag must survive"
    );
    assert_eq!(
        cluster.spec.image.digest.as_deref(),
        Some("sha256:abc123"),
        "image.digest must survive"
    );
    assert_eq!(
        cluster.spec.image.pull_policy,
        Some(PullPolicy::Always),
        "image.pullPolicy must survive"
    );
    assert_eq!(
        cluster.spec.image.pull_secrets.len(),
        1,
        "image.pullSecrets must survive"
    );
    let store = cluster
        .spec
        .object_store
        .explicit
        .as_ref()
        .expect("explicit store must survive");
    assert_eq!(
        store.endpoint.as_deref(),
        Some("https://s3.example.com"),
        "store endpoint must survive"
    );
    assert_eq!(
        store.region.as_deref(),
        Some("us-east-1"),
        "store region must survive"
    );
    assert!(
        store.credentials_secret_ref.is_some(),
        "store credentialsSecretRef must survive"
    );
    assert_eq!(
        cluster.spec.auth.existing_secret.as_deref(),
        Some("hydra-auth"),
        "auth.existingSecret must survive"
    );
    assert_eq!(
        cluster.spec.auth.secret_key.as_deref(),
        Some("token"),
        "auth.secretKey must survive"
    );
    assert!(
        cluster.spec.auth.generate,
        "auth.generate must survive as true"
    );
    assert_eq!(
        cluster.spec.auth.token.as_deref(),
        Some("s3cr3t"),
        "auth.token must survive"
    );
    assert!(cluster.spec.tls.enabled, "tls.enabled must survive as true");
    assert_eq!(
        cluster.spec.tls.secret_name.as_deref(),
        Some("hydra-tls"),
        "tls.secretName must survive"
    );
    assert_eq!(
        cluster.spec.tls.additional_dns_names,
        vec!["extra.example.com".to_string()],
        "tls.additionalDnsNames must survive"
    );
    assert_eq!(
        cluster.spec.deletion_policy,
        DeletionPolicy::Delete,
        "deletionPolicy must survive"
    );
    let first = serde_json::to_string(&cluster).expect("serialize must succeed");
    let reparsed: GraphCluster = serde_json::from_str(&first).expect("re-parse must succeed");
    let second = serde_json::to_string(&reparsed).expect("re-serialize must succeed");
    assert_eq!(
        first, second,
        "GraphCluster to_string -> from_str -> to_string must be byte-identical"
    );
}

#[test]
fn fully_populated_graphnodepool_round_trips_byte_identically() {
    let pool: GraphNodePool = serde_json::from_value(full_pool_value())
        .expect("fully-populated GraphNodePool must parse");
    assert!(
        pool.spec.image.is_some(),
        "pool image override must survive"
    );
    assert!(pool.spec.resources.is_some(), "pool resources must survive");
    assert_eq!(
        pool.spec.cache.memory_limit_bytes,
        Some(1_073_741_824),
        "cache.memoryLimitBytes must survive"
    );
    assert_eq!(
        pool.spec.tmp_size_limit.as_deref(),
        Some("1Gi"),
        "tmpSizeLimit must survive"
    );
    assert_eq!(
        pool.spec.service_account.as_deref(),
        Some("hydra-worker"),
        "serviceAccount must survive"
    );
    assert_eq!(
        pool.spec.priority_class_name.as_deref(),
        Some("high-priority"),
        "priorityClassName must survive"
    );
    assert_eq!(
        pool.spec.termination_grace_period_seconds,
        Some(60),
        "terminationGracePeriodSeconds must survive"
    );
    assert!(
        pool.spec.update_strategy.is_some(),
        "updateStrategy must survive"
    );
    assert!(pool.spec.affinity.is_some(), "affinity must survive");
    assert_eq!(
        pool.spec.topology_spread_constraints.len(),
        1,
        "topologySpreadConstraints must survive"
    );
    assert_eq!(pool.spec.tolerations.len(), 1, "tolerations must survive");
    assert!(
        pool.spec
            .node_selector
            .as_ref()
            .is_some_and(|m| m.get("disktype").is_some_and(|v| v == "ssd")),
        "nodeSelector must survive"
    );
    assert_eq!(pool.spec.extra_env.len(), 1, "extraEnv must survive");
    assert!(
        !pool.spec.pod_annotations.is_empty(),
        "podAnnotations must survive"
    );
    assert!(!pool.spec.pod_labels.is_empty(), "podLabels must survive");
    assert!(pool.spec.service.is_some(), "service must survive");
    let first = serde_json::to_string(&pool).expect("serialize must succeed");
    let reparsed: GraphNodePool = serde_json::from_str(&first).expect("re-parse must succeed");
    let second = serde_json::to_string(&reparsed).expect("re-serialize must succeed");
    assert_eq!(
        first, second,
        "GraphNodePool to_string -> from_str -> to_string must be byte-identical"
    );
}

#[test]
fn fully_populated_graphindexer_round_trips_byte_identically() {
    let indexer: GraphIndexer = serde_json::from_value(full_indexer_value())
        .expect("fully-populated GraphIndexer must parse");
    // Non-default values prove the fields survive the wire, not just echo defaults.
    assert_eq!(
        indexer.spec.interval_ms,
        Some(7000),
        "intervalMs must survive"
    );
    assert_eq!(
        indexer.spec.dirty_poll_interval_ms,
        Some(300),
        "dirtyPollIntervalMs must survive"
    );
    assert_eq!(
        indexer.spec.retain_previous_generations,
        Some(2),
        "retainPreviousGenerations must survive"
    );
    assert_eq!(
        indexer.spec.build_mode,
        Some(BuildMode::Incremental),
        "buildMode must survive"
    );
    assert_eq!(
        indexer.spec.incremental_min_edges,
        Some(100_000),
        "incrementalMinEdges must survive"
    );
    assert_eq!(
        indexer.spec.scope_concurrency,
        Some(4),
        "scopeConcurrency must survive"
    );
    assert_eq!(
        indexer.spec.scopes_per_cycle,
        Some(128),
        "scopesPerCycle must survive"
    );
    assert_eq!(
        indexer.spec.max_open_scopes,
        Some(64),
        "maxOpenScopes must survive"
    );
    assert_eq!(
        indexer.spec.readiness_failure_threshold,
        Some(5),
        "readinessFailureThreshold must survive"
    );
    assert_eq!(
        indexer.spec.max_wal_tail_files,
        Some(2048),
        "maxWalTailFiles must survive"
    );
    assert_eq!(
        indexer.spec.wal_tail_fetch_concurrency,
        Some(8),
        "walTailFetchConcurrency must survive"
    );
    assert!(indexer.spec.image.is_some(), "image must survive");
    assert!(indexer.spec.resources.is_some(), "resources must survive");
    assert_eq!(
        indexer.spec.service_account.as_deref(),
        Some("hydra-indexer"),
        "serviceAccount must survive"
    );
    assert!(
        indexer
            .spec
            .node_selector
            .as_ref()
            .is_some_and(|m| m.get("disktype").is_some_and(|v| v == "ssd")),
        "nodeSelector must survive"
    );
    assert_eq!(
        indexer.spec.tolerations.len(),
        1,
        "tolerations must survive"
    );
    assert!(indexer.spec.affinity.is_some(), "affinity must survive");
    assert!(
        !indexer.spec.pod_annotations.is_empty(),
        "podAnnotations must survive"
    );
    assert!(
        !indexer.spec.pod_labels.is_empty(),
        "podLabels must survive"
    );
    assert_eq!(indexer.spec.extra_env.len(), 1, "extraEnv must survive");
    let first = serde_json::to_string(&indexer).expect("serialize must succeed");
    let reparsed: GraphIndexer = serde_json::from_str(&first).expect("re-parse must succeed");
    let second = serde_json::to_string(&reparsed).expect("re-serialize must succeed");
    assert_eq!(
        first, second,
        "GraphIndexer to_string -> from_str -> to_string must be byte-identical"
    );
}

// --- 2. unknown-variant matrix for every extensible enum -------------------

#[test]
fn extensible_enum_build_mode_unknown_round_trip() {
    let known: BuildMode = serde_json::from_str("\"full\"").expect("known BuildMode must parse");
    assert_eq!(known, BuildMode::Full, "known variant must map exactly");
    let parsed: BuildMode =
        serde_json::from_str("\"hyper-full\"").expect("unknown BuildMode string must not error");
    assert_eq!(
        parsed,
        BuildMode::Unknown("hyper-full".to_string()),
        "unknown BuildMode must land in Unknown"
    );
    assert_eq!(
        serde_json::to_string(&parsed).expect("serialize must succeed"),
        "\"hyper-full\"",
        "unknown BuildMode must re-serialize verbatim"
    );
}

#[test]
fn extensible_enum_cache_type_unknown_round_trip() {
    let known: CacheType =
        serde_json::from_str("\"persistentVolume\"").expect("known CacheType must parse");
    assert_eq!(
        known,
        CacheType::PersistentVolume,
        "known variant must map exactly"
    );
    let parsed: CacheType =
        serde_json::from_str("\"future-cache\"").expect("unknown CacheType string must not error");
    assert_eq!(
        parsed,
        CacheType::Unknown("future-cache".to_string()),
        "unknown CacheType must land in Unknown"
    );
    assert_eq!(
        serde_json::to_string(&parsed).expect("serialize must succeed"),
        "\"future-cache\"",
        "unknown CacheType must re-serialize verbatim"
    );
}

#[test]
fn extensible_enum_service_type_unknown_round_trip() {
    let known: ServiceType =
        serde_json::from_str("\"NodePort\"").expect("known ServiceType must parse");
    assert_eq!(
        known,
        ServiceType::NodePort,
        "known variant must map exactly"
    );
    let parsed: ServiceType = serde_json::from_str("\"HyperClusterIP\"")
        .expect("unknown ServiceType string must not error");
    assert_eq!(
        parsed,
        ServiceType::Unknown("HyperClusterIP".to_string()),
        "unknown ServiceType must land in Unknown"
    );
    assert_eq!(
        serde_json::to_string(&parsed).expect("serialize must succeed"),
        "\"HyperClusterIP\"",
        "unknown ServiceType must re-serialize verbatim"
    );
}

#[test]
fn extensible_enum_pull_policy_unknown_round_trip() {
    let known: PullPolicy = serde_json::from_str("\"Never\"").expect("known PullPolicy must parse");
    assert_eq!(known, PullPolicy::Never, "known variant must map exactly");
    let parsed: PullPolicy =
        serde_json::from_str("\"Sometimes\"").expect("unknown PullPolicy string must not error");
    assert_eq!(
        parsed,
        PullPolicy::Unknown("Sometimes".to_string()),
        "unknown PullPolicy must land in Unknown"
    );
    assert_eq!(
        serde_json::to_string(&parsed).expect("serialize must succeed"),
        "\"Sometimes\"",
        "unknown PullPolicy must re-serialize verbatim"
    );
}

#[test]
fn extensible_enum_condition_reason_unknown_round_trip() {
    let known: ConditionReason =
        serde_json::from_str("\"Degraded\"").expect("known ConditionReason must parse");
    assert_eq!(
        known,
        ConditionReason::Degraded,
        "known variant must map exactly"
    );
    let parsed: ConditionReason = serde_json::from_str("\"QuantumFlux\"")
        .expect("unknown ConditionReason string must not error");
    assert_eq!(
        parsed,
        ConditionReason::Unknown("QuantumFlux".to_string()),
        "unknown ConditionReason must land in Unknown"
    );
    assert_eq!(
        serde_json::to_string(&parsed).expect("serialize must succeed"),
        "\"QuantumFlux\"",
        "unknown ConditionReason must re-serialize verbatim"
    );
}

// --- 3. documented defaults for every Option --------------------------------

#[test]
fn indexer_minimal_spec_carries_every_documented_default() {
    let value = json!({
        "apiVersion": "hydradb.kubexa.tech/v1alpha1",
        "kind": "GraphIndexer",
        "metadata": { "name": "example-indexer", "namespace": "default" },
        "spec": {
            "clusterRef": { "name": "example" },
            "enabled": true,
            "replicas": 2
        }
    });
    let indexer: GraphIndexer = serde_json::from_value(value).expect("minimal indexer must parse");
    assert_eq!(indexer.spec.interval_ms, Some(5000), "intervalMs default");
    assert_eq!(
        indexer.spec.dirty_poll_interval_ms,
        Some(250),
        "dirtyPollIntervalMs default"
    );
    assert_eq!(
        indexer.spec.retain_previous_generations,
        Some(1),
        "retainPreviousGenerations default"
    );
    assert_eq!(
        indexer.spec.build_mode,
        Some(BuildMode::Full),
        "buildMode default"
    );
    assert_eq!(
        indexer.spec.incremental_min_edges,
        Some(250_000),
        "incrementalMinEdges default"
    );
    assert_eq!(
        indexer.spec.scope_concurrency,
        Some(8),
        "scopeConcurrency default"
    );
    assert_eq!(
        indexer.spec.scopes_per_cycle,
        Some(256),
        "scopesPerCycle default"
    );
    assert_eq!(
        indexer.spec.max_open_scopes,
        Some(128),
        "maxOpenScopes default"
    );
    assert_eq!(
        indexer.spec.readiness_failure_threshold,
        Some(3),
        "readinessFailureThreshold default"
    );
    assert_eq!(
        indexer.spec.max_wal_tail_files,
        Some(4096),
        "maxWalTailFiles default"
    );
    assert_eq!(
        indexer.spec.wal_tail_fetch_concurrency,
        Some(16),
        "walTailFetchConcurrency default"
    );
}

#[test]
fn cluster_minimal_spec_carries_service_and_policy_defaults() {
    let value = json!({
        "apiVersion": "hydradb.kubexa.tech/v1alpha1",
        "kind": "GraphCluster",
        "metadata": { "name": "example", "namespace": "default" },
        "spec": {
            "image": { "repository": "example.com/org/hydradb" },
            "graph": {
                "namespace": "production",
                "id": "default",
                "database": "default",
                "cellId": "cell-0"
            },
            "objectStore": { "explicit": { "bucket": "my-bucket" } }
        }
    });
    let cluster: GraphCluster = serde_json::from_value(value).expect("minimal cluster must parse");
    assert_eq!(cluster.spec.service.bolt_port, 7687, "boltPort default");
    assert_eq!(cluster.spec.service.https_port, 443, "httpsPort default");
    assert_eq!(cluster.spec.service.admin_port, 9090, "adminPort default");
    assert_eq!(
        cluster.spec.deletion_policy,
        DeletionPolicy::Retain,
        "deletionPolicy defaults to Retain"
    );
}

#[test]
fn nodepool_minimal_spec_leaves_optionals_absent() {
    let value = json!({
        "apiVersion": "hydradb.kubexa.tech/v1alpha1",
        "kind": "GraphNodePool",
        "metadata": { "name": "pool-0", "namespace": "default" },
        "spec": {
            "clusterRef": { "name": "example" },
            "replicas": 3,
            "cache": { "storage": { "emptyDir": {} } }
        }
    });
    let pool: GraphNodePool = serde_json::from_value(value).expect("minimal pool must parse");
    assert_eq!(pool.spec.image, None, "image stays absent");
    assert_eq!(pool.spec.resources, None, "resources stay absent");
    assert_eq!(
        pool.spec.cache.memory_limit_bytes, None,
        "cache.memoryLimitBytes stays absent"
    );
    assert_eq!(pool.spec.tmp_size_limit, None, "tmpSizeLimit stays absent");
    assert_eq!(
        pool.spec.service_account, None,
        "serviceAccount stays absent"
    );
    assert_eq!(
        pool.spec.priority_class_name, None,
        "priorityClassName stays absent"
    );
    assert_eq!(
        pool.spec.termination_grace_period_seconds, None,
        "terminationGracePeriodSeconds stays absent"
    );
    assert_eq!(
        pool.spec.update_strategy, None,
        "updateStrategy stays absent"
    );
    assert_eq!(pool.spec.affinity, None, "affinity stays absent");
    assert!(
        pool.spec.topology_spread_constraints.is_empty(),
        "topologySpreadConstraints stay empty"
    );
    assert!(pool.spec.tolerations.is_empty(), "tolerations stay empty");
    assert_eq!(pool.spec.node_selector, None, "nodeSelector stays absent");
    assert!(pool.spec.extra_env.is_empty(), "extraEnv stays empty");
    assert!(
        pool.spec.pod_annotations.is_empty(),
        "podAnnotations stay empty"
    );
    assert!(pool.spec.pod_labels.is_empty(), "podLabels stay empty");
    assert_eq!(pool.spec.service, None, "service stays absent");
}

// --- 6. deny_unknown_fields at the nested-schema level -----------------------
//
// NOTE (deviation from the task text): `deny_unknown_fields` is enforced via
// `#[schemars(deny_unknown_fields)]`, i.e. at the JSON-Schema level that feeds
// the CRD. The serde layer has no `#[serde(deny_unknown_fields)]`, so an
// unknown key on the Rust wire is ignored rather than rejected. Changing that
// would touch `crates/api/src/**`, which this todo forbids; the gap is filed
// in `.omo/notepads/hydradb-operator/issues.md`. This test pins the schema
// side for every nested spec struct so the CRD surface really does deny.

#[test]
fn nested_spec_schemas_deny_unknown_fields() {
    for (schema, name) in [
        (
            schemars::schema_for!(hydradb_operator_api::crd::GraphIdentity),
            "GraphIdentity",
        ),
        (
            schemars::schema_for!(hydradb_operator_api::crd::AuthSpec),
            "AuthSpec",
        ),
        (
            schemars::schema_for!(hydradb_operator_api::crd::TlsSpec),
            "TlsSpec",
        ),
        (
            schemars::schema_for!(hydradb_operator_api::crd::ServiceSpec),
            "ServiceSpec",
        ),
        (
            schemars::schema_for!(hydradb_operator_api::crd::RuntimeSpec),
            "RuntimeSpec",
        ),
        (
            schemars::schema_for!(hydradb_operator_api::spec::Image),
            "Image",
        ),
        (schemars::schema_for!(hydradb_operator_api::spec::S3), "S3"),
        (
            schemars::schema_for!(hydradb_operator_api::spec::ObjectStore),
            "ObjectStore",
        ),
        (
            schemars::schema_for!(hydradb_operator_api::spec::EmptyDirCache),
            "EmptyDirCache",
        ),
        (
            schemars::schema_for!(hydradb_operator_api::spec::PersistentVolumeCache),
            "PersistentVolumeCache",
        ),
        (
            schemars::schema_for!(hydradb_operator_api::crd::CacheSpec),
            "CacheSpec",
        ),
        (
            schemars::schema_for!(hydradb_operator_api::crd::UpdateStrategy),
            "UpdateStrategy",
        ),
        (
            schemars::schema_for!(hydradb_operator_api::crd::NodeServiceSpec),
            "NodeServiceSpec",
        ),
    ] {
        assert_eq!(
            schema.get("additionalProperties"),
            Some(&serde_json::json!(false)),
            "{name} schema must deny unknown fields"
        );
    }
}
