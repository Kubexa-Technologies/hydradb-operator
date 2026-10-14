//! TDD tests for todos 8/9/10: the three HydraDB custom resource types.
//!
//! Written BEFORE the implementation (plan mandates TDD): every import below
//! must FAIL to compile until `crd::{cluster,nodepool,indexer}` land.
//! JSON payloads stand in for YAML (serde_json is already an api dependency;
//! no YAML crate may be added per the task constraints — and JSON is a valid
//! YAML 1.2 subset, so the wire contract tested is identical).

use hydradb_operator_api::constants::{GROUP, VERSION};
use hydradb_operator_api::crd::{
    GraphCluster, GraphClusterStatus, GraphIndexer, GraphIndexerStatus, GraphNodePool,
    GraphNodePoolStatus,
};
use hydradb_operator_api::spec::BuildMode;
use kube::CustomResourceExt;
use serde_json::json;

fn minimal_cluster_value() -> serde_json::Value {
    json!({
        "apiVersion": "hydradb.helmforge.dev/v1alpha1",
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
    })
}

#[test]
fn graphcluster_minimal_yaml_deserializes() {
    let cluster: GraphCluster =
        serde_json::from_value(minimal_cluster_value()).expect("minimal cluster must parse");
    assert_eq!(cluster.spec.image.repository, "example.com/org/hydradb");
    assert_eq!(cluster.spec.image.tag, None);
    assert_eq!(cluster.spec.graph.namespace, "production");
    assert_eq!(cluster.spec.graph.cell_id, "cell-0");
    assert!(
        cluster.spec.object_store.explicit.is_some(),
        "explicit store must survive"
    );
    assert_eq!(cluster.spec.object_store.object_bucket_claim, None);
    // Every optional sub-object falls back to its documented default.
    assert_eq!(cluster.spec.auth.existing_secret, None);
    assert_eq!(cluster.spec.auth.secret_key, None);
    assert!(!cluster.spec.auth.generate);
    assert_eq!(cluster.spec.auth.token, None);
    assert!(!cluster.spec.tls.enabled);
    assert_eq!(cluster.spec.tls.secret_name, None);
    assert!(cluster.spec.tls.additional_dns_names.is_empty());
    assert_eq!(
        cluster.spec.deletion_policy,
        hydradb_operator_api::crd::DeletionPolicy::Retain
    );
    assert_eq!(cluster.spec.service.bolt_port, 7687);
    assert_eq!(cluster.spec.service.https_port, 443);
    assert_eq!(cluster.spec.service.admin_port, 9090);
    assert_eq!(cluster.spec.runtime.rust_log, "info,hydradb=info");
    assert_eq!(cluster.spec.runtime.max_open_scopes, 8);
    assert_eq!(cluster.spec.runtime.slatedb_cache_bytes, 671_088_640);
    assert_eq!(cluster.spec.runtime.data_cache_max_open_files, 512);
    assert_eq!(cluster.spec.runtime.reader_wal_replay_concurrency, 16);
    assert_eq!(cluster.spec.runtime.max_concurrent_queries, 256);
    assert_eq!(cluster.spec.runtime.max_query_scan_edges, 1_000_000);
    // Status is absent on a fresh object.
    assert_eq!(cluster.status, None);
    // Status type is still nameable (todo 8 owns the shape).
    fn _assert_status(_: Option<GraphClusterStatus>) {}
}

#[test]
fn graphcluster_missing_object_store_errors() {
    let mut value = minimal_cluster_value();
    value
        .pointer_mut("/spec")
        .expect("spec must exist")
        .as_object_mut()
        .expect("spec must be an object")
        .remove("objectStore");
    let err = serde_json::from_value::<GraphCluster>(value).expect_err("objectStore is required");
    // serde reports the camelCase wire key (`objectStore`), not the Rust ident.
    assert!(
        err.to_string().contains("objectStore"),
        "error must name the missing key, got: {err}"
    );
}

#[test]
fn graphnodepool_cache_union_rejects_both_keys() {
    let value = json!({
        "apiVersion": "hydradb.helmforge.dev/v1alpha1",
        "kind": "GraphNodePool",
        "metadata": { "name": "pool-0", "namespace": "default" },
        "spec": {
            "clusterRef": { "name": "example" },
            "replicas": 3,
            "cache": {
                "storage": {
                    "emptyDir": { "sizeLimit": "20Gi" },
                    "persistentVolume": { "size": "40Gi" }
                }
            }
        }
    });
    let err =
        serde_json::from_value::<GraphNodePool>(value).expect_err("both union keys must fail");
    assert!(
        !err.to_string().is_empty(),
        "union violation must produce an error"
    );
}

#[test]
fn graphnodepool_missing_cluster_ref_errors() {
    let value = json!({
        "apiVersion": "hydradb.helmforge.dev/v1alpha1",
        "kind": "GraphNodePool",
        "metadata": { "name": "pool-0", "namespace": "default" },
        "spec": {
            "replicas": 3,
            "cache": { "storage": { "emptyDir": {} } }
        }
    });
    let err = serde_json::from_value::<GraphNodePool>(value).expect_err("clusterRef is required");
    assert!(
        err.to_string().contains("clusterRef"),
        "error must name the missing key, got: {err}"
    );
}

#[test]
fn graphnodepool_persistent_volume_cache_round_trips() {
    let value = json!({
        "apiVersion": "hydradb.helmforge.dev/v1alpha1",
        "kind": "GraphNodePool",
        "metadata": { "name": "pool-0", "namespace": "default" },
        "spec": {
            "clusterRef": { "name": "example" },
            "replicas": 3,
            "cache": {
                "storage": {
                    "persistentVolume": {
                        "size": "40Gi",
                        "storageClassName": "ceph-block",
                        "accessModes": ["ReadWriteOnce"]
                    }
                }
            }
        }
    });
    let pool: GraphNodePool =
        serde_json::from_value(value).expect("persistentVolume pool must parse");
    match &pool.spec.cache.storage {
        hydradb_operator_api::spec::CacheStorage::PersistentVolume(pv) => {
            assert_eq!(pv.storage_class_name.as_deref(), Some("ceph-block"));
            assert_eq!(pv.access_modes, vec!["ReadWriteOnce".to_string()]);
        }
        other => panic!("expected PersistentVolume variant, got {other:?}"),
    }
    // Status type is nameable.
    fn _assert_status(_: Option<GraphNodePoolStatus>) {}
}

#[test]
fn graphindexer_defaults() {
    let value = json!({
        "apiVersion": "hydradb.helmforge.dev/v1alpha1",
        "kind": "GraphIndexer",
        "metadata": { "name": "example-indexer", "namespace": "default" },
        "spec": {
            "clusterRef": { "name": "example" },
            "enabled": true,
            "replicas": 2
        }
    });
    let indexer: GraphIndexer = serde_json::from_value(value).expect("minimal indexer must parse");
    assert_eq!(indexer.spec.interval_ms, Some(5000));
    assert_eq!(indexer.spec.dirty_poll_interval_ms, Some(250));
    assert_eq!(indexer.spec.retain_previous_generations, Some(1));
    assert_eq!(indexer.spec.build_mode, Some(BuildMode::Full));
    assert_eq!(indexer.spec.incremental_min_edges, Some(250_000));
    assert_eq!(indexer.spec.scope_concurrency, Some(8));
    assert_eq!(indexer.spec.scopes_per_cycle, Some(256));
    assert_eq!(indexer.spec.max_open_scopes, Some(128));
    assert_eq!(indexer.spec.readiness_failure_threshold, Some(3));
    assert_eq!(indexer.spec.max_wal_tail_files, Some(4096));
    assert_eq!(indexer.spec.wal_tail_fetch_concurrency, Some(16));
    assert_eq!(indexer.spec.image, None);
    // Status type is nameable.
    fn _assert_status(_: Option<GraphIndexerStatus>) {}
}

#[test]
fn graphindexer_unknown_build_mode_is_tolerated() {
    let value = json!({
        "apiVersion": "hydradb.helmforge.dev/v1alpha1",
        "kind": "GraphIndexer",
        "metadata": { "name": "example-indexer", "namespace": "default" },
        "spec": {
            "clusterRef": { "name": "example" },
            "enabled": true,
            "replicas": 1,
            "buildMode": "not-a-mode"
        }
    });
    let indexer: GraphIndexer =
        serde_json::from_value(value).expect("unknown buildMode must not error");
    assert_eq!(
        indexer.spec.build_mode,
        Some(BuildMode::Unknown("not-a-mode".to_string()))
    );
}

#[test]
fn crd_emission_smoke() {
    for (crd, kind) in [
        (GraphCluster::crd(), "GraphCluster"),
        (GraphNodePool::crd(), "GraphNodePool"),
        (GraphIndexer::crd(), "GraphIndexer"),
    ] {
        assert_eq!(crd.spec.group, GROUP, "{kind} group mismatch");
        assert_eq!(crd.spec.names.kind, kind, "kind name mismatch");
        assert_eq!(crd.spec.scope, "Namespaced", "{kind} must be namespaced");
        let versions = &crd.spec.versions;
        assert_eq!(versions.len(), 1, "{kind} must expose exactly one version");
        assert_eq!(versions[0].name, VERSION, "{kind} version mismatch");
        assert!(versions[0].served, "{kind} version must be served");
        assert!(versions[0].storage, "{kind} version must be storage");
        let columns = &versions[0].additional_printer_columns;
        assert!(
            columns.as_ref().is_some_and(|c| !c.is_empty()),
            "{kind} must print at least one column"
        );
    }
}
