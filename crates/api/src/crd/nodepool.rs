//! `GraphNodePool` custom resource: a pool of graph worker nodes.
//!
//! A StatefulSet with a headless Service; peers address each other by stable
//! identity. Object-store, auth, TLS and graph identity live on
//! [`crate::crd::GraphCluster`], never here.

use std::collections::BTreeMap;

use k8s_openapi::api::core::v1::{
    Affinity, EnvVar, LocalObjectReference, ResourceRequirements, Toleration,
    TopologySpreadConstraint,
};
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::spec::{AutoscalingSpec, CacheStorage, Image, NodeRole, strict_option};

fn default_role() -> Option<NodeRole> {
    Some(NodeRole::All)
}

/// On-disk cache configuration for pool nodes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(deny_unknown_fields)]
pub struct CacheSpec {
    /// Disposable on-disk object-store cache backend (exactly one variant).
    pub storage: CacheStorage,
    /// RAM budget for the on-disk cache in bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_limit_bytes: Option<u64>,
}

/// StatefulSet update strategy for the pool.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(deny_unknown_fields)]
pub struct UpdateStrategy {
    /// Strategy type (`RollingUpdate` or `OnDelete`).
    pub r#type: String,
    /// Partition for partitioned rolling updates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rolling_update_partition: Option<i32>,
}

/// Per-pool Service overrides for the worker StatefulSet.
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(deny_unknown_fields)]
pub struct NodeServiceSpec {
    /// Advertised bolt address override; empty means pod DNS.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub advertised_bolt_address: Option<String>,
    /// Bolt Service type override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bolt_type: Option<String>,
}

/// Desired state of a graph worker pool.
#[derive(CustomResource, Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(deny_unknown_fields)]
#[kube(
    group = "hydradb.kubexa.tech",
    version = "v1alpha1",
    kind = "GraphNodePool",
    namespaced,
    status = "GraphNodePoolStatus",
    shortname = "hgnp",
    category = "hydradb",
    printcolumn = "{\"name\":\"Ready\",\"type\":\"integer\",\"jsonPath\":\".status.readyReplicas\"}",
    printcolumn = "{\"name\":\"Replicas\",\"type\":\"integer\",\"jsonPath\":\".spec.replicas\"}",
    printcolumn = "{\"name\":\"Age\",\"type\":\"date\",\"jsonPath\":\".metadata.creationTimestamp\"}"
)]
pub struct GraphNodePoolSpec {
    /// reference to the owning `GraphCluster` (`.name` only).
    pub cluster_ref: LocalObjectReference,
    /// Role this pool plays in the cluster (default `all`).
    ///
    /// `writer` pools contend for the durable writer lease, `reader` pools
    /// never do, and `all` keeps the pre-role behaviour.
    #[serde(default = "default_role", deserialize_with = "strict_option")]
    pub role: Option<NodeRole>,
    /// Desired worker replica count. Ignored while `autoscaling` is set, which
    /// then becomes the metric target instead of the replica override.
    #[schemars(range(min = 1))]
    pub replicas: i32,
    /// Horizontal pod autoscaling for this pool (opt-in).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub autoscaling: Option<AutoscalingSpec>,
    /// Image override; absent means the cluster's image.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<Image>,
    /// Compute resource requests and limits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resources: Option<ResourceRequirements>,
    /// On-disk cache configuration.
    pub cache: CacheSpec,
    /// Size limit for the `tmp` volume, e.g. `1Gi`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tmp_size_limit: Option<String>,
    /// Service account for worker pods; empty means the default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_account: Option<String>,
    /// Priority class for worker pods.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority_class_name: Option<String>,
    /// Grace period for pod termination in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub termination_grace_period_seconds: Option<i64>,
    /// StatefulSet update strategy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update_strategy: Option<UpdateStrategy>,
    /// Pod affinity and anti-affinity rules.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub affinity: Option<Affinity>,
    /// Pod topology spread constraints.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub topology_spread_constraints: Vec<TopologySpreadConstraint>,
    /// Pod tolerations.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tolerations: Vec<Toleration>,
    /// Pod node selector.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_selector: Option<BTreeMap<String, String>>,
    /// Extra environment variables for worker pods.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_env: Vec<EnvVar>,
    /// Annotations added to worker pods.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub pod_annotations: BTreeMap<String, String>,
    /// Labels added to worker pods.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub pod_labels: BTreeMap<String, String>,
    /// Per-pool Service overrides.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service: Option<NodeServiceSpec>,
}

/// Observed state of a graph worker pool.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(deny_unknown_fields)]
pub struct GraphNodePoolStatus {
    /// Standard Kubernetes conditions (`Ready`, etc.).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition>,
    /// Last reconciled generation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_generation: Option<i64>,
    /// Ready worker replicas.
    #[serde(default)]
    pub ready_replicas: i32,
    /// Current worker replicas.
    #[serde(default)]
    pub current_replicas: i32,
    /// Bolt addresses of every ready node.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bolt_addresses: Vec<String>,
    /// Effective role of this pool.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<NodeRole>,
}
