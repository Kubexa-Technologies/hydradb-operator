//! envtest integration suite for the three reconcilers (todo 26).
//!
//! Each test boots its own throwaway control plane via `envtest` 0.3
//! (`Environment::default().with_crds(...).create()`), installs the three
//! HydraDB CRDs from the Rust types, and exercises the full lifecycle
//! through the real `reconcile()` entrypoints: apply -> children ->
//! status -> delete. No live cluster is needed: envtest downloads its own
//! kube-apiserver/etcd binaries on first run (or reuses
//! `KUBEBUILDER_ASSETS` when set).
//!
//! Binary mechanism (also recorded in `.omo/notepads/hydradb-operator/learnings.md`
//! for CI): with default settings envtest fetches versioned
//! kube-apiserver/etcd tarballs from the controller-tools release index
//! into the kubebuilder default assets directory
//! (`~/.local/share/kubebuilder-envtest`), honouring `KUBEBUILDER_ASSETS`
//! when set. The build itself needs Go + libclang (rust2go bindgen).

// Gated behind the envtest-tests feature: the suite compiles envtest, whose build
// script shells out to "go build", so it must not be part of a default workspace
// build. Run it with: cargo test -p hydradb-operator --features envtest-tests
#![cfg(feature = "envtest-tests")]

use std::sync::Arc;
use std::time::Duration;

use hydradb_operator::context::{Ctx, WatchScope};
use hydradb_operator::reconcile::{cluster, indexer, nodepool};
use hydradb_operator_api::crd::{GraphCluster, GraphIndexer, GraphNodePool};
use k8s_openapi::api::apps::v1::{Deployment, StatefulSet};
use k8s_openapi::api::core::v1::{ConfigMap, Secret, Service};
use k8s_openapi::api::policy::v1::PodDisruptionBudget;
use kube::Resource as _;
use kube::api::{Api, DeleteParams, ListParams, PostParams};
use kube::{Client, CustomResourceExt};

const NAMESPACE: &str = "default";

/// Boot one isolated control plane with the three HydraDB CRDs installed.
///
/// A minimal `servicemonitors.monitoring.coreos.com` CRD rides along: the
/// cluster reconciler probes that group with `get_opt`, and a bare
/// apiserver answers unknown groups with a non-`Status` 404 page that kube
/// cannot map to `None`. The stub CRD makes the probe return a proper
/// `NotFound` (a real cluster serves one with or without prometheus-operator
/// CRDs installed).
async fn boot_env() -> envtest::Server {
    // Must run before the environment builds its own HTTPS client.
    hydradb_operator::install_crypto_provider();
    let mut crds: Vec<serde_json::Value> = [
        GraphCluster::crd(),
        GraphNodePool::crd(),
        GraphIndexer::crd(),
    ]
    .into_iter()
    .map(|crd| serde_json::to_value(&crd).expect("CRD serializes"))
    .collect();
    crds.push(serde_json::json!({
        "apiVersion": "apiextensions.k8s.io/v1",
        "kind": "CustomResourceDefinition",
        "metadata": {"name": "servicemonitors.monitoring.coreos.com"},
        "spec": {
            "group": "monitoring.coreos.com",
            "scope": "Namespaced",
            "names": {
                "plural": "servicemonitors",
                "singular": "servicemonitor",
                "kind": "ServiceMonitor",
                "listKind": "ServiceMonitorList"
            },
            "versions": [{
                "name": "v1",
                "served": true,
                "storage": true,
                "schema": {"openAPIV3Schema": {
                    "type": "object",
                    "x-kubernetes-preserve-unknown-fields": true
                }}
            }]
        }
    }));
    envtest::Environment::default()
        .with_crds(crds)
        .expect("CRDs serialize")
        .create()
        .await
        .expect("envtest control plane boots")
}

fn ctx_for(client: Client) -> Arc<Ctx> {
    hydradb_operator::install_crypto_provider();
    Arc::new(Ctx::new(client, WatchScope::Namespaced).expect("test context builds"))
}

fn cluster_fixture(name: &str, object_store: serde_json::Value) -> GraphCluster {
    serde_json::from_value(serde_json::json!({
        "apiVersion": "hydradb.kubexa.tech/v1alpha1",
        "kind": "GraphCluster",
        "metadata": {"name": name, "namespace": NAMESPACE},
        "spec": {
            "image": {"repository": "example.com/hydradb", "tag": "v0.1.0"},
            "graph": {"namespace": "ns", "id": "g", "database": "db", "cellId": "c0"},
            "objectStore": object_store,
            "auth": {"generate": true}
        }
    }))
    .expect("cluster fixture deserializes")
}

fn pool_fixture(name: &str, cluster_name: &str) -> GraphNodePool {
    serde_json::from_value(serde_json::json!({
        "apiVersion": "hydradb.kubexa.tech/v1alpha1",
        "kind": "GraphNodePool",
        "metadata": {"name": name, "namespace": NAMESPACE},
        "spec": {
            "clusterRef": {"name": cluster_name},
            "replicas": 1,
            "cache": {"storage": {"emptyDir": {"sizeLimit": "1Gi"}}}
        }
    }))
    .expect("pool fixture deserializes")
}

fn indexer_fixture(name: &str, cluster_name: &str) -> GraphIndexer {
    serde_json::from_value(serde_json::json!({
        "apiVersion": "hydradb.kubexa.tech/v1alpha1",
        "kind": "GraphIndexer",
        "metadata": {"name": name, "namespace": NAMESPACE},
        "spec": {
            "clusterRef": {"name": cluster_name},
            "enabled": true,
            "replicas": 1
        }
    }))
    .expect("indexer fixture deserializes")
}

fn ready_of(conditions: &[k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition]) -> &str {
    conditions
        .iter()
        .find(|condition| condition.type_ == "Ready")
        .map(|condition| condition.status.as_str())
        .unwrap_or("<missing>")
}

fn condition_of(
    conditions: &[k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition],
    type_: &str,
) -> Option<String> {
    conditions
        .iter()
        .find(|condition| condition.type_ == type_)
        .map(|condition| condition.status.clone())
}

/// Reconcile with a ceiling so a wedged apiserver fails the test loudly
/// instead of hanging the suite.
async fn settle<T, E>(label: &str, future: impl Future<Output = Result<T, E>>) -> T
where
    E: std::fmt::Display,
{
    tokio::time::timeout(Duration::from_secs(120), future)
        .await
        .unwrap_or_else(|_| panic!("{label} settled within 120s"))
        .unwrap_or_else(|error| panic!("{label} succeeded, got error: {error}"))
}

#[tokio::test]
async fn cluster_apply_creates_children_writes_status_and_deletes_cleanly() {
    let server = boot_env().await;
    let client = server.client().expect("envtest client builds");
    let ctx = ctx_for(client.clone());

    let clusters: Api<GraphCluster> = Api::namespaced(client.clone(), NAMESPACE);
    clusters
        .create(
            &PostParams::default(),
            &cluster_fixture(
                "demo",
                serde_json::json!({"explicit": {"bucket": "my-bucket", "region": "us-east-1"}}),
            ),
        )
        .await
        .expect("cluster creates");

    // First pass stamps the finalizer; kube defers the apply body to the
    // next reconcile, mirroring a live controller.
    let live = clusters.get("demo").await.expect("cluster reads back");
    settle(
        "cluster reconcile (finalizer stamp)",
        cluster::reconcile(Arc::new(live), ctx.clone()),
    )
    .await;
    let live = clusters.get("demo").await.expect("cluster reads back");
    settle(
        "cluster reconcile (apply)",
        cluster::reconcile(Arc::new(live), ctx.clone()),
    )
    .await;

    let configmaps: Api<ConfigMap> = Api::namespaced(client.clone(), NAMESPACE);
    let config = configmaps
        .get("demo-config")
        .await
        .expect("shared ConfigMap exists");
    let data = config.data.expect("config carries data");
    assert_eq!(
        data.get("AWS_BUCKET_NAME").map(String::as_str),
        Some("my-bucket"),
        "config must carry the resolved bucket"
    );
    assert!(
        !data.contains_key("AWS_SECRET_ACCESS_KEY"),
        "credentials must never land in the ConfigMap"
    );

    let services: Api<Service> = Api::namespaced(client.clone(), NAMESPACE);
    for name in [
        "demo-node-headless",
        "demo-bolt",
        "demo-https",
        "demo-metrics",
        "demo-indexer-metrics",
    ] {
        services
            .get(name)
            .await
            .unwrap_or_else(|error| panic!("service {name} exists after reconcile: {error}"));
    }

    let secrets: Api<Secret> = Api::namespaced(client.clone(), NAMESPACE);
    let auth = secrets.get("demo-auth").await.expect("auth Secret exists");
    let token_len = auth
        .data
        .as_ref()
        .and_then(|data| data.get("auth-token"))
        .map(|token| token.0.len())
        .unwrap_or(0);
    assert!(token_len > 0, "generated auth token must be non-empty");

    let status = clusters
        .get("demo")
        .await
        .expect("cluster reads back")
        .status
        .expect("status is written");
    assert_eq!(ready_of(&status.conditions), "True");
    assert_eq!(
        status.observed_object_store.as_deref(),
        Some("explicit:my-bucket")
    );

    clusters
        .delete("demo", &DeleteParams::default())
        .await
        .expect("cluster deletes");
    let deleting = clusters.get("demo").await.expect("object lingers");
    assert!(
        deleting.meta().deletion_timestamp.is_some(),
        "finalizer must hold the object in terminating state"
    );
    settle(
        "cluster cleanup reconcile",
        cluster::reconcile(Arc::new(deleting), ctx.clone()),
    )
    .await;
    assert!(
        clusters
            .get_opt("demo")
            .await
            .expect("get_opt works")
            .is_none(),
        "finalizer release must let the object disappear"
    );
    assert!(
        configmaps
            .get_opt("demo-config")
            .await
            .expect("get_opt works")
            .is_none(),
        "cleanup must remove the shared ConfigMap"
    );

    server.destroy().await.expect("envtest tears down");
}

#[tokio::test]
async fn nodepool_apply_creates_statefulset_writes_status_and_deletes_cleanly() {
    let server = boot_env().await;
    let client = server.client().expect("envtest client builds");
    let ctx = ctx_for(client.clone());

    let clusters: Api<GraphCluster> = Api::namespaced(client.clone(), NAMESPACE);
    clusters
        .create(
            &PostParams::default(),
            &cluster_fixture(
                "demo",
                serde_json::json!({"explicit": {"bucket": "my-bucket", "region": "us-east-1"}}),
            ),
        )
        .await
        .expect("owning cluster creates");

    let pools: Api<GraphNodePool> = Api::namespaced(client.clone(), NAMESPACE);
    pools
        .create(&PostParams::default(), &pool_fixture("demo-pool", "demo"))
        .await
        .expect("pool creates");

    let live = pools.get("demo-pool").await.expect("pool reads back");
    settle(
        "pool reconcile (finalizer stamp)",
        nodepool::reconcile(Arc::new(live), ctx.clone()),
    )
    .await;
    let live = pools.get("demo-pool").await.expect("pool reads back");
    settle(
        "pool reconcile (apply)",
        nodepool::reconcile(Arc::new(live), ctx.clone()),
    )
    .await;

    let statefulsets: Api<StatefulSet> = Api::namespaced(client.clone(), NAMESPACE);
    let statefulset = statefulsets
        .get("demo-pool")
        .await
        .expect("node StatefulSet exists");
    let image = statefulset
        .spec
        .as_ref()
        .and_then(|spec| spec.template.spec.as_ref())
        .and_then(|pod| pod.containers.first())
        .and_then(|container| container.image.clone())
        .unwrap_or_default();
    assert!(
        image.starts_with("example.com/hydradb:"),
        "statefulset must run the cluster image, got: {image}"
    );

    let pdbs: Api<PodDisruptionBudget> = Api::namespaced(client.clone(), NAMESPACE);
    pdbs.get("demo-pool").await.expect("pool PDB exists");

    let status = pools
        .get("demo-pool")
        .await
        .expect("pool reads back")
        .status
        .expect("status is written");
    assert!(
        status.observed_generation.is_some(),
        "observedGeneration must track the reconciled generation"
    );

    pools
        .delete("demo-pool", &DeleteParams::default())
        .await
        .expect("pool deletes");
    let deleting = pools.get("demo-pool").await.expect("object lingers");
    assert!(
        deleting.meta().deletion_timestamp.is_some(),
        "finalizer must hold the object in terminating state"
    );
    settle(
        "pool cleanup reconcile",
        nodepool::reconcile(Arc::new(deleting), ctx.clone()),
    )
    .await;
    assert!(
        pools
            .get_opt("demo-pool")
            .await
            .expect("get_opt works")
            .is_none(),
        "finalizer release must let the object disappear"
    );
    assert!(
        statefulsets
            .get_opt("demo-pool")
            .await
            .expect("get_opt works")
            .is_none(),
        "cleanup must remove the StatefulSet"
    );

    server.destroy().await.expect("envtest tears down");
}

#[tokio::test]
async fn indexer_apply_creates_deployment_writes_status_and_deletes_cleanly() {
    let server = boot_env().await;
    let client = server.client().expect("envtest client builds");
    let ctx = ctx_for(client.clone());

    let clusters: Api<GraphCluster> = Api::namespaced(client.clone(), NAMESPACE);
    clusters
        .create(
            &PostParams::default(),
            &cluster_fixture(
                "demo",
                serde_json::json!({"explicit": {"bucket": "my-bucket", "region": "us-east-1"}}),
            ),
        )
        .await
        .expect("owning cluster creates");

    let indexers: Api<GraphIndexer> = Api::namespaced(client.clone(), NAMESPACE);
    indexers
        .create(
            &PostParams::default(),
            &indexer_fixture("demo-indexer", "demo"),
        )
        .await
        .expect("indexer creates");

    let live = indexers
        .get("demo-indexer")
        .await
        .expect("indexer reads back");
    settle(
        "indexer reconcile (finalizer stamp)",
        indexer::reconcile(Arc::new(live), ctx.clone()),
    )
    .await;
    let live = indexers
        .get("demo-indexer")
        .await
        .expect("indexer reads back");
    settle(
        "indexer reconcile (apply)",
        indexer::reconcile(Arc::new(live), ctx.clone()),
    )
    .await;

    let deployments: Api<Deployment> = Api::namespaced(client.clone(), NAMESPACE);
    let deployment = deployments
        .get("demo-indexer")
        .await
        .expect("indexer Deployment exists");
    let command = deployment
        .spec
        .as_ref()
        .and_then(|spec| spec.template.spec.as_ref())
        .and_then(|pod| pod.containers.first())
        .and_then(|container| container.command.clone())
        .unwrap_or_default();
    assert_eq!(
        command,
        vec!["graph-indexer".to_owned()],
        "deployment must run the graph-indexer binary"
    );

    let status = indexers
        .get("demo-indexer")
        .await
        .expect("indexer reads back")
        .status
        .expect("status is written");
    assert!(
        status.observed_generation.is_some(),
        "observedGeneration must track the reconciled generation"
    );

    // No pods run in envtest, so zero ready replicas means Ready=False while
    // the generation is still tracked: converging, not stalled.
    assert_eq!(ready_of(&status.conditions), "False");
    assert_eq!(
        condition_of(&status.conditions, "Stalled").as_deref(),
        Some("False"),
        "a converging indexer must be Stalled=False, got: {:?}",
        status.conditions
    );

    indexers
        .delete("demo-indexer", &DeleteParams::default())
        .await
        .expect("indexer deletes");
    let deleting = indexers.get("demo-indexer").await.expect("object lingers");
    assert!(
        deleting.meta().deletion_timestamp.is_some(),
        "finalizer must hold the object in terminating state"
    );
    settle(
        "indexer cleanup reconcile",
        indexer::reconcile(Arc::new(deleting), ctx.clone()),
    )
    .await;
    assert!(
        indexers
            .get_opt("demo-indexer")
            .await
            .expect("get_opt works")
            .is_none(),
        "finalizer release must let the object disappear"
    );
    assert!(
        deployments
            .get_opt("demo-indexer")
            .await
            .expect("get_opt works")
            .is_none(),
        "cleanup must remove the Deployment"
    );

    server.destroy().await.expect("envtest tears down");
}

#[tokio::test]
async fn cluster_missing_object_store_stalls_instead_of_panicking() {
    let server = boot_env().await;
    let client = server.client().expect("envtest client builds");
    let ctx = ctx_for(client.clone());

    let clusters: Api<GraphCluster> = Api::namespaced(client.clone(), NAMESPACE);
    clusters
        .create(
            &PostParams::default(),
            &cluster_fixture("broken", serde_json::json!({})),
        )
        .await
        .expect("invalid cluster persists (validation is controller-side)");

    let live = clusters.get("broken").await.expect("cluster reads back");
    settle(
        "stalled cluster reconcile (finalizer stamp)",
        cluster::reconcile(Arc::new(live), ctx.clone()),
    )
    .await;
    let live = clusters.get("broken").await.expect("cluster reads back");
    settle(
        "stalled cluster reconcile (stall)",
        cluster::reconcile(Arc::new(live), ctx.clone()),
    )
    .await;

    let status = clusters
        .get("broken")
        .await
        .expect("cluster reads back")
        .status
        .expect("status is written even on the stalled path");
    assert_eq!(
        condition_of(&status.conditions, "Stalled").as_deref(),
        Some("True"),
        "missing objectStore must set Stalled=True, got: {:?}",
        status.conditions
    );

    let configmaps: Api<ConfigMap> = Api::namespaced(client.clone(), NAMESPACE);
    assert!(
        configmaps
            .list(&ListParams::default())
            .await
            .expect("configmaps list")
            .items
            .is_empty(),
        "stalled reconcile must create no children"
    );

    server.destroy().await.expect("envtest tears down");
}
