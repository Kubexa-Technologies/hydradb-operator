//! `GraphIndexer` reconciler: the indexer `Deployment` plus its
//! `PodDisruptionBudget` (todo 22).
//!
//! Builds and applies the stateless indexer `Deployment` via [`apply_child`]
//! (SSA with the pinned field manager). The pod runs the `graph-indexer`
//! binary with the resolved image, `replicas` from the spec, and the shared
//! graph/object-store/auth inputs via `envFrom` on the cluster's
//! `<cluster>-config` ConfigMap plus the `<cluster>-auth` Secret volume — the
//! same wiring shape as the node workload.
//!
//! Env-name authority: the operand's `configmap.yaml` spells every indexer
//! knob `GRAPH_INDEXER_*` (plus `GRAPH_WAL_TAIL_FETCH_CONCURRENCY`,
//! `GRAPH_INDEXER_RETAIN_PREVIOUS` and `GRAPH_INDEXER_MAX_TAIL_FILES` — note
//! the missing `GRAPH_` infix / shortened suffixes). The plan prose lists
//! bare names (`INDEXER_INTERVAL_MS`, ...); the chart wins verbatim per the
//! mirror-the-operand rule, so the constants below copy the chart exactly.
//!
//! `spec.enabled == false` scales the Deployment to 0 and reports
//! `Ready=False`/`Disabled`. The indexer is stateless: NEVER a PVC or
//! `volumeClaimTemplates` here, and no schedule/cron field exists.
//!
//! Finalizer wiring (todo 65): [`reconcile`] calls
//! `finalizer::with_finalizer` as its ONLY top-level action, stamping
//! [`constants::FINALIZER`] via `kube::runtime::finalizer`. The previous
//! apply body is the `Apply` closure; [`cleanup_indexer`] is the `Cleanup`
//! closure. The indexer owns no object-store data, so cleanup stamps
//! `pruned-by` and removes the `Deployment` under the owning cluster's
//! `deletionPolicy` (`Retain` when the cluster reference is empty or
//! unresolvable), and never plans a data-prefix step.

use std::sync::Arc;
use std::time::{Duration, Instant};

use hydradb_operator_api::constants;
use hydradb_operator_api::crd::{DeletionPolicy, GraphCluster, GraphIndexer, GraphIndexerStatus};
use hydradb_operator_api::spec::{BuildMode, Image, PhaseLabel};
use k8s_openapi::api::apps::v1::{Deployment, DeploymentSpec};
use k8s_openapi::api::core::v1::{
    Capabilities, ConfigMapEnvSource, Container, ContainerPort, EmptyDirVolumeSource,
    EnvFromSource, EnvVar, HTTPGetAction, KeyToPath, PodSecurityContext, PodSpec, PodTemplateSpec,
    Probe, SeccompProfile, SecretVolumeSource, SecurityContext, Volume, VolumeMount,
};
use k8s_openapi::api::policy::v1::{PodDisruptionBudget, PodDisruptionBudgetSpec};
use k8s_openapi::apimachinery::pkg::api::resource::Quantity;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{LabelSelector, ObjectMeta, OwnerReference};
use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;
use kube::Resource as _;
use kube::ResourceExt as _;
use kube::api::{Api, DeleteParams, ListParams, Patch, PatchParams};
use kube::runtime::controller::Action;

use super::cluster::{DEFAULT_AUTH_KEY, SUCCESS_REQUEUE_SECS, policy_reason};
use crate::apply::{
    ROLE_AUTH, ROLE_CONFIG, ROLE_PDB, apply_child, cluster_child_name, indexer_child_name,
    standard_labels,
};
use crate::conditions::{new_condition, set_ready};
use crate::context::Ctx;
use crate::error::{Error, ErrorClass};
use crate::metrics;

// Indexer operand env keys, verbatim from the upstream chart's `configmap.yaml`.
// None of these has a `constants.rs` pin, so they live here (same discipline
// as the `GRAPH_*` keys in `reconcile::cluster`).
const GRAPH_INDEXER_INTERVAL_MS: &str = "GRAPH_INDEXER_INTERVAL_MS";
const GRAPH_INDEXER_DIRTY_POLL_INTERVAL_MS: &str = "GRAPH_INDEXER_DIRTY_POLL_INTERVAL_MS";
const GRAPH_INDEXER_RETAIN_PREVIOUS: &str = "GRAPH_INDEXER_RETAIN_PREVIOUS";
const GRAPH_INDEXER_BUILD_MODE: &str = "GRAPH_INDEXER_BUILD_MODE";
const GRAPH_INDEXER_INCREMENTAL_MIN_EDGES: &str = "GRAPH_INDEXER_INCREMENTAL_MIN_EDGES";
const GRAPH_INDEXER_SCOPE_CONCURRENCY: &str = "GRAPH_INDEXER_SCOPE_CONCURRENCY";
const GRAPH_INDEXER_SCOPES_PER_CYCLE: &str = "GRAPH_INDEXER_SCOPES_PER_CYCLE";
const GRAPH_INDEXER_MAX_OPEN_SCOPES: &str = "GRAPH_INDEXER_MAX_OPEN_SCOPES";
const GRAPH_INDEXER_READINESS_FAILURE_THRESHOLD: &str = "GRAPH_INDEXER_READINESS_FAILURE_THRESHOLD";
const GRAPH_INDEXER_MAX_TAIL_FILES: &str = "GRAPH_INDEXER_MAX_TAIL_FILES";
const GRAPH_WAL_TAIL_FETCH_CONCURRENCY: &str = "GRAPH_WAL_TAIL_FETCH_CONCURRENCY";

/// Child role used for the indexer label set (`app.kubernetes.io/component`).
const ROLE_INDEXER: &str = "indexer";

/// Container name + binary: the operand image's indexer entrypoint.
const CONTAINER_NAME: &str = "graph-indexer";

/// Admin port serving `/livez` and `/readyz`.
const ADMIN_PORT: i32 = 9091;

/// `RUST_MIN_STACK` value (bytes): same floor as the node workload.
const RUST_MIN_STACK_VALUE: &str = "33554432";

/// UID/GID the operand runs as (never root).
const OPERAND_UID: i64 = 10001;

/// Writable SlateDB cache mount (ephemeral: the indexer is stateless).
const CACHE_MOUNT_PATH: &str = "/var/cache/slatedb";
/// Writable scratch mount.
const TMP_MOUNT_PATH: &str = "/tmp";
/// Read-only auth token mount (upstream chart convention).
const AUTH_MOUNT_PATH: &str = "/var/run/secrets/hydradb";

/// `Ready=False` reason when `spec.enabled == false`.
///
/// [`hydradb_operator_api::spec::ConditionReason`] pins no `Disabled` variant,
/// so the canonical vocabulary's decode-compat escape hatch carries it:
/// `Unknown("Disabled")` labels verbatim as `"Disabled"`.
fn disabled_reason_label() -> String {
    hydradb_operator_api::spec::ConditionReason::Unknown("Disabled".to_owned())
        .label()
        .to_owned()
}

/// Resolve `spec.clusterRef.name` (pure: no API call).
///
/// # Errors
///
/// Returns [`Error::UnresolvableRef`] (structural — never a panic) when the
/// reference is absent or empty.
pub fn cluster_ref_name(indexer: &GraphIndexer) -> Result<String, Error> {
    let name = indexer.spec.cluster_ref.name.clone();
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

/// Build the indexer's container env: the 11 chart-verbatim knobs (spec value
/// or documented default), then `RUST_MIN_STACK`, then `extra_env` last.
///
/// Shared graph/object-store/auth inputs are NOT listed here: they arrive via
/// `envFrom` on the cluster's shared config ConfigMap (see
/// [`desired_deployment`]).
#[must_use]
pub fn indexer_env(
    spec: &hydradb_operator_api::crd::GraphIndexerSpec,
    cluster_extra_env: &[EnvVar],
) -> Vec<EnvVar> {
    let build_mode = spec
        .build_mode
        .as_ref()
        .map(PhaseLabel::label)
        .unwrap_or(BuildMode::Full.label());
    let mut env = vec![
        EnvVar {
            name: GRAPH_INDEXER_INTERVAL_MS.to_owned(),
            value: Some(spec.interval_ms.unwrap_or(5000).to_string()),
            value_from: None,
        },
        EnvVar {
            name: GRAPH_INDEXER_DIRTY_POLL_INTERVAL_MS.to_owned(),
            value: Some(spec.dirty_poll_interval_ms.unwrap_or(250).to_string()),
            value_from: None,
        },
        EnvVar {
            name: GRAPH_INDEXER_RETAIN_PREVIOUS.to_owned(),
            value: Some(spec.retain_previous_generations.unwrap_or(1).to_string()),
            value_from: None,
        },
        EnvVar {
            name: GRAPH_INDEXER_BUILD_MODE.to_owned(),
            value: Some(build_mode.to_owned()),
            value_from: None,
        },
        EnvVar {
            name: GRAPH_INDEXER_INCREMENTAL_MIN_EDGES.to_owned(),
            value: Some(spec.incremental_min_edges.unwrap_or(250_000).to_string()),
            value_from: None,
        },
        EnvVar {
            name: GRAPH_INDEXER_SCOPE_CONCURRENCY.to_owned(),
            value: Some(spec.scope_concurrency.unwrap_or(8).to_string()),
            value_from: None,
        },
        EnvVar {
            name: GRAPH_INDEXER_SCOPES_PER_CYCLE.to_owned(),
            value: Some(spec.scopes_per_cycle.unwrap_or(256).to_string()),
            value_from: None,
        },
        EnvVar {
            name: GRAPH_INDEXER_MAX_OPEN_SCOPES.to_owned(),
            value: Some(spec.max_open_scopes.unwrap_or(128).to_string()),
            value_from: None,
        },
        EnvVar {
            name: GRAPH_INDEXER_READINESS_FAILURE_THRESHOLD.to_owned(),
            value: Some(spec.readiness_failure_threshold.unwrap_or(3).to_string()),
            value_from: None,
        },
        EnvVar {
            name: GRAPH_INDEXER_MAX_TAIL_FILES.to_owned(),
            value: Some(spec.max_wal_tail_files.unwrap_or(4096).to_string()),
            value_from: None,
        },
        EnvVar {
            name: GRAPH_WAL_TAIL_FETCH_CONCURRENCY.to_owned(),
            value: Some(spec.wal_tail_fetch_concurrency.unwrap_or(16).to_string()),
            value_from: None,
        },
        EnvVar {
            name: constants::RUST_MIN_STACK.to_owned(),
            value: Some(RUST_MIN_STACK_VALUE.to_owned()),
            value_from: None,
        },
    ];
    for var in cluster_extra_env {
        crate::children::push_or_replace(&mut env, var.clone());
    }
    env.extend(spec.extra_env.iter().cloned());
    env
}

/// `OwnerReference` pointing at the owning [`GraphIndexer`].
///
/// `controller` is `true` so garbage collection cascades from the indexer;
/// `block_owner_deletion` stays `false` so a stuck child can never block
/// indexer deletion.
///
/// # Errors
///
/// Returns [`Error::MissingField`] when the owner has no name or uid yet
/// (not yet persisted to the apiserver).
fn owner_ref_for_indexer(indexer: &GraphIndexer) -> Result<OwnerReference, Error> {
    let meta = indexer.meta();
    let name = meta
        .name
        .clone()
        .ok_or_else(|| Error::MissingField("GraphIndexer has no metadata.name".to_owned()))?;
    let uid = meta
        .uid
        .clone()
        .ok_or_else(|| Error::MissingField("GraphIndexer has no metadata.uid".to_owned()))?;
    Ok(OwnerReference {
        api_version: GraphIndexer::api_version(&()).into_owned(),
        kind: GraphIndexer::kind(&()).into_owned(),
        name,
        uid,
        controller: Some(true),
        block_owner_deletion: Some(false),
    })
}

/// Build the desired indexer `Deployment` (pure: no API call).
///
/// Replicas are `0` when `spec.enabled == false`, else `spec.replicas`. The
/// pod carries the node workload's security posture (uid/gid 10001,
/// `readOnlyRootFilesystem`, writable `/var/cache/slatedb` + `/tmp`) and
/// NEVER a PVC: the indexer is stateless, so volumes are `emptyDir` + the
/// auth Secret only.
///
/// # Errors
///
/// Returns [`Error::MissingField`] when the indexer has no name/uid yet, or
/// [`Error::UnresolvableRef`] when `spec.clusterRef.name` is empty.
pub fn desired_deployment(
    indexer: &GraphIndexer,
    cluster: &GraphCluster,
) -> Result<Deployment, Error> {
    let indexer_name = indexer.name_any();
    let namespace = indexer.namespace().unwrap_or_default();
    let cluster_name = cluster.name_any();
    // Validate the reference up front: an empty name is a structural error, not a panic.
    let _ = cluster_ref_name(indexer)?;

    let labels = standard_labels(&cluster_name, ROLE_INDEXER);
    let auth_key = cluster
        .spec
        .auth
        .secret_key
        .clone()
        .unwrap_or_else(|| DEFAULT_AUTH_KEY.to_owned());
    let image = resolve_image(indexer.spec.image.as_ref(), &cluster.spec.image);

    let mut pod_labels = labels.clone();
    pod_labels.extend(
        indexer
            .spec
            .pod_labels
            .iter()
            .map(|(k, v)| (k.clone(), v.clone())),
    );
    let pod_annotations = if indexer.spec.pod_annotations.is_empty() {
        None
    } else {
        Some(indexer.spec.pod_annotations.clone())
    };

    let replicas = if indexer.spec.enabled {
        indexer.spec.replicas
    } else {
        0
    };

    let container = Container {
        name: CONTAINER_NAME.to_owned(),
        image: Some(image),
        command: Some(vec![CONTAINER_NAME.to_owned()]),
        env_from: Some({
            let mut sources = vec![EnvFromSource {
                config_map_ref: Some(ConfigMapEnvSource {
                    name: cluster_child_name(&cluster_name, ROLE_CONFIG),
                    optional: None,
                }),
                prefix: None,
                secret_ref: None,
            }];
            // Credentials: the claim's own Secret for OBC backends, or the
            // user's Secret for explicit ones. Without either, the S3 client
            // falls back to IMDS and times out.
            if let Some(claim) = cluster.spec.object_store.object_bucket_claim.as_ref() {
                sources.push(EnvFromSource {
                    secret_ref: crate::children::secret_env_source(&claim.name),
                    prefix: None,
                    config_map_ref: None,
                });
            } else if let Some(credentials) = cluster
                .spec
                .object_store
                .explicit
                .as_ref()
                .and_then(|explicit| explicit.credentials_secret_ref.as_ref())
            {
                sources.push(EnvFromSource {
                    secret_ref: crate::children::secret_env_source(&credentials.name),
                    prefix: None,
                    config_map_ref: None,
                });
            }
            sources
        }),
        env: Some(indexer_env(&indexer.spec, &cluster.spec.extra_env)),
        ports: Some(vec![ContainerPort {
            name: Some("admin".to_owned()),
            container_port: ADMIN_PORT,
            protocol: Some("TCP".to_owned()),
            ..Default::default()
        }]),
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
        resources: indexer.spec.resources.clone(),
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

    let pod_spec = PodSpec {
        containers: vec![container],
        service_account_name: indexer.spec.service_account.clone(),
        node_selector: indexer.spec.node_selector.clone(),
        tolerations: if indexer.spec.tolerations.is_empty() {
            None
        } else {
            Some(indexer.spec.tolerations.clone())
        },
        affinity: indexer.spec.affinity.clone(),
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
        volumes: Some(vec![
            Volume {
                name: "cache".to_owned(),
                empty_dir: Some(EmptyDirVolumeSource {
                    medium: None,
                    size_limit: None,
                }),
                ..Default::default()
            },
            Volume {
                name: "tmp".to_owned(),
                empty_dir: Some(EmptyDirVolumeSource {
                    medium: None,
                    size_limit: Some(Quantity("1Gi".to_owned())),
                }),
                ..Default::default()
            },
            Volume {
                name: "auth".to_owned(),
                secret: Some(SecretVolumeSource {
                    secret_name: Some(cluster_child_name(&cluster_name, ROLE_AUTH)),
                    items: Some(vec![KeyToPath {
                        key: auth_key.clone(),
                        path: auth_key,
                        mode: None,
                    }]),
                    ..Default::default()
                }),
                ..Default::default()
            },
        ]),
        ..Default::default()
    };

    Ok(Deployment {
        metadata: ObjectMeta {
            name: Some(indexer_child_name(&indexer_name)),
            namespace: Some(namespace),
            labels: Some(labels.clone()),
            owner_references: Some(vec![owner_ref_for_indexer(indexer)?]),
            ..ObjectMeta::default()
        },
        spec: Some(DeploymentSpec {
            replicas: Some(replicas),
            selector: LabelSelector {
                match_labels: Some(labels.clone()),
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
            ..Default::default()
        }),
        status: None,
    })
}

/// Build the desired indexer `PodDisruptionBudget` (pure: no API call).
///
/// One PDB per enabled indexer, `minAvailable: 1`, selecting exactly the
/// indexer pods the Deployment manages. Owned by the indexer so removal
/// cascades with it. A disabled indexer gets no PDB: with zero replicas the
/// budget would be dead config.
///
/// # Errors
///
/// Returns [`Error::MissingField`] when the indexer has no name/uid yet, or
/// [`Error::UnresolvableRef`] when `spec.clusterRef.name` is empty.
pub fn desired_pdb(
    indexer: &GraphIndexer,
    cluster: &GraphCluster,
) -> Result<PodDisruptionBudget, Error> {
    let indexer_name = indexer.name_any();
    let namespace = indexer.namespace().unwrap_or_default();
    let cluster_name = cluster.name_any();
    let _ = cluster_ref_name(indexer)?;

    Ok(PodDisruptionBudget {
        metadata: ObjectMeta {
            name: Some(indexer_child_name(&indexer_name)),
            namespace: Some(namespace),
            labels: Some(standard_labels(&cluster_name, ROLE_PDB)),
            owner_references: Some(vec![owner_ref_for_indexer(indexer)?]),
            ..ObjectMeta::default()
        },
        spec: Some(PodDisruptionBudgetSpec {
            min_available: Some(IntOrString::Int(1)),
            selector: Some(LabelSelector {
                match_labels: Some(standard_labels(&cluster_name, ROLE_INDEXER)),
                ..LabelSelector::default()
            }),
            ..PodDisruptionBudgetSpec::default()
        }),
        ..PodDisruptionBudget::default()
    })
}

/// Desired status for the disabled path: `Ready=False`/`Disabled` plus
/// replica counts pinned at 0 and no served generation.
fn disabled_status(generation: Option<i64>) -> GraphIndexerStatus {
    let reason = disabled_reason_label();
    GraphIndexerStatus {
        conditions: vec![
            new_condition(
                "Ready",
                "False",
                &reason,
                "indexer is disabled (spec.enabled == false)",
                generation,
            ),
            new_condition(
                "Reconciling",
                "False",
                &reason,
                "indexer is disabled (spec.enabled == false)",
                generation,
            ),
            new_condition(
                "Stalled",
                "False",
                hydradb_operator_api::spec::ConditionReason::Available.label(),
                "no structural blockers",
                generation,
            ),
        ],
        observed_generation: generation,
        ready_replicas: 0,
        current_replicas: 0,
        current_generation_label: None,
    }
}

/// Desired status for the success path: kstatus `Ready` over the live set,
/// plus `readyReplicas`/`currentReplicas` and the served
/// `currentGenerationLabel`.
///
/// `Ready` is `True` only when every desired replica reports ready; a
/// partially ready indexer is `Ready=False` (still converging) while
/// `observed_generation` keeps tracking the reconciled generation.
/// `observed_generation` is the reconciler-observed `metadata.generation`,
/// never advanced optimistically.
fn ready_status(
    live_conditions_set: &[k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition],
    generation: Option<i64>,
    ready_replicas: i32,
    current_replicas: i32,
    desired_replicas: i32,
    current_generation_label: Option<String>,
) -> GraphIndexerStatus {
    let mut conditions = live_conditions_set.to_vec();
    set_ready(
        &mut conditions,
        crate::status::is_fully_ready(ready_replicas, desired_replicas),
        generation,
    );
    GraphIndexerStatus {
        conditions,
        observed_generation: generation,
        ready_replicas,
        current_replicas,
        current_generation_label,
    }
}

/// Patch the `/status` subresource when the desired status differs from live.
///
/// The comparison is field-exact over the full surface (`conditions`,
/// `observedGeneration`, `readyReplicas`, `currentReplicas`,
/// `currentGenerationLabel`), so an unchanged status produces NO write and
/// never hot-loops. The patch touches `/status` only, never `spec`.
///
/// # Errors
///
/// Returns [`Error::Api`] when the apiserver rejects the read or the patch.
async fn write_status_if_changed(
    api: &Api<GraphIndexer>,
    name: &str,
    desired: &GraphIndexerStatus,
) -> Result<(), Error> {
    let live = api.get(name).await.map_err(Error::from_kube_error)?;
    let live_status = live.status.clone().unwrap_or_else(|| GraphIndexerStatus {
        conditions: Vec::new(),
        observed_generation: None,
        ready_replicas: 0,
        current_replicas: 0,
        current_generation_label: None,
    });
    if !crate::status::indexer_status_needs_write(&live_status, desired) {
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
                "readyReplicas": desired.ready_replicas,
                "currentReplicas": desired.current_replicas,
                "currentGenerationLabel": desired.current_generation_label,
            }
        })),
    )
    .await
    .map_err(Error::from_kube_error)?;
    Ok(())
}

/// Reconcile one `GraphIndexer` into its indexer `Deployment`.
///
/// The cluster resolves via `spec.clusterRef`; a missing cluster is a
/// structural error (requeue), never a panic.
///
/// # Errors
///
/// Returns [`Error`] when the cluster reference is unresolvable, the child SSA
/// or the status write fails; the controller maps it through [`error_policy`].
async fn reconcile_inner(indexer: Arc<GraphIndexer>, ctx: Arc<Ctx>) -> Result<Action, Error> {
    let name = indexer.name_any();
    let namespace = indexer.namespace().unwrap_or_default();
    let generation = indexer.meta().generation;
    let indexers = Api::<GraphIndexer>::namespaced(ctx.client.clone(), &namespace);

    let cluster_ref = cluster_ref_name(&indexer)?;
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

    let desired = desired_deployment(&indexer, &cluster)?;
    let deployments = Api::<Deployment>::namespaced(ctx.client.clone(), &namespace);
    apply_child(&deployments, &desired).await?;

    // One PDB per enabled indexer; a disabled indexer (zero replicas) owns
    // no budget, and a stale one left from an earlier enabled generation is
    // removed so it never lingers.
    let pdbs = Api::<PodDisruptionBudget>::namespaced(ctx.client.clone(), &namespace);
    let pdb_name = indexer_child_name(&name);
    if indexer.spec.enabled {
        apply_child(&pdbs, &desired_pdb(&indexer, &cluster)?).await?;
    } else if pdbs
        .get_opt(&pdb_name)
        .await
        .map_err(Error::from_kube_error)?
        .is_some()
    {
        pdbs.delete(&pdb_name, &DeleteParams::default())
            .await
            .map_err(Error::from_kube_error)?;
    }

    if !indexer.spec.enabled {
        write_status_if_changed(&indexers, &name, &disabled_status(generation)).await?;
        return Ok(Action::requeue(Duration::from_secs(SUCCESS_REQUEUE_SECS)));
    }

    let (ready_replicas, current_replicas) = deployments
        .get_opt(&indexer_child_name(&name))
        .await
        .map_err(Error::from_kube_error)?
        .and_then(|deployment| deployment.status)
        .map(|status| {
            (
                status.ready_replicas.unwrap_or(0),
                status.replicas.unwrap_or(0),
            )
        })
        .unwrap_or_default();
    let live = indexers.get(&name).await.map_err(Error::from_kube_error)?;
    let live_status = live.status.as_ref();
    let live_conditions_set = live_status
        .map(|status| status.conditions.clone())
        .unwrap_or_default();
    // The indexer's SINGLE child-state list: one `Api::list` of indexer pods
    // scoped by the cluster label, never N per-pod gets. The served
    // generation label is the maximum pod label, falling back to the live
    // preserved value when no pod carries it.
    let pods_api =
        Api::<k8s_openapi::api::core::v1::Pod>::namespaced(ctx.client.clone(), &namespace);
    let indexer_pods = pods_api
        .list(&ListParams::default().labels(&crate::status::indexer_pod_selector(&cluster_ref)))
        .await
        .map_err(Error::from_kube_error)?
        .items;
    let pod_label_sets: Vec<Option<std::collections::BTreeMap<String, String>>> = indexer_pods
        .iter()
        .map(|pod| pod.metadata.labels.clone())
        .collect();
    let live_generation_label =
        live_status.and_then(|status| status.current_generation_label.clone());
    write_status_if_changed(
        &indexers,
        &name,
        &ready_status(
            &live_conditions_set,
            generation,
            ready_replicas,
            current_replicas,
            indexer.spec.replicas,
            crate::status::current_generation_label(&pod_label_sets, live_generation_label),
        ),
    )
    .await?;

    Ok(Action::requeue(Duration::from_secs(SUCCESS_REQUEUE_SECS)))
}

/// Reconcile one `GraphIndexer` (controller entrypoint).
///
/// The ONLY top-level action is `finalizer::with_finalizer` stamping
/// [`constants::FINALIZER`]: the previous apply body runs as the `Apply`
/// closure and [`cleanup_indexer`] as the `Cleanup` closure, so the owning
/// cluster's `deletionPolicy` is honoured on every delete.
///
/// # Errors
///
/// Returns [`Error`] when resolution, child SSA or the status write fails;
/// the controller maps it through [`error_policy`].
pub async fn reconcile(indexer: Arc<GraphIndexer>, ctx: Arc<Ctx>) -> Result<Action, Error> {
    let namespace = indexer.namespace().unwrap_or_default();
    let api: Api<GraphIndexer> = Api::namespaced(ctx.client.clone(), &namespace);
    let apply_ctx = ctx.clone();
    let cleanup_ctx = ctx.clone();
    crate::finalizer::with_finalizer(
        &api,
        indexer,
        |obj| async move {
            let span = metrics::reconcile_span(
                "GraphIndexer",
                &obj.namespace().unwrap_or_default(),
                &obj.name_any(),
            );
            let _entered = span.enter();
            let start = Instant::now();
            let outcome = reconcile_inner(obj, apply_ctx).await;
            let recorded: Result<(), ErrorClass> =
                outcome.as_ref().map(|_| ()).map_err(Error::class);
            metrics::record_reconcile("GraphIndexer", start.elapsed(), recorded);
            outcome
        },
        |obj| async move { cleanup_indexer(obj, cleanup_ctx).await },
    )
    .await
}

/// Effective `deletionPolicy` for the indexer: the owning cluster's policy.
///
/// The indexer spec carries no policy field of its own. An empty `clusterRef`
/// or an unresolvable owning cluster means `Retain`: cleanup must never block
/// deletion on a reference it cannot read.
///
/// # Errors
///
/// Returns [`Error::Api`] when the apiserver rejects the cluster read.
async fn resolve_deletion_policy(
    indexer: &GraphIndexer,
    ctx: &Ctx,
    namespace: &str,
) -> Result<DeletionPolicy, Error> {
    let cluster_name = indexer.spec.cluster_ref.name.clone();
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

/// Ordered cleanup actions for indexer deletion (pure): stamp first, children
/// iff they exist, and never a data-prefix step — the indexer owns no
/// object-store data, so the store always counts as unreachable here.
///
/// Pure so the never-delete-data contract stays unit-testable without a
/// cluster.
#[must_use]
fn indexer_cleanup_plan(
    policy: &DeletionPolicy,
    children_exist: bool,
    indexer_name: &str,
) -> Vec<crate::finalizer::CleanupAction> {
    crate::finalizer::cleanup_plan(policy, children_exist, false, indexer_name)
}

/// Delete the indexer-owned children, tolerating 404s.
///
/// The `Deployment` and the indexer `PodDisruptionBudget` are the indexer's
/// only direct children. A 404 surfaces as [`Error::Api`] and is treated as
/// success by [`crate::finalizer::execute_cleanup`], so a never-applied
/// indexer cleans up without touching anything else.
async fn delete_indexer_children(
    client: kube::Client,
    namespace: &str,
    indexer: &GraphIndexer,
) -> Result<(), Error> {
    let name = indexer.name_any();
    let params = DeleteParams::default();
    let deployments = Api::<Deployment>::namespaced(client.clone(), namespace);
    let _ = deployments
        .delete(&indexer_child_name(&name), &params)
        .await
        .map_err(Error::from_kube_error)?;
    let pdbs = Api::<PodDisruptionBudget>::namespaced(client, namespace);
    let _ = pdbs
        .delete(&indexer_child_name(&name), &params)
        .await
        .map_err(Error::from_kube_error)?;
    Ok(())
}

/// `Cleanup` closure for [`reconcile`]: honour the owning cluster's
/// `deletionPolicy`.
///
/// `Retain` (and an empty/unresolvable `clusterRef`, which defaults to
/// `Retain`) stamps `pruned-by` and removes the `Deployment`, leaving
/// object-store data intact. `Delete` stamps the `Delete` reason with the
/// same child plan; the data-prefix step is always omitted because the
/// indexer owns no object-store data, so deletion never blocks on the store.
async fn cleanup_indexer(indexer: Arc<GraphIndexer>, ctx: Arc<Ctx>) -> Result<Action, Error> {
    let name = indexer.name_any();
    let namespace = indexer.namespace().unwrap_or_default();
    let policy = resolve_deletion_policy(&indexer, &ctx, &namespace).await?;
    let reason = policy_reason(&policy);
    let client = ctx.client.clone();
    let deployments = Api::<Deployment>::namespaced(client.clone(), &namespace);
    let children_exist = deployments
        .get_opt(&indexer_child_name(&name))
        .await
        .map_err(Error::from_kube_error)?
        .is_some()
        || Api::<PodDisruptionBudget>::namespaced(client.clone(), &namespace)
            .get_opt(&indexer_child_name(&name))
            .await
            .map_err(Error::from_kube_error)?
            .is_some();
    let plan = indexer_cleanup_plan(&policy, children_exist, &name);
    let indexers_api: Api<GraphIndexer> = Api::namespaced(client.clone(), &namespace);
    crate::finalizer::execute_cleanup(
        &plan,
        || crate::finalizer::stamp_pruned_by(&indexers_api, &name, reason),
        || delete_indexer_children(client.clone(), &namespace, &indexer),
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
pub fn error_policy(indexer: Arc<GraphIndexer>, error: &Error, ctx: Arc<Ctx>) -> Action {
    crate::events::report_for(ctx.client.clone(), "GraphIndexer", indexer.as_ref(), error);
    let key = format!(
        "{}/{}",
        indexer.namespace().unwrap_or_default(),
        indexer.name_any()
    );
    Action::requeue(error.requeue_after(&key))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn cluster_fixture() -> GraphCluster {
        serde_json::from_value(serde_json::json!({
            "apiVersion": "hydradb.kubexa.tech/v1alpha1",
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

    fn indexer_fixture(enabled: bool) -> GraphIndexer {
        serde_json::from_value(serde_json::json!({
            "apiVersion": "hydradb.kubexa.tech/v1alpha1",
            "kind": "GraphIndexer",
            "metadata": {"name": "demo-indexer", "namespace": "default", "uid": "idx-uid-1"},
            "spec": {
                "clusterRef": {"name": "demo"},
                "enabled": enabled,
                "replicas": 2
            }
        }))
        .expect("fixture must deserialize")
    }

    fn env_value(env: &[EnvVar], name: &str) -> String {
        env.iter()
            .find(|var| var.name == name)
            .unwrap_or_else(|| panic!("env must carry {name}"))
            .value
            .clone()
            .unwrap_or_default()
    }

    fn pod_spec_of(deployment: &Deployment) -> &PodSpec {
        deployment
            .spec
            .as_ref()
            .expect("deployment carries a spec")
            .template
            .spec
            .as_ref()
            .expect("pod template carries a spec")
    }

    #[test]
    fn disabled_indexer_scales_to_zero_and_sets_ready_false_disabled() {
        let indexer = indexer_fixture(false);
        let cluster = cluster_fixture();
        let deployment =
            desired_deployment(&indexer, &cluster).expect("disabled deployment must build");
        assert_eq!(
            deployment.spec.as_ref().and_then(|spec| spec.replicas),
            Some(0),
            "enabled:false must scale the Deployment to 0"
        );

        let status = disabled_status(Some(3));
        let ready = status
            .conditions
            .iter()
            .find(|c| c.type_ == "Ready")
            .expect("disabled status carries Ready");
        assert_eq!(ready.status, "False");
        assert_eq!(ready.reason, "Disabled");
        assert_eq!(status.ready_replicas, 0);
        assert_eq!(status.current_replicas, 0);
        assert_eq!(status.current_generation_label, None);
    }

    #[test]
    fn ready_status_tracks_generation_and_marks_partial_replicas_not_ready() {
        let indexer = indexer_fixture(true);
        let generation = indexer.meta().generation;
        let full = ready_status(
            &[],
            generation,
            2,
            2,
            indexer.spec.replicas,
            Some("gen-7".to_owned()),
        );
        let ready = full
            .conditions
            .iter()
            .find(|c| c.type_ == "Ready")
            .expect("status carries Ready");
        assert_eq!(ready.status, "True");
        assert_eq!(full.observed_generation, generation);
        assert_eq!(full.current_replicas, 2);
        assert_eq!(full.current_generation_label.as_deref(), Some("gen-7"));

        let partial = ready_status(&[], generation, 1, 2, indexer.spec.replicas, None);
        let ready = partial
            .conditions
            .iter()
            .find(|c| c.type_ == "Ready")
            .expect("status carries Ready");
        assert_eq!(
            ready.status, "False",
            "a partially ready indexer must be Ready=False"
        );
        assert_eq!(
            partial.observed_generation, generation,
            "observedGeneration must still track generation while under-replicated"
        );
    }

    #[test]
    fn enabled_indexer_uses_the_spec_replicas() {
        let indexer = indexer_fixture(true);
        let cluster = cluster_fixture();
        let deployment =
            desired_deployment(&indexer, &cluster).expect("enabled deployment must build");
        assert_eq!(
            deployment.spec.as_ref().and_then(|spec| spec.replicas),
            Some(2),
            "enabled indexer must use spec.replicas"
        );
    }

    #[test]
    fn omitted_knobs_fall_back_to_the_documented_defaults() {
        let indexer = indexer_fixture(true);
        let env = indexer_env(&indexer.spec, &[]);
        for (name, want) in [
            (GRAPH_INDEXER_INTERVAL_MS, "5000"),
            (GRAPH_INDEXER_DIRTY_POLL_INTERVAL_MS, "250"),
            (GRAPH_INDEXER_RETAIN_PREVIOUS, "1"),
            (GRAPH_INDEXER_BUILD_MODE, "full"),
            (GRAPH_INDEXER_INCREMENTAL_MIN_EDGES, "250000"),
            (GRAPH_INDEXER_SCOPE_CONCURRENCY, "8"),
            (GRAPH_INDEXER_SCOPES_PER_CYCLE, "256"),
            (GRAPH_INDEXER_MAX_OPEN_SCOPES, "128"),
            (GRAPH_INDEXER_READINESS_FAILURE_THRESHOLD, "3"),
            (GRAPH_INDEXER_MAX_TAIL_FILES, "4096"),
            (GRAPH_WAL_TAIL_FETCH_CONCURRENCY, "16"),
        ] {
            assert_eq!(
                env_value(&env, name),
                want,
                "omitted knob {name} must default to {want}"
            );
        }
    }

    #[test]
    fn spec_image_overrides_the_cluster_image() {
        let cluster = cluster_fixture();
        // No override: inherit the cluster image verbatim.
        let inherited = resolve_image(None, &cluster.spec.image);
        assert_eq!(inherited, "example.com/hydradb:v0.1.0");

        // Override wins.
        let custom = Image {
            repository: "example.com/custom-indexer".to_owned(),
            tag: Some("v9.9.9".to_owned()),
            digest: None,
            pull_policy: None,
            pull_secrets: Vec::new(),
        };
        assert_eq!(
            resolve_image(Some(&custom), &cluster.spec.image),
            "example.com/custom-indexer:v9.9.9"
        );

        // Digest beats tag, mirroring the chart helper.
        let pinned = Image {
            repository: "example.com/custom-indexer".to_owned(),
            tag: Some("v9.9.9".to_owned()),
            digest: Some("sha256:abc123".to_owned()),
            pull_policy: None,
            pull_secrets: Vec::new(),
        };
        assert_eq!(
            resolve_image(Some(&pinned), &cluster.spec.image),
            "example.com/custom-indexer@sha256:abc123"
        );

        // The built Deployment carries the override.
        let mut indexer = indexer_fixture(true);
        indexer.spec.image = Some(custom);
        let deployment =
            desired_deployment(&indexer, &cluster).expect("override deployment must build");
        let image = &pod_spec_of(&deployment).containers[0]
            .image
            .clone()
            .unwrap_or_default();
        assert_eq!(image, "example.com/custom-indexer:v9.9.9");
    }

    #[test]
    fn missing_cluster_ref_is_a_structural_error_not_a_panic() {
        let mut indexer = indexer_fixture(true);
        indexer.spec.cluster_ref.name = String::new();
        let error = cluster_ref_name(&indexer).expect_err("empty clusterRef must fail");
        assert_eq!(
            error.class(),
            ErrorClass::Structural,
            "missing clusterRef must be structural"
        );
        assert!(
            error.to_string().contains("clusterRef"),
            "error must name clusterRef, got: {error}"
        );

        // desired_deployment surfaces the same structural error, never panics.
        let cluster = cluster_fixture();
        let error = desired_deployment(&indexer, &cluster)
            .expect_err("empty clusterRef must fail the builder");
        assert_eq!(error.class(), ErrorClass::Structural);
    }

    #[test]
    fn deployment_runs_the_graph_indexer_command_with_the_node_security_context() {
        let indexer = indexer_fixture(true);
        let cluster = cluster_fixture();
        let deployment = desired_deployment(&indexer, &cluster).expect("deployment must build");
        let pod = pod_spec_of(&deployment);
        let container = &pod.containers[0];
        assert_eq!(
            container.command.clone().unwrap_or_default(),
            vec!["graph-indexer".to_owned()],
            "container must run the graph-indexer binary"
        );

        let pod_ctx = pod
            .security_context
            .as_ref()
            .expect("pod carries a security context");
        assert_eq!(pod_ctx.run_as_user, Some(10001));
        assert_eq!(pod_ctx.run_as_group, Some(10001));
        assert_eq!(pod_ctx.fs_group, Some(10001));

        let ctx = container
            .security_context
            .as_ref()
            .expect("container carries a security context");
        assert_eq!(ctx.read_only_root_filesystem, Some(true));

        let mounts: BTreeMap<&str, &VolumeMount> = container
            .volume_mounts
            .as_ref()
            .expect("container carries mounts")
            .iter()
            .map(|mount| (mount.mount_path.as_str(), mount))
            .collect();
        assert!(
            mounts.contains_key("/var/cache/slatedb"),
            "writable slatedb cache mount must exist"
        );
        assert!(
            mounts.contains_key(TMP_MOUNT_PATH),
            "writable /tmp mount must exist"
        );
    }

    #[test]
    fn deployment_never_declares_a_persistent_volume_claim() {
        for enabled in [true, false] {
            let indexer = indexer_fixture(enabled);
            let cluster = cluster_fixture();
            let deployment = desired_deployment(&indexer, &cluster).expect("deployment must build");
            let pod = pod_spec_of(&deployment);
            for volume in pod.volumes.as_ref().expect("volumes exist") {
                assert!(
                    volume.persistent_volume_claim.is_none(),
                    "stateless indexer must never mount a PVC (volume {})",
                    volume.name
                );
                assert!(
                    volume.empty_dir.is_some() || volume.secret.is_some(),
                    "indexer volumes are emptyDir or Secret only (volume {})",
                    volume.name
                );
            }
        }
    }

    const RECONCILE_SOURCE: &str = include_str!("indexer.rs");

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

        let plan = indexer_cleanup_plan(&DeletionPolicy::Retain, true, "demo-indexer");
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
    fn delete_policy_on_an_indexer_still_never_plans_data_prefix_removal() {
        use crate::finalizer::CleanupAction;

        let plan = indexer_cleanup_plan(&DeletionPolicy::Delete, true, "demo-indexer");
        assert!(
            plan.iter()
                .any(|action| matches!(action, CleanupAction::DeleteChildren)),
            "Delete must still remove the indexer's own Deployment"
        );
        assert!(
            plan.iter()
                .all(|action| !matches!(action, CleanupAction::DeleteDataPrefix { .. })),
            "an indexer owns no object-store data, so Delete must never plan prefix removal"
        );
        match plan.first() {
            Some(CleanupAction::StampPrunedBy { reason }) => {
                assert_eq!(reason, "Delete", "the stamp must name the Delete policy")
            }
            other => panic!("stamp must lead every plan, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn cleanup_of_a_never_applied_indexer_returns_ok_without_touching_children_or_store() {
        use crate::finalizer::execute_cleanup;

        let plan = indexer_cleanup_plan(&DeletionPolicy::Retain, false, "demo-indexer");
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
    fn indexer_pdb_pins_min_available_1_and_selects_the_indexer_pods() {
        let indexer = indexer_fixture(true);
        let cluster = cluster_fixture();
        let pdb = desired_pdb(&indexer, &cluster).expect("indexer PDB builds without a cluster");
        assert_eq!(
            pdb.metadata.name.as_deref(),
            Some("demo-indexer"),
            "one PDB per indexer, named after the indexer"
        );
        let owner_refs = pdb
            .metadata
            .owner_references
            .expect("PDB must carry an owner references");
        assert_eq!(owner_refs.len(), 1, "exactly one owner references");
        assert_eq!(owner_refs[0].kind, "GraphIndexer");
        assert_eq!(owner_refs[0].controller, Some(true));
        let spec = pdb.spec.expect("PDB must carry a spec");
        assert_eq!(
            spec.min_available,
            Some(IntOrString::Int(1)),
            "indexer PDB pins minAvailable: 1"
        );
        assert_eq!(
            spec.selector
                .expect("PDB must carry a selector")
                .match_labels,
            Some(standard_labels("demo", ROLE_INDEXER)),
            "indexer PDB must select exactly the indexer pods"
        );
    }
}
