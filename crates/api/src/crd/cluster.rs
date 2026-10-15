//! `GraphCluster` custom resource: top-level HydraDB cluster identity.
//!
//! Owns the deployment lifecycle inputs the operand can honour directly:
//! image, graph identity, object store, auth, TLS, service ports and runtime
//! knobs. Replica configuration lives in [`crate::crd::GraphNodePool`] and
//! indexing in [`crate::crd::GraphIndexer`], never here.

use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::spec::{Image, ObjectStore};

/// Deletion policy for a `GraphCluster`.
///
/// Todo 8 owns this type; todo 11 owns its schema bounds. Wire values are
/// PascalCase (`"Retain"` / `"Delete"`). `Retain` is the default via a
/// derived [`Default`] impl plus `#[serde(default)]` on the owning field.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "PascalCase")]
pub enum DeletionPolicy {
    /// Keep backing data on cluster deletion (default).
    #[default]
    Retain,
    /// Remove backing data on cluster deletion (opt-in).
    Delete,
}

/// Graph identity served by the cluster (single-cell only).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(deny_unknown_fields)]
pub struct GraphIdentity {
    /// HydraDB graph namespace (`GRAPH_NAMESPACE`).
    pub namespace: String,
    /// HydraDB graph id (`GRAPH_ID`).
    pub id: String,
    /// HydraDB database name (`GRAPH_DATABASE`).
    pub database: String,
    /// Primary cell served by this cluster (`GRAPH_CELL_ID`).
    pub cell_id: String,
}

/// Client-auth configuration for the cluster.
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(deny_unknown_fields)]
pub struct AuthSpec {
    /// Name of an existing Secret holding the token; empty means generated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub existing_secret: Option<String>,
    /// Key inside the auth Secret holding the token.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret_key: Option<String>,
    /// Generate a token when no existing Secret supplies one.
    #[serde(default)]
    pub generate: bool,
    /// Inline token (testing only; prefer a Secret reference).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
}

/// TLS configuration for the cluster endpoints.
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(deny_unknown_fields)]
pub struct TlsSpec {
    /// Enable TLS on the public endpoints.
    #[serde(default)]
    pub enabled: bool,
    /// Secret holding the serving certificate; empty means cert-manager.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret_name: Option<String>,
    /// Extra DNS names on the serving certificate.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub additional_dns_names: Vec<String>,
}

/// Service ports exposed by the cluster.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(deny_unknown_fields)]
pub struct ServiceSpec {
    /// Bolt port (default 7687).
    #[serde(default = "default_bolt_port")]
    #[schemars(range(min = 1, max = 65535))]
    pub bolt_port: u16,
    /// HTTPS port (default 443).
    #[serde(default = "default_https_port")]
    #[schemars(range(min = 1, max = 65535))]
    pub https_port: u16,
    /// Admin/metrics port (default 9090).
    #[serde(default = "default_admin_port")]
    #[schemars(range(min = 1, max = 65535))]
    pub admin_port: u16,
}

fn default_bolt_port() -> u16 {
    7687
}

fn default_https_port() -> u16 {
    443
}

fn default_admin_port() -> u16 {
    9090
}

impl Default for ServiceSpec {
    fn default() -> Self {
        Self {
            bolt_port: default_bolt_port(),
            https_port: default_https_port(),
            admin_port: default_admin_port(),
        }
    }
}

/// Operand runtime knobs (mirrors the chart's `runtime.*` block).
///
/// Defaults match `charts/hydradb/values.yaml`; only the subset the operand
/// honours is modelled here.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(deny_unknown_fields)]
pub struct RuntimeSpec {
    /// Tracing/logging filter directive (`RUST_LOG`).
    #[serde(default = "default_rust_log")]
    pub rust_log: String,
    /// Maximum read-only scope handles retained between cycles.
    #[serde(default = "default_max_open_scopes")]
    pub max_open_scopes: u32,
    /// Process-wide SlateDB RAM cache budget in bytes.
    #[serde(default = "default_slatedb_cache_bytes")]
    pub slatedb_cache_bytes: u64,
    /// Process-wide file handles limit for SlateDB's disk caches.
    #[serde(default = "default_data_cache_max_open_files")]
    pub data_cache_max_open_files: u32,
    /// Parallel WAL reads used to establish a cold SlateDB reader.
    #[serde(default = "default_reader_wal_replay_concurrency")]
    pub reader_wal_replay_concurrency: u32,
    /// Maximum concurrent queries served.
    #[serde(default = "default_max_concurrent_queries")]
    pub max_concurrent_queries: u32,
    /// Maximum edges scanned by a single query.
    #[serde(default = "default_max_query_scan_edges")]
    pub max_query_scan_edges: u64,
}

fn default_rust_log() -> String {
    "info,hydradb=info".to_string()
}

fn default_max_open_scopes() -> u32 {
    8
}

fn default_slatedb_cache_bytes() -> u64 {
    671_088_640
}

fn default_data_cache_max_open_files() -> u32 {
    512
}

fn default_reader_wal_replay_concurrency() -> u32 {
    16
}

fn default_max_concurrent_queries() -> u32 {
    256
}

fn default_max_query_scan_edges() -> u64 {
    1_000_000
}

impl Default for RuntimeSpec {
    fn default() -> Self {
        Self {
            rust_log: default_rust_log(),
            max_open_scopes: default_max_open_scopes(),
            slatedb_cache_bytes: default_slatedb_cache_bytes(),
            data_cache_max_open_files: default_data_cache_max_open_files(),
            reader_wal_replay_concurrency: default_reader_wal_replay_concurrency(),
            max_concurrent_queries: default_max_concurrent_queries(),
            max_query_scan_edges: default_max_query_scan_edges(),
        }
    }
}

/// Prometheus `ServiceMonitor` toggle for the cluster.
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(deny_unknown_fields)]
pub struct ServiceMonitorSpec {
    /// Create a `ServiceMonitor` selecting the cluster metrics Services.
    /// Off by default: enabling it would force prometheus-operator CRDs
    /// to be installed (upstream parity).
    #[serde(default)]
    pub enabled: bool,
}

/// Monitoring integration for the cluster.
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(deny_unknown_fields)]
pub struct MonitoringSpec {
    /// Prometheus `ServiceMonitor` settings.
    #[serde(default)]
    pub service_monitor: ServiceMonitorSpec,
}

/// Advertised cluster endpoints observed by the controller.
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(deny_unknown_fields)]
pub struct Endpoints {
    /// Bolt endpoint (`host:port`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bolt: Option<String>,
    /// HTTPS endpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub https: Option<String>,
    /// Admin/metrics endpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admin: Option<String>,
}

/// Top-level HydraDB cluster.
///
/// `#[kube(scale)]` is deliberately omitted: replica counts live on
/// `GraphNodePool`, never here.
#[derive(CustomResource, Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(deny_unknown_fields)]
#[kube(
    group = "hydradb.helmforge.dev",
    version = "v1alpha1",
    kind = "GraphCluster",
    namespaced,
    status = "GraphClusterStatus",
    shortname = "hgc",
    category = "hydradb",
    printcolumn = "{\"name\":\"Ready\",\"type\":\"string\",\"jsonPath\":\".status.conditions[?(@.type==\\\"Ready\\\")].status\"}",
    printcolumn = "{\"name\":\"Age\",\"type\":\"date\",\"jsonPath\":\".metadata.creationTimestamp\"}"
)]
pub struct GraphClusterSpec {
    /// Operand container image.
    pub image: Image,
    /// Graph identity served by this cluster.
    pub graph: GraphIdentity,
    /// Backing object store (exactly one source; validated by todo 11).
    pub object_store: ObjectStore,
    /// Client-auth configuration.
    #[serde(default)]
    pub auth: AuthSpec,
    /// TLS configuration.
    #[serde(default)]
    pub tls: TlsSpec,
    /// What happens to backing data on cluster deletion.
    #[serde(default)]
    pub deletion_policy: DeletionPolicy,
    /// Service ports.
    #[serde(default)]
    pub service: ServiceSpec,
    /// Operand runtime knobs.
    #[serde(default)]
    pub runtime: RuntimeSpec,
    /// Monitoring integration (ServiceMonitor opt-in, off by default).
    #[serde(default)]
    pub monitoring: MonitoringSpec,
}

/// Observed state of a HydraDB cluster.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(deny_unknown_fields)]
pub struct GraphClusterStatus {
    /// Standard Kubernetes conditions (`Ready`, etc.).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<Condition>,
    /// Last reconciled generation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_generation: Option<i64>,
    /// Advertised endpoints.
    #[serde(default)]
    pub endpoints: Endpoints,
    /// Bolt addresses of every node.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub node_addresses: Vec<String>,
    /// Resolved object-store description (bucket/claim).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_object_store: Option<String>,
}
