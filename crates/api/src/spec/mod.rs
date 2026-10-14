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
