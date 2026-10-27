//! Shared status-writer helpers for all three kinds (todo 24).
//!
//! Every helper here is pure: no API call, so the whole surface stays
//! unit-testable without a cluster (no live cluster exists for envtest on
//! this host). The reconcilers in [`crate::reconcile`] own the single
//! `Api::list` per child kind; these helpers only shape what that observed
//! state becomes in `status`.

use std::collections::BTreeMap;

use hydradb_operator_api::crd::{
    Endpoints, GraphCluster, GraphClusterStatus, GraphIndexerStatus, GraphNodePoolStatus,
};
use kube::ResourceExt as _;

use crate::apply::{
    CLUSTER_SELECTOR_LABEL, ROLE_BOLT, ROLE_HTTPS, ROLE_METRICS, cluster_child_name,
};
use crate::conditions::StatusUpdate;
use crate::reconcile::cluster::{BOLT_PORT, headless_service_name};

/// Pod label carrying the index generation the indexer pod currently serves.
///
/// No pod template stamps it yet, so [`current_generation_label`] falls back
/// to the live preserved value when no listed pod carries it. The constant
/// exists so the convention is documented in exactly one place when the
/// workload starts stamping it.
pub const INDEX_GENERATION_LABEL: &str = "hydradb.kubexa.tech/index-generation";

/// Label selector scoping every child-state list to one cluster.
///
/// This is the ONLY selector the status writers list with: one `Api::list`
/// per child kind, never N per-object gets.
#[must_use]
pub fn cluster_selector(cluster_name: &str) -> String {
    format!("{CLUSTER_SELECTOR_LABEL}={cluster_name}")
}

/// Label selector for the node pods of one cluster: the cluster selector
/// plus the `component=node` value todo 19's Services select on.
#[must_use]
pub fn node_pod_selector(cluster_name: &str) -> String {
    format!(
        "{},app.kubernetes.io/component=node",
        cluster_selector(cluster_name)
    )
}

/// Label selector for the indexer pods of one cluster.
#[must_use]
pub fn indexer_pod_selector(cluster_name: &str) -> String {
    format!(
        "{},app.kubernetes.io/component=indexer",
        cluster_selector(cluster_name)
    )
}

/// Advertised endpoints for a `GraphCluster`, derived purely from the child
/// Service names and `spec.service` ports: `<svc>.<ns>.svc:<port>`.
///
/// The names match [`crate::reconcile::cluster::desired_services`] exactly,
/// so `status.endpoints.bolt` always names the live Bolt Service; the shape
/// matches what `hydradb endpoints` prints.
#[must_use]
pub fn cluster_endpoints(cluster: &GraphCluster) -> Endpoints {
    let name = cluster.name_any();
    let namespace = cluster.namespace().unwrap_or_default();
    let host = |service: &str, port: u16| format!("{service}.{namespace}.svc:{port}");
    Endpoints {
        bolt: Some(host(
            &cluster_child_name(&name, ROLE_BOLT),
            cluster.spec.service.bolt_port,
        )),
        https: Some(host(
            &cluster_child_name(&name, ROLE_HTTPS),
            cluster.spec.service.https_port,
        )),
        admin: Some(host(
            &cluster_child_name(&name, ROLE_METRICS),
            cluster.spec.service.admin_port,
        )),
    }
}

/// Bolt address of one node pod: `<pod>.<headless>.<ns>.svc.cluster.local:7687`.
///
/// Same host shape as [`crate::reconcile::cluster::routing_table_value`]
/// minus the `pod=` routing-table prefix: one plain address per entry, so
/// `status.nodeAddresses` / `status.boltAddresses` stay a string list.
#[must_use]
pub fn pod_bolt_address(pod_name: &str, cluster_name: &str, namespace: &str) -> String {
    let headless = headless_service_name(cluster_name);
    format!("{pod_name}.{headless}.{namespace}.svc.cluster.local:{BOLT_PORT}")
}

/// Deterministic, sorted, de-duplicated bolt addresses for a pod-name set.
///
/// Empty membership yields an empty list, never a stale table.
#[must_use]
pub fn sorted_bolt_addresses(
    pod_names: &[String],
    cluster_name: &str,
    namespace: &str,
) -> Vec<String> {
    let mut addresses: Vec<String> = pod_names
        .iter()
        .map(|pod| pod_bolt_address(pod, cluster_name, namespace))
        .collect();
    addresses.sort();
    addresses.dedup();
    addresses
}

/// Fully ready only when every desired replica reports ready.
///
/// A partially ready pool/workload is `Ready=False` while
/// `observedGeneration` still tracks `metadata.generation` (the Flux/Argo
/// contract: stalled generation tracking must never mask under-replication).
#[must_use]
pub fn is_fully_ready(ready_replicas: i32, desired_replicas: i32) -> bool {
    desired_replicas > 0 && ready_replicas >= desired_replicas
}

/// Currently served index generation from the listed indexer pod labels.
///
/// The lexicographically-maximum `hydradb.kubexa.tech/index-generation`
/// pod label wins; when no listed pod carries it the live value is preserved
/// verbatim (never invented), and `None` stays `None`.
#[must_use]
pub fn current_generation_label(
    pod_label_sets: &[Option<BTreeMap<String, String>>],
    live: Option<String>,
) -> Option<String> {
    let best = pod_label_sets
        .iter()
        .flatten()
        .filter_map(|labels| labels.get(INDEX_GENERATION_LABEL))
        .filter(|label| !label.is_empty())
        .max()
        .cloned();
    best.or(live)
}

/// Whether the cluster status write must happen: conditions plus every scalar
/// surface (`observedGeneration`, `endpoints`, `nodeAddresses`,
/// `observedObjectStore`).
///
/// `false` means the caller must skip the patch: an identical write would bump
/// `resourceVersion` and hot-loop the reconcile (Flux kstatus/Argo churn).
#[must_use]
pub fn cluster_status_needs_write(live: &GraphClusterStatus, desired: &GraphClusterStatus) -> bool {
    if !matches!(
        crate::conditions::live_conditions(&live.conditions, &desired.conditions),
        StatusUpdate::NoChange
    ) {
        return true;
    }
    live.observed_generation != desired.observed_generation
        || live.endpoints != desired.endpoints
        || live.node_addresses != desired.node_addresses
        || live.observed_object_store != desired.observed_object_store
}

/// Whether the pool status write must happen: conditions plus
/// `observedGeneration`, both replica counts and `boltAddresses`.
#[must_use]
pub fn pool_status_needs_write(live: &GraphNodePoolStatus, desired: &GraphNodePoolStatus) -> bool {
    if !matches!(
        crate::conditions::live_conditions(&live.conditions, &desired.conditions),
        StatusUpdate::NoChange
    ) {
        return true;
    }
    live.observed_generation != desired.observed_generation
        || live.ready_replicas != desired.ready_replicas
        || live.current_replicas != desired.current_replicas
        || live.bolt_addresses != desired.bolt_addresses
}

/// Whether the indexer status write must happen: conditions plus
/// `observedGeneration`, both replica counts and `currentGenerationLabel`.
#[must_use]
pub fn indexer_status_needs_write(live: &GraphIndexerStatus, desired: &GraphIndexerStatus) -> bool {
    if !matches!(
        crate::conditions::live_conditions(&live.conditions, &desired.conditions),
        StatusUpdate::NoChange
    ) {
        return true;
    }
    live.observed_generation != desired.observed_generation
        || live.ready_replicas != desired.ready_replicas
        || live.current_replicas != desired.current_replicas
        || live.current_generation_label != desired.current_generation_label
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cluster_fixture() -> GraphCluster {
        serde_json::from_value(serde_json::json!({
            "apiVersion": "hydradb.kubexa.tech/v1alpha1",
            "kind": "GraphCluster",
            "metadata": {"name": "demo", "namespace": "default", "uid": "abc-123", "generation": 7},
            "spec": {
                "image": {"repository": "example.com/hydradb", "tag": "v0.1.0"},
                "graph": {"namespace": "ns", "id": "g", "database": "db", "cellId": "c0"},
                "objectStore": {
                    "explicit": {"bucket": "my-bucket", "region": "us-east-1"}
                },
                "auth": {"generate": true}
            }
        }))
        .expect("fixture must deserialize")
    }

    #[test]
    fn selectors_are_scoped_by_the_cluster_label() {
        assert_eq!(cluster_selector("demo"), "hydradb.kubexa.tech/cluster=demo");
        for selector in [node_pod_selector("demo"), indexer_pod_selector("demo")] {
            assert!(
                selector.contains("hydradb.kubexa.tech/cluster=demo"),
                "status list selector must carry the cluster label, got: {selector}"
            );
        }
        assert!(
            node_pod_selector("demo").contains("app.kubernetes.io/component=node"),
            "node selector must pin component=node"
        );
        assert!(
            indexer_pod_selector("demo").contains("app.kubernetes.io/component=indexer"),
            "indexer selector must pin component=indexer"
        );
    }

    #[test]
    fn endpoints_name_the_child_services_with_the_spec_ports() {
        let cluster = cluster_fixture();
        let endpoints = cluster_endpoints(&cluster);
        assert_eq!(
            endpoints.bolt.as_deref(),
            Some("demo-bolt.default.svc:7687"),
            "bolt endpoint must name the Bolt Service"
        );
        assert_eq!(
            endpoints.https.as_deref(),
            Some("demo-https.default.svc:443"),
            "https endpoint must name the HTTPS Service"
        );
        assert_eq!(
            endpoints.admin.as_deref(),
            Some("demo-metrics.default.svc:9090"),
            "admin endpoint must name the metrics Service"
        );
    }

    #[test]
    fn endpoints_honor_custom_service_ports() {
        let mut cluster = cluster_fixture();
        cluster.spec.service.bolt_port = 7688;
        cluster.spec.service.https_port = 444;
        cluster.spec.service.admin_port = 9099;
        let endpoints = cluster_endpoints(&cluster);
        assert_eq!(
            endpoints.bolt.as_deref(),
            Some("demo-bolt.default.svc:7688")
        );
        assert_eq!(
            endpoints.https.as_deref(),
            Some("demo-https.default.svc:444")
        );
        assert_eq!(
            endpoints.admin.as_deref(),
            Some("demo-metrics.default.svc:9099")
        );
    }

    #[test]
    fn bolt_addresses_are_sorted_deterministic_and_share_the_routing_table_host() {
        let routing = crate::reconcile::cluster::routing_table_value(
            "demo",
            "default",
            &["pod-b".to_owned(), "pod-a".to_owned()],
        );
        let addresses = sorted_bolt_addresses(
            &["pod-b".to_owned(), "pod-a".to_owned(), "pod-a".to_owned()],
            "demo",
            "default",
        );
        assert_eq!(
            addresses,
            vec![
                "pod-a.demo-node-headless.default.svc.cluster.local:7687".to_owned(),
                "pod-b.demo-node-headless.default.svc.cluster.local:7687".to_owned(),
            ],
            "addresses must be sorted, de-duplicated pod FQDNs"
        );
        for address in &addresses {
            assert!(
                routing.contains(address),
                "address {address} must share the routing-table host shape, got: {routing}"
            );
        }
        assert!(
            sorted_bolt_addresses(&[], "demo", "default").is_empty(),
            "empty membership must yield an empty list, never stale entries"
        );
    }

    #[test]
    fn generation_label_prefers_the_max_pod_label_then_the_live_value() {
        let pods = vec![
            Some(BTreeMap::from([(
                INDEX_GENERATION_LABEL.to_owned(),
                "gen-3".to_owned(),
            )])),
            Some(BTreeMap::from([(
                INDEX_GENERATION_LABEL.to_owned(),
                "gen-7".to_owned(),
            )])),
        ];
        assert_eq!(
            current_generation_label(&pods, Some("gen-1".to_owned())),
            Some("gen-7".to_owned()),
            "the maximum pod label must win over the live value"
        );
        let unlabeled = vec![Some(BTreeMap::new()), None];
        assert_eq!(
            current_generation_label(&unlabeled, Some("gen-1".to_owned())),
            Some("gen-1".to_owned()),
            "pods without the label must preserve the live value verbatim"
        );
        assert_eq!(
            current_generation_label(&[], None),
            None,
            "no pods and no live value must stay None, never invented"
        );
    }

    #[test]
    fn partial_replication_is_not_fully_ready() {
        assert!(is_fully_ready(3, 3));
        assert!(!is_fully_ready(2, 3), "one missing replica is not ready");
        assert!(!is_fully_ready(0, 3), "zero ready replicas are not ready");
        assert!(
            !is_fully_ready(0, 0),
            "zero desired replicas are never ready"
        );
    }

    #[test]
    fn identical_status_needs_no_write_on_any_kind() {
        let cluster_status = GraphClusterStatus {
            indexing: Default::default(),
            conditions: Vec::new(),
            observed_generation: Some(7),
            endpoints: cluster_endpoints(&cluster_fixture()),
            node_addresses: vec![
                "pod-a.demo-node-headless.default.svc.cluster.local:7687".to_owned(),
            ],
            observed_object_store: Some("explicit:my-bucket".to_owned()),
        };
        assert!(
            !cluster_status_needs_write(&cluster_status, &cluster_status),
            "identical cluster status must produce NO write"
        );
        let mut changed = cluster_status.clone();
        changed.node_addresses.push("pod-b".to_owned());
        assert!(
            cluster_status_needs_write(&cluster_status, &changed),
            "changed nodeAddresses must produce a write"
        );
        let mut changed_endpoints = cluster_status.clone();
        changed_endpoints.endpoints.bolt = Some("other".to_owned());
        assert!(
            cluster_status_needs_write(&cluster_status, &changed_endpoints),
            "changed endpoints must produce a write"
        );

        let pool_status = GraphNodePoolStatus {
            role: None,
            conditions: Vec::new(),
            observed_generation: Some(7),
            ready_replicas: 3,
            current_replicas: 3,
            bolt_addresses: vec!["pod-a".to_owned()],
        };
        assert!(
            !pool_status_needs_write(&pool_status, &pool_status),
            "identical pool status must produce NO write"
        );
        let mut changed_pool = pool_status.clone();
        changed_pool.bolt_addresses = Vec::new();
        assert!(
            pool_status_needs_write(&pool_status, &changed_pool),
            "changed boltAddresses must produce a write"
        );

        let indexer_status = GraphIndexerStatus {
            conditions: Vec::new(),
            observed_generation: Some(7),
            ready_replicas: 2,
            current_replicas: 2,
            current_generation_label: Some("gen-7".to_owned()),
        };
        assert!(
            !indexer_status_needs_write(&indexer_status, &indexer_status),
            "identical indexer status must produce NO write"
        );
        let mut changed_indexer = indexer_status.clone();
        changed_indexer.current_replicas = 1;
        assert!(
            indexer_status_needs_write(&indexer_status, &changed_indexer),
            "changed currentReplicas must produce a write"
        );
    }
}
