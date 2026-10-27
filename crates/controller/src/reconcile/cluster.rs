//! `GraphCluster` reconciler: shared config `ConfigMap`, auth `Secret` and
//! object-store resolution (todo 18).
//!
//! The operand is ENV-ONLY: it calls `slatedb::admin::load_object_store_from_env`,
//! so every object-store input must surface as an env var in the shared
//! `<cluster>-config` ConfigMap. Credentials NEVER land in the ConfigMap:
//! OBC credential keys (`AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY`) stay in
//! the OBC's own Secret by reference; todo 20 wires that Secret into the
//! StatefulSet via `secretKeyRef`.
//!
//! Object-store resolution order (documented, do not reorder):
//! 1. `spec.objectStore.explicit` -> used directly.
//! 2. `spec.objectStore.objectBucketClaim` -> the OBC's generated Secret
//!    (`AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY` / `BUCKET_HOST` /
//!    `BUCKET_NAME` / `BUCKET_PORT` / `BUCKET_REGION`) plus its ConfigMap
//!    (endpoint) are mapped onto the operand env.
//! 3. Neither -> `Stalled=True` naming `objectStore`, NO workload, requeue.
//!
//! Auth resolution:
//! - `spec.auth.token` (inline, testing only) -> written verbatim.
//! - `spec.auth.generate == true` -> token generated ONCE and preserved:
//!   the existing `<cluster>-auth` Secret is read first and a stored
//!   non-empty token is kept byte-identical; a new token is minted only when
//!   none exists. Never rotates on a plain reconcile (mirrors the upstream
//!   chart's `preserveGeneratedToken: true` + `lookup` behaviour).
//! - `spec.auth.existingSecret` set -> passthrough reference: the operator
//!   creates NOTHING and todo 20 mounts the referenced Secret directly.
//! - None of the above -> `Stalled=True` naming `auth`.
//!
//! DEVIATION (documented): `hydradb_operator_api::constants` only pins the
//! graph identity / object-store / `RUST_LOG` env names. The six
//! `GRAPH_*` runtime keys and `GRAPH_AUTH_TOKEN_FILE` below mirror the
//! upstream chart's `configmap.yaml` verbatim (same spelling, including
//! `GRAPH_SLATE_DB_CACHE_BYTES` with the underscore); they are defined ONCE
//! here so no literal is re-typed at the call sites.
//!
//! Finalizer wiring (todo 65): [`reconcile`] calls
//! `finalizer::with_finalizer` as its ONLY top-level action, stamping
//! [`constants::FINALIZER`] via `kube::runtime::finalizer`. The
//! previous apply body is the `Apply` closure; [`cleanup_cluster`] is the
//! `Cleanup` closure and enforces `spec.deletionPolicy` (`Retain` default).
//!
//! Services and the Bolt routing table (todo 19): the five Services mirror
//! the upstream chart's `services.yaml` shapes (headless `clusterIP: None`
//! with `publishNotReadyAddresses: true`; bolt 7687; https 443->8443;
//! metrics 9090; indexer-metrics 9091) and `GRAPH_BOLT_NODE_ADDRESSES` is
//! recomputed from the LIVE node-pod list on every reconcile, replacing the
//! upstream `_helpers.tpl` `until (int .Values.node.replicaCount)` defect.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use hydradb_operator_api::constants;
use hydradb_operator_api::crd::{
    DeletionPolicy, Endpoints, GraphCluster, GraphClusterStatus, IndexingStatus,
};
use hydradb_operator_api::spec::{PhaseLabel as _, S3};
use hydradb_operator_api::validation::ValidationError;
use k8s_openapi::ByteString;
use k8s_openapi::api::core::v1::{ConfigMap, Pod, Secret, Service, ServicePort, ServiceSpec};
use k8s_openapi::api::networking::v1::{
    NetworkPolicy, NetworkPolicyEgressRule, NetworkPolicyIngressRule, NetworkPolicyPeer,
    NetworkPolicyPort, NetworkPolicySpec,
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{LabelSelector, ObjectMeta};
use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;
use kube::Resource as _;
use kube::ResourceExt as _;
use kube::api::{Api, ApiResource, DeleteParams, DynamicObject, ListParams, Patch, PatchParams};
use kube::core::GroupVersionKind;
use kube::runtime::controller::Action;

use crate::apply::{
    ROLE_AUTH, ROLE_BOLT, ROLE_CONFIG, ROLE_HTTPS, ROLE_METRICS, ROLE_NETPOL, ROLE_SERVICEMONITOR,
    child_object_meta, cluster_child_name, owner_ref_for, standard_labels,
};
use crate::conditions::{STALLED, new_condition, set_ready};
use crate::context::Ctx;
use crate::error::{Error, ErrorClass, STRUCTURAL_REQUEUE_SECS};
use crate::metrics;

// Runtime operand env keys mirroring the upstream chart's `configmap.yaml`.
// See the module docs: these have no `constants.rs` pin, so they live here.
const GRAPH_MAX_OPEN_SCOPES: &str = "GRAPH_MAX_OPEN_SCOPES";
const GRAPH_SLATE_DB_CACHE_BYTES: &str = "GRAPH_SLATE_DB_CACHE_BYTES";
const GRAPH_DATA_CACHE_MAX_OPEN_FILES: &str = "GRAPH_DATA_CACHE_MAX_OPEN_FILES";
const GRAPH_READER_WAL_REPLAY_CONCURRENCY: &str = "GRAPH_READER_WAL_REPLAY_CONCURRENCY";
const GRAPH_MAX_CONCURRENT_QUERIES: &str = "GRAPH_MAX_CONCURRENT_QUERIES";
const GRAPH_MAX_QUERY_SCAN_EDGES: &str = "GRAPH_MAX_QUERY_SCAN_EDGES";
/// Path where todo 20 mounts the auth token; mirrors the upstream chart's
/// `GRAPH_AUTH_TOKEN_FILE: "/var/run/secrets/hydradb/{{ secretKey }}"`.
const GRAPH_AUTH_TOKEN_FILE: &str = "GRAPH_AUTH_TOKEN_FILE";
/// Mount directory for the auth token file (upstream chart convention).
const AUTH_MOUNT_DIR: &str = "/var/run/secrets/hydradb";

/// Default key inside the auth Secret (upstream `values.yaml` `secretKey`).
pub const DEFAULT_AUTH_KEY: &str = "auth-token";

/// Default object-store region when the spec sets none (upstream
/// `values.yaml` `objectStore.aws.region` default).
pub const DEFAULT_REGION: &str = "us-east-1";

/// Cloud provider value written to `CLOUD_PROVIDER` (only `aws` is modelled).
pub const CLOUD_PROVIDER_AWS: &str = "aws";

/// Success requeue: the shared config is static per spec generation, so the
/// controller sleeps until something changes.
pub const SUCCESS_REQUEUE_SECS: u64 = 600;

/// OBC Secret keys read during claim resolution.
const OBC_ACCESS_KEY_ID: &str = "AWS_ACCESS_KEY_ID";
const OBC_SECRET_ACCESS_KEY: &str = "AWS_SECRET_ACCESS_KEY";
const OBC_BUCKET_HOST: &str = "BUCKET_HOST";
const OBC_BUCKET_NAME: &str = "BUCKET_NAME";
const OBC_BUCKET_PORT: &str = "BUCKET_PORT";
const OBC_BUCKET_REGION: &str = "BUCKET_REGION";

/// Resolved object-store inputs, ready to map onto the operand env.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedObjectStore {
    /// Bucket name -> `AWS_BUCKET_NAME`. Never invented: always from the spec
    /// or the OBC Secret.
    pub bucket: String,
    /// Region -> `AWS_DEFAULT_REGION`.
    pub region: String,
    /// Whether plain-HTTP endpoints are allowed -> `AWS_ALLOW_HTTP`.
    pub allow_http: bool,
    /// S3-compatible endpoint override -> `AWS_ENDPOINT` (empty = provider default).
    pub endpoint: String,
    /// Recorded in `status.observedObjectStore`: `explicit:<bucket>` or
    /// `obc:<namespace>/<name>`.
    pub source: String,
}

/// Resolve `spec.objectStore.explicit` directly (resolution step 1).
///
/// # Errors
///
/// Returns [`Error::InvalidSpec`] when the bucket is empty.
pub fn resolve_explicit(s3: &S3) -> Result<ResolvedObjectStore, Error> {
    if s3.bucket.is_empty() {
        return Err(Error::InvalidSpec(
            "objectStore.explicit.bucket must be non-empty".to_owned(),
        ));
    }
    Ok(ResolvedObjectStore {
        source: format!("explicit:{}", s3.bucket),
        bucket: s3.bucket.clone(),
        region: s3
            .region
            .clone()
            .unwrap_or_else(|| DEFAULT_REGION.to_owned()),
        allow_http: s3.allow_http,
        endpoint: s3.endpoint.clone().unwrap_or_default(),
    })
}

/// Decode one Secret data entry to a string.
///
/// # Errors
///
/// Returns [`Error::MissingSecret`] when the key is absent or not UTF-8.
fn obc_secret_string(secret: &Secret, secret_name: &str, key: &str) -> Result<String, Error> {
    let raw: &[u8] = secret
        .data
        .as_ref()
        .and_then(|data| data.get(key))
        .map(|bytes| bytes.0.as_slice())
        .unwrap_or_default();
    if raw.is_empty() {
        return Err(Error::MissingSecret(format!(
            "secret {secret_name} carries no non-empty key {key}"
        )));
    }
    String::from_utf8(raw.to_vec())
        .map_err(|_| Error::UnresolvableRef(format!("secret {secret_name} key {key} is not UTF-8")))
}

/// Resolve the endpoint from the OBC ConfigMap, falling back to composing
/// `http://<host>:<port>` from the OBC host/port values.
///
/// The Rook provisioner writes the endpoint pieces (`BUCKET_HOST`,
/// `BUCKET_PORT`) rather than a full URL; a full URL is honoured when the
/// ConfigMap already carries one under `ENDPOINT`, `AWS_ENDPOINT`,
/// `BUCKET_ENDPOINT` or `endpoint`.
fn obc_endpoint(configmap: &ConfigMap, host: &str, port: Option<&str>) -> (String, bool) {
    let data = configmap.data.as_ref();
    for key in ["ENDPOINT", "AWS_ENDPOINT", "BUCKET_ENDPOINT", "endpoint"] {
        if let Some(value) = data
            .and_then(|data| data.get(key))
            .filter(|v| !v.is_empty())
        {
            let allow_http = !value.starts_with("https://");
            let endpoint = if value.contains("://") {
                value.clone()
            } else {
                format!("http://{value}")
            };
            return (endpoint, allow_http);
        }
    }
    match port.filter(|p| !p.is_empty()) {
        Some(port) => (format!("http://{host}:{port}"), true),
        None => (format!("http://{host}"), true),
    }
}

/// Resolve `spec.objectStore.objectBucketClaim` from the OBC's generated
/// Secret + ConfigMap (resolution step 2).
///
/// `obc_secret` / `obc_configmap` are the live objects named by the claim in
/// the cluster's namespace (the provisioner creates both in the OBC's own
/// namespace, which is the cluster's namespace for a namespaced claim
/// reference). Credentials are REQUIRED present (fail fast with a structural
/// error otherwise) but are never copied: they stay in the OBC Secret by
/// reference for todo 20 to wire via `secretKeyRef`.
///
/// # Errors
///
/// Returns [`Error::MissingSecret`] / [`Error::MissingConfigMap`] /
/// [`Error::UnresolvableRef`] (all structural) when any piece is absent.
pub fn resolve_obc(
    obc_secret: &Secret,
    obc_configmap: &ConfigMap,
    obc_namespace: &str,
    obc_name: &str,
) -> Result<ResolvedObjectStore, Error> {
    let secret_name = obc_secret
        .metadata
        .name
        .clone()
        .unwrap_or_else(|| obc_name.to_owned());
    let bucket = obc_secret_string(obc_secret, &secret_name, OBC_BUCKET_NAME)?;
    let region = obc_secret_string(obc_secret, &secret_name, OBC_BUCKET_REGION)
        .unwrap_or_else(|_| DEFAULT_REGION.to_owned());
    let host = obc_secret_string(obc_secret, &secret_name, OBC_BUCKET_HOST)?;
    let port = obc_secret_string(obc_secret, &secret_name, OBC_BUCKET_PORT).ok();
    // Fail fast when the credential keys are absent; the values stay in the
    // OBC Secret and are never copied into the ConfigMap.
    obc_secret_string(obc_secret, &secret_name, OBC_ACCESS_KEY_ID)?;
    obc_secret_string(obc_secret, &secret_name, OBC_SECRET_ACCESS_KEY)?;
    let (endpoint, allow_http) = obc_endpoint(obc_configmap, &host, port.as_deref());
    Ok(ResolvedObjectStore {
        bucket,
        region,
        allow_http,
        endpoint,
        source: format!("obc:{obc_namespace}/{obc_name}"),
    })
}

/// Child role for the headless node `Service` (upstream
/// `<cluster>-node-headless`).
pub const ROLE_HEADLESS: &str = "headless";
/// Child role for the indexer metrics `Service` (upstream
/// `<cluster>-indexer-metrics`).
pub const ROLE_INDEXER_METRICS: &str = "indexer-metrics";

/// Bolt port served by every node (upstream `service.bolt.port`).
pub const BOLT_PORT: i32 = 7687;
/// Public HTTPS port (upstream `service.https.port`).
pub const HTTPS_PORT: i32 = 443;
/// Node HTTPS container port the public Service targets.
pub const HTTPS_TARGET_PORT: i32 = 8443;
/// Node admin port (upstream `service.adminPort`).
pub const METRICS_PORT: i32 = 9090;
/// Indexer admin port.
pub const INDEXER_METRICS_PORT: i32 = 9091;

/// Name of the headless node `Service`: `<cluster>-node-headless` (upstream
/// `<fullname>-node-headless`; the StatefulSet's `serviceName` points here).
#[must_use]
pub fn headless_service_name(cluster_name: &str) -> String {
    format!("{cluster_name}-node-headless")
}

/// Name of the indexer metrics `Service`: `<cluster>-indexer-metrics`.
#[must_use]
pub fn indexer_metrics_service_name(cluster_name: &str) -> String {
    format!("{cluster_name}-{ROLE_INDEXER_METRICS}")
}

/// Selector matching the node pods (upstream `componentLabels` with
/// `component: node`). Todo 20 labels the StatefulSet pods with exactly this
/// set, so the Services select them without any extra coordination.
#[must_use]
pub fn node_pod_selector(cluster_name: &str) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("app.kubernetes.io/name".to_owned(), "hydradb".to_owned()),
        (
            "app.kubernetes.io/instance".to_owned(),
            cluster_name.to_owned(),
        ),
        ("app.kubernetes.io/component".to_owned(), "node".to_owned()),
    ])
}

/// Selector matching the indexer pods.
#[must_use]
pub fn indexer_pod_selector(cluster_name: &str) -> BTreeMap<String, String> {
    let mut selector = node_pod_selector(cluster_name);
    selector.insert(
        "app.kubernetes.io/component".to_owned(),
        "indexer".to_owned(),
    );
    selector
}

/// Recompute `GRAPH_BOLT_NODE_ADDRESSES` from the LIVE node-pod names.
///
/// Each live pod `p` contributes
/// `p=p.<headless>.<namespace>.svc.cluster.local:7687`, mirroring the
/// upstream `_helpers.tpl` `nodeAddresses` entry shape exactly — except the
/// member list comes from the observed pods, never from `replicaCount`.
/// Empty membership yields an empty string (never a stale table). Names are
/// sorted so the value is deterministic for a given membership set.
#[must_use]
pub fn routing_table_value(cluster_name: &str, namespace: &str, pod_names: &[String]) -> String {
    let mut names = pod_names.to_vec();
    names.sort();
    let headless = headless_service_name(cluster_name);
    names
        .iter()
        .map(|pod| format!("{pod}={pod}.{headless}.{namespace}.svc.cluster.local:{BOLT_PORT}"))
        .collect::<Vec<_>>()
        .join(",")
}

/// One TCP `ServicePort` with a numeric target port.
/// Client-facing Bolt port from the spec (the operand always listens on
/// [`BOLT_PORT`] inside the pod, so only the Service port follows the spec).
#[must_use]
fn spec_bolt_port(cluster: &GraphCluster) -> i32 {
    i32::from(cluster.spec.service.bolt_port)
}

/// Client-facing HTTPS port from the spec.
#[must_use]
fn spec_https_port(cluster: &GraphCluster) -> i32 {
    i32::from(cluster.spec.service.https_port)
}

fn tcp_port(name: &str, port: i32, target_port: i32) -> ServicePort {
    ServicePort {
        name: Some(name.to_owned()),
        port,
        protocol: Some("TCP".to_owned()),
        target_port: Some(IntOrString::Int(target_port)),
        ..ServicePort::default()
    }
}

/// `ObjectMeta` for a cluster-owned `Service` with an explicit name:
/// standard labels (including [`CLUSTER_SELECTOR_LABEL`]) plus the
/// controller owner references No API call.
fn service_object_meta(
    cluster: &GraphCluster,
    service_name: &str,
    role: &str,
) -> Result<ObjectMeta, Error> {
    let cluster_name = cluster.name_any();
    Ok(ObjectMeta {
        name: Some(service_name.to_owned()),
        labels: Some(standard_labels(&cluster_name, role)),
        owner_references: Some(vec![owner_ref_for(cluster)?]),
        ..ObjectMeta::default()
    })
}

/// Desired headless node `Service` (pure): `clusterIP: None`,
/// `publishNotReadyAddresses: true`, and the bolt/https/admin ports the
/// upstream headless Service declares.
pub fn desired_headless_service(cluster: &GraphCluster) -> Result<Service, Error> {
    let cluster_name = cluster.name_any();
    Ok(Service {
        metadata: service_object_meta(
            cluster,
            &headless_service_name(&cluster_name),
            ROLE_HEADLESS,
        )?,
        spec: Some(ServiceSpec {
            cluster_ip: Some("None".to_owned()),
            publish_not_ready_addresses: Some(true),
            selector: Some(node_pod_selector(&cluster_name)),
            ports: Some(vec![
                // Headless peers address container ports directly: no
                // client-facing port mapping applies here.
                tcp_port("bolt", BOLT_PORT, BOLT_PORT),
                tcp_port("https", HTTPS_TARGET_PORT, HTTPS_TARGET_PORT),
                tcp_port("admin", METRICS_PORT, METRICS_PORT),
            ]),
            ..ServiceSpec::default()
        }),
        status: None,
    })
}

/// Desired public Bolt `Service` (pure): 7687 -> 7687.
pub fn desired_bolt_service(cluster: &GraphCluster) -> Result<Service, Error> {
    let cluster_name = cluster.name_any();
    Ok(Service {
        metadata: service_object_meta(
            cluster,
            &cluster_child_name(&cluster_name, ROLE_BOLT),
            ROLE_BOLT,
        )?,
        spec: Some(ServiceSpec {
            selector: Some(node_pod_selector(&cluster_name)),
            ports: Some(vec![tcp_port("bolt", spec_bolt_port(cluster), BOLT_PORT)]),
            ..ServiceSpec::default()
        }),
        status: None,
    })
}

/// Desired public HTTPS `Service` (pure): 443 -> 8443.
pub fn desired_https_service(cluster: &GraphCluster) -> Result<Service, Error> {
    let cluster_name = cluster.name_any();
    Ok(Service {
        metadata: service_object_meta(
            cluster,
            &cluster_child_name(&cluster_name, ROLE_HTTPS),
            ROLE_HTTPS,
        )?,
        spec: Some(ServiceSpec {
            selector: Some(node_pod_selector(&cluster_name)),
            ports: Some(vec![tcp_port(
                "https",
                spec_https_port(cluster),
                HTTPS_TARGET_PORT,
            )]),
            ..ServiceSpec::default()
        }),
        status: None,
    })
}

/// Desired node metrics `Service` (pure): 9090 -> 9090.
pub fn desired_metrics_service(cluster: &GraphCluster) -> Result<Service, Error> {
    let cluster_name = cluster.name_any();
    Ok(Service {
        metadata: service_object_meta(
            cluster,
            &cluster_child_name(&cluster_name, ROLE_METRICS),
            ROLE_METRICS,
        )?,
        spec: Some(ServiceSpec {
            selector: Some(node_pod_selector(&cluster_name)),
            ports: Some(vec![tcp_port("metrics", METRICS_PORT, METRICS_PORT)]),
            ..ServiceSpec::default()
        }),
        status: None,
    })
}

/// Desired indexer metrics `Service` (pure): 9091 -> 9091, selecting the
/// indexer pods.
pub fn desired_indexer_metrics_service(cluster: &GraphCluster) -> Result<Service, Error> {
    let cluster_name = cluster.name_any();
    Ok(Service {
        metadata: service_object_meta(
            cluster,
            &indexer_metrics_service_name(&cluster_name),
            ROLE_INDEXER_METRICS,
        )?,
        spec: Some(ServiceSpec {
            selector: Some(indexer_pod_selector(&cluster_name)),
            ports: Some(vec![tcp_port(
                "metrics",
                INDEXER_METRICS_PORT,
                INDEXER_METRICS_PORT,
            )]),
            ..ServiceSpec::default()
        }),
        status: None,
    })
}

/// All five desired `Service` objects (pure, fixed order: headless, bolt,
/// https, metrics, indexer-metrics).
pub fn desired_services(cluster: &GraphCluster) -> Result<Vec<Service>, Error> {
    Ok(vec![
        desired_headless_service(cluster)?,
        desired_bolt_service(cluster)?,
        desired_https_service(cluster)?,
        desired_metrics_service(cluster)?,
        desired_indexer_metrics_service(cluster)?,
    ])
}

/// Labels selecting every pod of the cluster (node + indexer): the shared
/// `name`/`instance` pair without a `component`, so the `NetworkPolicy`
/// covers both roles with one rule set.
#[must_use]
pub fn cluster_pod_selector(cluster_name: &str) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("app.kubernetes.io/name".to_owned(), "hydradb".to_owned()),
        (
            "app.kubernetes.io/instance".to_owned(),
            cluster_name.to_owned(),
        ),
    ])
}

/// One TCP ingress/egress port for the `NetworkPolicy`.
fn netpol_port(port: i32) -> NetworkPolicyPort {
    NetworkPolicyPort {
        port: Some(IntOrString::Int(port)),
        protocol: Some("TCP".to_owned()),
        ..NetworkPolicyPort::default()
    }
}

/// Desired default-on `NetworkPolicy` (pure): ingress limited to the
/// documented ports (Bolt 7687, HTTPS 8443, admin 9090/9091) from pods in
/// the same namespace, plus egress for DNS and the object-store HTTPS
/// path. There is no per-endpoint CIDR field on the CR, so object-store
/// egress is the standard HTTPS ports rather than a pinned range.
pub fn desired_network_policy(cluster: &GraphCluster) -> Result<NetworkPolicy, Error> {
    let cluster_name = cluster.name_any();
    let namespace_peer = NetworkPolicyPeer {
        pod_selector: Some(LabelSelector::default()),
        ..NetworkPolicyPeer::default()
    };
    let ingress = NetworkPolicyIngressRule {
        from: Some(vec![namespace_peer]),
        ports: Some(vec![
            netpol_port(BOLT_PORT),
            netpol_port(HTTPS_TARGET_PORT),
            netpol_port(METRICS_PORT),
            netpol_port(INDEXER_METRICS_PORT),
        ]),
    };
    let dns_egress = NetworkPolicyEgressRule {
        ports: Some(vec![
            NetworkPolicyPort {
                port: Some(IntOrString::Int(53)),
                protocol: Some("UDP".to_owned()),
                ..NetworkPolicyPort::default()
            },
            NetworkPolicyPort {
                port: Some(IntOrString::Int(53)),
                protocol: Some("TCP".to_owned()),
                ..NetworkPolicyPort::default()
            },
        ]),
        ..NetworkPolicyEgressRule::default()
    };
    let store_egress = NetworkPolicyEgressRule {
        ports: Some(vec![netpol_port(80), netpol_port(443)]),
        ..NetworkPolicyEgressRule::default()
    };
    Ok(NetworkPolicy {
        metadata: child_object_meta(&cluster_name, ROLE_NETPOL, cluster)?,
        spec: Some(NetworkPolicySpec {
            pod_selector: Some(LabelSelector {
                match_labels: Some(cluster_pod_selector(&cluster_name)),
                ..LabelSelector::default()
            }),
            policy_types: Some(vec!["Ingress".to_owned(), "Egress".to_owned()]),
            ingress: Some(vec![ingress]),
            egress: Some(vec![dns_egress, store_egress]),
        }),
    })
}

/// Whether the cluster opts into the `ServiceMonitor` child: only
/// `spec.monitoring.serviceMonitor.enabled == true`. Absent (default) means
/// no `ServiceMonitor` exists at all.
#[must_use]
pub fn service_monitor_enabled(cluster: &GraphCluster) -> bool {
    cluster.spec.monitoring.service_monitor.enabled
}

/// `ApiResource` for the `ServiceMonitor` child kind.
///
/// `monitoring.coreos.com` ships no `k8s-openapi` type, so the reconciler
/// drives applies through a [`DynamicObject`]; the group/version/kind/plural
/// pinned here match the `gen-rbac` marker exactly.
#[must_use]
pub fn service_monitor_api_resource() -> ApiResource {
    ApiResource::from_gvk_with_plural(
        &GroupVersionKind::gvk("monitoring.coreos.com", "v1", "ServiceMonitor"),
        "servicemonitors",
    )
}

/// Typed watch handle for the `ServiceMonitor` child kind.
///
/// `Controller::owns` needs `Resource<DynamicType = ()>`, which
/// [`DynamicObject`] (typed by [`ApiResource`]) does not satisfy, so this
/// minimal shape carries the same group/version/kind/plural for watches
/// only. Unknown fields decode past it; the apply path stays on
/// [`DynamicObject`].
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct ServiceMonitorObject {
    metadata: ObjectMeta,
    #[serde(default)]
    spec: serde_json::Value,
}

impl kube::Resource for ServiceMonitorObject {
    type DynamicType = ();
    type Scope = kube::core::NamespaceResourceScope;

    fn meta(&self) -> &ObjectMeta {
        &self.metadata
    }

    fn meta_mut(&mut self) -> &mut ObjectMeta {
        &mut self.metadata
    }

    fn group(_: &()) -> std::borrow::Cow<'_, str> {
        std::borrow::Cow::Borrowed("monitoring.coreos.com")
    }

    fn version(_: &()) -> std::borrow::Cow<'_, str> {
        std::borrow::Cow::Borrowed("v1")
    }

    fn kind(_: &()) -> std::borrow::Cow<'_, str> {
        std::borrow::Cow::Borrowed("ServiceMonitor")
    }

    fn plural(_: &()) -> std::borrow::Cow<'_, str> {
        std::borrow::Cow::Borrowed("servicemonitors")
    }
}

/// Name of the cluster-owned `ServiceMonitor`: `<cluster>-servicemonitor`.
#[must_use]
pub fn service_monitor_name(cluster_name: &str) -> String {
    cluster_child_name(cluster_name, ROLE_SERVICEMONITOR)
}

/// Desired opt-in `ServiceMonitor` (pure): scrapes `/metrics` on the `metrics`
/// port of the node metrics Service every 30s. Callers must gate on
/// [`service_monitor_enabled`]: building without the flag is a bug, not a
/// default.
pub fn desired_service_monitor(cluster: &GraphCluster) -> Result<DynamicObject, Error> {
    debug_assert!(
        service_monitor_enabled(cluster),
        "ServiceMonitor must only be built when spec.monitoring.serviceMonitor.enabled is true"
    );
    let cluster_name = cluster.name_any();
    let api_resource = service_monitor_api_resource();
    let mut obj = DynamicObject::new(&service_monitor_name(&cluster_name), &api_resource);
    obj.metadata.labels = Some(standard_labels(&cluster_name, ROLE_SERVICEMONITOR));
    obj.metadata.owner_references = Some(vec![owner_ref_for(cluster)?]);
    obj.data = serde_json::json!({
        "spec": {
            "selector": {"matchLabels": standard_labels(&cluster_name, ROLE_METRICS)},
            "endpoints": [{"port": "metrics", "path": "/metrics", "interval": "30s"}],
        }
    });
    Ok(obj)
}

/// Build the shared ConfigMap `data`: every operand env var.
///
/// Keys that have a `constants.rs` pin use it; the six `GRAPH_*` runtime keys
/// and `GRAPH_AUTH_TOKEN_FILE` use the module-local consts documented above.
/// Credential keys (`AWS_ACCESS_KEY_ID`, ...) are NEVER present here.
/// `bolt_node_addresses` is the freshly recomputed routing table from
/// [`routing_table_value`]: callers pass it in on every reconcile and never
/// read it back from a stored ConfigMap.
#[must_use]
pub fn config_data(
    cluster: &GraphCluster,
    resolved: &ResolvedObjectStore,
    bolt_node_addresses: &str,
) -> BTreeMap<String, String> {
    let spec = &cluster.spec;
    let auth_key = spec
        .auth
        .secret_key
        .clone()
        .unwrap_or_else(|| DEFAULT_AUTH_KEY.to_owned());
    let mut data = BTreeMap::from([
        (
            constants::GRAPH_NAMESPACE.to_owned(),
            spec.graph.namespace.clone(),
        ),
        (constants::GRAPH_ID.to_owned(), spec.graph.id.clone()),
        (
            constants::GRAPH_DATABASE.to_owned(),
            spec.graph.database.clone(),
        ),
        (
            constants::GRAPH_CELL_ID.to_owned(),
            spec.graph.cell_id.clone(),
        ),
        // Every declared cell; a single-cell cluster emits just its own.
        (
            constants::GRAPH_CELLS.to_owned(),
            if spec.graph.cells.is_empty() {
                spec.graph.cell_id.clone()
            } else {
                spec.graph.cells.join(",")
            },
        ),
        (
            constants::CLOUD_PROVIDER.to_owned(),
            CLOUD_PROVIDER_AWS.to_owned(),
        ),
        (
            constants::AWS_BUCKET_NAME.to_owned(),
            resolved.bucket.clone(),
        ),
        (
            constants::AWS_DEFAULT_REGION.to_owned(),
            resolved.region.clone(),
        ),
        (
            constants::AWS_ALLOW_HTTP.to_owned(),
            resolved.allow_http.to_string(),
        ),
        (
            constants::AWS_ENDPOINT.to_owned(),
            resolved.endpoint.clone(),
        ),
        (
            constants::RUST_LOG.to_owned(),
            spec.runtime.rust_log.clone(),
        ),
        (
            GRAPH_MAX_OPEN_SCOPES.to_owned(),
            spec.runtime.max_open_scopes.to_string(),
        ),
        (
            GRAPH_SLATE_DB_CACHE_BYTES.to_owned(),
            spec.runtime.slatedb_cache_bytes.to_string(),
        ),
        (
            GRAPH_DATA_CACHE_MAX_OPEN_FILES.to_owned(),
            spec.runtime.data_cache_max_open_files.to_string(),
        ),
        (
            GRAPH_READER_WAL_REPLAY_CONCURRENCY.to_owned(),
            spec.runtime.reader_wal_replay_concurrency.to_string(),
        ),
        (
            GRAPH_MAX_CONCURRENT_QUERIES.to_owned(),
            spec.runtime.max_concurrent_queries.to_string(),
        ),
        (
            GRAPH_MAX_QUERY_SCAN_EDGES.to_owned(),
            spec.runtime.max_query_scan_edges.to_string(),
        ),
        (
            GRAPH_AUTH_TOKEN_FILE.to_owned(),
            format!("{AUTH_MOUNT_DIR}/{auth_key}"),
        ),
        (
            constants::GRAPH_BOLT_NODE_ADDRESSES.to_owned(),
            bolt_node_addresses.to_owned(),
        ),
    ]);
    // Typed tuning surface: only the leaves the user set become env vars, so
    // an untouched `tuning` block changes nothing about the operand defaults.
    for (key, value) in crate::children::tuning_env(&spec.tuning) {
        data.insert(key, value);
    }
    data
}

/// Build the desired shared `<cluster>-config` ConfigMap (pure: no API call).
///
/// # Errors
///
/// Returns [`Error::MissingField`] when the owner has no name/uid yet.
pub fn desired_config_map(
    cluster: &GraphCluster,
    resolved: &ResolvedObjectStore,
    bolt_node_addresses: &str,
) -> Result<ConfigMap, Error> {
    let name = cluster.name_any();
    let mut meta = child_object_meta(&name, ROLE_CONFIG, cluster)?;
    meta.namespace = cluster.namespace();
    Ok(ConfigMap {
        metadata: meta,
        data: Some(config_data(cluster, resolved, bolt_node_addresses)),
        immutable: None,
        binary_data: None,
    })
}

/// What the reconciler must do about client auth.
#[derive(Clone, Debug, PartialEq)]
pub enum AuthPlan {
    /// Apply this Secret (generated or inline token; boxed: `Secret` is ~10x
    /// the `Reference` variant).
    Create(Box<Secret>),
    /// Create nothing: todo 20 mounts this Secret directly.
    Reference {
        /// Name of the user-supplied Secret.
        name: String,
        /// Key inside it holding the token.
        key: String,
    },
}

/// Resolve the auth plan (pure: no API call).
///
/// `existing` is the live `<cluster>-auth` Secret when it already exists (read
/// by the reconciler before calling). A stored non-empty token is preserved
/// byte-identical; a fresh token is minted only when none exists.
///
/// # Errors
///
/// Returns [`Error::UnresolvableRef`] (structural, naming `auth`) when the
/// spec supplies no token source at all.
pub fn plan_auth(cluster: &GraphCluster, existing: Option<&Secret>) -> Result<AuthPlan, Error> {
    let name = cluster.name_any();
    let key = cluster
        .spec
        .auth
        .secret_key
        .clone()
        .unwrap_or_else(|| DEFAULT_AUTH_KEY.to_owned());
    if let Some(inline) = cluster.spec.auth.token.clone() {
        return Ok(AuthPlan::Create(Box::new(auth_secret(
            cluster,
            &key,
            inline.as_bytes(),
        )?)));
    }
    if cluster.spec.auth.generate {
        let preserved: Option<Vec<u8>> = existing
            .and_then(|secret| secret.data.as_ref())
            .and_then(|data| data.get(&key))
            .map(|token| token.0.clone())
            .filter(|token| !token.is_empty());
        let token: Vec<u8> = match preserved {
            Some(token) => token,
            None => uuid::Uuid::new_v4().simple().to_string().into_bytes(),
        };
        return Ok(AuthPlan::Create(Box::new(auth_secret(
            cluster, &key, &token,
        )?)));
    }
    if let Some(secret_name) = cluster.spec.auth.existing_secret.clone()
        && !secret_name.is_empty()
    {
        return Ok(AuthPlan::Reference {
            name: secret_name,
            key,
        });
    }
    let _ = name;
    Err(Error::UnresolvableRef(
        "auth has no token source: set one of auth.token (testing only), auth.generate: true, or auth.existingSecret".to_owned(),
    ))
}

/// Build the `<cluster>-auth` Secret carrying `token` under `key` (pure).
///
/// # Errors
///
/// Returns [`Error::MissingField`] when the owner has no name/uid yet.
fn auth_secret(cluster: &GraphCluster, key: &str, token: &[u8]) -> Result<Secret, Error> {
    let name = cluster.name_any();
    let mut meta = child_object_meta(&name, ROLE_AUTH, cluster)?;
    meta.namespace = cluster.namespace();
    Ok(Secret {
        metadata: meta,
        data: Some(BTreeMap::from([(
            key.to_owned(),
            ByteString(token.to_vec()),
        )])),
        string_data: None,
        type_: Some("Opaque".to_owned()),
        immutable: None,
    })
}

/// Join every [`ValidationError`] into one `Stalled` message naming the fields.
#[must_use]
pub fn stalled_message_for_validation(errors: &[ValidationError]) -> String {
    errors
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ")
}

/// Desired status for the stalled path: `Stalled=True` plus `Ready=False`.
fn stalled_status(
    message: &str,
    generation: Option<i64>,
    observed_object_store: Option<String>,
) -> GraphClusterStatus {
    GraphClusterStatus {
        conditions: vec![
            new_condition(
                "Ready",
                "False",
                hydradb_operator_api::spec::ConditionReason::Failed.label(),
                message,
                generation,
            ),
            new_condition(
                "Reconciling",
                "False",
                hydradb_operator_api::spec::ConditionReason::Failed.label(),
                "reconcile stalled",
                generation,
            ),
            new_condition(
                STALLED,
                "True",
                hydradb_operator_api::spec::ConditionReason::Failed.label(),
                message,
                generation,
            ),
        ],
        observed_generation: generation,
        endpoints: hydradb_operator_api::crd::Endpoints::default(),
        node_addresses: Vec::new(),
        observed_object_store,
        indexing: IndexingStatus::default(),
    }
}

/// Desired status for the success path: kstatus `Ready` over the live set,
/// plus the full todo-24 surface: advertised `endpoints`, live
/// `node_addresses` and the resolved `observed_object_store`.
///
/// `observed_generation` is the reconciler-observed `metadata.generation`:
/// callers pass the generation of the object they actually reconciled, never
/// an optimistically advanced value.
fn ready_status(
    live_conditions_set: &[k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition],
    generation: Option<i64>,
    observed_object_store: String,
    endpoints: Endpoints,
    node_addresses: Vec<String>,
) -> GraphClusterStatus {
    let mut conditions = live_conditions_set.to_vec();
    set_ready(&mut conditions, true, generation);
    GraphClusterStatus {
        conditions,
        observed_generation: generation,
        endpoints,
        node_addresses,
        observed_object_store: Some(observed_object_store),
        indexing: IndexingStatus::default(),
    }
}

/// Patch the `/status` subresource when the desired status differs from live.
///
/// The comparison is field-exact over the full surface (`conditions`,
/// `observedGeneration`, `endpoints`, `nodeAddresses`,
/// `observedObjectStore`), so an unchanged status produces NO write and never
/// hot-loops. The patch touches `/status` only, never `spec`.
///
/// # Errors
///
/// Returns [`Error::Api`] when the apiserver rejects the read or the patch.
async fn write_status_if_changed(
    api: &Api<GraphCluster>,
    name: &str,
    desired: &GraphClusterStatus,
) -> Result<(), Error> {
    let live = api.get(name).await.map_err(Error::from_kube_error)?;
    let live_status = live.status.clone().unwrap_or_else(|| GraphClusterStatus {
        conditions: Vec::new(),
        observed_generation: None,
        endpoints: Endpoints::default(),
        node_addresses: Vec::new(),
        observed_object_store: None,
        indexing: IndexingStatus::default(),
    });
    if !crate::status::cluster_status_needs_write(&live_status, desired) {
        return Ok(());
    }
    api.patch_status(
        name,
        &PatchParams::default(),
        // Wrapped in `status`: the `/status` subresource endpoint drops a
        // bare field patch silently (proven by the envtest suite, todo 26),
        // while the wrapped form persists exactly.
        &Patch::Merge(serde_json::json!({
            "status": {
                "conditions": desired.conditions,
                "observedGeneration": desired.observed_generation,
                "endpoints": desired.endpoints,
                "nodeAddresses": desired.node_addresses,
                "observedObjectStore": desired.observed_object_store,
                "indexing": desired.indexing,
            }
        })),
    )
    .await
    .map_err(Error::from_kube_error)?;
    Ok(())
}

/// Reconcile one `GraphCluster` into its shared ConfigMap + auth Secret.
///
/// Validation runs FIRST; every [`ValidationError`] lands on the `Stalled`
/// condition message before any child is touched.
async fn reconcile_inner(cluster: Arc<GraphCluster>, ctx: Arc<Ctx>) -> Result<Action, Error> {
    let name = cluster.name_any();
    let namespace = cluster.namespace().unwrap_or_default();
    let generation = cluster.meta().generation;
    let clusters = Api::<GraphCluster>::namespaced(ctx.client.clone(), &namespace);

    if let Err(errors) = cluster.spec.validate() {
        let message = stalled_message_for_validation(&errors);
        write_status_if_changed(
            &clusters,
            &name,
            &stalled_status(&message, generation, None),
        )
        .await?;
        return Ok(Action::requeue(Duration::from_secs(
            STRUCTURAL_REQUEUE_SECS,
        )));
    }

    // Resolution step 1: explicit wins when present.
    let resolved: Option<ResolvedObjectStore> = cluster
        .spec
        .object_store
        .explicit
        .as_ref()
        .map(resolve_explicit)
        .transpose()?;

    // Resolution step 2: the OBC's generated Secret + ConfigMap.
    let resolved = match resolved {
        Some(resolved) => resolved,
        None => {
            let Some(claim) = cluster.spec.object_store.object_bucket_claim.clone() else {
                // Resolution step 3: neither source set.
                let message = "objectStore has no source: set exactly one of \
                    objectStore.explicit or objectStore.objectBucketClaim"
                    .to_owned();
                write_status_if_changed(
                    &clusters,
                    &name,
                    &stalled_status(&message, generation, None),
                )
                .await?;
                return Ok(Action::requeue(Duration::from_secs(
                    STRUCTURAL_REQUEUE_SECS,
                )));
            };
            let secrets = Api::<Secret>::namespaced(ctx.client.clone(), &namespace);
            let configmaps = Api::<ConfigMap>::namespaced(ctx.client.clone(), &namespace);
            let obc_secret = secrets
                .get_opt(&claim.name)
                .await
                .map_err(Error::from_kube_error)?
                .ok_or_else(|| {
                    Error::MissingSecret(format!(
                        "objectBucketClaim {namespace}/{} has no generated Secret yet",
                        claim.name
                    ))
                })?;
            let obc_configmap = configmaps
                .get_opt(&claim.name)
                .await
                .map_err(Error::from_kube_error)?
                .ok_or_else(|| {
                    Error::MissingConfigMap(format!(
                        "objectBucketClaim {namespace}/{} has no generated ConfigMap yet",
                        claim.name
                    ))
                })?;
            match resolve_obc(&obc_secret, &obc_configmap, &namespace, &claim.name) {
                Ok(resolved) => resolved,
                Err(error) => {
                    let message = error.to_string();
                    write_status_if_changed(
                        &clusters,
                        &name,
                        &stalled_status(&message, generation, None),
                    )
                    .await?;
                    return Ok(Action::requeue(
                        error.requeue_after(&format!("{namespace}/{name}")),
                    ));
                }
            }
        }
    };

    let config_api = Api::<ConfigMap>::namespaced(ctx.client.clone(), &namespace);
    // Recompute the routing table from LIVE node-pod membership on every
    // reconcile: never from `replicaCount`, never cached across reconciles.
    // This is the reconciler's SINGLE child-state list: one `Api::list` of
    // node pods scoped by the cluster label, never N per-pod gets. The same
    // membership feeds both the routing table and `status.nodeAddresses`.
    let pods_api = Api::<Pod>::namespaced(ctx.client.clone(), &namespace);
    let node_selector_label = crate::status::node_pod_selector(&name);
    let live_pod_names: Vec<String> = pods_api
        .list(&ListParams::default().labels(&node_selector_label))
        .await
        .map_err(Error::from_kube_error)?
        .items
        .iter()
        .filter_map(|pod| pod.metadata.name.clone())
        .collect();
    let routing_table = routing_table_value(&name, &namespace, &live_pod_names);
    let desired_cm = desired_config_map(&cluster, &resolved, &routing_table)?;
    crate::apply::apply_child(&config_api, &desired_cm).await?;

    let services_api = Api::<Service>::namespaced(ctx.client.clone(), &namespace);
    for desired_svc in desired_services(&cluster)? {
        crate::apply::apply_child(&services_api, &desired_svc).await?;
    }

    let netpol_api = Api::<NetworkPolicy>::namespaced(ctx.client.clone(), &namespace);
    crate::apply::apply_child(&netpol_api, &desired_network_policy(&cluster)?).await?;

    // The ServiceMonitor is opt-in only: absent unless the spec flag is set.
    // A stale object (flag flipped back to false) is deleted so the
    // "no ServiceMonitor by default" contract holds across updates too.
    let sm_api: Api<DynamicObject> = Api::namespaced_with(
        ctx.client.clone(),
        &namespace,
        &service_monitor_api_resource(),
    );
    let sm_name = service_monitor_name(&name);
    if service_monitor_enabled(&cluster) {
        crate::apply::apply_child(&sm_api, &desired_service_monitor(&cluster)?).await?;
    } else if sm_api
        .get_opt(&sm_name)
        .await
        .map_err(Error::from_kube_error)?
        .is_some()
    {
        sm_api
            .delete(&sm_name, &DeleteParams::default())
            .await
            .map_err(Error::from_kube_error)?;
    }

    // cert-manager Certificate (opt-in via `tls.issuerRef`): the operator owns
    // the request, cert-manager owns the issuing, and the served Secret name is
    // what the node pods already mount.
    let cert_api: Api<DynamicObject> = Api::namespaced_with(
        ctx.client.clone(),
        &namespace,
        &crate::children::certificate_api_resource(),
    );
    let cert_name = crate::apply::cluster_child_name(&name, crate::children::ROLE_CERTIFICATE);
    if crate::children::certificate_enabled(&cluster) {
        crate::apply::apply_child(&cert_api, &crate::children::desired_certificate(&cluster)?)
            .await?;
    } else if crate::children::get_opt_tolerant(&cert_api, &cert_name)
        .await?
        .is_some()
    {
        cert_api
            .delete(&cert_name, &DeleteParams::default())
            .await
            .map_err(Error::from_kube_error)?;
    }

    // prometheus-operator PrometheusRule (opt-in): alerts on index staleness,
    // generation failures, unready pools, backups and restores.
    let rules_api: Api<DynamicObject> = Api::namespaced_with(
        ctx.client.clone(),
        &namespace,
        &crate::children::prometheus_rule_api_resource(),
    );
    let rules_name = crate::children::prometheus_rule_name(&name);
    if crate::children::prometheus_rule_enabled(&cluster) {
        crate::apply::apply_child(
            &rules_api,
            &crate::children::desired_prometheus_rule(&cluster)?,
        )
        .await?;
    } else if crate::children::get_opt_tolerant(&rules_api, &rules_name)
        .await?
        .is_some()
    {
        rules_api
            .delete(&rules_name, &DeleteParams::default())
            .await
            .map_err(Error::from_kube_error)?;
    }

    let secrets_api = Api::<Secret>::namespaced(ctx.client.clone(), &namespace);
    let existing_auth = secrets_api
        .get_opt(&crate::apply::cluster_child_name(&name, ROLE_AUTH))
        .await
        .map_err(Error::from_kube_error)?;
    match plan_auth(&cluster, existing_auth.as_ref()) {
        Ok(AuthPlan::Create(secret)) => {
            crate::apply::apply_child(&secrets_api, &secret).await?;
        }
        Ok(AuthPlan::Reference { .. }) => {}
        Err(error) => {
            let message = error.to_string();
            write_status_if_changed(
                &clusters,
                &name,
                &stalled_status(&message, generation, Some(resolved.source.clone())),
            )
            .await?;
            return Ok(Action::requeue(
                error.requeue_after(&format!("{namespace}/{name}")),
            ));
        }
    }

    // One-shot restore: create the job, wait for it, publish `Restored`. Pools
    // refuse to scale up until that condition is true, so no writer ever opens
    // a half-copied store.
    let restore_condition = reconcile_restore(&cluster, &ctx, &namespace).await?;

    let live = clusters.get(&name).await.map_err(Error::from_kube_error)?;
    let live_conditions_set = live
        .status
        .as_ref()
        .map(|status| status.conditions.clone())
        .unwrap_or_default();
    let mut desired_status = ready_status(
        &live_conditions_set,
        generation,
        resolved.source.clone(),
        crate::status::cluster_endpoints(&cluster),
        crate::status::sorted_bolt_addresses(&live_pod_names, &name, &namespace),
    );
    // Index telemetry: scrape the indexer's own metrics service. A failure is
    // recorded as `reachable: false` and never fails the reconcile: an indexer
    // that is down is exactly when the status must still be written.
    desired_status.indexing = scrape_indexing(&name, &namespace).await;
    if let Some(condition) = restore_condition {
        desired_status.conditions =
            crate::conditions::upsert_condition_returning(desired_status.conditions, condition);
    }
    write_status_if_changed(&clusters, &name, &desired_status).await?;

    // An in-flight restore keeps the requeue short so the condition flips as
    // soon as the job finishes.
    if cluster.spec.restore_from.is_some() && !restore_finished(&desired_status) {
        return Ok(Action::requeue(Duration::from_secs(RESTORE_POLL_SECS)));
    }

    Ok(Action::requeue(Duration::from_secs(SUCCESS_REQUEUE_SECS)))
}

/// Scrape the indexer metrics service and derive [`IndexingStatus`].
///
/// Staleness is tracked through the operator's own gauge: it compares the
/// published-generations counter with the previous observation, so
/// `staleSeconds` grows exactly while no new generation lands. Purely local
/// bookkeeping — no extra API objects, no CRD field to keep in sync.
async fn scrape_indexing(cluster_name: &str, namespace: &str) -> IndexingStatus {
    let service = indexer_metrics_service_name(cluster_name);
    let address = format!("{service}.{namespace}.svc:{}", INDEXER_METRICS_PORT);
    let Ok(body) = crate::scrape::fetch(&address, "/metrics").await else {
        crate::metrics::set_gauge(
            "hydradb_operator_index_reachable",
            &[("name", cluster_name)],
            0.0,
        );
        return IndexingStatus {
            reachable: false,
            ..IndexingStatus::default()
        };
    };
    let published = crate::scrape::metric_value(&body, "graph_indexer_generations_published");
    let failures = crate::scrape::metric_value(&body, "graph_indexer_generation_failures");
    let fallbacks = crate::scrape::metric_value(&body, "graph_indexer_incremental_fallbacks");

    let previous = crate::metrics::gauge_value(
        "hydradb_operator_index_published_generations",
        &[("name", cluster_name)],
    );
    let changed = published.is_some_and(|current| previous.is_none_or(|old| current > old));
    let stale_seconds = if changed || previous.is_none() {
        crate::metrics::set_gauge(
            "hydradb_operator_index_last_publish_timestamp_seconds",
            &[("name", cluster_name)],
            unix_now(),
        );
        crate::metrics::set_gauge(
            "hydradb_operator_index_stale_seconds",
            &[("name", cluster_name)],
            0.0,
        );
        Some(0)
    } else {
        let last = crate::metrics::gauge_value(
            "hydradb_operator_index_last_publish_timestamp_seconds",
            &[("name", cluster_name)],
        )
        .unwrap_or_else(unix_now);
        let stale = unix_now() - last;
        crate::metrics::set_gauge(
            "hydradb_operator_index_stale_seconds",
            &[("name", cluster_name)],
            stale,
        );
        // Only non-negative staleness reaches here; the cast is bounded by the
        // epoch-second magnitude, far inside u64.
        Some(stale.max(0.0).round() as u64)
    };

    if let Some(current) = published {
        crate::metrics::set_gauge(
            "hydradb_operator_index_published_generations",
            &[("name", cluster_name)],
            current,
        );
    }
    crate::metrics::set_gauge(
        "hydradb_operator_index_reachable",
        &[("name", cluster_name)],
        1.0,
    );
    IndexingStatus {
        generation: None,
        published_generations: published.map(|value| value as u64),
        incremental_fallbacks: fallbacks.map(|value| value as u64),
        generation_failures: failures.map(|value| value as u64),
        stale_seconds,
        observed_at: Some(chrono::Utc::now().to_rfc3339()),
        reachable: true,
    }
}

/// Current unix time as an `f64` second count.
fn unix_now() -> f64 {
    chrono::Utc::now().timestamp() as f64
}

/// Requeue cadence while a restore job is in flight.
const RESTORE_POLL_SECS: u64 = 10;

/// Whether the cluster reports a completed restore.
#[must_use]
pub fn restore_finished(status: &GraphClusterStatus) -> bool {
    status
        .conditions
        .iter()
        .any(|condition| condition.type_ == "Restored" && condition.status == "True")
}

/// Apply the restore job and derive its condition.
///
/// Returns `None` when the cluster declares no `restoreFrom`: nothing is
/// created and no condition is published, so an ordinary cluster has no trace
/// of the feature in its status.
async fn reconcile_restore(
    cluster: &GraphCluster,
    ctx: &Ctx,
    namespace: &str,
) -> Result<Option<k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition>, Error> {
    if cluster.spec.restore_from.is_none() {
        return Ok(None);
    }
    let jobs = Api::<k8s_openapi::api::batch::v1::Job>::namespaced(ctx.client.clone(), namespace);
    let job_name = crate::reconcile::backup::restore_job_name(&cluster.name_any());
    let existing = jobs
        .get_opt(&job_name)
        .await
        .map_err(Error::from_kube_error)?;
    let generation = cluster.meta().generation;

    if existing.is_none() {
        crate::apply::apply_child(
            &jobs,
            &crate::reconcile::backup::desired_restore_job(cluster)?,
        )
        .await?;
        return Ok(Some(crate::conditions::new_condition(
            "Restored",
            "False",
            hydradb_operator_api::spec::ConditionReason::Reconciling.label(),
            "restore job started",
            generation,
        )));
    }

    let status = existing.as_ref().and_then(|job| job.status.as_ref());
    if status
        .and_then(|status| status.succeeded)
        .is_some_and(|succeeded| succeeded > 0)
    {
        crate::metrics::set_gauge(
            "hydradb_operator_restore_failed",
            &[("name", &cluster.name_any())],
            0.0,
        );
        return Ok(Some(crate::conditions::new_condition(
            "Restored",
            "True",
            hydradb_operator_api::spec::ConditionReason::Available.label(),
            "restore completed",
            generation,
        )));
    }
    if status
        .and_then(|status| status.failed)
        .is_some_and(|failed| failed > 0)
    {
        crate::metrics::set_gauge(
            "hydradb_operator_restore_failed",
            &[("name", &cluster.name_any())],
            1.0,
        );
        return Ok(Some(crate::conditions::new_condition(
            "Restored",
            "False",
            hydradb_operator_api::spec::ConditionReason::Failed.label(),
            "restore job failed: inspect the job logs, then delete it to retry",
            generation,
        )));
    }
    Ok(Some(crate::conditions::new_condition(
        "Restored",
        "False",
        hydradb_operator_api::spec::ConditionReason::Reconciling.label(),
        "restore in progress",
        generation,
    )))
}

/// Reconcile one `GraphCluster` (controller entrypoint).
///
/// The ONLY top-level action is `finalizer::with_finalizer` stamping
/// [`constants::FINALIZER`]: the previous apply body runs as the `Apply`
/// closure and [`cleanup_cluster`] as the `Cleanup` closure, so
/// `spec.deletionPolicy` is enforced on every delete.
///
/// # Errors
///
/// Returns [`Error`] when validation, resolution, child SSA or the status
/// write fails; the controller maps it through [`error_policy`].
pub async fn reconcile(cluster: Arc<GraphCluster>, ctx: Arc<Ctx>) -> Result<Action, Error> {
    let namespace = cluster.namespace().unwrap_or_default();
    let api: Api<GraphCluster> = Api::namespaced(ctx.client.clone(), &namespace);
    let apply_ctx = ctx.clone();
    let cleanup_ctx = ctx.clone();
    crate::finalizer::with_finalizer(
        &api,
        cluster,
        |obj| async move {
            let span = metrics::reconcile_span(
                "GraphCluster",
                &obj.namespace().unwrap_or_default(),
                &obj.name_any(),
            );
            let _entered = span.enter();
            let start = Instant::now();
            let outcome = reconcile_inner(obj, apply_ctx).await;
            let recorded: Result<(), ErrorClass> =
                outcome.as_ref().map(|_| ()).map_err(Error::class);
            metrics::record_reconcile("GraphCluster", start.elapsed(), recorded);
            outcome
        },
        |obj| async move { cleanup_cluster(obj, cleanup_ctx).await },
    )
    .await
}

/// Effective `deletionPolicy` for the cluster: the spec field, which defaults
/// to `Retain` via `#[serde(default)]` when unset.
#[must_use]
pub fn deletion_policy_of(cluster: &GraphCluster) -> DeletionPolicy {
    cluster.spec.deletion_policy.clone()
}

/// Audit reason stamped to `pruned-by`: the policy name verbatim.
#[must_use]
pub fn policy_reason(policy: &DeletionPolicy) -> &'static str {
    match policy {
        DeletionPolicy::Retain => "Retain",
        DeletionPolicy::Delete => "Delete",
    }
}

/// Whether the object store is reachable enough to attempt prefix deletion.
///
/// Explicit spec input counts as reachable; an OBC claim counts as reachable
/// only while its generated Secret and ConfigMap both exist. Anything else
/// (no source, missing claim objects) is unreachable, so the cleanup plan
/// omits the data-prefix step and deletion never blocks on the store.
async fn store_is_reachable(
    cluster: &GraphCluster,
    ctx: &Ctx,
    namespace: &str,
) -> Result<bool, Error> {
    if cluster.spec.object_store.explicit.is_some() {
        return Ok(true);
    }
    let Some(claim) = cluster.spec.object_store.object_bucket_claim.clone() else {
        return Ok(false);
    };
    let secrets = Api::<Secret>::namespaced(ctx.client.clone(), namespace);
    let configmaps = Api::<ConfigMap>::namespaced(ctx.client.clone(), namespace);
    let secret = secrets
        .get_opt(&claim.name)
        .await
        .map_err(Error::from_kube_error)?;
    let configmap = configmaps
        .get_opt(&claim.name)
        .await
        .map_err(Error::from_kube_error)?;
    Ok(secret.is_some() && configmap.is_some())
}

/// Delete every operator-owned child of the cluster, tolerating 404s.
///
/// The auth Secret is deleted only when the operator created it (inline token
/// or generated): an `existingSecret` passthrough belongs to the user and is
/// never touched. A 404 from any delete is success (never-applied CR); any
/// other failure aborts so the delete is retried.
async fn delete_cluster_children(
    client: kube::Client,
    namespace: &str,
    cluster: &GraphCluster,
) -> Result<(), Error> {
    let name = cluster.name_any();
    let params = DeleteParams::default();
    let configmaps = Api::<ConfigMap>::namespaced(client.clone(), namespace);
    let _ = configmaps
        .delete(&cluster_child_name(&name, ROLE_CONFIG), &params)
        .await
        .map_err(Error::from_kube_error)?;
    if !matches!(plan_auth(cluster, None), Ok(AuthPlan::Reference { .. })) {
        let secrets = Api::<Secret>::namespaced(client.clone(), namespace);
        let _ = secrets
            .delete(&cluster_child_name(&name, ROLE_AUTH), &params)
            .await
            .map_err(Error::from_kube_error)?;
    }
    let services = Api::<Service>::namespaced(client.clone(), namespace);
    for service in [
        headless_service_name(&name),
        indexer_metrics_service_name(&name),
        cluster_child_name(&name, ROLE_BOLT),
        cluster_child_name(&name, ROLE_HTTPS),
        cluster_child_name(&name, ROLE_METRICS),
    ] {
        let _ = services
            .delete(&service, &params)
            .await
            .map_err(Error::from_kube_error)?;
    }
    let netpols = Api::<NetworkPolicy>::namespaced(client.clone(), namespace);
    let _ = netpols
        .delete(&cluster_child_name(&name, ROLE_NETPOL), &params)
        .await
        .map_err(Error::from_kube_error)?;
    // The ServiceMonitor may never have existed (opt-in): a 404 from this
    // delete is success, handled by `execute_cleanup` like every other child.
    let sm_api: Api<DynamicObject> =
        Api::namespaced_with(client, namespace, &service_monitor_api_resource());
    let _ = sm_api
        .delete(&service_monitor_name(&name), &params)
        .await
        .map_err(Error::from_kube_error)?;
    Ok(())
}

/// `Cleanup` closure for [`reconcile`]: enforce `spec.deletionPolicy`.
///
/// `Retain` (default) stamps `pruned-by` and removes children, leaving
/// object-store data intact. `Delete` additionally plans the cluster-scoped
/// data-prefix removal; the prefix hook records the exact prefix and releases
/// the finalizer because this crate ships no object-store client (adding one
/// needs a new dependency, forbidden to this row), while an unreachable store
/// omits the step entirely so deletion still terminates.
async fn cleanup_cluster(cluster: Arc<GraphCluster>, ctx: Arc<Ctx>) -> Result<Action, Error> {
    let name = cluster.name_any();
    let namespace = cluster.namespace().unwrap_or_default();
    let policy = deletion_policy_of(&cluster);
    let reason = policy_reason(&policy);
    let client = ctx.client.clone();
    let selector = format!("{}={name}", crate::apply::CLUSTER_SELECTOR_LABEL);
    let list = ListParams::default().labels(&selector);
    let owned_configmaps = Api::<ConfigMap>::namespaced(client.clone(), &namespace)
        .list(&list)
        .await
        .map_err(Error::from_kube_error)?;
    let owned_secrets = Api::<Secret>::namespaced(client.clone(), &namespace)
        .list(&list)
        .await
        .map_err(Error::from_kube_error)?;
    let owned_services = Api::<Service>::namespaced(client.clone(), &namespace)
        .list(&list)
        .await
        .map_err(Error::from_kube_error)?;
    let owned_netpols = Api::<NetworkPolicy>::namespaced(client.clone(), &namespace)
        .list(&list)
        .await
        .map_err(Error::from_kube_error)?;
    // The ServiceMonitor only exists when the spec flag is set; a missing
    // CRD surfaces as 404, which `get_opt` maps to `None`.
    let sm_api: Api<DynamicObject> =
        Api::namespaced_with(client.clone(), &namespace, &service_monitor_api_resource());
    let owned_sm = if service_monitor_enabled(&cluster) {
        sm_api
            .get_opt(&service_monitor_name(&name))
            .await
            .map_err(Error::from_kube_error)?
            .is_some()
    } else {
        false
    };
    let children_exist = !owned_configmaps.items.is_empty()
        || !owned_secrets.items.is_empty()
        || !owned_services.items.is_empty()
        || !owned_netpols.items.is_empty()
        || owned_sm;
    let store_reachable = store_is_reachable(&cluster, &ctx, &namespace).await?;
    let plan = crate::finalizer::cleanup_plan(&policy, children_exist, store_reachable, &name);
    let cr_api: Api<GraphCluster> = Api::namespaced(client.clone(), &namespace);
    crate::finalizer::execute_cleanup(
        &plan,
        || crate::finalizer::stamp_pruned_by(&cr_api, &name, reason),
        || delete_cluster_children(client.clone(), &namespace, &cluster),
        |prefix| async move {
            tracing::warn!(
                prefix,
                reason,
                "no object-store client in this crate; leaving the data prefix and releasing \
                 the finalizer (physical prefix removal needs a store client)"
            );
            Ok::<(), Error>(())
        },
    )
    .await?;
    Ok(Action::await_change())
}

/// Map a reconcile failure to a requeue delay via [`Error::class`].
#[must_use]
pub fn error_policy(cluster: Arc<GraphCluster>, error: &Error, _ctx: Arc<Ctx>) -> Action {
    let key = format!(
        "{}/{}",
        cluster.namespace().unwrap_or_default(),
        cluster.name_any()
    );
    Action::requeue(error.requeue_after(&key))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cluster_fixture() -> GraphCluster {
        serde_json::from_value(serde_json::json!({
            "apiVersion": "hydradb.kubexa.tech/v1alpha1",
            "kind": "GraphCluster",
            "metadata": {"name": "demo", "namespace": "default", "uid": "abc-123"},
            "spec": {
                "image": {"repository": "example.com/hydradb"},
                "graph": {"namespace": "ns", "id": "g", "database": "db", "cellId": "c0"},
                "objectStore": {
                    "explicit": {"bucket": "my-bucket", "region": "us-east-1"}
                },
                "auth": {"generate": true}
            }
        }))
        .expect("fixture must deserialize")
    }

    fn obc_secret_fixture() -> Secret {
        let data = BTreeMap::from([
            ("AWS_ACCESS_KEY_ID".to_owned(), ByteString(b"akid".to_vec())),
            (
                "AWS_SECRET_ACCESS_KEY".to_owned(),
                ByteString(b"secret".to_vec()),
            ),
            ("BUCKET_NAME".to_owned(), ByteString(b"obc-bucket".to_vec())),
            (
                "BUCKET_HOST".to_owned(),
                ByteString(b"s3.example.com".to_vec()),
            ),
            ("BUCKET_PORT".to_owned(), ByteString(b"80".to_vec())),
            (
                "BUCKET_REGION".to_owned(),
                ByteString(b"eu-west-1".to_vec()),
            ),
        ]);
        Secret {
            metadata: k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta {
                name: Some("my-claim".to_owned()),
                namespace: Some("default".to_owned()),
                ..Default::default()
            },
            data: Some(data),
            string_data: None,
            type_: Some("Opaque".to_owned()),
            immutable: None,
        }
    }

    fn obc_configmap_fixture() -> ConfigMap {
        ConfigMap {
            metadata: k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta {
                name: Some("my-claim".to_owned()),
                namespace: Some("default".to_owned()),
                ..Default::default()
            },
            data: Some(BTreeMap::from([(
                "BUCKET_HOST".to_owned(),
                "s3.example.com".to_owned(),
            )])),
            immutable: None,
            binary_data: None,
        }
    }

    #[test]
    fn config_keys_are_exactly_the_operand_env_names() {
        let cluster = cluster_fixture();
        let resolved = resolve_explicit(
            cluster
                .spec
                .object_store
                .explicit
                .as_ref()
                .expect("fixture sets explicit"),
        )
        .expect("fixture explicit resolves");
        let data = config_data(&cluster, &resolved, "");

        // Every pinned constant MUST be a key: the ConfigMap speaks the
        // operand's env language, never a re-typed literal.
        for key in [
            constants::GRAPH_NAMESPACE,
            constants::GRAPH_ID,
            constants::GRAPH_DATABASE,
            constants::GRAPH_CELL_ID,
            constants::GRAPH_CELLS,
            constants::CLOUD_PROVIDER,
            constants::AWS_BUCKET_NAME,
            constants::AWS_DEFAULT_REGION,
            constants::AWS_ALLOW_HTTP,
            constants::AWS_ENDPOINT,
            constants::RUST_LOG,
        ] {
            assert!(
                data.contains_key(key),
                "config data must carry the operand env {key}"
            );
        }
        // Exactly one cell: GRAPH_CELLS is the cell itself.
        assert_eq!(
            data.get(constants::GRAPH_CELLS).map(String::as_str),
            Some("c0")
        );
        assert_eq!(
            data.get(constants::GRAPH_CELL_ID).map(String::as_str),
            Some("c0")
        );
        // Resolved object-store values land on the operand env.
        assert_eq!(
            data.get(constants::AWS_BUCKET_NAME).map(String::as_str),
            Some("my-bucket")
        );
        // Credentials NEVER land in the ConfigMap.
        for credential in [
            "AWS_ACCESS_KEY_ID",
            "AWS_SECRET_ACCESS_KEY",
            "BUCKET_HOST",
            "BUCKET_NAME",
            "BUCKET_PORT",
            "BUCKET_REGION",
        ] {
            assert!(
                !data.contains_key(credential),
                "credential {credential} must never be a ConfigMap key"
            );
        }
    }

    #[test]
    fn generated_token_is_preserved_across_two_reconciles() {
        let cluster = cluster_fixture();
        let first = plan_auth(&cluster, None).expect("first plan creates the Secret");
        let created = match first {
            AuthPlan::Create(secret) => secret,
            AuthPlan::Reference { .. } => {
                panic!("generate:true must create, not reference");
            }
        };
        let first_token = created
            .data
            .as_ref()
            .and_then(|data| data.get(DEFAULT_AUTH_KEY))
            .expect("created Secret carries the token")
            .0
            .clone();
        assert!(!first_token.is_empty(), "generated token is non-empty");

        // Second reconcile sees the existing Secret: byte-identical token.
        let second = plan_auth(&cluster, Some(&created)).expect("second plan preserves");
        let kept = match second {
            AuthPlan::Create(secret) => secret,
            AuthPlan::Reference { .. } => {
                panic!("generate:true must create, not reference");
            }
        };
        let second_token = kept
            .data
            .as_ref()
            .and_then(|data| data.get(DEFAULT_AUTH_KEY))
            .expect("kept Secret carries the token")
            .0
            .clone();
        assert_eq!(
            first_token, second_token,
            "plain reconcile must never rotate the token"
        );
    }

    #[test]
    fn resolution_picks_explicit_when_present() {
        let cluster = cluster_fixture();
        let resolved = resolve_explicit(
            cluster
                .spec
                .object_store
                .explicit
                .as_ref()
                .expect("fixture sets explicit"),
        )
        .expect("explicit resolves");
        assert_eq!(resolved.bucket, "my-bucket");
        assert_eq!(resolved.source, "explicit:my-bucket");
    }

    #[test]
    fn resolution_reads_the_obc_secret_and_configmap() {
        let resolved = resolve_obc(
            &obc_secret_fixture(),
            &obc_configmap_fixture(),
            "default",
            "my-claim",
        )
        .expect("OBC fixtures resolve");
        assert_eq!(resolved.bucket, "obc-bucket");
        assert_eq!(resolved.region, "eu-west-1");
        assert_eq!(resolved.endpoint, "http://s3.example.com:80");
        assert!(resolved.allow_http, "plain-http compose allows http");
        assert_eq!(resolved.source, "obc:default/my-claim");
    }

    #[test]
    fn missing_store_yields_stalled_naming_object_store() {
        let mut cluster = cluster_fixture();
        cluster.spec.object_store.explicit = None;
        cluster.spec.object_store.object_bucket_claim = None;
        let errors = cluster
            .spec
            .validate()
            .expect_err("empty object store must fail validation");
        let message = stalled_message_for_validation(&errors);
        assert!(
            message.contains("object_store"),
            "Stalled message must name objectStore, got: {message}"
        );
        let status = stalled_status(&message, Some(1), None);
        let stalled = status
            .conditions
            .iter()
            .find(|c| c.type_ == STALLED)
            .expect("stalled status carries Stalled");
        assert_eq!(stalled.status, "True");
        assert!(
            stalled.message.contains("object_store"),
            "Stalled condition must name objectStore, got: {}",
            stalled.message
        );
    }

    #[test]
    fn ready_status_carries_the_full_todo_24_surface() {
        let cluster = cluster_fixture();
        let generation = cluster.meta().generation;
        let endpoints = crate::status::cluster_endpoints(&cluster);
        let node_addresses =
            crate::status::sorted_bolt_addresses(&["demo-node-0".to_owned()], "demo", "default");
        let status = ready_status(
            &[],
            generation,
            "explicit:my-bucket".to_owned(),
            endpoints,
            node_addresses,
        );
        assert_eq!(
            status.observed_generation, generation,
            "observedGeneration must equal the reconciled metadata.generation"
        );
        assert_eq!(
            status.endpoints.bolt.as_deref(),
            Some("demo-bolt.default.svc:7687"),
            "endpoints.bolt must name the Bolt Service"
        );
        assert!(
            status.endpoints.https.is_some() && status.endpoints.admin.is_some(),
            "https and admin endpoints must be set"
        );
        assert_eq!(
            status.node_addresses.len(),
            1,
            "one live pod means one node address"
        );
        assert_eq!(
            status.observed_object_store.as_deref(),
            Some("explicit:my-bucket")
        );
        let ready = status
            .conditions
            .iter()
            .find(|c| c.type_ == "Ready")
            .expect("ready status carries Ready");
        assert_eq!(ready.status, "True");
        assert_eq!(ready.observed_generation, generation);
    }

    #[test]
    fn observed_generation_tracks_a_spec_change_generation() {
        let mut cluster = cluster_fixture();
        cluster.metadata.generation = Some(9);
        let status = ready_status(
            &[],
            cluster.meta().generation,
            "explicit:my-bucket".to_owned(),
            crate::status::cluster_endpoints(&cluster),
            Vec::new(),
        );
        assert_eq!(
            status.observed_generation,
            Some(9),
            "observedGeneration must advance to the new generation once reconciled"
        );
        for condition in &status.conditions {
            assert_eq!(
                condition.observed_generation,
                Some(9),
                "every condition must track the reconciled generation"
            );
        }
    }

    #[test]
    fn validation_failures_surface_on_the_stalled_message() {
        let mut cluster = cluster_fixture();
        cluster.spec.graph.cell_id.clear();
        let errors = cluster
            .spec
            .validate()
            .expect_err("empty cell_id must fail validation");
        let message = stalled_message_for_validation(&errors);
        assert!(
            message.contains("graph.cell_id"),
            "Stalled message must name the field, got: {message}"
        );
    }

    #[test]
    fn passthrough_reference_creates_nothing() {
        let mut cluster = cluster_fixture();
        cluster.spec.auth.generate = false;
        cluster.spec.auth.existing_secret = Some("user-auth".to_owned());
        cluster.spec.auth.secret_key = Some("token".to_owned());
        match plan_auth(&cluster, None).expect("passthrough plans") {
            AuthPlan::Reference { name, key } => {
                assert_eq!(name, "user-auth");
                assert_eq!(key, "token");
            }
            AuthPlan::Create(_) => {
                panic!("existingSecret must reference, not create");
            }
        }
    }

    #[test]
    fn auth_without_any_source_is_structural_naming_auth() {
        let mut cluster = cluster_fixture();
        cluster.spec.auth.generate = false;
        let error = plan_auth(&cluster, None).expect_err("no source must fail");
        assert_eq!(error.class(), ErrorClass::Structural);
        assert!(
            error.to_string().contains("auth"),
            "error must name auth, got: {error}"
        );
    }

    fn service_by_name<'a>(
        services: &'a [k8s_openapi::api::core::v1::Service],
        name: &str,
    ) -> &'a k8s_openapi::api::core::v1::Service {
        services
            .iter()
            .find(|svc| svc.metadata.name.as_deref() == Some(name))
            .unwrap_or_else(|| panic!("expected a Service named {name}"))
    }

    fn service_ports(svc: &k8s_openapi::api::core::v1::Service) -> Vec<(String, i32, String)> {
        svc.spec
            .as_ref()
            .expect("service must carry a spec")
            .ports
            .clone()
            .unwrap_or_default()
            .into_iter()
            .map(|port| {
                let target = port
                    .target_port
                    .map(|target| match target {
                        k8s_openapi::apimachinery::pkg::util::intstr::IntOrString::Int(value) => {
                            value.to_string()
                        }
                        k8s_openapi::apimachinery::pkg::util::intstr::IntOrString::String(
                            value,
                        ) => value,
                    })
                    .unwrap_or_default();
                (port.name.unwrap_or_default(), port.port, target)
            })
            .collect()
    }

    #[test]
    fn all_five_services_are_built_with_the_expected_ports_and_selectors() {
        let cluster = cluster_fixture();
        let services = desired_services(&cluster).expect("services build without a cluster");
        assert_eq!(services.len(), 5, "exactly five Services must be built");

        let headless = service_by_name(&services, "demo-node-headless");
        assert_eq!(
            service_ports(headless),
            vec![
                ("bolt".to_owned(), 7687, "7687".to_owned()),
                ("https".to_owned(), 8443, "8443".to_owned()),
                ("admin".to_owned(), 9090, "9090".to_owned()),
            ]
        );

        let bolt = service_by_name(&services, "demo-bolt");
        assert_eq!(
            service_ports(bolt),
            vec![("bolt".to_owned(), 7687, "7687".to_owned())]
        );

        let https = service_by_name(&services, "demo-https");
        assert_eq!(
            service_ports(https),
            vec![("https".to_owned(), 443, "8443".to_owned())]
        );

        let metrics = service_by_name(&services, "demo-metrics");
        assert_eq!(
            service_ports(metrics),
            vec![("metrics".to_owned(), 9090, "9090".to_owned())]
        );

        let indexer_metrics = service_by_name(&services, "demo-indexer-metrics");
        assert_eq!(
            service_ports(indexer_metrics),
            vec![("metrics".to_owned(), 9091, "9091".to_owned())]
        );

        // Node services select the node pods; the indexer metrics Service
        // selects the indexer pods.
        let node_selector = node_pod_selector("demo");
        for svc in [headless, bolt, https, metrics] {
            assert_eq!(
                svc.spec.as_ref().expect("service has a spec").selector,
                Some(node_selector.clone()),
                "node Service {} must select the node pods",
                svc.metadata.name.as_deref().unwrap_or_default()
            );
        }
        assert_eq!(
            indexer_metrics
                .spec
                .as_ref()
                .expect("service has a spec")
                .selector,
            Some(indexer_pod_selector("demo")),
            "indexer metrics Service must select the indexer pods"
        );
    }

    #[test]
    fn headless_service_is_clusterip_none_and_publishes_not_ready_addresses() {
        let cluster = cluster_fixture();
        let headless =
            desired_headless_service(&cluster).expect("headless Service builds without a cluster");
        let spec = headless.spec.expect("headless Service must carry a spec");
        assert_eq!(
            spec.cluster_ip.as_deref(),
            Some("None"),
            "headless Service must be clusterIP: None"
        );
        assert_eq!(
            spec.publish_not_ready_addresses,
            Some(true),
            "headless Service must publish not-ready addresses"
        );
    }

    #[test]
    fn routing_table_is_recomputed_not_read_from_the_config_map() {
        let cluster = cluster_fixture();
        let resolved = resolve_explicit(
            cluster
                .spec
                .object_store
                .explicit
                .as_ref()
                .expect("fixture sets explicit"),
        )
        .expect("fixture explicit resolves");

        // First reconcile sees two live pods.
        let first = routing_table_value(
            "demo",
            "default",
            &["demo-node-0".to_owned(), "demo-node-1".to_owned()],
        );
        let data = config_data(&cluster, &resolved, &first);
        assert_eq!(
            data.get(constants::GRAPH_BOLT_NODE_ADDRESSES)
                .map(String::as_str),
            Some(first.as_str())
        );

        // Second reconcile sees different live membership: the value follows
        // the live list, never a cached copy of the first ConfigMap.
        let second = routing_table_value("demo", "default", &["demo-node-0".to_owned()]);
        assert_ne!(
            first, second,
            "different live membership must yield a different routing table"
        );
        let data = config_data(&cluster, &resolved, &second);
        assert_eq!(
            data.get(constants::GRAPH_BOLT_NODE_ADDRESSES)
                .map(String::as_str),
            Some(second.as_str())
        );
    }

    #[test]
    fn routing_table_for_a_three_replica_pool_has_three_addresses() {
        let value = routing_table_value(
            "demo",
            "default",
            &[
                "demo-node-0".to_owned(),
                "demo-node-1".to_owned(),
                "demo-node-2".to_owned(),
            ],
        );
        let entries: Vec<&str> = value.split(',').collect();
        assert_eq!(entries.len(), 3, "three live pods yield three addresses");
        for (ordinal, entry) in entries.iter().enumerate() {
            assert_eq!(
                *entry,
                format!(
                    "demo-node-{ordinal}=demo-node-{ordinal}.\
                     demo-node-headless.default.svc.cluster.local:7687"
                )
            );
        }
    }

    #[test]
    fn routing_table_for_zero_replicas_is_empty_and_not_stale() {
        let populated = routing_table_value("demo", "default", &["demo-node-0".to_owned()]);
        assert!(
            !populated.is_empty(),
            "one live pod must yield a non-empty table"
        );
        let emptied: Vec<String> = Vec::new();
        assert_eq!(
            routing_table_value("demo", "default", &emptied),
            String::new(),
            "zero live pods must yield an empty value, never a stale table"
        );
    }

    #[test]
    fn every_service_carries_the_standard_labels_and_cluster_selector_label() {
        let cluster = cluster_fixture();
        let services = desired_services(&cluster).expect("services build without a cluster");
        for svc in &services {
            let labels = svc.metadata.labels.clone().unwrap_or_default();
            let name = svc.metadata.name.clone().unwrap_or_default();
            for (key, value) in [
                ("app.kubernetes.io/name", "hydradb"),
                ("app.kubernetes.io/instance", "demo"),
                ("app.kubernetes.io/managed-by", "hydradb-operator"),
                ("app.kubernetes.io/part-of", "hydradb"),
                (crate::apply::CLUSTER_SELECTOR_LABEL, "demo"),
            ] {
                assert_eq!(
                    labels.get(key).map(String::as_str),
                    Some(value),
                    "Service {name} must carry label {key}={value}"
                );
            }
        }
    }

    const RECONCILE_SOURCE: &str = include_str!("cluster.rs");

    #[test]
    fn reconcile_wraps_everything_in_the_shared_finalizer() {
        assert!(
            RECONCILE_SOURCE.contains("finalizer::with_finalizer"),
            "reconcile must call finalizer::with_finalizer as its top-level action"
        );
        // The production half of this file (everything before the test
        // module) must never spell the finalizer string: the name comes from
        // `constants::FINALIZER`. The assertion below necessarily names the
        // literal, so only the pre-test half is scanned.
        let production = RECONCILE_SOURCE
            .split("#[cfg(test)]")
            .next()
            .unwrap_or(RECONCILE_SOURCE);
        assert!(
            !production.contains("hydradb.kubexa.tech/finalizer"),
            "the finalizer name must come from constants::FINALIZER, never a literal"
        );
        assert_eq!(
            crate::finalizer::finalizer_name(),
            constants::FINALIZER,
            "the wired finalizer must be exactly the shared constant"
        );
    }

    #[test]
    fn deletion_policy_defaults_to_retain_when_the_field_is_unset() {
        let cluster: GraphCluster = serde_json::from_value(serde_json::json!({
            "apiVersion": "hydradb.kubexa.tech/v1alpha1",
            "kind": "GraphCluster",
            "metadata": {"name": "demo", "namespace": "default", "uid": "abc-123"},
            "spec": {
                "image": {"repository": "example.com/hydradb"},
                "graph": {"namespace": "ns", "id": "g", "database": "db", "cellId": "c0"},
                "objectStore": {
                    "explicit": {"bucket": "my-bucket", "region": "us-east-1"}
                },
                "auth": {"generate": true}
            }
        }))
        .expect("fixture without deletionPolicy must deserialize");
        assert_eq!(
            deletion_policy_of(&cluster),
            DeletionPolicy::Retain,
            "an unset deletionPolicy must default to Retain"
        );
        assert_eq!(policy_reason(&DeletionPolicy::Retain), "Retain");
        assert_eq!(policy_reason(&DeletionPolicy::Delete), "Delete");
    }

    #[tokio::test]
    async fn retain_cleanup_stamps_first_and_leaves_data_intact() {
        use crate::finalizer::{CleanupAction, cleanup_plan, execute_cleanup};
        use std::sync::Mutex;

        let cluster = cluster_fixture();
        let policy = deletion_policy_of(&cluster);
        assert_eq!(policy, DeletionPolicy::Retain);
        let plan = cleanup_plan(&policy, true, true, "demo");
        assert!(
            plan.iter()
                .all(|action| !matches!(action, CleanupAction::DeleteDataPrefix { .. })),
            "Retain must never plan data-prefix removal"
        );
        let order = Mutex::new(Vec::new());
        execute_cleanup(
            &plan,
            || async { Ok(()) },
            || {
                order.lock().expect("order log unlocks").push("children");
                async { Ok(()) }
            },
            |prefix: String| async move {
                panic!("Retain must never invoke the prefix hook, got: {prefix}");
                #[allow(unreachable_code)]
                Ok::<(), Error>(())
            },
        )
        .await
        .expect("retain cleanup must succeed");
        assert_eq!(
            order.lock().expect("order log unlocks").as_slice(),
            ["children"],
            "children deletion must run after the stamp step"
        );
    }

    #[tokio::test]
    async fn cleanup_of_a_never_applied_cr_returns_ok_without_touching_children_or_store() {
        use crate::finalizer::{cleanup_plan, execute_cleanup};

        let cluster = cluster_fixture();
        let policy = deletion_policy_of(&cluster);
        let plan = cleanup_plan(&policy, false, false, "demo");
        assert_eq!(plan.len(), 1, "nothing applied means stamp only");
        execute_cleanup(
            &plan,
            || async { Ok(()) },
            || async {
                panic!("no children exist; the child hook must never fire");
                #[allow(unreachable_code)]
                Ok::<(), Error>(())
            },
            |_: String| async {
                panic!("no prefix step exists; the store hook must never fire");
                #[allow(unreachable_code)]
                Ok::<(), Error>(())
            },
        )
        .await
        .expect("never-applied cleanup must return Ok, never panic");
    }

    #[test]
    fn network_policy_covers_the_documented_ports_from_the_same_namespace() {
        let cluster = cluster_fixture();
        let policy =
            desired_network_policy(&cluster).expect("network policy builds without a cluster");
        assert_eq!(
            policy.metadata.name.as_deref(),
            Some("demo-netpol"),
            "network policy name is deterministic"
        );
        let spec = policy.spec.expect("network policy must carry a spec");
        assert_eq!(
            spec.pod_selector.expect("policy selects pods").match_labels,
            Some(cluster_pod_selector("demo")),
            "policy must cover every pod of the cluster"
        );
        assert_eq!(
            spec.policy_types,
            Some(vec!["Ingress".to_owned(), "Egress".to_owned()]),
            "policy must restrict both directions"
        );
        let ingress = spec.ingress.expect("policy must carry an ingress rule");
        assert_eq!(ingress.len(), 1, "exactly one ingress rule");
        let mut ports: Vec<i32> = ingress[0]
            .ports
            .clone()
            .unwrap_or_default()
            .into_iter()
            .map(|port| match port.port {
                Some(IntOrString::Int(value)) => value,
                other => panic!("ingress port must be numeric, got: {other:?}"),
            })
            .collect();
        ports.sort_unstable();
        assert_eq!(
            ports,
            vec![7687, 8443, 9090, 9091],
            "ingress must open exactly Bolt/HTTPS/admin/indexer-admin"
        );
        let from = ingress[0].from.clone().unwrap_or_default();
        assert_eq!(from.len(), 1, "exactly one ingress source");
        assert!(
            from[0].pod_selector.is_some() && from[0].ip_block.is_none(),
            "ingress source must be same-namespace pods, got: {from:?}"
        );
        let egress = spec.egress.expect("policy must carry egress rules");
        assert_eq!(egress.len(), 2, "DNS plus object-store egress");
    }

    #[test]
    fn service_monitor_is_absent_by_default_and_builds_when_enabled() {
        let cluster = cluster_fixture();
        assert!(
            !service_monitor_enabled(&cluster),
            "a default cluster must not enable the ServiceMonitor"
        );
        let mut enabled = cluster;
        enabled.spec.monitoring.service_monitor.enabled = true;
        assert!(
            service_monitor_enabled(&enabled),
            "the spec flag must opt into the ServiceMonitor"
        );
        let obj =
            desired_service_monitor(&enabled).expect("service monitor builds without a cluster");
        assert_eq!(
            obj.metadata.name.as_deref(),
            Some("demo-servicemonitor"),
            "service monitor name is deterministic"
        );
        let spec = obj
            .data
            .get("spec")
            .expect("service monitor must carry a spec");
        let match_labels = spec
            .get("selector")
            .and_then(|selector| selector.get("matchLabels"))
            .expect("service monitor must carry a selector");
        assert_eq!(
            match_labels.get("app.kubernetes.io/component"),
            Some(&serde_json::Value::String("metrics".to_owned())),
            "monitor must select the node metrics Service, got: {spec}"
        );
        let endpoints = spec
            .get("endpoints")
            .expect("service monitor must carry endpoints");
        assert_eq!(
            endpoints,
            &serde_json::json!([{"port": "metrics", "path": "/metrics", "interval": "30s"}]),
            "monitor must scrape /metrics on the metrics port, got: {endpoints}"
        );
    }
}
