//! `GraphCluster` custom resource: top-level HydraDB cluster identity.
//!
//! Owns the deployment lifecycle inputs the operand can honour directly:
//! image, graph identity, object store, auth, TLS, service ports and runtime
//! knobs. Replica configuration lives in [`crate::crd::GraphNodePool`] and
//! indexing in [`crate::crd::GraphIndexer`], never here.

use k8s_openapi::api::core::v1::EnvVar;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::spec::{Image, IssuerRef, ObjectStore, PrometheusRuleSpec, S3, TuningSpec};

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
    /// Root tenant namespace (`GRAPH_NAMESPACE`).
    pub namespace: String,
    /// Nested subnamespaces below the root, outermost first.
    ///
    /// The operand addresses a graph inside a namespace tree whose depth is at
    /// most eight levels including the root, so at most seven entries are
    /// allowed here. Each entry is a single component (no dots); the controller
    /// joins the whole path for the node's `GRAPH_SUBNAMESPACE_PATH` env var and
    /// keeps the root in `GRAPH_NAMESPACE`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subnamespaces: Vec<String>,
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
    /// Secret holding the serving certificate. Mutually exclusive with
    /// `issuerRef`; at least one of the two is required when `enabled`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret_name: Option<String>,
    /// cert-manager issuer used to obtain the serving certificate.
    ///
    /// When set, the controller owns a `cert-manager.io/v1 Certificate` named
    /// `<cluster>-tls` whose secret lands in `secretName`'s slot (that name, or
    /// `<cluster>-tls` when `secretName` is absent).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issuer_ref: Option<IssuerRef>,
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
    /// Prometheus `PrometheusRule` settings (opt-in, off by default).
    #[serde(default)]
    pub prometheus_rule: PrometheusRuleSpec,
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

/// Source of a one-shot restore performed before the cluster starts serving.
///
/// The operator runs the restore job once, waits for it, and only then scales
/// the node pools up. The job image must implement the documented backup
/// contract (see `docs/backup.md`); the operator injects every `HYDRADB_BACKUP_*`
/// and `AWS_*` variable itself.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(deny_unknown_fields)]
pub struct RestoreSource {
    /// Backup location: bucket, endpoint, region and credentials.
    pub store: S3,
    /// Prefix inside the bucket holding the copy (the backup's `target.prefix`).
    ///
    /// Absent means the bucket root, which only makes sense when the bucket
    /// holds exactly one backup.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
    /// Image running the restore job.
    pub image: Image,
    /// Verify the restored object set before the cluster starts.
    #[serde(default = "default_true")]
    pub verify: bool,
}

fn default_true() -> bool {
    true
}

/// Index-build progress observed from the indexer's metrics endpoint.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(deny_unknown_fields)]
pub struct IndexingStatus {
    /// Generation label currently published by the indexer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<String>,
    /// Monotonic count of published index generations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_generations: Option<u64>,
    /// Incremental builds that declined and fell back to a full build.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub incremental_fallbacks: Option<u64>,
    /// Generation build failures.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation_failures: Option<u64>,
    /// Seconds since `publishedGenerations` last advanced. This is the
    /// operator's staleness signal: it grows only while no new generation
    /// lands, so an alert on it catches a stalled indexer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale_seconds: Option<u64>,
    /// RFC 3339 timestamp of the last successful metrics scrape.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_at: Option<String>,
    /// Whether the last scrape reached the indexer metrics endpoint.
    #[serde(default)]
    pub reachable: bool,
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
    /// Typed tuning surface for the operand's optional limits.
    ///
    /// Preferred over `extraEnv`: every leaf is validated and only set leaves
    /// produce an env var.
    #[serde(default)]
    pub tuning: TuningSpec,
    /// Extra environment variables applied to every operand pod of the cluster.
    ///
    /// Pool- and indexer-level `extraEnv` are merged on top of these, so a
    /// pool can override a cluster-wide default. Variables that the operator
    /// itself owns (`GRAPH_NAMESPACE`, `GRAPH_ID`, ...) cannot be overridden:
    /// those are rejected during validation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_env: Vec<EnvVar>,
    /// One-shot restore performed before the cluster starts serving.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restore_from: Option<RestoreSource>,
    /// Monitoring integration (ServiceMonitor/PrometheusRule opt-in).
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
    /// Index-build progress observed from the indexer.
    #[serde(default)]
    pub indexing: IndexingStatus,
}
