//! Shared spec primitives reused by every HydraDB CRD.
//!
//! These types are defined once here and referenced from the per-kind specs
//! in `crate::crd`. They intentionally carry no `Default` impl that would
//! silently pick a backend: an absent [`ObjectStore`] or [`CacheStorage`]
//! must stay representable and be rejected by [`crate::validation`], never
//! resolved to a hidden default.

use k8s_openapi::api::core::v1::LocalObjectReference;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Container image reference for an operand component.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(deny_unknown_fields)]
pub struct Image {
    /// Image repository (registry + path), e.g. `example.com/org/hydradb`.
    pub repository: String,
    /// Image tag. Mutually combinable with `digest`; never `:latest`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    /// Pinned image digest (without the `sha256:` prefix handling by runtime).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
    /// Kubernetes pull policy (`IfNotPresent`, `Always`, `Never`).
    ///
    /// Unknown strings decode to [`PullPolicy::Unknown`] rather than erroring;
    /// an absent field stays [`None`] via [`strict_option`].
    #[serde(
        default,
        deserialize_with = "strict_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub pull_policy: Option<PullPolicy>,
    /// Secrets used to pull the image.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pull_secrets: Vec<LocalObjectReference>,
}

/// Explicit S3-compatible object-store configuration.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(deny_unknown_fields)]
pub struct S3 {
    /// Bucket name. Required when `explicit` is selected.
    pub bucket: String,
    /// Custom endpoint override. Empty/absent means the provider default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    /// Region, e.g. `us-east-1`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    /// Allow plain-HTTP endpoints (non-TLS, testing only).
    #[serde(default)]
    pub allow_http: bool,
    /// Secret holding the object-store credentials.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credentials_secret_ref: Option<LocalObjectReference>,
}

/// Object-store backend selection.
///
/// Exactly one source must be set; the all-`None` state is representable and
/// rejected by validation (todo 11), never silently defaulted.
///
/// A bare backend string is not a valid construction path: there is deliberately
/// no `From<&str>` (or any stringly) impl, so a literal cannot become a store:
///
/// ```compile_fail
/// let store = hydradb_operator_api::spec::ObjectStore::from("my-bucket");
/// ```
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(deny_unknown_fields)]
pub struct ObjectStore {
    /// Inline S3-compatible configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explicit: Option<S3>,
    /// Reference to an `ObjectBucketClaim` provisioning the bucket.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub object_bucket_claim: Option<LocalObjectReference>,
}

/// In-memory `emptyDir` cache configuration.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(deny_unknown_fields)]
pub struct EmptyDirCache {
    /// Maximum size of the `emptyDir` volume, e.g. `20Gi`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_limit: Option<String>,
}

/// Persistent-volume cache configuration.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(deny_unknown_fields)]
pub struct PersistentVolumeCache {
    /// Requested volume size, e.g. `40Gi`.
    pub size: String,
    /// Storage class; absent means the cluster default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage_class_name: Option<String>,
    /// Volume access modes, e.g. `["ReadWriteOnce"]`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub access_modes: Vec<String>,
}

/// Disposable on-disk object-store cache backend.
///
/// Discriminated union: exactly one variant is present on the wire. There is
/// no `Default` impl; absence must be rejected by validation, never resolved.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum CacheStorage {
    /// Ephemeral `emptyDir`-backed cache.
    EmptyDir(EmptyDirCache),
    /// Persistent-volume-backed cache.
    PersistentVolume(PersistentVolumeCache),
}

#[cfg(test)]
mod tests;

// --- Unknown-fallback extensible enums (todo 7) -----------------------------

/// Wire contract for every externally-extensible string enum.
///
/// A value written by a newer operator (or a future Kubernetes API) that this
/// build does not recognise must decode to `Unknown(raw)` instead of failing
/// the whole object, and must serialise back verbatim so a read-modify-write
/// never mutates a value this build does not understand. An unknown value is
/// never silently treated as a known one: [`PhaseLabel::parse`] returns
/// [`None`] for anything outside [`PhaseLabel::ALL`].
pub trait PhaseLabel: Clone + PartialEq + 'static {
    /// Every canonical variant, in declaration order. Never contains `Unknown`.
    const ALL: &'static [Self];

    /// The stable wire label for this variant; the `Unknown` arm echoes the
    /// stored string verbatim.
    fn label(&self) -> &str;

    /// Build the decode-compat fallback for a non-canonical stored string.
    fn unknown(raw: String) -> Self;

    /// Parse a **canonical** label; [`None`] for anything else (including a
    /// value that would decode to `Unknown`).
    fn parse(s: &str) -> Option<Self> {
        Self::ALL.iter().find(|v| v.label() == s).cloned()
    }

    /// The canonical label set, for the CRD schema and "valid values" messages.
    fn canonical() -> Vec<&'static str> {
        Self::ALL.iter().map(PhaseLabel::label).collect()
    }

    /// Whether this value is the decode-compat fallback.
    fn is_unknown(&self) -> bool {
        Self::parse(self.label()).is_none()
    }
}

/// Give an extensible enum the `Unknown`-tolerant wire contract: `Serialize`
/// echoes [`PhaseLabel::label`], `Deserialize` falls back to `Unknown(raw)`
/// instead of erroring, `Display` prints the same string, and `JsonSchema`
/// publishes **only** the canonical values.
///
/// Deserialization requires a JSON/YAML string (`String::deserialize`), so
/// `Unknown` is never constructible from a non-string: numbers, bools, nulls
/// and objects are hard errors. `#[serde(other)]` is deliberately NOT used:
/// it would drop the original string and break the verbatim round-trip.
macro_rules! phase_serde {
    ($ty:ty, $desc:literal) => {
        impl ::serde::Serialize for $ty {
            fn serialize<S: ::serde::Serializer>(
                &self,
                serializer: S,
            ) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(crate::spec::PhaseLabel::label(self))
            }
        }

        impl<'de> ::serde::Deserialize<'de> for $ty {
            fn deserialize<D: ::serde::Deserializer<'de>>(
                deserializer: D,
            ) -> Result<Self, D::Error> {
                let raw = String::deserialize(deserializer)?;
                Ok(crate::spec::PhaseLabel::parse(&raw)
                    .unwrap_or_else(|| crate::spec::PhaseLabel::unknown(raw)))
            }
        }

        impl ::std::fmt::Display for $ty {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                f.write_str(crate::spec::PhaseLabel::label(self))
            }
        }

        impl ::schemars::JsonSchema for $ty {
            fn schema_name() -> ::std::borrow::Cow<'static, str> {
                stringify!($ty).into()
            }
            fn json_schema(
                _: &mut ::schemars::SchemaGenerator,
            ) -> ::schemars::Schema {
                // Canonical values only: `Unknown` is a decode-compat artifact,
                // never an admissible write. `oneOf` of `const`s (not a flat
                // `enum`) so `Option<Self>` folds back into
                // `enum: [..] + nullable: true` in the generated CRD.
                ::schemars::json_schema!({
                    "description": $desc,
                    "oneOf": <$ty as crate::spec::PhaseLabel>::canonical()
                        .into_iter()
                        .map(|v| ::serde_json::json!({ "type": "string", "const": v }))
                        .collect::<Vec<_>>(),
                })
            }
        }
    };
}

/// Strict `Option` deserializer for [`PhaseLabel`] enums.
///
/// An absent field (via `#[serde(default)]`) or an explicit null stays
/// [`None`]: it NEVER defaults into a safe-looking variant. A present string
/// parses through the enum's `Deserialize` (unknown strings become
/// `Some(Unknown(..))`); a present non-string is an error.
pub fn strict_option<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

/// Indexer build mode (`indexer.buildMode`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BuildMode {
    /// Rebuild the index from the block cache (pre-incremental behaviour).
    Full,
    /// Build incrementally from the WAL tail, falling back to full when the
    /// tail is unavailable or oversized.
    Incremental,
    /// A build-mode string this build does not recognise. Decode-compat only.
    Unknown(String),
}

phase_serde!(BuildMode, "Indexer build mode. Closed enum.");

impl PhaseLabel for BuildMode {
    const ALL: &'static [Self] = &[Self::Full, Self::Incremental];
    fn label(&self) -> &str {
        match self {
            Self::Full => "full",
            Self::Incremental => "incremental",
            Self::Unknown(raw) => raw,
        }
    }
    fn unknown(raw: String) -> Self {
        Self::Unknown(raw)
    }
}

/// Node on-disk cache backend selector (`node.cache.type`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CacheType {
    /// Ephemeral `emptyDir`-backed cache.
    EmptyDir,
    /// Persistent-volume-backed cache.
    PersistentVolume,
    /// A cache-type string this build does not recognise. Decode-compat only.
    Unknown(String),
}

phase_serde!(CacheType, "Node cache backend type. Closed enum.");

impl PhaseLabel for CacheType {
    const ALL: &'static [Self] = &[Self::EmptyDir, Self::PersistentVolume];
    fn label(&self) -> &str {
        match self {
            Self::EmptyDir => "emptyDir",
            Self::PersistentVolume => "persistentVolume",
            Self::Unknown(raw) => raw,
        }
    }
    fn unknown(raw: String) -> Self {
        Self::Unknown(raw)
    }
}

/// Kubernetes Service type for the bolt/https endpoints.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServiceType {
    /// Cluster-internal virtual IP.
    ClusterIP,
    /// Exposes the Service on each node's IP at a static port.
    NodePort,
    /// Provisions an external load balancer.
    LoadBalancer,
    /// Maps to an external DNS name.
    ExternalName,
    /// A service-type string this build does not recognise. Decode-compat only.
    Unknown(String),
}

phase_serde!(ServiceType, "Kubernetes Service type. Closed enum.");

impl PhaseLabel for ServiceType {
    const ALL: &'static [Self] = &[
        Self::ClusterIP,
        Self::NodePort,
        Self::LoadBalancer,
        Self::ExternalName,
    ];
    fn label(&self) -> &str {
        match self {
            Self::ClusterIP => "ClusterIP",
            Self::NodePort => "NodePort",
            Self::LoadBalancer => "LoadBalancer",
            Self::ExternalName => "ExternalName",
            Self::Unknown(raw) => raw,
        }
    }
    fn unknown(raw: String) -> Self {
        Self::Unknown(raw)
    }
}

/// Container image pull policy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PullPolicy {
    /// Pull only when the image is not already present.
    IfNotPresent,
    /// Always pull on pod start.
    Always,
    /// Never pull; the image must exist locally.
    Never,
    /// A pull-policy string this build does not recognise. Decode-compat only.
    Unknown(String),
}

phase_serde!(PullPolicy, "Image pull policy. Closed enum.");

impl PhaseLabel for PullPolicy {
    const ALL: &'static [Self] = &[Self::IfNotPresent, Self::Always, Self::Never];
    fn label(&self) -> &str {
        match self {
            Self::IfNotPresent => "IfNotPresent",
            Self::Always => "Always",
            Self::Never => "Never",
            Self::Unknown(raw) => raw,
        }
    }
    fn unknown(raw: String) -> Self {
        Self::Unknown(raw)
    }
}

/// Reference to a cert-manager issuer.
///
/// Modelled locally rather than depending on `cert-manager`'s Go types: the
/// controller emits the `Certificate` through a [`serde_json::Value`] body, so
/// only the three fields the API server validates are carried here.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(deny_unknown_fields)]
pub struct IssuerRef {
    /// Name of the `Issuer`/`ClusterIssuer` (or external issuer) object.
    pub name: String,
    /// Kind of the referenced issuer: `Issuer` or `ClusterIssuer`.
    #[serde(default = "default_issuer_kind")]
    pub kind: String,
    /// API group of the issuer. Empty means `cert-manager.io`.
    #[serde(default = "default_issuer_group")]
    pub group: String,
}

fn default_issuer_kind() -> String {
    "ClusterIssuer".to_owned()
}

fn default_issuer_group() -> String {
    "cert-manager.io".to_owned()
}

impl Default for IssuerRef {
    fn default() -> Self {
        Self {
            name: String::new(),
            kind: default_issuer_kind(),
            group: default_issuer_group(),
        }
    }
}

/// Role a `GraphNodePool` plays in the cluster.
///
/// The operand distinguishes a single admitted writer per `(scope, cell)` from
/// any number of readers; `All` keeps the pre-role behaviour where every node
/// may serve both. Decoding is `Unknown`-tolerant for forward compatibility.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NodeRole {
    /// Every node may write and read (single-pool deployments).
    All,
    /// Candidate writer nodes; placement elects one admitted writer.
    Writer,
    /// Read-only nodes; they never contend for the writer lease.
    Reader,
    /// A role string this build does not recognise. Decode-compat only.
    Unknown(String),
}

phase_serde!(NodeRole, "Graph node role. Closed enum.");

impl PhaseLabel for NodeRole {
    const ALL: &'static [Self] = &[Self::All, Self::Writer, Self::Reader];
    fn label(&self) -> &str {
        match self {
            Self::All => "all",
            Self::Writer => "writer",
            Self::Reader => "reader",
            Self::Unknown(raw) => raw,
        }
    }
    fn unknown(raw: String) -> Self {
        Self::Unknown(raw)
    }
}

/// GraphBLAS matrix budget knobs (`tuning.graphblas`).
///
/// Absent fields emit no env var, so the operand's own default applies; this
/// type never invents a value the operand did not document.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(deny_unknown_fields)]
pub struct GraphBlasTuning {
    /// Byte ceiling for all compiled matrices.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1))]
    pub max_bytes: Option<u64>,
    /// Compiled matrices held at once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1))]
    pub max_matrices: Option<u32>,
    /// Matrix adjacencies held at once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1))]
    pub max_adjacencies: Option<u32>,
    /// Byte ceiling for one adjacency.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1))]
    pub max_adjacency_bytes: Option<u64>,
    /// Concurrent matrix compilations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1))]
    pub max_concurrent_compilations: Option<u32>,
}

/// Query-execution limit knobs (`tuning.query`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(deny_unknown_fields)]
pub struct QueryTuning {
    /// Wall-clock ceiling for one query, in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1))]
    pub max_runtime_ms: Option<u64>,
    /// Default result page size.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1))]
    pub default_page_size: Option<u32>,
    /// Maximum live server-side cursors.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1))]
    pub max_server_cursors: Option<u32>,
    /// Byte ceiling per cursor buffer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1))]
    pub max_cursor_buffer_bytes: Option<u64>,
    /// Cursor idle TTL in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1))]
    pub cursor_ttl_ms: Option<u64>,
    /// Slow-query logging threshold in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1))]
    pub slow_query_ms: Option<u64>,
    /// Concurrent metadata hydrations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1))]
    pub max_concurrent_hydrations: Option<u32>,
    /// Bookmark wait ceiling in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1))]
    pub max_bookmark_wait_ms: Option<u64>,
    /// Transaction retry ceiling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1))]
    pub txn_max_retries: Option<u32>,
}

/// Bolt connection-management knobs (`tuning.connection`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(deny_unknown_fields)]
pub struct ConnectionTuning {
    /// Maximum concurrent Bolt connections.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1))]
    pub max_connections: Option<u32>,
    /// Idle timeout in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1))]
    pub idle_timeout_ms: Option<u64>,
    /// Maximum connection age in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1))]
    pub max_age_ms: Option<u64>,
    /// Authentication handshake timeout in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1))]
    pub authentication_timeout_ms: Option<u64>,
    /// Graceful shutdown drain window in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1))]
    pub graceful_shutdown_ms: Option<u64>,
}

/// Storage and index-artifact knobs (`tuning.storage`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(deny_unknown_fields)]
pub struct StorageTuning {
    /// Object-store cache chunk size in bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1024))]
    pub cache_part_bytes: Option<u64>,
    /// Parallel L0 flush tasks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1))]
    pub l0_flush_parallelism: Option<u32>,
    /// Target L0 SST size in bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1))]
    pub l0_sst_size_bytes: Option<u64>,
    /// Key batch size for maintenance compaction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1))]
    pub maintenance_batch_keys: Option<u32>,
    /// Key batch size for artifact garbage collection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1))]
    pub artifact_gc_batch_keys: Option<u32>,
    /// Key batch size for artifact writes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1))]
    pub artifact_write_batch_keys: Option<u32>,
    /// WAL flush interval in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1))]
    pub wal_flush_interval_ms: Option<u64>,
    /// Return hydration scratch memory to the OS after each hydration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trim_memory_after_hydration: Option<bool>,
}

/// Query-engine selector knobs (`tuning.engine`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(deny_unknown_fields)]
pub struct EngineTuning {
    /// OpenCypher engine selector.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cypher_engine: Option<String>,
    /// Compiled-kernel selector.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compiled_kernel: Option<String>,
    /// Sparse-kernel selector.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sparse_kernel: Option<String>,
    /// Emit matrix profiling telemetry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_matrix: Option<bool>,
}

/// Placement, writer-lease and read-routing knobs (`tuning.placement`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(deny_unknown_fields)]
pub struct PlacementTuning {
    /// Placement heartbeat interval in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1))]
    pub heartbeat_interval_ms: Option<u64>,
    /// Placement heartbeat expiry in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1))]
    pub heartbeat_timeout_ms: Option<u64>,
    /// Durable writer-lease duration in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1))]
    pub writer_lease_ms: Option<u64>,
    /// Concurrent write lanes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1))]
    pub write_lanes: Option<u32>,
    /// Bolt read-routing policy selector.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_routing: Option<String>,
    /// Follower-reader manifest poll interval in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1))]
    pub reader_manifest_poll_interval_ms: Option<u64>,
    /// Index discovery interval in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1))]
    pub index_discovery_interval_ms: Option<u64>,
}

/// Typed tuning surface for the operand's non-defaultable limits.
///
/// Every group is optional and every leaf is optional: an unset leaf emits no
/// env var at all, so the operand's documented default applies. This type is
/// the typed replacement for the previously untyped `extraEnv` escape hatch.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(deny_unknown_fields)]
pub struct TuningSpec {
    /// GraphBLAS matrix budgets.
    #[serde(default)]
    pub graphblas: GraphBlasTuning,
    /// Query-execution limits.
    #[serde(default)]
    pub query: QueryTuning,
    /// Bolt connection management.
    #[serde(default)]
    pub connection: ConnectionTuning,
    /// Storage and index-artifact batching.
    #[serde(default)]
    pub storage: StorageTuning,
    /// Query-engine selectors.
    #[serde(default)]
    pub engine: EngineTuning,
    /// Placement, writer lease and read routing.
    #[serde(default)]
    pub placement: PlacementTuning,
}

/// One HPA scaling policy (a `Pods`/`Percent` step over a period).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(deny_unknown_fields)]
pub struct HpaScalingPolicy {
    /// Policy type: `Pods` or `Percent`.
    #[serde(rename = "type")]
    pub policy_type: String,
    /// Magnitude of the step.
    #[schemars(range(min = 1))]
    pub value: i32,
    /// Window the step applies over, in seconds.
    #[schemars(range(min = 1))]
    pub period_seconds: i32,
}

/// Scale-up or scale-down rules for an HPA.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(deny_unknown_fields)]
pub struct HpaScalingRules {
    /// Window during which recommendations are taken into account.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 0))]
    pub stabilization_window_seconds: Option<i32>,
    /// How multiple policies combine: `Max`, `Min` or `Disabled`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub select_policy: Option<String>,
    /// The policies themselves.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub policies: Vec<HpaScalingPolicy>,
}

/// Scale behaviour for an HPA.
///
/// Modelled as real fields instead of a free-form object: a `serde_json::Value`
/// here produces a CRD schema whose `behavior` has no `type`, which the
/// apiserver rejects outright.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(deny_unknown_fields)]
pub struct HpaBehavior {
    /// Scale-up rules.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale_up: Option<HpaScalingRules>,
    /// Scale-down rules.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale_down: Option<HpaScalingRules>,
}

/// Horizontal pod autoscaling for a `GraphNodePool`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(deny_unknown_fields)]
pub struct AutoscalingSpec {
    /// Lower replica bound.
    #[schemars(range(min = 1))]
    pub min_replicas: i32,
    /// Upper replica bound.
    #[schemars(range(min = 1))]
    pub max_replicas: i32,
    /// Target average CPU utilisation percentage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1, max = 100))]
    pub target_cpu_utilization_percentage: Option<i32>,
    /// Scale-up/scale-down behaviour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub behavior: Option<HpaBehavior>,
}

/// Prometheus alerting integration.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(deny_unknown_fields)]
pub struct PrometheusRuleSpec {
    /// Create a `PrometheusRule` with the operator's alert set.
    /// Off by default: enabling it requires prometheus-operator CRDs.
    #[serde(default)]
    pub enabled: bool,
    /// Extra labels merged onto the `PrometheusRule` (e.g. `release`).
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub labels: std::collections::BTreeMap<String, String>,
}

/// Machine-readable reason attached to a status condition.
///
/// Condition reasons are free-form on the wire; this enum pins the reasons
/// this operator emits while tolerating reasons written by newer builds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConditionReason {
    /// The controller is actively working toward the desired state.
    Reconciling,
    /// The desired state is reached and observed.
    Available,
    /// The resource is serving in a reduced-capacity state.
    Degraded,
    /// Reconciliation failed; see the condition message.
    Failed,
    /// A reason string this build does not recognise. Decode-compat only.
    Unknown(String),
}

phase_serde!(ConditionReason, "Status condition reason. Closed enum.");

impl PhaseLabel for ConditionReason {
    const ALL: &'static [Self] = &[
        Self::Reconciling,
        Self::Available,
        Self::Degraded,
        Self::Failed,
    ];
    fn label(&self) -> &str {
        match self {
            Self::Reconciling => "Reconciling",
            Self::Available => "Available",
            Self::Degraded => "Degraded",
            Self::Failed => "Failed",
            Self::Unknown(raw) => raw,
        }
    }
    fn unknown(raw: String) -> Self {
        Self::Unknown(raw)
    }
}
