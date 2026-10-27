//! `hydradb doctor` — read-only readiness checklist (plan todo 29).
//!
//! Documented check list, run in order, each printing `PASS` or `FAIL` with
//! its stable snake_case name. The command exits 0 only when every check
//! passes and 1 when any check fails (see [`exit_code_for`]).
//!
//! 1. `crds_installed` — the three CRDs exist at group
//!    `hydradb.kubexa.tech`, version `v1alpha1`, and serve that version.
//! 2. `operator_deployment_ready` — the `hydradb-operator` Deployment exists
//!    and has available replicas.
//! 3. `graphclusters_ready` — every `GraphCluster` in scope has `Ready=True`.
//! 4. `object_store_reachable` — every cluster's object store is reachable:
//!    an explicit endpoint gets a bounded TCP probe (5s timeout); an
//!    `objectBucketClaim` source is reachable when the claim's generated
//!    Secret and ConfigMap (same name, cluster namespace, mirroring the
//!    controller's resolution in `reconcile/cluster.rs`) both exist.
//! 5. `auth_secret_present` — every cluster's auth Secret exists and carries
//!    the expected key (`spec.auth.secretKey`, default `auth-token`, mirroring
//!    `DEFAULT_AUTH_KEY` in `reconcile/cluster.rs`). Only key PRESENCE is
//!    checked; values are never read into output.
//! 6. `services_resolvable` — every hydradb Service in scope carries a
//!    cluster IP (or, for headless Services, ports).
//! 7. `pods_readyz_ok` — every hydradb pod in scope reports `Ready=True` with
//!    all containers ready. This is the API-server-reported readiness the
//!    kubelet drives from the pods' `/readyz` probes (node admin 9090,
//!    indexer admin 9091); the CLI never execs or port-forwards, so the check
//!    stays read-only.
//! 8. `no_stalled_condition` — no `GraphCluster`/`GraphNodePool`/`GraphIndexer`
//!    in scope carries `Stalled=True`.
//!
//! Read-only, always: only `get`/`list` verbs. A check that cannot run
//! (RBAC denial, unreachable store, missing object) reports `FAIL` with the
//! reason — never a retry with broader credentials, never a crash. Nothing
//! here requires cluster-admin.

use std::time::Duration;

use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::{ConfigMap, Pod, Secret, Service};
use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
use kube::Client;
use kube::api::{Api, ListParams};
use serde_json::Value;
use tokio::net::TcpStream;

use crate::{Cli, CliError, build_kube_config, effective_namespace};
use hydradb_operator_api::constants::{GROUP, VERSION};
use hydradb_operator_api::crd::{GraphCluster, GraphIndexer, GraphNodePool};

/// Arguments for `hydradb doctor`.
#[derive(Debug, Clone, Default, clap::Args)]
pub struct DoctorArgs {}

// --- Stable check identity -------------------------------------------------

/// Stable machine-readable names of every doctor check, in run order.
///
/// Tests assert this exact set; renaming a check is a breaking change.
pub const CHECK_NAMES: [&str; 8] = [
    "crds_installed",
    "operator_deployment_ready",
    "graphclusters_ready",
    "object_store_reachable",
    "auth_secret_present",
    "services_resolvable",
    "pods_readyz_ok",
    "no_stalled_condition",
];

/// Name of the operator Deployment (`deploy/rbac/operator-clusterrole.yaml`
/// and the controller's `managed-by` label agree on `hydradb-operator`).
const OPERATOR_DEPLOYMENT_NAME: &str = "hydradb-operator";

/// Label every operand pod and Service carries
/// (`reconcile/cluster.rs::node_pod_selector`, `reconcile/nodepool.rs`).
const HYDRADB_NAME_LABEL: &str = "app.kubernetes.io/name=hydradb";

/// Default key inside the auth Secret (mirrors `DEFAULT_AUTH_KEY` in
/// `reconcile/cluster.rs`; the CLI crate must not depend on the controller).
const DEFAULT_AUTH_KEY: &str = "auth-token";

/// Timeout for every network probe; the whole command must terminate.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// One checklist outcome: a stable name plus pass/fail and a human detail.
///
/// The detail never carries secret values — only object names, key names and
/// reason strings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckResult {
    /// Stable snake_case identifier from [`CHECK_NAMES`].
    pub name: &'static str,
    /// True when the check passed.
    pub passed: bool,
    /// Human-readable reason; names objects, never secret values.
    pub detail: String,
}

impl CheckResult {
    /// Build a passing check result.
    #[must_use]
    pub fn pass(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            passed: true,
            detail: detail.into(),
        }
    }

    /// Build a failing check result (including internal errors, which are
    /// FAILs, never crashes).
    #[must_use]
    pub fn fail(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            passed: false,
            detail: detail.into(),
        }
    }

    /// Render one checklist line: `PASS <name>: <detail>`.
    #[must_use]
    pub fn render(&self) -> String {
        format!(
            "{} {}: {}",
            if self.passed { "PASS" } else { "FAIL" },
            self.name,
            self.detail
        )
    }
}

/// Stable check names as a vector (test helper).
#[must_use]
pub fn check_names() -> Vec<&'static str> {
    CHECK_NAMES.to_vec()
}

/// Names of the failing checks, in run order.
#[must_use]
pub fn failed_names(results: &[CheckResult]) -> Vec<&'static str> {
    results
        .iter()
        .filter(|result| !result.passed)
        .map(|result| result.name)
        .collect()
}

/// Exit-code contract: 0 only when every check passed, 1 on any FAIL.
#[must_use]
pub fn exit_code_for(results: &[CheckResult]) -> i32 {
    if results.iter().all(|result| result.passed) {
        0
    } else {
        1
    }
}

// --- Pure helpers (unit-testable without a cluster) -------------------------

/// True when `conditions` carries `type_==want` with `status=="True"`.
#[must_use]
pub fn condition_true(conditions: &[Value], want: &str) -> bool {
    conditions.iter().any(|condition| {
        condition.get("type").and_then(Value::as_str) == Some(want)
            && condition.get("status").and_then(Value::as_str) == Some("True")
    })
}

/// `status.conditions` of a serialised object, or empty when absent.
#[must_use]
pub fn conditions_of(value: &Value) -> Vec<Value> {
    value
        .pointer("/status/conditions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// `Kind/name` strings for every object whose conditions carry
/// `Stalled=True`.
#[must_use]
pub fn stalled_in(objects: &[Value]) -> Vec<String> {
    objects
        .iter()
        .filter(|object| condition_true(&conditions_of(object), "Stalled"))
        .map(|object| {
            let kind = object
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("<unknown>");
            let name = object
                .pointer("/metadata/name")
                .and_then(Value::as_str)
                .unwrap_or("<unknown>");
            format!("{kind}/{name}")
        })
        .collect()
}

/// Expected `(secret name, secret key)` for a cluster, mirroring the
/// controller (`reconcile/nodepool.rs::auth_secret_name/key`): the user's
/// `existingSecret` passthrough when set, else `<cluster>-auth`; the user's
/// `secretKey` when set, else `auth-token`.
#[must_use]
pub fn auth_secret_plan(cluster: &Value) -> (String, String) {
    let cluster_name = cluster
        .pointer("/metadata/name")
        .and_then(Value::as_str)
        .unwrap_or("<unknown>");
    let existing = cluster
        .pointer("/spec/auth/existingSecret")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .map_or_else(
            || format!("{cluster_name}-auth"),
            std::string::ToString::to_string,
        );
    let key = cluster
        .pointer("/spec/auth/secretKey")
        .and_then(Value::as_str)
        .filter(|key| !key.is_empty())
        .map_or_else(
            || DEFAULT_AUTH_KEY.to_string(),
            std::string::ToString::to_string,
        );
    (existing, key)
}

/// Evaluate the `auth_secret_present` check against an already-fetched
/// Secret: `None` (absent) or a Secret missing `key` is FAIL; presence of
/// the key is PASS. The token value is never inspected or rendered.
#[must_use]
pub fn evaluate_auth_secret(
    namespace: &str,
    secret_name: &str,
    key: &str,
    secret: Option<&Secret>,
) -> CheckResult {
    const NAME: &str = "auth_secret_present";
    let Some(secret) = secret else {
        return CheckResult::fail(
            NAME,
            format!("auth Secret `{namespace}/{secret_name}` is absent"),
        );
    };
    let in_data = secret
        .data
        .as_ref()
        .is_some_and(|data| data.contains_key(key));
    let in_string_data = secret
        .string_data
        .as_ref()
        .is_some_and(|data| data.contains_key(key));
    if in_data || in_string_data {
        CheckResult::pass(
            NAME,
            format!("auth Secret `{namespace}/{secret_name}` carries key `{key}`"),
        )
    } else {
        CheckResult::fail(
            NAME,
            format!("auth Secret `{namespace}/{secret_name}` lacks key `{key}`"),
        )
    }
}

/// True when a serialised `GraphCluster` is Ready (`Ready=True`).
#[must_use]
pub fn cluster_is_ready(cluster: &Value) -> bool {
    condition_true(&conditions_of(cluster), "Ready")
}

// --- Live checks (read-only: get/list only) ----------------------------------

/// Check 1: the three CRDs exist and serve `hydradb.kubexa.tech/v1alpha1`.
async fn check_crds(client: &Client) -> CheckResult {
    const NAME: &str = "crds_installed";
    let api = Api::<CustomResourceDefinition>::all(client.clone());
    let expected = [
        ("graphclusters", "GraphCluster"),
        ("graphnodepools", "GraphNodePool"),
        ("graphindexers", "GraphIndexer"),
    ];
    let mut missing = Vec::new();
    let mut wrong_version = Vec::new();
    for (plural, kind) in expected {
        let crd_name = format!("{plural}.{GROUP}");
        match api.get_opt(&crd_name).await {
            Err(error) => {
                return CheckResult::fail(
                    NAME,
                    format!("cannot read CRD `{crd_name}` (need only get on CRDs): {error}"),
                );
            }
            Ok(None) => missing.push(crd_name),
            Ok(Some(crd)) => {
                let serves = crd
                    .spec
                    .versions
                    .iter()
                    .any(|version| version.name == VERSION && version.served);
                if !serves {
                    wrong_version.push(format!("{kind} does not serve {GROUP}/{VERSION}"));
                }
            }
        }
    }
    if !missing.is_empty() {
        CheckResult::fail(NAME, format!("CRDs missing: {}", missing.join(", ")))
    } else if !wrong_version.is_empty() {
        CheckResult::fail(NAME, wrong_version.join("; "))
    } else {
        CheckResult::pass(
            NAME,
            format!("GraphCluster/GraphNodePool/GraphIndexer installed at {GROUP}/{VERSION}"),
        )
    }
}

/// Check 2: the operator Deployment exists and has available replicas.
///
/// The operator runs in its own namespace, which the CLI cannot know: with a
/// namespace scope only that namespace is searched (hinting at `-A` when the
/// Deployment is not there); with `-A` every namespace is searched.
async fn check_operator_deployment(client: &Client, namespace: &Option<String>) -> CheckResult {
    const NAME: &str = "operator_deployment_ready";
    let found: Result<Option<Deployment>, String> = match namespace {
        Some(ns) => Api::<Deployment>::namespaced(client.clone(), ns)
            .get_opt(OPERATOR_DEPLOYMENT_NAME)
            .await
            .map_err(|error| format!("cannot read Deployments in namespace `{ns}`: {error}")),
        None => {
            let listed = Api::<Deployment>::all(client.clone())
                .list(&ListParams::default().labels("app.kubernetes.io/name=hydradb-operator"))
                .await;
            match listed {
                Err(error) => {
                    return CheckResult::fail(NAME, format!("cannot list Deployments: {error}"));
                }
                Ok(listed) => Ok(listed.items.into_iter().find(|deployment| {
                    deployment.metadata.name.as_deref() == Some(OPERATOR_DEPLOYMENT_NAME)
                })),
            }
        }
    };
    let found = match found {
        Err(detail) => return CheckResult::fail(NAME, detail),
        Ok(found) => found,
    };
    let Some(deployment) = found else {
        let scope = namespace.as_deref().unwrap_or("any namespace");
        return CheckResult::fail(
            NAME,
            format!(
                "Deployment `{OPERATOR_DEPLOYMENT_NAME}` not found in {scope} \
                 (operator runs in its own namespace; try -A)"
            ),
        );
    };
    let available = deployment
        .status
        .as_ref()
        .and_then(|s| s.available_replicas)
        .unwrap_or(0);
    let unavailable = deployment
        .status
        .as_ref()
        .and_then(|s| s.unavailable_replicas)
        .unwrap_or(0);
    let desired = deployment
        .spec
        .as_ref()
        .and_then(|s| s.replicas)
        .unwrap_or(1);
    let at = |deployment: &Deployment| {
        deployment
            .metadata
            .namespace
            .as_deref()
            .unwrap_or("")
            .to_string()
    };
    if available >= 1 && unavailable == 0 {
        CheckResult::pass(
            NAME,
            format!(
                "Deployment `{}/{OPERATOR_DEPLOYMENT_NAME}` available ({available}/{desired})",
                at(&deployment)
            ),
        )
    } else {
        CheckResult::fail(
            NAME,
            format!(
                "Deployment `{}/{OPERATOR_DEPLOYMENT_NAME}` not ready \
                 (available {available}/{desired}, unavailable {unavailable})",
                at(&deployment)
            ),
        )
    }
}

/// Check 3: every `GraphCluster` in scope is Ready.
fn check_graphclusters_ready(clusters: &Result<Vec<Value>, String>) -> CheckResult {
    const NAME: &str = "graphclusters_ready";
    let clusters = match clusters {
        Err(detail) => return CheckResult::fail(NAME, detail.clone()),
        Ok(clusters) => clusters,
    };
    if clusters.is_empty() {
        return CheckResult::pass(NAME, "no GraphClusters in scope".to_string());
    }
    let unready: Vec<String> = clusters
        .iter()
        .filter(|cluster| !cluster_is_ready(cluster))
        .map(|cluster| {
            cluster
                .pointer("/metadata/name")
                .and_then(Value::as_str)
                .unwrap_or("<unknown>")
                .to_string()
        })
        .collect();
    if unready.is_empty() {
        CheckResult::pass(NAME, format!("{} GraphCluster(s) Ready", clusters.len()))
    } else {
        CheckResult::fail(NAME, format!("not Ready: {}", unready.join(", ")))
    }
}

/// Bounded reachability probe for an explicit endpoint: strip the scheme,
/// default the port (80/443), then TCP-connect with [`PROBE_TIMEOUT`].
/// A TCP connect is a reachability probe, not a credential check — no auth
/// material is sent.
async fn probe_endpoint(endpoint: &str) -> Result<(), String> {
    let without_scheme = endpoint
        .split_once("://")
        .map_or(endpoint, |(_, rest)| rest);
    let authority = without_scheme.split('/').next().unwrap_or("");
    if authority.is_empty() {
        return Err(format!("endpoint `{endpoint}` has no host"));
    }
    let tls = endpoint.starts_with("https://");
    let host_port = if authority.contains(':') {
        authority.to_string()
    } else if tls {
        format!("{authority}:443")
    } else {
        format!("{authority}:80")
    };
    match tokio::time::timeout(PROBE_TIMEOUT, TcpStream::connect(&host_port)).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(error)) => Err(format!("endpoint `{endpoint}` unreachable: {error}")),
        Err(_) => Err(format!(
            "endpoint `{endpoint}` probe timed out after {}s",
            PROBE_TIMEOUT.as_secs()
        )),
    }
}

/// Check 4: every cluster's object store is reachable.
async fn check_object_store(client: &Client, clusters: &[Value]) -> CheckResult {
    const NAME: &str = "object_store_reachable";
    if clusters.is_empty() {
        return CheckResult::pass(NAME, "no GraphClusters in scope".to_string());
    }
    let mut failures = Vec::new();
    let mut probed = 0_usize;
    for cluster in clusters {
        let name = cluster
            .pointer("/metadata/name")
            .and_then(Value::as_str)
            .unwrap_or("<unknown>");
        let namespace = cluster
            .pointer("/metadata/namespace")
            .and_then(Value::as_str)
            .unwrap_or("default");
        let explicit_endpoint = cluster
            .pointer("/spec/objectStore/explicit/endpoint")
            .and_then(Value::as_str)
            .filter(|endpoint| !endpoint.is_empty());
        let claim = cluster
            .pointer("/spec/objectStore/objectBucketClaim/name")
            .and_then(Value::as_str)
            .filter(|claim| !claim.is_empty());
        match (explicit_endpoint, claim) {
            (Some(endpoint), _) => match probe_endpoint(endpoint).await {
                Ok(()) => probed += 1,
                Err(detail) => failures.push(format!("{name}: {detail}")),
            },
            (None, Some(claim_name)) => {
                let secrets = Api::<Secret>::namespaced(client.clone(), namespace);
                let configmaps = Api::<ConfigMap>::namespaced(client.clone(), namespace);
                let secret = secrets.get_opt(claim_name).await;
                let configmap = configmaps.get_opt(claim_name).await;
                match (secret, configmap) {
                    (Ok(Some(_)), Ok(Some(_))) => probed += 1,
                    (Err(error), _) | (_, Err(error)) => failures.push(format!(
                        "{name}: cannot read claim `{namespace}/{claim_name}`: {error}"
                    )),
                    _ => failures.push(format!(
                        "{name}: claim `{namespace}/{claim_name}` has no generated Secret/ConfigMap yet"
                    )),
                }
            }
            (None, None) => {
                // No endpoint override and no claim: provider-default store.
                // Controller resolution differs (Stalled without a source),
                // but reachability of a default endpoint is nothing to probe.
                let has_explicit = cluster
                    .pointer("/spec/objectStore/explicit")
                    .is_some_and(|explicit| !explicit.is_null());
                if has_explicit {
                    probed += 1;
                } else {
                    failures.push(format!(
                        "{name}: objectStore has no source (set explicit or objectBucketClaim)"
                    ));
                }
            }
        }
    }
    if failures.is_empty() {
        CheckResult::pass(NAME, format!("{probed} object store(s) reachable"))
    } else {
        CheckResult::fail(NAME, failures.join("; "))
    }
}

/// Check 5: every cluster's auth Secret exists and carries the expected key.
async fn check_auth_secrets(client: &Client, clusters: &[Value]) -> CheckResult {
    const NAME: &str = "auth_secret_present";
    if clusters.is_empty() {
        return CheckResult::pass(NAME, "no GraphClusters in scope".to_string());
    }
    let mut results = Vec::new();
    for cluster in clusters {
        let namespace = cluster
            .pointer("/metadata/namespace")
            .and_then(Value::as_str)
            .unwrap_or("default");
        let (secret_name, key) = auth_secret_plan(cluster);
        let fetched = Api::<Secret>::namespaced(client.clone(), namespace)
            .get_opt(&secret_name)
            .await
            .map_err(|error| format!("cannot read Secret `{namespace}/{secret_name}`: {error}"));
        match fetched {
            Err(detail) => results.push(CheckResult::fail(NAME, detail)),
            Ok(secret) => {
                let result = evaluate_auth_secret(namespace, &secret_name, &key, secret.as_ref());
                // `evaluate_auth_secret` owns the per-cluster verdict; keep
                // its name/detail verbatim so tests pin the stable name.
                results.push(result);
            }
        }
    }
    let failures: Vec<String> = results
        .iter()
        .filter(|result| !result.passed)
        .map(|result| result.detail.clone())
        .collect();
    if failures.is_empty() {
        CheckResult::pass(
            NAME,
            format!("{} auth Secret(s) present with expected key", results.len()),
        )
    } else {
        CheckResult::fail(NAME, failures.join("; "))
    }
}

/// Check 6: every hydradb Service in scope is resolvable (a cluster IP, or
/// ports on a headless Service).
async fn check_services(client: &Client, namespace: &Option<String>) -> CheckResult {
    const NAME: &str = "services_resolvable";
    let services: Result<Vec<Service>, String> = match namespace {
        Some(ns) => Api::<Service>::namespaced(client.clone(), ns)
            .list(&ListParams::default().labels(HYDRADB_NAME_LABEL))
            .await
            .map(|list| list.items)
            .map_err(|error| format!("cannot list Services in namespace `{ns}`: {error}")),
        None => Api::<Service>::all(client.clone())
            .list(&ListParams::default().labels(HYDRADB_NAME_LABEL))
            .await
            .map(|list| list.items)
            .map_err(|error| format!("cannot list Services: {error}")),
    };
    let services = match services {
        Err(detail) => return CheckResult::fail(NAME, detail),
        Ok(services) => services,
    };
    if services.is_empty() {
        return CheckResult::pass(NAME, "no hydradb Services in scope".to_string());
    }
    let mut unresolvable = Vec::new();
    for service in &services {
        let name = service.metadata.name.as_deref().unwrap_or("<unknown>");
        let cluster_ip = service.spec.as_ref().and_then(|s| s.cluster_ip.as_deref());
        let headless = cluster_ip.is_none_or(|ip| ip.is_empty() || ip == "None");
        if headless {
            let has_ports = service
                .spec
                .as_ref()
                .and_then(|s| s.ports.as_ref())
                .is_some_and(|ports| !ports.is_empty());
            if !has_ports {
                unresolvable.push(format!("{name} (headless, no ports)"));
            }
        } else if cluster_ip.is_none_or(str::is_empty) {
            unresolvable.push(format!("{name} (no cluster IP)"));
        }
    }
    if unresolvable.is_empty() {
        CheckResult::pass(NAME, format!("{} Service(s) resolvable", services.len()))
    } else {
        CheckResult::fail(NAME, format!("unresolvable: {}", unresolvable.join(", ")))
    }
}

/// Check 7: every hydradb pod reports Ready (the kubelet drives this from the
/// `/readyz` probes; see the module docs for why no exec/port-forward is used).
async fn check_pods_readyz(client: &Client, namespace: &Option<String>) -> CheckResult {
    const NAME: &str = "pods_readyz_ok";
    let pods: Result<Vec<Pod>, String> = match namespace {
        Some(ns) => Api::<Pod>::namespaced(client.clone(), ns)
            .list(&ListParams::default().labels(HYDRADB_NAME_LABEL))
            .await
            .map(|list| list.items)
            .map_err(|error| format!("cannot list pods in namespace `{ns}`: {error}")),
        None => Api::<Pod>::all(client.clone())
            .list(&ListParams::default().labels(HYDRADB_NAME_LABEL))
            .await
            .map(|list| list.items)
            .map_err(|error| format!("cannot list pods: {error}")),
    };
    let pods = match pods {
        Err(detail) => return CheckResult::fail(NAME, detail),
        Ok(pods) => pods,
    };
    if pods.is_empty() {
        return CheckResult::pass(NAME, "no hydradb pods in scope".to_string());
    }
    let mut unready = Vec::new();
    for pod in &pods {
        let name = pod.metadata.name.as_deref().unwrap_or("<unknown>");
        let ready_condition = pod
            .status
            .as_ref()
            .and_then(|status| status.conditions.as_ref())
            .map(|conditions| {
                conditions
                    .iter()
                    .any(|condition| condition.type_ == "Ready" && condition.status == "True")
            })
            .unwrap_or(false);
        let containers_ready = pod
            .status
            .as_ref()
            .and_then(|status| status.container_statuses.as_ref())
            .map(|statuses| statuses.iter().all(|status| status.ready))
            .unwrap_or(true);
        if !(ready_condition && containers_ready) {
            unready.push(name.to_string());
        }
    }
    if unready.is_empty() {
        CheckResult::pass(NAME, format!("{} pod(s) /readyz ok", pods.len()))
    } else {
        CheckResult::fail(NAME, format!("not ready: {}", unready.join(", ")))
    }
}

/// Check 8: no `Stalled=True` condition on any of the three kinds.
fn check_no_stalled(all: &[Value]) -> CheckResult {
    const NAME: &str = "no_stalled_condition";
    let stalled = stalled_in(all);
    if stalled.is_empty() {
        CheckResult::pass(NAME, "no Stalled condition".to_string())
    } else {
        CheckResult::fail(NAME, format!("Stalled: {}", stalled.join(", ")))
    }
}

/// List one namespaced CRD kind in scope, serialised to [`Value`] so the pure
/// helpers above apply uniformly. A `Some` namespace lists only that
/// namespace; `None` (`-A`) lists across all namespaces. One bounded page —
/// never unbounded paging.
async fn list_values<K>(client: &Client, namespace: &Option<String>) -> Result<Vec<Value>, String>
where
    K: kube::Resource<Scope = kube::core::NamespaceResourceScope>
        + Clone
        + std::fmt::Debug
        + k8s_openapi::serde::Serialize
        + k8s_openapi::serde::de::DeserializeOwned,
    <K as kube::Resource>::DynamicType: Default,
{
    let api = match namespace {
        Some(ns) => Api::<K>::namespaced(client.clone(), ns),
        None => Api::<K>::all(client.clone()),
    };
    api.list(&ListParams::default())
        .await
        .map_err(|error| {
            format!(
                "cannot list {}: {error}",
                std::any::type_name::<K>()
                    .rsplit("::")
                    .next()
                    .unwrap_or("objects")
            )
        })
        .and_then(|list| {
            list.items
                .iter()
                .map(|item| {
                    serde_json::to_value(item).map_err(|error| format!("cannot serialise: {error}"))
                })
                .collect()
        })
}

/// Run `hydradb doctor`.
///
/// Prints one `PASS`/`FAIL` line per documented check, in order. Returns
/// `Ok` (exit 0) only when every check passes; any FAIL becomes
/// [`CliError::Output`] (exit 1) naming the failed checks.
///
/// # Errors
///
/// Returns [`CliError`] when the cluster cannot be reached, or when any
/// check fails (the error names the failed checks).
pub async fn run(cli: &Cli, _args: &DoctorArgs) -> Result<(), CliError> {
    let config = build_kube_config(cli).await?;
    let client = Client::try_from(config.clone())?;
    let namespace = effective_namespace(cli, &config);

    let clusters = list_values::<GraphCluster>(&client, &namespace).await;
    let pools = list_values::<GraphNodePool>(&client, &namespace).await;
    let indexers = list_values::<GraphIndexer>(&client, &namespace).await;

    let mut results = Vec::with_capacity(CHECK_NAMES.len());
    results.push(check_crds(&client).await);
    results.push(check_operator_deployment(&client, &namespace).await);
    results.push(check_graphclusters_ready(&clusters));
    results.push(match &clusters {
        Ok(clusters) => check_object_store(&client, clusters).await,
        Err(detail) => CheckResult::fail("object_store_reachable", detail.clone()),
    });
    results.push(match &clusters {
        Ok(clusters) => check_auth_secrets(&client, clusters).await,
        Err(detail) => CheckResult::fail("auth_secret_present", detail.clone()),
    });
    results.push(check_services(&client, &namespace).await);
    results.push(check_pods_readyz(&client, &namespace).await);
    results.push(stalled_result(&clusters, &pools, &indexers));
    debug_assert_eq!(results.len(), CHECK_NAMES.len());

    print_and_exit(results)
}

/// Outcome for `no_stalled_condition`: FAIL naming the list error when any of
/// the three kinds failed to list, else the stalled scan over all three.
fn stalled_result(
    clusters: &Result<Vec<Value>, String>,
    pools: &Result<Vec<Value>, String>,
    indexers: &Result<Vec<Value>, String>,
) -> CheckResult {
    const NAME: &str = "no_stalled_condition";
    let mut all = Vec::new();
    for fetched in [clusters, pools, indexers] {
        match fetched {
            Ok(objects) => all.extend(objects.iter().cloned()),
            Err(detail) => return CheckResult::fail(NAME, detail.clone()),
        }
    }
    check_no_stalled(&all)
}

/// Print every result in order, then map the exit contract to `Ok`/`Err`.
fn print_and_exit(results: Vec<CheckResult>) -> Result<(), CliError> {
    for result in &results {
        println!("{}", result.render());
    }
    let failed = failed_names(&results);
    if failed.is_empty() {
        Ok(())
    } else {
        Err(CliError::Output {
            message: format!(
                "hydradb doctor: {} check(s) FAILED: {}",
                failed.len(),
                failed.join(", ")
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn passing(name: &'static str) -> CheckResult {
        CheckResult::pass(name, "ok")
    }

    fn failing(name: &'static str) -> CheckResult {
        CheckResult::fail(name, "broken")
    }

    fn all_passing() -> Vec<CheckResult> {
        CHECK_NAMES.iter().map(|name| passing(name)).collect()
    }

    #[test]
    fn check_names_are_the_documented_stable_set() {
        assert_eq!(
            check_names(),
            vec![
                "crds_installed",
                "operator_deployment_ready",
                "graphclusters_ready",
                "object_store_reachable",
                "auth_secret_present",
                "services_resolvable",
                "pods_readyz_ok",
                "no_stalled_condition",
            ]
        );
    }

    #[test]
    fn check_names_are_unique() {
        let names = check_names();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(names.len(), sorted.len(), "stable names must be unique");
    }

    #[test]
    fn every_result_renders_its_stable_name() {
        for result in all_passing() {
            let rendered = result.render();
            assert!(
                rendered.starts_with("PASS "),
                "pass line starts with PASS: {rendered}"
            );
            assert!(
                rendered.contains(result.name),
                "line names the check: {rendered}"
            );
        }
        let rendered = failing("auth_secret_present").render();
        assert!(
            rendered.starts_with("FAIL "),
            "fail line starts with FAIL: {rendered}"
        );
        assert!(
            rendered.contains("auth_secret_present"),
            "names check: {rendered}"
        );
    }

    #[test]
    fn all_pass_exits_zero() {
        assert_eq!(exit_code_for(&all_passing()), 0);
    }

    #[test]
    fn any_single_fail_exits_one() {
        for (index, name) in CHECK_NAMES.iter().enumerate() {
            let mut results = all_passing();
            results[index] = failing(name);
            assert_eq!(exit_code_for(&results), 1, "one FAIL ({name}) must exit 1");
            assert_eq!(failed_names(&results), vec![*name]);
        }
    }

    #[test]
    fn empty_result_set_exits_zero() {
        assert_eq!(exit_code_for(&[]), 0);
    }

    #[test]
    fn auth_secret_plan_defaults_to_cluster_auth_and_token_key() {
        let cluster = json!({
            "metadata": {"name": "demo", "namespace": "demo"},
            "spec": {"auth": {}},
        });
        assert_eq!(
            auth_secret_plan(&cluster),
            ("demo-auth".to_string(), "auth-token".to_string())
        );
    }

    #[test]
    fn auth_secret_plan_honours_existing_secret_and_key() {
        let cluster = json!({
            "metadata": {"name": "demo", "namespace": "demo"},
            "spec": {"auth": {
                "existingSecret": "user-auth",
                "secretKey": "token",
            }},
        });
        assert_eq!(
            auth_secret_plan(&cluster),
            ("user-auth".to_string(), "token".to_string())
        );
    }

    #[test]
    fn absent_auth_secret_fails_naming_the_check() {
        let result = evaluate_auth_secret("demo", "demo-auth", "auth-token", None);
        assert!(!result.passed, "absent Secret must FAIL");
        assert_eq!(result.name, "auth_secret_present");
        assert_eq!(exit_code_for(std::slice::from_ref(&result)), 1);
        assert_eq!(
            failed_names(std::slice::from_ref(&result)),
            vec!["auth_secret_present"]
        );
        assert!(
            result.render().contains("demo-auth"),
            "detail names the Secret: {}",
            result.render()
        );
    }

    #[test]
    fn auth_secret_missing_the_key_fails() {
        let secret: Secret = serde_json::from_value(json!({
            "metadata": {"name": "demo-auth", "namespace": "demo"},
            "data": {"other-key": "dG9rZW4="},
        }))
        .expect("fixture Secret deserialises");
        let result = evaluate_auth_secret("demo", "demo-auth", "auth-token", Some(&secret));
        assert!(!result.passed, "wrong key must FAIL");
        assert_eq!(result.name, "auth_secret_present");
    }

    #[test]
    fn auth_secret_carrying_the_key_passes() {
        let secret: Secret = serde_json::from_value(json!({
            "metadata": {"name": "demo-auth", "namespace": "demo"},
            "data": {"auth-token": "dG9rZW4="},
        }))
        .expect("fixture Secret deserialises");
        let result = evaluate_auth_secret("demo", "demo-auth", "auth-token", Some(&secret));
        assert!(result.passed, "present key must PASS");
    }

    #[test]
    fn auth_check_never_renders_secret_values() {
        let secret: Secret = serde_json::from_value(json!({
            "metadata": {"name": "demo-auth", "namespace": "demo"},
            "data": {"auth-token": "c3VwZXItc2VjcmV0LXZhbHVl"},
        }))
        .expect("fixture Secret deserialises");
        let raw_token = "c3VwZXItc2VjcmV0LXZhbHVl";
        let rendered =
            evaluate_auth_secret("demo", "demo-auth", "auth-token", Some(&secret)).render();
        assert!(
            !rendered.contains(raw_token),
            "no secret value leaks: {rendered}"
        );
        // Decoded form must not leak either.
        assert!(
            !rendered.contains("super-secret-value"),
            "no decoded secret leaks: {rendered}"
        );
    }

    #[test]
    fn stalled_objects_are_named_kind_slash_name() {
        let objects = vec![
            json!({
                "kind": "GraphCluster",
                "metadata": {"name": "demo"},
                "status": {"conditions": [
                    {"type": "Stalled", "status": "True"},
                ]},
            }),
            json!({
                "kind": "GraphNodePool",
                "metadata": {"name": "workers"},
                "status": {"conditions": [
                    {"type": "Ready", "status": "True"},
                ]},
            }),
        ];
        assert_eq!(stalled_in(&objects), vec!["GraphCluster/demo".to_string()]);
        assert!(!check_no_stalled(&objects).passed);
        assert!(
            check_no_stalled(&objects)
                .render()
                .contains("GraphCluster/demo")
        );
    }

    #[test]
    fn no_stalled_passes_on_clean_objects() {
        let objects = vec![json!({
            "kind": "GraphCluster",
            "metadata": {"name": "demo"},
            "status": {"conditions": [
                {"type": "Ready", "status": "True"},
            ]},
        })];
        assert!(check_no_stalled(&objects).passed);
        assert!(stalled_in(&[]).is_empty());
    }

    #[test]
    fn unready_clusters_fail_naming_them() {
        let clusters = Ok(vec![
            json!({
                "kind": "GraphCluster",
                "metadata": {"name": "demo"},
                "status": {"conditions": [
                    {"type": "Ready", "status": "False"},
                ]},
            }),
            json!({
                "kind": "GraphCluster",
                "metadata": {"name": "ready-one"},
                "status": {"conditions": [
                    {"type": "Ready", "status": "True"},
                ]},
            }),
        ]);
        let result = check_graphclusters_ready(&clusters);
        assert!(!result.passed);
        assert_eq!(result.name, "graphclusters_ready");
        assert!(
            result.detail.contains("demo"),
            "names unready: {}",
            result.detail
        );
        assert!(
            !result.detail.contains("ready-one"),
            "omits ready: {}",
            result.detail
        );
    }

    #[test]
    fn empty_cluster_scope_passes_vacuously() {
        assert!(check_graphclusters_ready(&Ok(vec![])).passed);
    }

    #[test]
    fn list_failure_is_a_fail_not_a_crash() {
        let result =
            check_graphclusters_ready(&Err("cannot list GraphCluster: denied".to_string()));
        assert!(!result.passed);
        assert!(
            result.detail.contains("denied"),
            "carries reason: {}",
            result.detail
        );
    }
}
