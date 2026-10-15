//! `GraphIndexer` custom resource: continuous index builder for a cluster.
//!
//! The indexer publishes immutable generations to the object store and is
//! otherwise stateless, so it runs as a Deployment. Indexing is continuous:
//! there is deliberately no schedule/cron field and no PVC cache.

use std::collections::BTreeMap;

use k8s_openapi::api::core::v1::{
    Affinity, EnvVar, LocalObjectReference, ResourceRequirements, Toleration,
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::spec::{BuildMode, Image, strict_option};

fn default_interval_ms() -> Option<u64> {
    Some(5000)
}

fn default_dirty_poll_interval_ms() -> Option<u64> {
    Some(250)
}

fn default_retain_previous_generations() -> Option<u32> {
    Some(1)
}

fn default_build_mode() -> Option<BuildMode> {
    Some(BuildMode::Full)
}

fn default_incremental_min_edges() -> Option<u64> {
    Some(250_000)
}

fn default_scope_concurrency() -> Option<u32> {
    Some(8)
}

fn default_scopes_per_cycle() -> Option<u32> {
    Some(256)
}

fn default_max_open_scopes() -> Option<u32> {
    Some(128)
}

fn default_readiness_failure_threshold() -> Option<i32> {
    Some(3)
}

fn default_max_wal_tail_files() -> Option<u32> {
    Some(4096)
}

fn default_wal_tail_fetch_concurrency() -> Option<u32> {
    Some(16)
}

/// Desired state of a cluster index builder.
#[derive(CustomResource, Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(deny_unknown_fields)]
#[kube(
    group = "hydradb.helmforge.dev",
    version = "v1alpha1",
    kind = "GraphIndexer",
    namespaced,
    status = "GraphIndexerStatus",
    shortname = "hgi",
    category = "hydradb",
    printcolumn = "{\"name\":\"Ready\",\"type\":\"integer\",\"jsonPath\":\".status.readyReplicas\"}",
    printcolumn = "{\"name\":\"Replicas\",\"type\":\"integer\",\"jsonPath\":\".spec.replicas\"}",
    printcolumn = "{\"name\":\"Age\",\"type\":\"date\",\"jsonPath\":\".metadata.creationTimestamp\"}"
)]
pub struct GraphIndexerSpec {
    /// reference to the owning `GraphCluster` (`.name` only).
    pub cluster_ref: LocalObjectReference,
    /// Whether the indexer Deployment runs.
    pub enabled: bool,
    /// Desired indexer replica count.
    #[schemars(range(min = 1))]
    pub replicas: i32,
    /// Image override; absent means the cluster's image.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<Image>,
    /// Scheduler cycle interval in ms (default 5000).
    #[serde(default = "default_interval_ms")]
    pub interval_ms: Option<u64>,
    /// Changed-graph discovery poll interval in ms (default 250).
    #[serde(default = "default_dirty_poll_interval_ms")]
    pub dirty_poll_interval_ms: Option<u64>,
    /// Previous index generations retained (default 1).
    #[serde(default = "default_retain_previous_generations")]
    pub retain_previous_generations: Option<u32>,
    /// Build strategy (default `full`); unknown strings stay `Unknown`.
    #[serde(
        default = "default_build_mode",
        deserialize_with = "strict_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub build_mode: Option<BuildMode>,
    /// Edge-count floor below which incremental mode uses the full path.
    #[serde(default = "default_incremental_min_edges")]
    pub incremental_min_edges: Option<u64>,
    /// Scopes indexed concurrently (default 8).
    #[serde(default = "default_scope_concurrency")]
    pub scope_concurrency: Option<u32>,
    /// Scopes processed per scheduler cycle (default 256).
    #[serde(default = "default_scopes_per_cycle")]
    pub scopes_per_cycle: Option<u32>,
    /// Read-only scope handles retained between cycles (default 128).
    #[serde(default = "default_max_open_scopes")]
    pub max_open_scopes: Option<u32>,
    /// Consecutive failed cycles before the Pod goes unready (default 3).
    #[serde(default = "default_readiness_failure_threshold")]
    pub readiness_failure_threshold: Option<i32>,
    /// WAL-tail size cap before falling back to a full build (default 4096).
    #[serde(default = "default_max_wal_tail_files")]
    pub max_wal_tail_files: Option<u32>,
    /// Concurrent WAL object reads per incremental build (default 16).
    #[serde(default = "default_wal_tail_fetch_concurrency")]
    pub wal_tail_fetch_concurrency: Option<u32>,
    /// Compute resource requests and limits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resources: Option<ResourceRequirements>,
    /// Service account for indexer pods; empty means the default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_account: Option<String>,
    /// Pod node selector.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_selector: Option<BTreeMap<String, String>>,
    /// Pod tolerations.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tolerations: Vec<Toleration>,
    /// Pod affinity and anti-affinity rules.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub affinity: Option<Affinity>,
    /// Annotations added to indexer pods.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub pod_annotations: BTreeMap<String, String>,
    /// Labels added to indexer pods.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub pod_labels: BTreeMap<String, String>,
    /// Extra environment variables for indexer pods.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_env: Vec<EnvVar>,
}

/// Observed state of a cluster index builder.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(deny_unknown_fields)]
pub struct GraphIndexerStatus {
    /// Standard Kubernetes conditions (`Ready`, etc.).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<Condition>,
    /// Last reconciled generation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_generation: Option<i64>,
    /// Ready indexer replicas.
    #[serde(default)]
    pub ready_replicas: i32,
    /// Current indexer replicas (observed on the Deployment).
    #[serde(default)]
    pub current_replicas: i32,
    /// Label of the currently served index generation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_generation_label: Option<String>,
}
