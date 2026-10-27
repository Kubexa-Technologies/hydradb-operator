//! `GraphBackup` reconciler: scheduled or one-shot object-store copies.
//!
//! The operator never touches the object store itself. It builds a `CronJob`
//! (when `spec.schedule` is set) or a single `Job` (when it is not) from a
//! user-supplied copier image and injects the whole contract as environment
//! variables, so no credential ever reaches the controller:
//!
//! ```text
//! HYDRADB_BACKUP_DIRECTION=backup
//! HYDRADB_BACKUP_SOURCE_BUCKET / _PREFIX   (the cluster's own store)
//! HYDRADB_BACKUP_TARGET_BUCKET / _PREFIX   (where the copy goes)
//! HYDRADB_BACKUP_VERIFY=true|false
//! ```
//!
//! Managed objects are the `CronJob`/`Job` only. A finished `Job` is left in
//! place (bounded by `successfulJobsHistoryLimit`/`failedJobsHistoryLimit`) so
//! the run log survives long enough to be inspected.

use std::sync::Arc;
use std::time::Duration;

use hydradb_operator_api::constants;
use hydradb_operator_api::crd::{GraphBackup, GraphCluster};
use k8s_openapi::api::batch::v1::{CronJob, CronJobSpec, Job, JobSpec};
use k8s_openapi::api::core::v1::{
    Container, EnvFromSource, EnvVar, PodSpec, PodTemplateSpec, ResourceRequirements,
    SecretEnvSource, SecretVolumeSource, Volume, VolumeMount,
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use kube::api::{Api, DeleteParams, ListParams, Patch, PatchParams};
use kube::runtime::controller::Action;
use kube::{Resource, ResourceExt};
use tracing::info;

use crate::apply::owner_ref_for;
use crate::context::Ctx;
use crate::error::Error;

/// Requeue interval while waiting on an in-flight run.
const IN_FLIGHT_REQUEUE_SECS: u64 = 15;
/// Requeue interval when everything is settled.
const SETTLED_REQUEUE_SECS: u64 = 300;

/// Resolved destination prefix for a backup.
///
/// Absent `spec.target.prefix` means `backups/<namespace>/<cluster>`, so two
/// clusters in one namespace can never overwrite each other's copies.
#[must_use]
pub fn resolve_target_prefix(backup: &GraphBackup, namespace: &str) -> String {
    backup
        .spec
        .target
        .prefix
        .clone()
        .unwrap_or_else(|| format!("backups/{namespace}/{}", backup.spec.cluster_ref.name))
}

/// Name of the owned `CronJob`/`Job` for a backup.
#[must_use]
pub fn backup_child_name(backup_name: &str) -> String {
    format!("{backup_name}-backup")
}

/// Env contract shared by the backup and restore jobs.
fn backup_env(direction: &str, verify: bool) -> Vec<EnvVar> {
    vec![
        EnvVar {
            name: constants::HYDRADB_BACKUP_DIRECTION.to_owned(),
            value: Some(direction.to_owned()),
            ..EnvVar::default()
        },
        EnvVar {
            name: constants::HYDRADB_BACKUP_VERIFY.to_owned(),
            value: Some(verify.to_string()),
            ..EnvVar::default()
        },
    ]
}

/// Retention env handed to the copier so it can prune old copies itself.
fn retention_env(retention: i32) -> EnvVar {
    EnvVar {
        name: constants::HYDRADB_BACKUP_RETENTION.to_owned(),
        value: Some(retention.to_string()),
        ..EnvVar::default()
    }
}

/// Env describing the source store of a backup (the cluster's own store).
fn source_env(cluster: &GraphCluster) -> Vec<EnvVar> {
    let mut env = vec![EnvVar {
        name: constants::HYDRADB_BACKUP_SOURCE_BUCKET.to_owned(),
        value: Some(resolved_bucket(cluster)),
        ..EnvVar::default()
    }];
    env.push(EnvVar {
        name: constants::HYDRADB_BACKUP_SOURCE_PREFIX.to_owned(),
        value: Some(source_prefix(cluster)),
        ..EnvVar::default()
    });
    env
}

/// Bucket the cluster writes to, as far as the spec can tell.
///
/// With an `ObjectBucketClaim` the generated Secret owns the real name, so the
/// claim name is emitted instead: the copier resolves it from the same Secret
/// mounted next to it (see [`secret_env_from`]).
#[must_use]
pub fn resolved_bucket(cluster: &GraphCluster) -> String {
    match &cluster.spec.object_store.explicit {
        Some(explicit) => explicit.bucket.clone(),
        None => cluster
            .spec
            .object_store
            .object_bucket_claim
            .as_ref()
            .map(|claim| claim.name.clone())
            .unwrap_or_default(),
    }
}

/// Object prefix the cluster's graph lives under.
#[must_use]
pub fn source_prefix(cluster: &GraphCluster) -> String {
    let mut path = format!("namespaces/{}", cluster.spec.graph.namespace);
    for sub in &cluster.spec.graph.subnamespaces {
        path.push_str("/subnamespaces/");
        path.push_str(sub);
    }
    path.push_str("/graphs/");
    path.push_str(&cluster.spec.graph.id);
    path
}

/// Target env for a backup.
fn target_env(backup: &GraphBackup, namespace: &str) -> Vec<EnvVar> {
    vec![
        EnvVar {
            name: constants::HYDRADB_BACKUP_TARGET_BUCKET.to_owned(),
            value: Some(backup.spec.target.store.bucket.clone()),
            ..EnvVar::default()
        },
        EnvVar {
            name: constants::HYDRADB_BACKUP_TARGET_PREFIX.to_owned(),
            value: Some(resolve_target_prefix(backup, namespace)),
            ..EnvVar::default()
        },
    ]
}

/// `envFrom` for the credentials Secret, when one is referenced.
fn secret_env_from(secret: Option<&str>) -> Option<Vec<EnvFromSource>> {
    secret.map(|name| {
        vec![EnvFromSource {
            secret_ref: Some(SecretEnvSource {
                name: name.to_owned(),
                optional: Some(false),
            }),
            ..EnvFromSource::default()
        }]
    })
}

/// Mounted Secret holding `AWS_ACCESS_KEY_ID`/`AWS_SECRET_ACCESS_KEY`.
fn secret_volume(secret: Option<&str>) -> Option<(Vec<Volume>, Vec<VolumeMount>)> {
    secret.map(|name| {
        (
            vec![Volume {
                name: "credentials".to_owned(),
                secret: Some(SecretVolumeSource {
                    secret_name: Some(name.to_owned()),
                    optional: Some(false),
                    ..SecretVolumeSource::default()
                }),
                ..Volume::default()
            }],
            vec![VolumeMount {
                name: "credentials".to_owned(),
                mount_path: "/var/run/secrets/hydradb-backup".to_owned(),
                read_only: Some(true),
                ..VolumeMount::default()
            }],
        )
    })
}

/// Build the pod template shared by the `Job` and `CronJob` shapes.
fn pod_template(backup: &GraphBackup, cluster: &GraphCluster, namespace: &str) -> PodTemplateSpec {
    let mut env = backup_env("backup", backup.spec.verify);
    env.extend(source_env(cluster));
    env.extend(target_env(backup, namespace));
    env.push(retention_env(backup.spec.retention));
    // Contract variables are operator-owned: a user value replaces in place
    // only where it is allowed, never duplicating a contract variable (a
    // duplicate would silently win by being last in the container env list).
    for var in &backup.spec.extra_env {
        crate::children::push_or_replace(&mut env, var.clone());
    }

    let secret_name = backup
        .spec
        .target
        .store
        .credentials_secret_ref
        .as_ref()
        .map(|reference| reference.name.clone());

    let (volumes, volume_mounts) = match secret_volume(secret_name.as_deref()) {
        Some((volumes, mounts)) => (Some(volumes), Some(mounts)),
        None => (None, None),
    };

    PodTemplateSpec {
        metadata: Some(ObjectMeta {
            labels: Some(crate::apply::standard_labels(&cluster.name_any(), "backup")),
            annotations: if backup.spec.pod_annotations.is_empty() {
                None
            } else {
                Some(backup.spec.pod_annotations.clone())
            },
            ..ObjectMeta::default()
        }),
        spec: Some(PodSpec {
            restart_policy: Some("Never".to_owned()),
            containers: vec![Container {
                name: "backup".to_owned(),
                image: Some(image_ref(&backup.spec.image)),
                image_pull_policy: backup
                    .spec
                    .image
                    .pull_policy
                    .as_ref()
                    .map(|policy| hydradb_operator_api::spec::PhaseLabel::label(policy).to_owned()),
                env: Some(env),
                env_from: secret_env_from(secret_name.as_deref()),
                resources: backup.spec.resources.clone().or_else(|| {
                    Some(ResourceRequirements {
                        requests: Some(
                            [(
                                "cpu".to_owned(),
                                k8s_openapi::apimachinery::pkg::api::resource::Quantity(
                                    "100m".to_owned(),
                                ),
                            )]
                            .into_iter()
                            .collect(),
                        ),
                        ..ResourceRequirements::default()
                    })
                }),
                volume_mounts,
                ..Container::default()
            }],
            volumes,
            ..PodSpec::default()
        }),
    }
}

/// Render an [`Image`](hydradb_operator_api::spec::Image) as a pullable reference.
#[must_use]
pub fn image_ref(image: &hydradb_operator_api::spec::Image) -> String {
    if let Some(digest) = &image.digest {
        return format!("{}@{digest}", image.repository);
    }
    match &image.tag {
        Some(tag) => format!("{}:{tag}", image.repository),
        None => image.repository.clone(),
    }
}

/// Desired `CronJob` for a scheduled backup.
///
/// # Errors
/// Returns [`Error::MissingField`] when the backup or cluster has no name/uid.
pub fn desired_cron_job(backup: &GraphBackup, cluster: &GraphCluster) -> Result<CronJob, Error> {
    let namespace = backup.namespace().unwrap_or_default();
    Ok(CronJob {
        metadata: crate::apply::child_object_meta_for(
            &backup.name_any(),
            "backup",
            backup_owner_ref(backup)?,
        ),
        spec: CronJobSpec {
            schedule: backup.spec.schedule.clone().unwrap_or_default(),
            suspend: Some(backup.spec.suspend),
            concurrency_policy: Some("Forbid".to_owned()),
            successful_jobs_history_limit: backup.spec.successful_jobs_history_limit,
            failed_jobs_history_limit: backup.spec.failed_jobs_history_limit,
            job_template: JobTemplateSpec {
                metadata: Some(ObjectMeta {
                    labels: Some(crate::apply::standard_labels(&cluster.name_any(), "backup")),
                    ..ObjectMeta::default()
                }),
                spec: Some(job_spec(backup, cluster, &namespace)),
            },
            ..CronJobSpec::default()
        },
        status: None,
    })
}

use k8s_openapi::api::batch::v1::JobTemplateSpec;

fn job_spec(backup: &GraphBackup, cluster: &GraphCluster, namespace: &str) -> JobSpec {
    JobSpec {
        template: pod_template(backup, cluster, namespace),
        backoff_limit: backup.spec.backoff_limit,
        active_deadline_seconds: backup.spec.active_deadline_seconds,
        ..JobSpec::default()
    }
}

/// Desired one-shot `Job` for an unscheduled backup.
///
/// # Errors
/// Returns [`Error::MissingField`] when required metadata is absent.
pub fn desired_job(backup: &GraphBackup, cluster: &GraphCluster) -> Result<Job, Error> {
    let namespace = backup.namespace().unwrap_or_default();
    Ok(Job {
        metadata: crate::apply::child_object_meta_for(
            &backup.name_any(),
            "backup",
            backup_owner_ref(backup)?,
        ),
        spec: Some(job_spec(backup, cluster, &namespace)),
        status: None,
    })
}

/// `OwnerReference` pointing at the owning `GraphBackup`.
fn backup_owner_ref(
    backup: &GraphBackup,
) -> Result<k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference, Error> {
    let meta = backup.meta();
    let name = meta
        .name
        .clone()
        .ok_or_else(|| Error::MissingField("GraphBackup has no metadata.name".to_owned()))?;
    let uid = meta
        .uid
        .clone()
        .ok_or_else(|| Error::MissingField("GraphBackup has no metadata.uid".to_owned()))?;
    Ok(
        k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference {
            api_version: GraphBackup::api_version(&()).into_owned(),
            kind: GraphBackup::kind(&()).into_owned(),
            name,
            uid,
            controller: Some(true),
            block_owner_deletion: Some(false),
        },
    )
}

/// Reconcile one `GraphBackup`.
///
/// # Errors
/// Returns [`Error`] when the cluster reference does not resolve or a child
/// write fails.
pub async fn reconcile(backup: Arc<GraphBackup>, ctx: Arc<Ctx>) -> Result<Action, Error> {
    let name = backup.name_any();
    let namespace = backup.namespace().unwrap_or_default();
    let generation = backup.meta().generation;
    let backups = Api::<GraphBackup>::namespaced(ctx.client.clone(), &namespace);

    let cluster_name = backup.spec.cluster_ref.name.clone();
    let clusters = Api::<GraphCluster>::namespaced(ctx.client.clone(), &namespace);
    let Some(cluster) = clusters
        .get_opt(&cluster_name)
        .await
        .map_err(Error::from_kube_error)?
    else {
        let message =
            format!("clusterRef {namespace}/{cluster_name} does not resolve to a GraphCluster");
        write_status(&backups, &name, generation, "False", &message, None).await?;
        return Ok(Action::requeue(Duration::from_secs(SETTLED_REQUEUE_SECS)));
    };

    if let Err(errors) = backup.spec.validate() {
        let message = errors
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; ");
        write_status(&backups, &name, generation, "False", &message, None).await?;
        return Ok(Action::requeue(Duration::from_secs(SETTLED_REQUEUE_SECS)));
    }

    // A backup into the cluster's own store is a no-op that doubles write
    // amplification; refuse it loudly instead of copying onto itself.
    if let Some(explicit) = &cluster.spec.object_store.explicit
        && backup.spec.targets(explicit)
    {
        let message = "target.store points at the cluster's own object store";
        write_status(&backups, &name, generation, "False", message, None).await?;
        return Ok(Action::requeue(Duration::from_secs(SETTLED_REQUEUE_SECS)));
    }

    let target = format!(
        "{}/{}",
        backup.spec.target.store.bucket,
        resolve_target_prefix(&backup, &namespace)
    );

    let cron_api = Api::<CronJob>::namespaced(ctx.client.clone(), &namespace);
    let job_api = Api::<Job>::namespaced(ctx.client.clone(), &namespace);
    let child = backup_child_name(&name);

    if backup.spec.schedule.is_some() {
        crate::apply::apply_child(&cron_api, &desired_cron_job(&backup, &cluster)?).await?;
        // A previously-applied one-shot job is not part of this shape.
        let _ = job_api.delete(&child, &DeleteParams::default()).await;
        write_status(
            &backups,
            &name,
            generation,
            "True",
            "scheduled",
            Some(&target),
        )
        .await?;
        return Ok(Action::requeue(Duration::from_secs(SETTLED_REQUEUE_SECS)));
    }

    // One-shot: create only when absent so a failed run is never silently
    // recreated in a loop.
    let _ = cron_api.delete(&child, &DeleteParams::default()).await;
    let existing = job_api
        .get_opt(&child)
        .await
        .map_err(Error::from_kube_error)?;
    let in_flight = match &existing {
        None => {
            crate::apply::apply_child(&job_api, &desired_job(&backup, &cluster)?).await?;
            info!(backup = %name, %target, "backup job created");
            true
        }
        Some(job) => job
            .status
            .as_ref()
            .and_then(|status| status.active.as_ref())
            .is_some_and(|active| *active > 0),
    };

    let (ready, message) = match &existing {
        None => ("False", "backup job started".to_owned()),
        Some(job) => {
            let status = job.status.as_ref();
            if status
                .and_then(|s| s.succeeded)
                .is_some_and(|succeeded| succeeded > 0)
            {
                ("True", "backup completed".to_owned())
            } else if status
                .and_then(|s| s.failed)
                .is_some_and(|failed| failed > 0)
            {
                ("False", "backup job failed".to_owned())
            } else {
                ("False", "backup in progress".to_owned())
            }
        }
    };
    if ready == "True" {
        let now = chrono::Utc::now();
        crate::metrics::set_gauge(
            "hydradb_operator_backup_last_success_timestamp_seconds",
            &[("name", &name)],
            now.timestamp() as f64,
        );
        write_backup_success(&backups, &name, generation, &now.to_rfc3339(), &child).await?;
        prune_old_runs(&job_api, &namespace, &name, backup.spec.retention).await?;
    } else {
        write_status(&backups, &name, generation, ready, &message, Some(&target)).await?;
    }
    Ok(Action::requeue(Duration::from_secs(if in_flight {
        IN_FLIGHT_REQUEUE_SECS
    } else {
        SETTLED_REQUEUE_SECS
    })))
}

/// Patch `/status` for a backup, skipping the write when nothing changed.
///
/// A blind patch is an unbounded write loop: every write bumps
/// `resourceVersion`, wakes the watcher and re-enters reconcile. The Ready
/// condition is therefore compared first and the previous condition object is
/// reused (keeping its `lastTransitionTime`) when status/message already match.
async fn write_status(
    api: &Api<GraphBackup>,
    name: &str,
    generation: Option<i64>,
    ready: &str,
    message: &str,
    target: Option<&str>,
) -> Result<(), Error> {
    let live = api.get_opt(name).await.map_err(Error::from_kube_error)?;
    let live_status = live
        .as_ref()
        .and_then(|object| object.status.clone())
        .unwrap_or_default();
    let existing_ready = live_status
        .conditions
        .iter()
        .find(|condition| condition.type_ == "Ready")
        .cloned();
    let unchanged = existing_ready
        .as_ref()
        .is_some_and(|condition| condition.status == ready && condition.message == message)
        && live_status.last_target.as_deref() == target
        && live_status.observed_generation == generation;
    if unchanged {
        return Ok(());
    }

    let condition = existing_ready
        .filter(|condition| condition.status == ready && condition.message == message);
    let condition = condition.unwrap_or_else(|| {
        crate::conditions::new_condition(
            "Ready",
            ready,
            <hydradb_operator_api::spec::ConditionReason as hydradb_operator_api::spec::PhaseLabel>::label(
                &hydradb_operator_api::spec::ConditionReason::Reconciling,
            ),
            message,
            generation,
        )
    });
    let patch = serde_json::json!({
        "status": {
            "conditions": [condition],
            "observedGeneration": generation,
            "lastTarget": target,
        }
    });
    api.patch_status(name, &PatchParams::default(), &Patch::Merge(&patch))
        .await
        .map_err(Error::from_kube_error)?;
    Ok(())
}

/// Delete finished runs beyond `retention`.
///
/// Only the operator's own `Job` records are pruned here; the object copies are
/// the copier's to remove (it is told the retention count). Deleting a run
/// record never touches data.
async fn prune_old_runs(
    api: &Api<Job>,
    namespace: &str,
    backup_name: &str,
    retention: i32,
) -> Result<(), Error> {
    let selector = format!("{}={}", crate::apply::CLUSTER_SELECTOR_LABEL, backup_name);
    let mut finished: Vec<Job> = api
        .list(&ListParams::default().labels(&selector))
        .await
        .map_err(Error::from_kube_error)?
        .items
        .into_iter()
        .filter(|job| {
            job.status
                .as_ref()
                .is_some_and(|status| status.completion_time.is_some())
        })
        .collect();
    if finished.len() <= usize::try_from(retention).unwrap_or(0) {
        return Ok(());
    }
    finished.sort_by_key(|job| {
        job.status
            .as_ref()
            .and_then(|status| status.completion_time.clone())
    });
    let excess = finished.len() - usize::try_from(retention).unwrap_or(0);
    for job in finished.into_iter().take(excess) {
        if let Some(name) = job.metadata.name {
            let _ = api.delete(&name, &DeleteParams::background()).await;
            let _ = namespace;
        }
    }
    Ok(())
}

/// Record a successful run: timestamp, counter and the job that produced it.
async fn write_backup_success(
    api: &Api<GraphBackup>,
    name: &str,
    generation: Option<i64>,
    finished_at: &str,
    job: &str,
) -> Result<(), Error> {
    let live = api.get_opt(name).await.map_err(Error::from_kube_error)?;
    let live_status = live
        .as_ref()
        .and_then(|object| object.status.clone())
        .unwrap_or_default();
    if live_status.last_successful_time.as_deref() == Some(finished_at) {
        return Ok(());
    }
    let condition = crate::conditions::new_condition(
        "Ready",
        "True",
        <hydradb_operator_api::spec::ConditionReason as hydradb_operator_api::spec::PhaseLabel>::label(
            &hydradb_operator_api::spec::ConditionReason::Available,
        ),
        "backup completed",
        generation,
    );
    let patch = serde_json::json!({
        "status": {
            "conditions": [condition],
            "observedGeneration": generation,
            "lastSuccessfulTime": finished_at,
            "lastRunName": job,
            "successfulBackups": live_status.successful_backups + 1,
        }
    });
    api.patch_status(name, &PatchParams::default(), &Patch::Merge(&patch))
        .await
        .map_err(Error::from_kube_error)?;
    Ok(())
}

/// Map a backup reconcile error onto a requeue action.
#[must_use]
pub fn error_policy(backup: Arc<GraphBackup>, error: &Error, _ctx: Arc<Ctx>) -> Action {
    Action::requeue(error.requeue_after(&backup.name_any()))
}

/// List the backup jobs currently owned by `name` (newest first).
///
/// # Errors
/// Returns [`Error::Api`] when the list fails.
pub async fn owned_jobs(api: &Api<Job>, namespace: &str, name: &str) -> Result<Vec<Job>, Error> {
    let selector = format!("{}={}", crate::apply::CLUSTER_SELECTOR_LABEL, name);
    let _ = namespace;
    Ok(api
        .list(&ListParams::default().labels(&selector))
        .await
        .map_err(Error::from_kube_error)?
        .items)
}

/// Name of the cluster-owned restore job: `<cluster>-restore`.
#[must_use]
pub fn restore_job_name(cluster_name: &str) -> String {
    crate::apply::cluster_child_name(cluster_name, "restore")
}

/// Env contract for a restore run.
fn restore_env(cluster: &GraphCluster, verify: bool) -> Vec<EnvVar> {
    let restore = cluster.spec.restore_from.as_ref();
    let mut env = backup_env("restore", verify);
    if let Some(restore) = restore {
        env.push(EnvVar {
            name: constants::HYDRADB_BACKUP_SOURCE_BUCKET.to_owned(),
            value: Some(restore.store.bucket.clone()),
            ..EnvVar::default()
        });
        env.push(EnvVar {
            name: constants::HYDRADB_BACKUP_TARGET_BUCKET.to_owned(),
            value: Some(resolved_bucket(cluster)),
            ..EnvVar::default()
        });
        env.push(EnvVar {
            name: constants::HYDRADB_BACKUP_SOURCE_PREFIX.to_owned(),
            value: Some(restore.prefix.clone().unwrap_or_default()),
            ..EnvVar::default()
        });
        env.push(EnvVar {
            name: constants::HYDRADB_BACKUP_TARGET_PREFIX.to_owned(),
            value: Some(source_prefix(cluster)),
            ..EnvVar::default()
        });
    }
    env
}

/// Desired one-shot `Job` that restores a cluster's data before it serves.
///
/// The copy direction is inverted relative to a backup: the configured
/// `restoreFrom.store` is the source and the cluster's own store is the target.
///
/// # Errors
/// Returns [`Error::MissingField`] when `spec.restoreFrom` is unset or the
/// cluster has no name/uid.
pub fn desired_restore_job(cluster: &GraphCluster) -> Result<Job, Error> {
    let restore = cluster
        .spec
        .restore_from
        .as_ref()
        .ok_or_else(|| Error::MissingField("spec.restoreFrom is unset".to_owned()))?;
    let cluster_name = cluster.name_any();
    let env = restore_env(cluster, restore.verify);
    let secret_name = restore
        .store
        .credentials_secret_ref
        .as_ref()
        .map(|reference| reference.name.clone());
    let (volumes, volume_mounts) = match secret_volume(secret_name.as_deref()) {
        Some((volumes, mounts)) => (Some(volumes), Some(mounts)),
        None => (None, None),
    };
    Ok(Job {
        metadata: crate::apply::child_object_meta_for(
            &cluster_name,
            "restore",
            owner_ref_for(cluster)?,
        ),
        spec: Some(JobSpec {
            template: PodTemplateSpec {
                metadata: Some(ObjectMeta {
                    labels: Some(crate::apply::standard_labels(&cluster_name, "restore")),
                    ..ObjectMeta::default()
                }),
                spec: Some(PodSpec {
                    restart_policy: Some("Never".to_owned()),
                    containers: vec![Container {
                        name: "restore".to_owned(),
                        image: Some(image_ref(&restore.image)),
                        image_pull_policy: restore.image.pull_policy.as_ref().map(|policy| {
                            hydradb_operator_api::spec::PhaseLabel::label(policy).to_owned()
                        }),
                        env: Some(env),
                        env_from: secret_env_from(secret_name.as_deref()),
                        volume_mounts,
                        ..Container::default()
                    }],
                    volumes,
                    ..PodSpec::default()
                }),
            },
            backoff_limit: Some(0),
            ..JobSpec::default()
        }),
        status: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cluster() -> GraphCluster {
        serde_json::from_value(serde_json::json!({
            "apiVersion": "hydradb.kubexa.tech/v1alpha1",
            "kind": "GraphCluster",
            "metadata": {"name": "demo", "namespace": "graphs", "uid": "uid-1"},
            "spec": {
                "image": {"repository": "example.com/hydradb", "tag": "v1"},
                "graph": {
                    "namespace": "tenant",
                    "subnamespaces": ["team", "svc"],
                    "id": "g1",
                    "database": "default",
                    "cellId": "cell-0"
                },
                "objectStore": {"explicit": {"bucket": "data-bucket"}}
            }
        }))
        .expect("cluster decodes")
    }

    fn backup() -> GraphBackup {
        serde_json::from_value(serde_json::json!({
            "apiVersion": "hydradb.kubexa.tech/v1alpha1",
            "kind": "GraphBackup",
            "metadata": {"name": "nightly", "namespace": "graphs", "uid": "uid-2"},
            "spec": {
                "clusterRef": {"name": "demo"},
                "schedule": "0 3 * * *",
                "image": {"repository": "docker.io/rclone/rclone", "tag": "1.71"},
                "target": {"store": {"bucket": "backup-bucket"}}
            }
        }))
        .expect("backup decodes")
    }

    #[test]
    fn default_target_prefix_is_namespaced_by_cluster() {
        let prefix = resolve_target_prefix(&backup(), "graphs");
        assert_eq!(prefix, "backups/graphs/demo");
    }

    #[test]
    fn explicit_prefix_wins_over_the_default() {
        let mut backup = backup();
        backup.spec.target.prefix = Some("custom/path".to_owned());
        assert_eq!(resolve_target_prefix(&backup, "graphs"), "custom/path");
    }

    #[test]
    fn source_prefix_follows_the_operand_storage_layout() {
        assert_eq!(
            source_prefix(&cluster()),
            "namespaces/tenant/subnamespaces/team/subnamespaces/svc/graphs/g1"
        );
    }

    #[test]
    fn scheduled_backup_builds_a_cronjob_and_unscheduled_builds_a_job() {
        let cluster = cluster();
        let mut backup = backup();
        let cron = desired_cron_job(&backup, &cluster).expect("cronjob builds");
        assert_eq!(cron.spec.schedule, "0 3 * * *");
        assert_eq!(cron.spec.concurrency_policy.as_deref(), Some("Forbid"));

        backup.spec.schedule = None;
        let job = desired_job(&backup, &cluster).expect("job builds");
        assert_eq!(job.metadata.name.as_deref(), Some("nightly-backup"));
    }

    #[test]
    fn job_carries_the_whole_backup_contract() {
        let job = desired_job(&backup(), &cluster()).expect("job builds");
        let pod = job.spec.expect("spec").template.spec.expect("pod spec");
        let env = pod.containers[0].env.clone().unwrap_or_default();
        let get = |name: &str| {
            env.iter()
                .find(|var| var.name == name)
                .and_then(|var| var.value.clone())
                .unwrap_or_default()
        };
        assert_eq!(get(constants::HYDRADB_BACKUP_DIRECTION), "backup");
        assert_eq!(get(constants::HYDRADB_BACKUP_SOURCE_BUCKET), "data-bucket");
        assert_eq!(
            get(constants::HYDRADB_BACKUP_TARGET_BUCKET),
            "backup-bucket"
        );
        assert_eq!(
            get(constants::HYDRADB_BACKUP_TARGET_PREFIX),
            "backups/graphs/demo"
        );
        assert_eq!(get(constants::HYDRADB_BACKUP_VERIFY), "true");
        assert_eq!(pod.restart_policy.as_deref(), Some("Never"));
    }

    #[test]
    fn image_ref_prefers_the_digest_over_the_tag() {
        use hydradb_operator_api::spec::Image;
        let tagged = Image {
            repository: "r.example/x".to_owned(),
            tag: Some("v1".to_owned()),
            digest: None,
            pull_policy: None,
            pull_secrets: Vec::new(),
        };
        assert_eq!(image_ref(&tagged), "r.example/x:v1");
        let pinned = Image {
            digest: Some("sha256:abc".to_owned()),
            ..tagged
        };
        assert_eq!(image_ref(&pinned), "r.example/x@sha256:abc");
    }

    #[test]
    fn two_builds_of_the_same_backup_are_identical() {
        let cluster = cluster();
        let backup = backup();
        let first =
            serde_json::to_string(&desired_job(&backup, &cluster).expect("job")).expect("json");
        let second =
            serde_json::to_string(&desired_job(&backup, &cluster).expect("job")).expect("json");
        assert_eq!(first, second);
    }
}
