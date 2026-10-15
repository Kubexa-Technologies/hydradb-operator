//! `GraphNodePool` reconciler: the node `StatefulSet` (todo 20) plus its
//! `PodDisruptionBudget` (todo 22).
//!
//! Builds and applies the worker `StatefulSet` via [`apply_child`](crate::apply::apply_child)
//! (SSA with the pinned field manager). The pod runs the `graph-node`
//! binary with the resolved image, `replicas` from the spec, and the shared
//! graph/object-store inputs via `envFrom` on the cluster's `<cluster>-config`
//! ConfigMap. The auth token reaches the operand ONLY as the mounted file
//! named by `GRAPH_AUTH_TOKEN_FILE` (set in the shared ConfigMap by
//! `reconcile::cluster`) — never as an env var.
//!
//! Selector contract (todo 19): the Services built by [`super::cluster`]
//! select node pods with [`node_pod_selector`](super::cluster::node_pod_selector)
//! (`app.kubernetes.io/name=hydradb`, `instance=<cluster>`,
//! `component=node`). The StatefulSet's `spec.selector.matchLabels` is
//! exactly that set and the pod template carries it (plus the standard
//! labels and `spec.pod_labels`), or the Services select nothing and the
//! recomputed `GRAPH_BOLT_NODE_ADDRESSES` routing table stays empty.
//!
//! Cache storage comes from the [`CacheStorage`](hydradb_operator_api::spec::CacheStorage)
//! union: `EmptyDir` yields an `emptyDir` volume (with `sizeLimit` when set);
//! `PersistentVolume` yields a `volumeClaimTemplates` entry (never a bare
//! volume), with `storageClassName` set ONLY when the user set it — never
//! defaulted, so the cluster default applies.
//!
//! Auth wiring (verified against the operand source of truth): the operand
//! reads its token ONLY from the file named by `GRAPH_AUTH_TOKEN_FILE`
//! (`/var/run/secrets/hydradb/<key>`, set in the shared ConfigMap by
//! `reconcile::cluster`), so the auth Secret is volume-mounted at that path
//! (same shape as the indexer). No other token env var exists in the operand
//! (`rg` over its `src/`, `charts/`, `examples/` returns zero hits for any
//! alternative) and none must ever be set — it would be dead config, and a
//! token-in-env `secretKeyRef` would needlessly expose the credential.
//!
//! DEVIATION (documented): [`PersistentVolumeCache`](hydradb_operator_api::spec::PersistentVolumeCache)
//! exposes only `size` / `storage_class_name` / `access_modes` — there is no
//! annotations field in the (frozen) API, so the PVC template carries the
//! standard labels and no extra annotations. `crates/api` is untouched by
//! this todo.
//!
//! Finalizer wiring (todo 65): [`reconcile`] calls
//! `finalizer::with_finalizer` as its ONLY top-level action, stamping
//! [`constants::FINALIZER`] via `kube::runtime::finalizer`. The previous
//! apply body is the `Apply` closure; [`cleanup_pool`] is the `Cleanup`
//! closure. The pool owns no object-store data, so cleanup stamps `pruned-by`
//! and removes the `StatefulSet` under the owning cluster's `deletionPolicy`
//! (`Retain` when the cluster reference is empty or unresolvable), and never
//! plans a data-prefix step.
//!
//! TODO(todo-24): the full `GraphNodePoolStatus` writer (`boltAddresses`,
//! endpoints). This module writes only the `Ready` conditions plus the
//! replica counts, mirroring `reconcile::indexer`; live `boltAddresses` are
//! preserved verbatim, never recomputed here.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use hydradb_operator_api::constants;
use hydradb_operator_api::crd::{DeletionPolicy, GraphCluster, GraphNodePool, GraphNodePoolStatus};
use hydradb_operator_api::spec::{CacheStorage, Image};
use k8s_openapi::api::apps::v1::{
    RollingUpdateStatefulSetStrategy, StatefulSet, StatefulSetSpec, StatefulSetUpdateStrategy,
};
use k8s_openapi::api::core::v1::{
    Capabilities, ConfigMapEnvSource, Container, ContainerPort, EmptyDirVolumeSource,
    EnvFromSource, EnvVar, HTTPGetAction, KeyToPath, PersistentVolumeClaim,
    PersistentVolumeClaimSpec, PodSecurityContext, PodSpec, PodTemplateSpec, Probe,
    ResourceRequirements, SeccompProfile, SecretVolumeSource, SecurityContext, Volume, VolumeMount,
    VolumeResourceRequirements,
};
use k8s_openapi::api::policy::v1::{PodDisruptionBudget, PodDisruptionBudgetSpec};
use k8s_openapi::apimachinery::pkg::api::resource::Quantity;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{LabelSelector, ObjectMeta, OwnerReference};
use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;
use kube::Resource as _;
use kube::ResourceExt as _;
use kube::api::{Api, DeleteParams, Patch, PatchParams};
use kube::runtime::controller::Action;

use super::cluster::{
    DEFAULT_AUTH_KEY, SUCCESS_REQUEUE_SECS, headless_service_name, node_pod_selector, policy_reason,
};
use crate::apply::{
    ROLE_AUTH, ROLE_CONFIG, ROLE_PDB, apply_child, cluster_child_name, pool_child_name,
    standard_labels,
};
use crate::conditions::{live_conditions, set_ready};
use crate::context::Ctx;
use crate::error::{Error, ErrorClass};
use crate::metrics;

/// Child role used for the node label set (`app.kubernetes.io/component`).
///
/// This is the `component: node` value todo 19's Services select on — it must
/// stay in sync with [`node_pod_selector`](super::cluster::node_pod_selector).
const ROLE_NODE: &str = "node";

/// Container name + binary: the operand image's node entrypoint.
const CONTAINER_NAME: &str = "graph-node";

/// Bolt port served by every node.
const BOLT_PORT: i32 = 7687;
/// Node HTTPS container port the public Service targets.
const HTTPS_PORT: i32 = 8443;
/// Admin port serving `/livez` and `/readyz`.
const ADMIN_PORT: i32 = 9090;

/// `RUST_MIN_STACK` value (bytes): same floor as the indexer workload.
const RUST_MIN_STACK_VALUE: &str = "33554432";

/// UID/GID the operand runs as (never root).
const OPERAND_UID: i64 = 10001;

/// Writable SlateDB cache mount.
const CACHE_MOUNT_PATH: &str = "/var/cache/slatedb";
/// Writable scratch mount.
const TMP_MOUNT_PATH: &str = "/tmp";
/// Read-only auth token mount (upstream chart convention).
const AUTH_MOUNT_PATH: &str = "/var/run/secrets/hydradb";

/// Default `terminationGracePeriodSeconds` when the spec sets none.
const DEFAULT_TERMINATION_GRACE_SECS: i64 = 60;

/// Upstream node resource defaults (requests 2cpu/4Gi/12Gi, limits
/// 8cpu/16Gi/24Gi) used when `spec.resources` is absent.
fn default_node_resources() -> ResourceRequirements {
    ResourceRequirements {
        claims: None,
        limits: Some(BTreeMap::from([
            ("cpu".to_owned(), Quantity("8".to_owned())),
            ("memory".to_owned(), Quantity("16Gi".to_owned())),
            ("ephemeral-storage".to_owned(), Quantity("24Gi".to_owned())),
        ])),
        requests: Some(BTreeMap::from([
            ("cpu".to_owned(), Quantity("2".to_owned())),
            ("memory".to_owned(), Quantity("4Gi".to_owned())),
            ("ephemeral-storage".to_owned(), Quantity("12Gi".to_owned())),
        ])),
    }
}

/// Resolve `spec.clusterRef.name` (pure: no API call).
///
/// # Errors
///
/// Returns [`Error::UnresolvableRef`] (structural — never a panic) when the
/// reference is absent or empty.
pub fn cluster_ref_name(pool: &GraphNodePool) -> Result<String, Error> {
    let name = pool.spec.cluster_ref.name.clone();
    if name.is_empty() {
        return Err(Error::UnresolvableRef(
            "spec.clusterRef.name must be a non-empty cluster name".to_owned(),
        ));
    }
    Ok(name)
}

/// Resolve the container image: `spec.image` wins, otherwise the cluster's.
///
/// Mirrors the chart's `hydradb.image` helper: digest beats tag, tag beats
/// bare repository, and the tag is never `:latest` (rejected upstream by the
/// CRD schema bounds).
#[must_use]
pub fn resolve_image(spec_image: Option<&Image>, cluster_image: &Image) -> String {
    let image = spec_image.unwrap_or(cluster_image);
    match (&image.digest, &image.tag) {
        (Some(digest), _) if !digest.is_empty() => {
            format!("{}@{digest}", image.repository)
        }
        (_, Some(tag)) if !tag.is_empty() => {
            format!("{}:{tag}", image.repository)
        }
        _ => image.repository.clone(),
    }
}

/// Resolve the auth Secret NAME the node wires: the user's `existingSecret`
/// passthrough when set, else the cluster's `<cluster>-auth` Secret.
#[must_use]
pub fn auth_secret_name(cluster: &GraphCluster) -> String {
    cluster
        .spec
        .auth
        .existing_secret
        .clone()
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| cluster_child_name(&cluster.name_any(), ROLE_AUTH))
}

/// Resolve the auth Secret KEY holding the token.
#[must_use]
pub fn auth_secret_key(cluster: &GraphCluster) -> String {
    cluster
        .spec
        .auth
        .secret_key
        .clone()
        .unwrap_or_else(|| DEFAULT_AUTH_KEY.to_owned())
}

/// Build the node's container env: `RUST_MIN_STACK`, then `spec.extra_env`
/// LAST so a user override wins.
///
/// Shared graph/object-store/auth inputs are NOT listed here: they arrive via
/// `envFrom` on the cluster's shared config ConfigMap (see
/// [`desired_statefulset`]), and the auth token ONLY via the mounted Secret
/// file named by `GRAPH_AUTH_TOKEN_FILE` — never as an env var (see the
/// module docs).
#[must_use]
pub fn node_env(spec: &hydradb_operator_api::crd::GraphNodePoolSpec) -> Vec<EnvVar> {
    let mut env = vec![EnvVar {
        name: constants::RUST_MIN_STACK.to_owned(),
        value: Some(RUST_MIN_STACK_VALUE.to_owned()),
        value_from: None,
    }];
    env.extend(spec.extra_env.iter().cloned());
    env
}

/// Map `spec.updateStrategy` onto the StatefulSet field (pure).
///
/// `None` when the spec sets none, so the Kubernetes default
/// (`RollingUpdate`) applies. A `partition` only maps when the strategy type
/// is `RollingUpdate`; otherwise it is dropped rather than emitting an
/// invalid `rollingUpdate` block under `OnDelete`.
#[must_use]
pub fn desired_update_strategy(
    spec: &hydradb_operator_api::crd::GraphNodePoolSpec,
) -> Option<StatefulSetUpdateStrategy> {
    spec.update_strategy.as_ref().map(|strategy| {
        let rolling_update = if strategy.r#type == "RollingUpdate" {
            strategy
                .rolling_update_partition
                .map(|partition| RollingUpdateStatefulSetStrategy {
                    partition: Some(partition),
                    max_unavailable: None,
                })
        } else {
            None
        };
        StatefulSetUpdateStrategy {
            type_: Some(strategy.r#type.clone()),
            rolling_update,
        }
    })
}

/// `OwnerReference` pointing at the owning [`GraphNodePool`].
///
/// `controller` is `true` so garbage collection cascades from the pool;
/// `block_owner_deletion` stays `false` so a stuck child can never block
/// pool deletion.
///
/// # Errors
///
/// Returns [`Error::MissingField`] when the owner has no name or uid yet
/// (not yet persisted to the apiserver).
fn owner_ref_for_pool(pool: &GraphNodePool) -> Result<OwnerReference, Error> {
    let meta = pool.meta();
    let name = meta
        .name
        .clone()
        .ok_or_else(|| Error::MissingField("GraphNodePool has no metadata.name".to_owned()))?;
    let uid = meta
        .uid
        .clone()
        .ok_or_else(|| Error::MissingField("GraphNodePool has no metadata.uid".to_owned()))?;
    Ok(OwnerReference {
        api_version: GraphNodePool::api_version(&()).into_owned(),
        kind: GraphNodePool::kind(&()).into_owned(),
        name,
        uid,
        controller: Some(true),
        block_owner_deletion: Some(false),
    })
}

/// Build the cache `volumeClaimTemplates` entry for the `PersistentVolume`
/// union variant (pure).
///
/// `storageClassName` is set ONLY when the user set it — never defaulted, so
/// the cluster default storage class applies when it is absent.
fn cache_volume_claim_template(
    cluster_name: &str,
    cache: &hydradb_operator_api::spec::PersistentVolumeCache,
) -> PersistentVolumeClaim {
    PersistentVolumeClaim {
        metadata: ObjectMeta {
            name: Some("cache".to_owned()),
            labels: Some(standard_labels(cluster_name, ROLE_NODE)),
            ..ObjectMeta::default()
        },
        spec: Some(PersistentVolumeClaimSpec {
            access_modes: if cache.access_modes.is_empty() {
                None
            } else {
                Some(cache.access_modes.clone())
            },
            resources: Some(VolumeResourceRequirements {
                limits: None,
                requests: Some(BTreeMap::from([(
                    "storage".to_owned(),
                    Quantity(cache.size.clone()),
                )])),
            }),
            storage_class_name: cache.storage_class_name.clone(),
            ..Default::default()
        }),
        status: None,
    }
}

/// Build the desired node `StatefulSet` (pure: no API call).
///
/// `serviceName` is the cluster's headless Service, `podManagementPolicy` is
/// `Parallel`, and the pod template carries the exact selector labels todo
/// 19's Services select on. The container NEVER runs as root
/// (`runAsUser`/`runAsGroup` 10001, `readOnlyRootFilesystem`) and always
/// mounts writable `/var/cache/slatedb` + `/tmp` volumes.
///
/// # Errors
///
/// Returns [`Error::MissingField`] when the pool has no name/uid yet, or
/// [`Error::UnresolvableRef`] when `spec.clusterRef.name` is empty.
pub fn desired_statefulset(
    pool: &GraphNodePool,
    cluster: &GraphCluster,
) -> Result<StatefulSet, Error> {
    let pool_name = pool.name_any();
    let namespace = pool.namespace().unwrap_or_default();
    let cluster_name = cluster.name_any();
    // Validate the reference up front: an empty name is a structural error, not a panic.
    let _ = cluster_ref_name(pool)?;

    let labels = standard_labels(&cluster_name, ROLE_NODE);
    let selector = node_pod_selector(&cluster_name);
    let mut pod_labels = labels.clone();
    pod_labels.extend(
        pool.spec
            .pod_labels
            .iter()
            .map(|(k, v)| (k.clone(), v.clone())),
    );
    let pod_annotations = if pool.spec.pod_annotations.is_empty() {
        None
    } else {
        Some(pool.spec.pod_annotations.clone())
    };

    let auth_secret = auth_secret_name(cluster);
    let auth_key = auth_secret_key(cluster);
    let image = resolve_image(pool.spec.image.as_ref(), &cluster.spec.image);

    let container = Container {
        name: CONTAINER_NAME.to_owned(),
        image: Some(image),
        command: Some(vec![CONTAINER_NAME.to_owned()]),
        env_from: Some(vec![EnvFromSource {
            config_map_ref: Some(ConfigMapEnvSource {
                name: cluster_child_name(&cluster_name, ROLE_CONFIG),
                optional: None,
            }),
            prefix: None,
            secret_ref: None,
        }]),
        env: Some(node_env(&pool.spec)),
        ports: Some(vec![
            ContainerPort {
                name: Some("bolt".to_owned()),
                container_port: BOLT_PORT,
                protocol: Some("TCP".to_owned()),
                ..Default::default()
            },
            ContainerPort {
                name: Some("https".to_owned()),
                container_port: HTTPS_PORT,
                protocol: Some("TCP".to_owned()),
                ..Default::default()
            },
            ContainerPort {
                name: Some("admin".to_owned()),
                container_port: ADMIN_PORT,
                protocol: Some("TCP".to_owned()),
                ..Default::default()
            },
        ]),
        liveness_probe: Some(Probe {
            http_get: Some(HTTPGetAction {
                path: Some("/livez".to_owned()),
                port: IntOrString::Int(ADMIN_PORT),
                ..Default::default()
            }),
            ..Default::default()
        }),
        readiness_probe: Some(Probe {
            http_get: Some(HTTPGetAction {
                path: Some("/readyz".to_owned()),
                port: IntOrString::Int(ADMIN_PORT),
                ..Default::default()
            }),
            ..Default::default()
        }),
        resources: Some(
            pool.spec
                .resources
                .clone()
                .unwrap_or_else(default_node_resources),
        ),
        security_context: Some(SecurityContext {
            allow_privilege_escalation: Some(false),
            read_only_root_filesystem: Some(true),
            capabilities: Some(Capabilities {
                drop: Some(vec!["ALL".to_owned()]),
                add: None,
            }),
            ..Default::default()
        }),
        volume_mounts: Some(vec![
            VolumeMount {
                name: "cache".to_owned(),
                mount_path: CACHE_MOUNT_PATH.to_owned(),
                read_only: None,
                ..Default::default()
            },
            VolumeMount {
                name: "tmp".to_owned(),
                mount_path: TMP_MOUNT_PATH.to_owned(),
                read_only: None,
                ..Default::default()
            },
            VolumeMount {
                name: "auth".to_owned(),
                mount_path: AUTH_MOUNT_PATH.to_owned(),
                read_only: Some(true),
                ..Default::default()
            },
        ]),
        ..Default::default()
    };

    // Cache volumes: `EmptyDir` is a plain volume; `PersistentVolume` is a
    // `volumeClaimTemplates` entry (never a bare volume). `/tmp` is always an
    // `emptyDir` so the read-only root filesystem stays writable where needed.
    let mut volumes = vec![
        Volume {
            name: "tmp".to_owned(),
            empty_dir: Some(EmptyDirVolumeSource {
                medium: None,
                size_limit: pool
                    .spec
                    .tmp_size_limit
                    .as_ref()
                    .map(|limit| Quantity(limit.clone())),
            }),
            ..Default::default()
        },
        Volume {
            name: "auth".to_owned(),
            secret: Some(SecretVolumeSource {
                secret_name: Some(auth_secret.clone()),
                items: Some(vec![KeyToPath {
                    key: auth_key.clone(),
                    path: auth_key,
                    mode: None,
                }]),
                ..Default::default()
            }),
            ..Default::default()
        },
    ];
    let mut volume_claim_templates = None;
    if let CacheStorage::EmptyDir(empty) = &pool.spec.cache.storage {
        volumes.insert(
            0,
            Volume {
                name: "cache".to_owned(),
                empty_dir: Some(EmptyDirVolumeSource {
                    medium: None,
                    size_limit: empty
                        .size_limit
                        .as_ref()
                        .map(|limit| Quantity(limit.clone())),
                }),
                ..Default::default()
            },
        );
    } else if let CacheStorage::PersistentVolume(persistent) = &pool.spec.cache.storage {
        volume_claim_templates = Some(vec![cache_volume_claim_template(&cluster_name, persistent)]);
    }

    let pod_spec = PodSpec {
        containers: vec![container],
        service_account_name: pool.spec.service_account.clone(),
        priority_class_name: pool.spec.priority_class_name.clone(),
        node_selector: pool.spec.node_selector.clone(),
        tolerations: if pool.spec.tolerations.is_empty() {
            None
        } else {
            Some(pool.spec.tolerations.clone())
        },
        affinity: pool.spec.affinity.clone(),
        topology_spread_constraints: if pool.spec.topology_spread_constraints.is_empty() {
            None
        } else {
            Some(pool.spec.topology_spread_constraints.clone())
        },
        termination_grace_period_seconds: Some(
            pool.spec
                .termination_grace_period_seconds
                .unwrap_or(DEFAULT_TERMINATION_GRACE_SECS),
        ),
        security_context: Some(PodSecurityContext {
            run_as_non_root: Some(true),
            run_as_user: Some(OPERAND_UID),
            run_as_group: Some(OPERAND_UID),
            fs_group: Some(OPERAND_UID),
            seccomp_profile: Some(SeccompProfile {
                type_: "RuntimeDefault".to_owned(),
                localhost_profile: None,
            }),
            ..Default::default()
        }),
        volumes: Some(volumes),
        ..Default::default()
    };

    Ok(StatefulSet {
        metadata: ObjectMeta {
            name: Some(pool_child_name(&pool_name)),
            namespace: Some(namespace),
            labels: Some(labels.clone()),
            owner_references: Some(vec![owner_ref_for_pool(pool)?]),
            ..ObjectMeta::default()
        },
        spec: Some(StatefulSetSpec {
            replicas: Some(pool.spec.replicas),
            selector: LabelSelector {
                match_labels: Some(selector),
                ..Default::default()
            },
            template: PodTemplateSpec {
                metadata: Some(ObjectMeta {
                    labels: Some(pod_labels),
                    annotations: pod_annotations,
                    ..ObjectMeta::default()
                }),
                spec: Some(pod_spec),
            },
            service_name: Some(headless_service_name(&cluster_name)),
            pod_management_policy: Some("Parallel".to_owned()),
            update_strategy: desired_update_strategy(&pool.spec),
            volume_claim_templates,
            ..Default::default()
        }),
        status: None,
    })
}

/// Build the desired pool `PodDisruptionBudget` (pure: no API call).
///
/// One PDB per pool, `minAvailable: 1`, selecting the same node pods the
/// cluster Services select. Owned by the pool so removal cascades with it.
///
/// # Errors
///
/// Returns [`Error::MissingField`] when the pool has no name/uid yet, or
/// [`Error::UnresolvableRef`] when `spec.clusterRef.name` is empty.
pub fn desired_pdb(
    pool: &GraphNodePool,
    cluster: &GraphCluster,
) -> Result<PodDisruptionBudget, Error> {
    let pool_name = pool.name_any();
    let namespace = pool.namespace().unwrap_or_default();
    let cluster_name = cluster.name_any();
    let _ = cluster_ref_name(pool)?;

    Ok(PodDisruptionBudget {
        metadata: ObjectMeta {
            name: Some(pool_child_name(&pool_name)),
            namespace: Some(namespace),
            labels: Some(standard_labels(&cluster_name, ROLE_PDB)),
            owner_references: Some(vec![owner_ref_for_pool(pool)?]),
            ..ObjectMeta::default()
        },
        spec: Some(PodDisruptionBudgetSpec {
            min_available: Some(IntOrString::Int(1)),
            selector: Some(LabelSelector {
                match_labels: Some(node_pod_selector(&cluster_name)),
                ..LabelSelector::default()
            }),
            ..PodDisruptionBudgetSpec::default()
        }),
        ..PodDisruptionBudget::default()
    })
}

/// Desired status for the success path: kstatus `Ready` over the live set.
///
/// TODO(todo-24): the full status writer owns `boltAddresses` and the
/// endpoint surface; the addresses below are the preserved live value, never
/// recomputed here.
fn ready_status(
    live_conditions_set: &[k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition],
    generation: Option<i64>,
    ready_replicas: i32,
    current_replicas: i32,
    bolt_addresses: Vec<String>,
) -> GraphNodePoolStatus {
    let mut conditions = live_conditions_set.to_vec();
    set_ready(&mut conditions, true, generation);
    GraphNodePoolStatus {
        conditions,
        observed_generation: generation,
        ready_replicas,
        current_replicas,
        bolt_addresses,
    }
}

/// Patch the `/status` subresource when the desired status differs from live.
///
/// The comparison is field-exact, so an unchanged status produces NO write
/// and never hot-loops (todo 24 owns the full status writer; this covers only
/// the fields todo 20 sets).
///
/// # Errors
///
/// Returns [`Error::Api`] when the apiserver rejects the read or the patch.
async fn write_status_if_changed(
    api: &Api<GraphNodePool>,
    name: &str,
    desired: &GraphNodePoolStatus,
) -> Result<(), Error> {
    let live = api.get(name).await.map_err(Error::from_kube_error)?;
    let live_status = live.status.clone().unwrap_or_else(|| GraphNodePoolStatus {
        conditions: Vec::new(),
        observed_generation: None,
        ready_replicas: 0,
        current_replicas: 0,
        bolt_addresses: Vec::new(),
    });
    let conditions_changed = !matches!(
        live_conditions(&live_status.conditions, &desired.conditions),
        crate::conditions::StatusUpdate::NoChange
    );
    if !conditions_changed
        && live_status.observed_generation == desired.observed_generation
        && live_status.ready_replicas == desired.ready_replicas
        && live_status.current_replicas == desired.current_replicas
        && live_status.bolt_addresses == desired.bolt_addresses
    {
        return Ok(());
    }
    api.patch_status(
        name,
        &PatchParams::default(),
        &Patch::Merge(serde_json::json!({
            "conditions": desired.conditions,
            "observedGeneration": desired.observed_generation,
            "readyReplicas": desired.ready_replicas,
            "currentReplicas": desired.current_replicas,
        })),
    )
    .await
    .map_err(Error::from_kube_error)?;
    Ok(())
}

/// Reconcile one `GraphNodePool` into its node `StatefulSet`.
///
/// The cluster resolves via `spec.clusterRef`; a missing cluster is a
/// structural error (requeue), never a panic.
///
/// # Errors
///
/// Returns [`Error`] when the cluster reference is unresolvable, the child SSA
/// or the status write fails; the controller maps it through [`error_policy`].
async fn reconcile_inner(pool: Arc<GraphNodePool>, ctx: Arc<Ctx>) -> Result<Action, Error> {
    let name = pool.name_any();
    let namespace = pool.namespace().unwrap_or_default();
    let generation = pool.meta().generation;
    let pools = Api::<GraphNodePool>::namespaced(ctx.client.clone(), &namespace);

    let cluster_ref = cluster_ref_name(&pool)?;
    let clusters = Api::<GraphCluster>::namespaced(ctx.client.clone(), &namespace);
    let cluster = clusters
        .get_opt(&cluster_ref)
        .await
        .map_err(Error::from_kube_error)?
        .ok_or_else(|| {
            Error::UnresolvableRef(format!(
                "clusterRef {namespace}/{cluster_ref} does not resolve to a GraphCluster"
            ))
        })?;

    let desired = desired_statefulset(&pool, &cluster)?;
    let statefulsets = Api::<StatefulSet>::namespaced(ctx.client.clone(), &namespace);
    apply_child(&statefulsets, &desired).await?;

    let pdbs = Api::<PodDisruptionBudget>::namespaced(ctx.client.clone(), &namespace);
    apply_child(&pdbs, &desired_pdb(&pool, &cluster)?).await?;

    let (ready_replicas, current_replicas) = statefulsets
        .get_opt(&pool_child_name(&name))
        .await
        .map_err(Error::from_kube_error)?
        .and_then(|statefulset| statefulset.status)
        .map(|status| {
            (
                status.ready_replicas.unwrap_or(0),
                status.current_replicas.unwrap_or(0),
            )
        })
        .unwrap_or_default();
    let live = pools.get(&name).await.map_err(Error::from_kube_error)?;
    let live_status = live.status.as_ref();
    let live_conditions_set = live_status
        .map(|status| status.conditions.clone())
        .unwrap_or_default();
    // TODO(todo-24): `boltAddresses` are populated by the status writer; the
    // live value is preserved here so this write never clobbers it.
    let bolt_addresses = live_status
        .map(|status| status.bolt_addresses.clone())
        .unwrap_or_default();
    write_status_if_changed(
        &pools,
        &name,
        &ready_status(
            &live_conditions_set,
            generation,
            ready_replicas,
            current_replicas,
            bolt_addresses,
        ),
    )
    .await?;

    Ok(Action::requeue(Duration::from_secs(SUCCESS_REQUEUE_SECS)))
}

/// Reconcile one `GraphNodePool` (controller entrypoint).
///
/// The ONLY top-level action is `finalizer::with_finalizer` stamping
/// [`constants::FINALIZER`]: the previous apply body runs as the `Apply`
/// closure and [`cleanup_pool`] as the `Cleanup` closure, so the owning
/// cluster's `deletionPolicy` is honoured on every delete.
///
/// # Errors
///
/// Returns [`Error`] when resolution, child SSA or the status write fails;
/// the controller maps it through [`error_policy`].
pub async fn reconcile(pool: Arc<GraphNodePool>, ctx: Arc<Ctx>) -> Result<Action, Error> {
    let namespace = pool.namespace().unwrap_or_default();
    let api: Api<GraphNodePool> = Api::namespaced(ctx.client.clone(), &namespace);
    let apply_ctx = ctx.clone();
    let cleanup_ctx = ctx.clone();
    crate::finalizer::with_finalizer(
        &api,
        pool,
        |obj| async move {
            let span = metrics::reconcile_span(
                "GraphNodePool",
                &obj.namespace().unwrap_or_default(),
                &obj.name_any(),
            );
            let _entered = span.enter();
            let start = Instant::now();
            let outcome = reconcile_inner(obj, apply_ctx).await;
            let recorded: Result<(), ErrorClass> =
                outcome.as_ref().map(|_| ()).map_err(Error::class);
            metrics::record_reconcile("GraphNodePool", start.elapsed(), recorded);
            outcome
        },
        |obj| async move { cleanup_pool(obj, cleanup_ctx).await },
    )
    .await
}

/// Effective `deletionPolicy` for the pool: the owning cluster's policy.
///
/// The pool spec carries no policy field of its own. An empty `clusterRef`
/// or an unresolvable owning cluster means `Retain`: cleanup must never block
/// deletion on a reference it cannot read.
///
/// # Errors
///
/// Returns [`Error::Api`] when the apiserver rejects the cluster read.
async fn resolve_deletion_policy(
    pool: &GraphNodePool,
    ctx: &Ctx,
    namespace: &str,
) -> Result<DeletionPolicy, Error> {
    let cluster_name = pool.spec.cluster_ref.name.clone();
    if cluster_name.is_empty() {
        return Ok(DeletionPolicy::Retain);
    }
    let clusters = Api::<GraphCluster>::namespaced(ctx.client.clone(), namespace);
    let cluster = clusters
        .get_opt(&cluster_name)
        .await
        .map_err(Error::from_kube_error)?;
    Ok(policy_from_lookup(cluster.as_ref()))
}

/// Map an owning-cluster lookup onto the effective policy: the cluster's own
/// `deletionPolicy`, or `Retain` when the lookup found nothing.
///
/// Pure so the `Retain`-default contract stays unit-testable without a
/// cluster.
#[must_use]
fn policy_from_lookup(cluster: Option<&GraphCluster>) -> DeletionPolicy {
    cluster.map_or(DeletionPolicy::Retain, |cluster| {
        cluster.spec.deletion_policy.clone()
    })
}

/// Ordered cleanup actions for pool deletion (pure): stamp first, children
/// iff they exist, and never a data-prefix step — the pool owns no
/// object-store data, so the store always counts as unreachable here.
///
/// Pure so the never-delete-data contract stays unit-testable without a
/// cluster.
#[must_use]
fn pool_cleanup_plan(
    policy: &DeletionPolicy,
    children_exist: bool,
    pool_name: &str,
) -> Vec<crate::finalizer::CleanupAction> {
    crate::finalizer::cleanup_plan(policy, children_exist, false, pool_name)
}

/// Delete the pool-owned children, tolerating 404s.
///
/// The `StatefulSet` and the pool `PodDisruptionBudget` are the pool's only
/// direct children (PVCs cascade from the StatefulSet via garbage
/// collection). A 404 surfaces as [`Error::Api`] and is treated as success
/// by [`crate::finalizer::execute_cleanup`], so a never-applied pool cleans
/// up without touching anything else.
async fn delete_pool_children(
    client: kube::Client,
    namespace: &str,
    pool: &GraphNodePool,
) -> Result<(), Error> {
    let name = pool.name_any();
    let params = DeleteParams::default();
    let statefulsets = Api::<StatefulSet>::namespaced(client.clone(), namespace);
    let _ = statefulsets
        .delete(&pool_child_name(&name), &params)
        .await
        .map_err(Error::from_kube_error)?;
    let pdbs = Api::<PodDisruptionBudget>::namespaced(client, namespace);
    let _ = pdbs
        .delete(&pool_child_name(&name), &params)
        .await
        .map_err(Error::from_kube_error)?;
    Ok(())
}

/// `Cleanup` closure for [`reconcile`]: honour the owning cluster's
/// `deletionPolicy`.
///
/// `Retain` (and an empty/unresolvable `clusterRef`, which defaults to
/// `Retain`) stamps `pruned-by` and removes the `StatefulSet`, leaving
/// object-store data intact. `Delete` stamps the `Delete` reason with the
/// same child plan; the data-prefix step is always omitted because the pool
/// owns no object-store data, so deletion never blocks on the store.
async fn cleanup_pool(pool: Arc<GraphNodePool>, ctx: Arc<Ctx>) -> Result<Action, Error> {
    let name = pool.name_any();
    let namespace = pool.namespace().unwrap_or_default();
    let policy = resolve_deletion_policy(&pool, &ctx, &namespace).await?;
    let reason = policy_reason(&policy);
    let client = ctx.client.clone();
    let statefulsets = Api::<StatefulSet>::namespaced(client.clone(), &namespace);
    let children_exist = statefulsets
        .get_opt(&pool_child_name(&name))
        .await
        .map_err(Error::from_kube_error)?
        .is_some()
        || Api::<PodDisruptionBudget>::namespaced(client.clone(), &namespace)
            .get_opt(&pool_child_name(&name))
            .await
            .map_err(Error::from_kube_error)?
            .is_some();
    let plan = pool_cleanup_plan(&policy, children_exist, &name);
    let pools_api: Api<GraphNodePool> = Api::namespaced(client.clone(), &namespace);
    crate::finalizer::execute_cleanup(
        &plan,
        || crate::finalizer::stamp_pruned_by(&pools_api, &name, reason),
        || delete_pool_children(client.clone(), &namespace, &pool),
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
pub fn error_policy(pool: Arc<GraphNodePool>, error: &Error, _ctx: Arc<Ctx>) -> Action {
    let key = format!(
        "{}/{}",
        pool.namespace().unwrap_or_default(),
        pool.name_any()
    );
    Action::requeue(error.requeue_after(&key))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn cluster_fixture() -> GraphCluster {
        serde_json::from_value(serde_json::json!({
            "apiVersion": "hydradb.helmforge.dev/v1alpha1",
            "kind": "GraphCluster",
            "metadata": {"name": "demo", "namespace": "default", "uid": "abc-123"},
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

    fn pool_fixture() -> GraphNodePool {
        serde_json::from_value(serde_json::json!({
            "apiVersion": "hydradb.helmforge.dev/v1alpha1",
            "kind": "GraphNodePool",
            "metadata": {"name": "demo-pool", "namespace": "default", "uid": "pool-uid-1"},
            "spec": {
                "clusterRef": {"name": "demo"},
                "replicas": 3,
                "cache": {"storage": {"emptyDir": {"sizeLimit": "20Gi"}}}
            }
        }))
        .expect("fixture must deserialize")
    }

    fn persistent_pool_fixture(storage_class_name: Option<&str>) -> GraphNodePool {
        let mut pool = pool_fixture();
        let cache = if let Some(class) = storage_class_name {
            serde_json::json!({"persistentVolume": {
                "size": "40Gi",
                "storageClassName": class,
                "accessModes": ["ReadWriteOnce"]
            }})
        } else {
            serde_json::json!({"persistentVolume": {
                "size": "40Gi",
                "accessModes": ["ReadWriteOnce"]
            }})
        };
        pool.spec.cache.storage =
            serde_json::from_value(cache).expect("persistent cache must deserialize");
        pool
    }

    fn pod_spec_of(statefulset: &StatefulSet) -> &PodSpec {
        statefulset
            .spec
            .as_ref()
            .expect("statefulset carries a spec")
            .template
            .spec
            .as_ref()
            .expect("pod template carries a spec")
    }

    fn container_of(statefulset: &StatefulSet) -> &Container {
        &pod_spec_of(statefulset).containers[0]
    }

    #[test]
    fn statefulset_uses_the_headless_service_and_parallel_pod_management() {
        let pool = pool_fixture();
        let cluster = cluster_fixture();
        let statefulset = desired_statefulset(&pool, &cluster).expect("statefulset must build");
        let spec = statefulset
            .spec
            .as_ref()
            .expect("statefulset carries a spec");
        assert_eq!(
            spec.service_name.as_deref(),
            Some("demo-node-headless"),
            "serviceName must be the cluster's headless Service"
        );
        assert_eq!(
            spec.pod_management_policy.as_deref(),
            Some("Parallel"),
            "podManagementPolicy must be Parallel"
        );
    }

    #[test]
    fn pod_template_carries_the_selector_labels_the_services_select_on() {
        let pool = pool_fixture();
        let cluster = cluster_fixture();
        let statefulset = desired_statefulset(&pool, &cluster).expect("statefulset must build");
        let spec = statefulset
            .spec
            .as_ref()
            .expect("statefulset carries a spec");
        let expected = node_pod_selector("demo");
        assert_eq!(
            spec.selector.match_labels.as_ref(),
            Some(&expected),
            "matchLabels must be exactly the set the Services select on"
        );
        let template_labels = statefulset
            .spec
            .as_ref()
            .expect("statefulset carries a spec")
            .template
            .metadata
            .as_ref()
            .expect("pod template carries metadata")
            .labels
            .as_ref()
            .expect("pod template carries labels");
        for (key, value) in &expected {
            assert_eq!(
                template_labels.get(key).map(String::as_str),
                Some(value.as_str()),
                "pod template must carry selector label {key}"
            );
        }
    }

    #[test]
    fn empty_dir_cache_mounts_a_writable_var_cache_slatedb_volume() {
        let pool = pool_fixture();
        let cluster = cluster_fixture();
        let statefulset = desired_statefulset(&pool, &cluster).expect("statefulset must build");
        let pod = pod_spec_of(&statefulset);
        let cache_volume = pod
            .volumes
            .as_ref()
            .expect("pod carries volumes")
            .iter()
            .find(|volume| volume.name == "cache")
            .expect("emptyDir cache volume must exist");
        let empty_dir = cache_volume
            .empty_dir
            .as_ref()
            .expect("cache volume must be an emptyDir");
        assert_eq!(
            empty_dir
                .size_limit
                .as_ref()
                .map(|quantity| quantity.0.clone()),
            Some("20Gi".to_owned()),
            "emptyDir must carry the requested size limit"
        );
        let mounts: BTreeMap<&str, &VolumeMount> = container_of(&statefulset)
            .volume_mounts
            .as_ref()
            .expect("container carries mounts")
            .iter()
            .map(|mount| (mount.mount_path.as_str(), mount))
            .collect();
        let cache_mount = mounts
            .get(CACHE_MOUNT_PATH)
            .expect("writable /var/cache/slatedb mount must exist");
        assert_eq!(cache_mount.name, "cache");
        assert!(
            cache_mount.read_only != Some(true),
            "cache mount must be writable"
        );
        assert!(
            statefulset
                .spec
                .as_ref()
                .expect("statefulset carries a spec")
                .volume_claim_templates
                .is_none(),
            "emptyDir cache must not create a volumeClaimTemplates entry"
        );
    }

    #[test]
    fn persistent_volume_cache_creates_a_volume_claim_template_without_defaulting_the_storage_class()
     {
        let pool = persistent_pool_fixture(None);
        let cluster = cluster_fixture();
        let statefulset = desired_statefulset(&pool, &cluster).expect("statefulset must build");
        let spec = statefulset
            .spec
            .as_ref()
            .expect("statefulset carries a spec");
        let templates = spec
            .volume_claim_templates
            .as_ref()
            .expect("persistent cache must create a volumeClaimTemplates entry");
        assert_eq!(templates.len(), 1, "exactly one claim template");
        assert_eq!(
            templates[0].metadata.name.as_deref(),
            Some("cache"),
            "claim template must be named cache"
        );
        let claim_spec = templates[0]
            .spec
            .as_ref()
            .expect("claim template carries a spec");
        assert!(
            claim_spec.storage_class_name.is_none(),
            "storageClassName must stay unset when the user set none"
        );
        assert_eq!(
            claim_spec.access_modes.clone().unwrap_or_default(),
            vec!["ReadWriteOnce".to_owned()],
            "claim template must carry the requested access modes"
        );
        let requests = claim_spec
            .resources
            .as_ref()
            .expect("claim template carries resources")
            .requests
            .as_ref()
            .expect("claim template requests storage");
        assert_eq!(
            requests.get("storage").map(|quantity| quantity.0.clone()),
            Some("40Gi".to_owned()),
            "claim template must request the configured size"
        );
        // Never a bare volume for the persistent cache.
        for volume in pod_spec_of(&statefulset)
            .volumes
            .as_ref()
            .expect("pod carries volumes")
        {
            assert_ne!(
                volume.name, "cache",
                "persistent cache must not appear as a bare pod volume"
            );
        }
        // The container still mounts `cache`: the StatefulSet controller maps
        // the claim template onto that name.
        assert!(
            container_of(&statefulset)
                .volume_mounts
                .as_ref()
                .expect("container carries mounts")
                .iter()
                .any(|mount| mount.name == "cache" && mount.mount_path == CACHE_MOUNT_PATH),
            "container must mount the cache claim at /var/cache/slatedb"
        );
    }

    #[test]
    fn persistent_volume_cache_sets_the_storage_class_only_when_provided() {
        let cluster = cluster_fixture();
        let with_class = persistent_pool_fixture(Some("ceph-block"));
        let statefulset =
            desired_statefulset(&with_class, &cluster).expect("statefulset must build");
        let templates = statefulset
            .spec
            .as_ref()
            .expect("statefulset carries a spec")
            .volume_claim_templates
            .as_ref()
            .expect("persistent cache must create a claim template");
        assert_eq!(
            templates[0]
                .spec
                .as_ref()
                .expect("claim template carries a spec")
                .storage_class_name
                .as_deref(),
            Some("ceph-block"),
            "provided storage class must land on the claim template"
        );
    }

    #[test]
    fn container_runs_as_uid_10001_with_a_read_only_root_filesystem_and_writable_tmp() {
        let pool = pool_fixture();
        let cluster = cluster_fixture();
        let statefulset = desired_statefulset(&pool, &cluster).expect("statefulset must build");
        let pod = pod_spec_of(&statefulset);
        let pod_ctx = pod
            .security_context
            .as_ref()
            .expect("pod carries a security context");
        assert_eq!(pod_ctx.run_as_user, Some(10001));
        assert_eq!(pod_ctx.run_as_group, Some(10001));
        assert_eq!(pod_ctx.run_as_non_root, Some(true));
        let container = &pod.containers[0];
        let ctx = container
            .security_context
            .as_ref()
            .expect("container carries a security context");
        assert_eq!(ctx.read_only_root_filesystem, Some(true));
        assert_eq!(ctx.allow_privilege_escalation, Some(false));
        let mounts: BTreeMap<&str, &VolumeMount> = container
            .volume_mounts
            .as_ref()
            .expect("container carries mounts")
            .iter()
            .map(|mount| (mount.mount_path.as_str(), mount))
            .collect();
        let tmp_mount = mounts
            .get(TMP_MOUNT_PATH)
            .expect("writable /tmp mount must exist");
        assert!(
            tmp_mount.read_only != Some(true),
            "/tmp mount must be writable"
        );
        assert!(
            pod.volumes
                .as_ref()
                .expect("pod carries volumes")
                .iter()
                .any(|volume| volume.name == "tmp" && volume.empty_dir.is_some()),
            "/tmp must be backed by an emptyDir volume"
        );
    }

    #[test]
    fn rust_min_stack_is_set_to_33554432() {
        let pool = pool_fixture();
        let cluster = cluster_fixture();
        let statefulset = desired_statefulset(&pool, &cluster).expect("statefulset must build");
        let value = container_of(&statefulset)
            .env
            .as_ref()
            .expect("container carries env")
            .iter()
            .find(|var| var.name == constants::RUST_MIN_STACK)
            .expect("RUST_MIN_STACK must be set")
            .value
            .clone()
            .unwrap_or_default();
        assert_eq!(value, "33554432");
    }

    #[test]
    fn spec_image_overrides_the_cluster_image() {
        let cluster = cluster_fixture();
        let inherited = resolve_image(None, &cluster.spec.image);
        assert_eq!(inherited, "example.com/hydradb:v0.1.0");

        let custom = Image {
            repository: "example.com/custom-node".to_owned(),
            tag: Some("v9.9.9".to_owned()),
            digest: None,
            pull_policy: None,
            pull_secrets: Vec::new(),
        };
        assert_eq!(
            resolve_image(Some(&custom), &cluster.spec.image),
            "example.com/custom-node:v9.9.9"
        );

        let pinned = Image {
            repository: "example.com/custom-node".to_owned(),
            tag: Some("v9.9.9".to_owned()),
            digest: Some("sha256:abc123".to_owned()),
            pull_policy: None,
            pull_secrets: Vec::new(),
        };
        assert_eq!(
            resolve_image(Some(&pinned), &cluster.spec.image),
            "example.com/custom-node@sha256:abc123"
        );

        let mut pool = pool_fixture();
        pool.spec.image = Some(custom);
        let statefulset =
            desired_statefulset(&pool, &cluster).expect("override statefulset must build");
        assert_eq!(
            container_of(&statefulset).image.as_deref(),
            Some("example.com/custom-node:v9.9.9")
        );
    }

    #[test]
    fn omitted_resources_fall_back_to_the_upstream_node_defaults() {
        let pool = pool_fixture();
        let cluster = cluster_fixture();
        let statefulset = desired_statefulset(&pool, &cluster).expect("statefulset must build");
        let resources = container_of(&statefulset)
            .resources
            .as_ref()
            .expect("container carries resources");
        let requests = resources
            .requests
            .as_ref()
            .expect("defaults carry requests");
        for (name, want) in [
            ("cpu", "2"),
            ("memory", "4Gi"),
            ("ephemeral-storage", "12Gi"),
        ] {
            assert_eq!(
                requests
                    .get(name)
                    .map(|quantity| quantity.0.clone())
                    .as_deref(),
                Some(want),
                "omitted requests.{name} must default to {want}"
            );
        }
        let limits = resources.limits.as_ref().expect("defaults carry limits");
        for (name, want) in [
            ("cpu", "8"),
            ("memory", "16Gi"),
            ("ephemeral-storage", "24Gi"),
        ] {
            assert_eq!(
                limits
                    .get(name)
                    .map(|quantity| quantity.0.clone())
                    .as_deref(),
                Some(want),
                "omitted limits.{name} must default to {want}"
            );
        }

        // An explicit spec wins over the defaults.
        let mut custom = pool_fixture();
        custom.spec.resources = Some(ResourceRequirements {
            claims: None,
            limits: None,
            requests: Some(BTreeMap::from([(
                "cpu".to_owned(),
                Quantity("4".to_owned()),
            )])),
        });
        let statefulset =
            desired_statefulset(&custom, &cluster).expect("custom statefulset must build");
        let requests = container_of(&statefulset)
            .resources
            .as_ref()
            .expect("container carries resources")
            .requests
            .as_ref()
            .expect("custom resources carry requests");
        assert_eq!(
            requests
                .get("cpu")
                .map(|quantity| quantity.0.clone())
                .as_deref(),
            Some("4"),
            "explicit spec resources must win over the defaults"
        );
    }

    #[test]
    fn missing_cluster_ref_is_a_structural_error_not_a_panic() {
        let mut pool = pool_fixture();
        pool.spec.cluster_ref.name = String::new();
        let error = cluster_ref_name(&pool).expect_err("empty clusterRef must fail");
        assert_eq!(
            error.class(),
            ErrorClass::Structural,
            "missing clusterRef must be structural"
        );
        assert!(
            error.to_string().contains("clusterRef"),
            "error must name clusterRef, got: {error}"
        );

        let cluster = cluster_fixture();
        let error = desired_statefulset(&pool, &cluster)
            .expect_err("empty clusterRef must fail the builder");
        assert_eq!(error.class(), ErrorClass::Structural);
    }

    #[test]
    fn replicas_follow_the_spec() {
        let pool = pool_fixture();
        let cluster = cluster_fixture();
        let statefulset = desired_statefulset(&pool, &cluster).expect("statefulset must build");
        assert_eq!(
            statefulset
                .spec
                .as_ref()
                .expect("statefulset carries a spec")
                .replicas,
            Some(3),
            "replicas must follow spec.replicas"
        );
        assert_eq!(
            container_of(&statefulset)
                .command
                .clone()
                .unwrap_or_default(),
            vec!["graph-node".to_owned()],
            "container must run the graph-node binary"
        );
    }

    #[test]
    fn extra_env_is_applied_last_so_a_user_override_wins() {
        use k8s_openapi::api::core::v1::EnvVar;
        let mut pool = pool_fixture();
        pool.spec.extra_env = vec![EnvVar {
            name: constants::RUST_MIN_STACK.to_owned(),
            value: Some("1".to_owned()),
            value_from: None,
        }];
        let cluster = cluster_fixture();
        let statefulset = desired_statefulset(&pool, &cluster).expect("statefulset must build");
        let env = container_of(&statefulset)
            .env
            .as_ref()
            .expect("container carries env");
        let last = env
            .iter()
            .rposition(|var| var.name == constants::RUST_MIN_STACK)
            .expect("override must be present");
        assert_eq!(
            env[last].value.as_deref(),
            Some("1"),
            "the LAST duplicate env entry wins in Kubernetes"
        );
        assert!(
            last > 0,
            "user extra_env must come after the built-in entries"
        );
    }

    #[test]
    fn update_strategy_defaults_to_the_kubernetes_default_and_passes_through_when_set() {
        let pool = pool_fixture();
        let cluster = cluster_fixture();
        let statefulset = desired_statefulset(&pool, &cluster).expect("statefulset must build");
        assert!(
            statefulset
                .spec
                .as_ref()
                .expect("statefulset carries a spec")
                .update_strategy
                .is_none(),
            "absent spec.updateStrategy must leave the field unset (Kubernetes defaults to RollingUpdate)"
        );

        let mut rolling = pool_fixture();
        rolling.spec.update_strategy =
            serde_json::from_value(serde_json::json!({"type": "RollingUpdate"}))
                .expect("update strategy fixture must deserialize");
        let statefulset = desired_statefulset(&rolling, &cluster).expect("statefulset must build");
        let strategy = statefulset
            .spec
            .as_ref()
            .expect("statefulset carries a spec")
            .update_strategy
            .as_ref()
            .expect("strategy must pass through");
        assert_eq!(strategy.type_.as_deref(), Some("RollingUpdate"));

        let mut on_delete = pool_fixture();
        on_delete.spec.update_strategy = serde_json::from_value(serde_json::json!({
            "type": "OnDelete",
            "rollingUpdatePartition": 2
        }))
        .expect("update strategy fixture must deserialize");
        let statefulset =
            desired_statefulset(&on_delete, &cluster).expect("statefulset must build");
        let strategy = statefulset
            .spec
            .as_ref()
            .expect("statefulset carries a spec")
            .update_strategy
            .as_ref()
            .expect("strategy must pass through");
        assert_eq!(strategy.type_.as_deref(), Some("OnDelete"));
        assert!(
            strategy.rolling_update.is_none(),
            "a partition under OnDelete must be dropped, never an invalid block"
        );
    }

    const RECONCILE_SOURCE: &str = include_str!("nodepool.rs");

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
            !production.contains("hydradb.helmforge.dev/finalizer"),
            "the finalizer name must come from constants::FINALIZER, never a literal"
        );
        assert_eq!(
            crate::finalizer::finalizer_name(),
            constants::FINALIZER,
            "the wired finalizer must be exactly the shared constant"
        );
    }

    #[test]
    fn missing_cluster_means_retain_never_delete() {
        assert_eq!(
            policy_from_lookup(None),
            DeletionPolicy::Retain,
            "an unresolvable owning cluster must default to Retain"
        );
        let mut cluster = cluster_fixture();
        assert_eq!(
            policy_from_lookup(Some(&cluster)),
            DeletionPolicy::Retain,
            "an unset deletionPolicy on the cluster must default to Retain"
        );
        cluster.spec.deletion_policy = DeletionPolicy::Delete;
        assert_eq!(
            policy_from_lookup(Some(&cluster)),
            DeletionPolicy::Delete,
            "an explicit Delete on the cluster must flow through"
        );
    }

    #[tokio::test]
    async fn retain_cleanup_stamps_first_and_leaves_data_intact() {
        use crate::finalizer::{CleanupAction, execute_cleanup};
        use std::sync::Mutex;

        let plan = pool_cleanup_plan(&DeletionPolicy::Retain, true, "demo-pool");
        assert!(
            plan.iter()
                .all(|action| !matches!(action, CleanupAction::DeleteDataPrefix { .. })),
            "Retain must never plan data-prefix removal"
        );
        match plan.first() {
            Some(CleanupAction::StampPrunedBy { reason }) => {
                assert_eq!(reason, "Retain", "the stamp must name the Retain policy")
            }
            other => panic!("stamp must lead every plan, got: {other:?}"),
        }
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

    #[test]
    fn delete_policy_on_a_pool_still_never_plans_data_prefix_removal() {
        use crate::finalizer::CleanupAction;

        let plan = pool_cleanup_plan(&DeletionPolicy::Delete, true, "demo-pool");
        assert!(
            plan.iter()
                .any(|action| matches!(action, CleanupAction::DeleteChildren)),
            "Delete must still remove the pool's own StatefulSet"
        );
        assert!(
            plan.iter()
                .all(|action| !matches!(action, CleanupAction::DeleteDataPrefix { .. })),
            "a pool owns no object-store data, so Delete must never plan prefix removal"
        );
        match plan.first() {
            Some(CleanupAction::StampPrunedBy { reason }) => {
                assert_eq!(reason, "Delete", "the stamp must name the Delete policy")
            }
            other => panic!("stamp must lead every plan, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn cleanup_of_a_never_applied_pool_returns_ok_without_touching_children_or_store() {
        use crate::finalizer::execute_cleanup;

        let plan = pool_cleanup_plan(&DeletionPolicy::Retain, false, "demo-pool");
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
    fn pool_pdb_pins_min_available_1_and_selects_the_node_pods() {
        let pool = pool_fixture();
        let cluster = cluster_fixture();
        let pdb = desired_pdb(&pool, &cluster).expect("pool PDB builds without a cluster");
        assert_eq!(
            pdb.metadata.name.as_deref(),
            Some("demo-pool"),
            "one PDB per pool, named after the pool"
        );
        let owner_refs = pdb
            .metadata
            .owner_references
            .expect("PDB must carry an owner references");
        assert_eq!(owner_refs.len(), 1, "exactly one owner references");
        assert_eq!(owner_refs[0].kind, "GraphNodePool");
        assert_eq!(owner_refs[0].controller, Some(true));
        let spec = pdb.spec.expect("PDB must carry a spec");
        assert_eq!(
            spec.min_available,
            Some(IntOrString::Int(1)),
            "pool PDB pins minAvailable: 1"
        );
        assert_eq!(
            spec.selector
                .expect("PDB must carry a selector")
                .match_labels,
            Some(node_pod_selector("demo")),
            "pool PDB must select the node pods"
        );
    }
}
