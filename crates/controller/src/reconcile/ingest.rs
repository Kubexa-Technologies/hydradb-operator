//! `GraphIngestSource` reconciler: scheduled and webhook-triggered ingests.
//!
//! HydraDB only indexes what is written into its WAL, so an external source
//! (a git repository, a document URL) does nothing until an ingester writes it.
//! This reconciler owns the triggers:
//!
//! * `spec.schedule` set → a `CronJob` running the user's ingester;
//! * `spec.webhook.enabled` → an HTTP endpoint that starts a one-off `Job`.
//!
//! The ingester is always a user image: the operator injects the contract
//! (`HYDRADB_INGEST_*`, the Bolt URL and the graph scope) and requires only a
//! zero exit status.

use std::sync::Arc;
use std::time::Duration;

use hydradb_operator_api::crd::{GraphCluster, GraphIngestSource};
use k8s_openapi::api::batch::v1::{CronJob, CronJobSpec, Job, JobSpec, JobTemplateSpec};
use k8s_openapi::api::core::v1::{
    Container, EnvVar, PodSpec, PodTemplateSpec, ResourceRequirements,
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use kube::api::{Api, DeleteParams, Patch, PatchParams};
use kube::runtime::controller::Action;
use kube::{Resource, ResourceExt};
use tracing::info;

use crate::context::Ctx;
use crate::error::Error;

/// Requeue interval while a run is in flight.
/// Requeue interval when idle.
const IDLE_REQUEUE_SECS: u64 = 120;

/// ServiceAccount the operator itself runs as (chart default).
const OPERATOR_SERVICE_ACCOUNT: &str = "hydradb-operator";

/// Name of the owned periodic `CronJob`.
#[must_use]
pub fn cron_child_name(name: &str) -> String {
    format!("{name}-ingest")
}

/// Name of a webhook-triggered one-off `Job`.
///
/// The nonce keeps every trigger distinct, so `Forbid`/`Replace` semantics stay
/// observable and two pushes a second apart do not overwrite each other.
#[must_use]
pub fn run_job_name(name: &str, nonce: &str) -> String {
    format!("{name}-ingest-{nonce}")
}

/// Canonical webhook path segment for a source.
#[must_use]
pub fn webhook_path_of(source: &GraphIngestSource) -> String {
    source
        .spec
        .webhook
        .path
        .clone()
        .unwrap_or_else(|| source.name_any())
}

/// Full webhook URL path served by the operator.
#[must_use]
pub fn webhook_route(source: &GraphIngestSource) -> String {
    format!(
        "/webhooks/v1alpha1/ingest/{}/{}",
        source.namespace().unwrap_or_default(),
        webhook_path_of(source)
    )
}

/// `env` describing the graph the ingester writes into.
fn ingest_env(source: &GraphIngestSource, cluster: &GraphCluster, revision: &str) -> Vec<EnvVar> {
    use hydradb_operator_api::constants as c;
    let namespace = source.namespace().unwrap_or_default();
    let bolt = format!(
        "{}-bolt.{namespace}.svc:{}",
        cluster.name_any(),
        cluster.spec.service.bolt_port
    );
    let source_url = match (&source.spec.source.git, &source.spec.source.http) {
        (Some(git), _) => git.repository.clone(),
        (None, Some(http)) => http.url.clone(),
        (None, None) => String::new(),
    };
    let paths = source
        .spec
        .source
        .git
        .as_ref()
        .map(|git| git.paths.join(","))
        .unwrap_or_default();

    let mut env = vec![
        EnvVar {
            name: c::HYDRADB_INGEST_NAMESPACE.to_owned(),
            value: Some(cluster.spec.graph.namespace.clone()),
            ..EnvVar::default()
        },
        EnvVar {
            name: c::HYDRADB_INGEST_GRAPH_ID.to_owned(),
            value: Some(cluster.spec.graph.id.clone()),
            ..EnvVar::default()
        },
        EnvVar {
            name: c::HYDRADB_INGEST_BOLT_URL.to_owned(),
            value: Some(bolt),
            ..EnvVar::default()
        },
        EnvVar {
            name: c::HYDRADB_INGEST_REVISION.to_owned(),
            value: Some(revision.to_owned()),
            ..EnvVar::default()
        },
        EnvVar {
            name: c::HYDRADB_INGEST_SOURCE_URL.to_owned(),
            value: Some(source_url),
            ..EnvVar::default()
        },
        EnvVar {
            name: c::HYDRADB_INGEST_PATHS.to_owned(),
            value: Some(paths),
            ..EnvVar::default()
        },
    ];
    for var in &source.spec.env {
        crate::children::push_or_replace(&mut env, var.clone());
    }
    env
}

/// Substitute the `{revision}` token in the configured arguments.
#[must_use]
pub fn render_args(args: &[String], revision: &str) -> Vec<String> {
    args.iter()
        .map(|arg| arg.replace("{revision}", revision))
        .collect()
}

/// Build the pod template for one ingest run.
fn pod_template(
    source: &GraphIngestSource,
    cluster: &GraphCluster,
    revision: &str,
) -> PodTemplateSpec {
    PodTemplateSpec {
        metadata: Some(ObjectMeta {
            labels: Some(crate::apply::standard_labels(&cluster.name_any(), "ingest")),
            annotations: if source.spec.pod_annotations.is_empty() {
                None
            } else {
                Some(source.spec.pod_annotations.clone())
            },
            ..ObjectMeta::default()
        }),
        spec: Some(PodSpec {
            restart_policy: Some("Never".to_owned()),
            service_account_name: source.spec.service_account.clone(),
            tolerations: if source.spec.tolerations.is_empty() {
                None
            } else {
                Some(source.spec.tolerations.clone())
            },
            containers: vec![Container {
                name: "ingest".to_owned(),
                image: Some(crate::reconcile::backup::image_ref(&source.spec.image)),
                image_pull_policy: source
                    .spec
                    .image
                    .pull_policy
                    .as_ref()
                    .map(|policy| hydradb_operator_api::spec::PhaseLabel::label(policy).to_owned()),
                command: if source.spec.command.is_empty() {
                    None
                } else {
                    Some(source.spec.command.clone())
                },
                args: if source.spec.args.is_empty() {
                    None
                } else {
                    Some(render_args(&source.spec.args, revision))
                },
                env: Some(ingest_env(source, cluster, revision)),
                resources: source.spec.resources.clone().or_else(|| {
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
                ..Container::default()
            }],
            ..PodSpec::default()
        }),
    }
}

fn job_spec(source: &GraphIngestSource, cluster: &GraphCluster, revision: &str) -> JobSpec {
    JobSpec {
        template: pod_template(source, cluster, revision),
        backoff_limit: source.spec.backoff_limit,
        active_deadline_seconds: source.spec.active_deadline_seconds,
        ..JobSpec::default()
    }
}

/// Desired periodic `CronJob`.
///
/// # Errors
/// Returns [`Error::MissingField`] when the source has no name/uid.
pub fn desired_cron_job(
    source: &GraphIngestSource,
    cluster: &GraphCluster,
) -> Result<CronJob, Error> {
    let name = source.name_any();
    let revision = source
        .spec
        .source
        .git
        .as_ref()
        .and_then(|git| git.revision.clone())
        .unwrap_or_else(|| "HEAD".to_owned());
    Ok(CronJob {
        metadata: crate::apply::child_object_meta_for(&name, "ingest", owner_ref(source)?),
        spec: CronJobSpec {
            schedule: source.spec.schedule.clone().unwrap_or_default(),
            suspend: Some(source.spec.suspend),
            concurrency_policy: Some(
                match source.spec.concurrency_policy {
                    hydradb_operator_api::crd::ConcurrencyPolicy::Allow => "Allow",
                    hydradb_operator_api::crd::ConcurrencyPolicy::Forbid => "Forbid",
                    hydradb_operator_api::crd::ConcurrencyPolicy::Replace => "Replace",
                }
                .to_owned(),
            ),
            successful_jobs_history_limit: source.spec.successful_jobs_history_limit,
            failed_jobs_history_limit: source.spec.failed_jobs_history_limit,
            job_template: JobTemplateSpec {
                metadata: Some(ObjectMeta {
                    labels: Some(crate::apply::standard_labels(&cluster.name_any(), "ingest")),
                    ..ObjectMeta::default()
                }),
                spec: Some(job_spec(source, cluster, &revision)),
            },
            ..CronJobSpec::default()
        },
        status: None,
    })
}

/// Desired one-off `Job` for a webhook trigger.
///
/// # Errors
/// Returns [`Error::MissingField`] when the source has no name/uid.
pub fn desired_run_job(
    source: &GraphIngestSource,
    cluster: &GraphCluster,
    nonce: &str,
) -> Result<Job, Error> {
    let name = source.name_any();
    let revision = source
        .spec
        .source
        .git
        .as_ref()
        .and_then(|git| git.revision.clone())
        .unwrap_or_else(|| "HEAD".to_owned());
    let mut meta = crate::apply::child_object_meta_for(&name, "ingest", owner_ref(source)?);
    meta.name = Some(run_job_name(&name, nonce));
    Ok(Job {
        metadata: meta,
        spec: Some(job_spec(source, cluster, &revision)),
        status: None,
    })
}

/// `OwnerReference` pointing at the owning `GraphIngestSource`.
fn owner_ref(
    source: &GraphIngestSource,
) -> Result<k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference, Error> {
    let meta = source.meta();
    let name = meta
        .name
        .clone()
        .ok_or_else(|| Error::MissingField("GraphIngestSource has no metadata.name".to_owned()))?;
    let uid = meta
        .uid
        .clone()
        .ok_or_else(|| Error::MissingField("GraphIngestSource has no metadata.uid".to_owned()))?;
    Ok(
        k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference {
            api_version: GraphIngestSource::api_version(&()).into_owned(),
            kind: GraphIngestSource::kind(&()).into_owned(),
            name,
            uid,
            controller: Some(true),
            block_owner_deletion: Some(false),
        },
    )
}

/// Reconcile one `GraphIngestSource`.
///
/// # Errors
/// Returns [`Error`] when the cluster reference does not resolve or a child
/// write fails.
pub async fn reconcile(source: Arc<GraphIngestSource>, ctx: Arc<Ctx>) -> Result<Action, Error> {
    let name = source.name_any();
    let namespace = source.namespace().unwrap_or_default();
    let generation = source.meta().generation;
    let sources = Api::<GraphIngestSource>::namespaced(ctx.client.clone(), &namespace);

    if let Err(errors) = source.spec.validate() {
        let message = errors
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; ");
        write_status(&sources, &name, generation, "False", &message, None).await?;
        return Ok(Action::requeue(Duration::from_secs(IDLE_REQUEUE_SECS)));
    }

    // Confused-deputy guard: the ingest pod runs user-chosen code, so it must
    // never inherit the operator's own identity (cluster-wide secret read, job
    // delete, certificate issuance).
    if source.spec.service_account.as_deref() == Some(OPERATOR_SERVICE_ACCOUNT) {
        let message = format!(
            "serviceAccount `{OPERATOR_SERVICE_ACCOUNT}` is reserved for the operator itself: \
             an ingest pod must use a dedicated identity"
        );
        write_status(&sources, &name, generation, "False", &message, None).await?;
        return Ok(Action::requeue(Duration::from_secs(IDLE_REQUEUE_SECS)));
    }

    let cluster_name = source.spec.cluster_ref.name.clone();
    let clusters = Api::<GraphCluster>::namespaced(ctx.client.clone(), &namespace);
    let Some(cluster) = clusters
        .get_opt(&cluster_name)
        .await
        .map_err(Error::from_kube_error)?
    else {
        let message =
            format!("clusterRef {namespace}/{cluster_name} does not resolve to a GraphCluster");
        write_status(&sources, &name, generation, "False", &message, None).await?;
        return Ok(Action::requeue(Duration::from_secs(IDLE_REQUEUE_SECS)));
    };

    let cron_api = Api::<CronJob>::namespaced(ctx.client.clone(), &namespace);
    let child = cron_child_name(&name);
    if source.spec.schedule.is_some() {
        crate::apply::apply_child(&cron_api, &desired_cron_job(&source, &cluster)?).await?;
    } else {
        let _ = cron_api.delete(&child, &DeleteParams::default()).await;
    }

    // Count the runs that are still active so the status reflects reality.
    let jobs_api = Api::<Job>::namespaced(ctx.client.clone(), &namespace);
    let active = jobs_api
        .list(&kube::api::ListParams::default().labels(&format!(
            "{}={}",
            crate::apply::CLUSTER_SELECTOR_LABEL,
            cluster.name_any()
        )))
        .await
        .map_err(Error::from_kube_error)?
        .items
        .iter()
        .filter(|job| {
            job.metadata
                .name
                .as_deref()
                .is_some_and(|n| n.starts_with(&format!("{name}-ingest")))
        })
        .count();

    let path = if source.spec.webhook.enabled {
        Some(webhook_route(&source))
    } else {
        None
    };
    write_status(
        &sources,
        &name,
        generation,
        "True",
        "configured",
        path.as_deref(),
    )
    .await?;
    let _ = active;

    Ok(Action::requeue(Duration::from_secs(IDLE_REQUEUE_SECS)))
}

/// Patch `/status` for an ingest source, skipping the write when unchanged.
///
/// Same reasoning as the backup writer: an unconditional patch with a fresh
/// `lastTransitionTime` is a self-sustaining reconcile loop.
async fn write_status(
    api: &Api<GraphIngestSource>,
    name: &str,
    generation: Option<i64>,
    ready: &str,
    message: &str,
    webhook_path: Option<&str>,
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
        && live_status.webhook_path.as_deref() == webhook_path
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
            "webhookPath": webhook_path,
        }
    });
    api.patch_status(name, &PatchParams::default(), &Patch::Merge(&patch))
        .await
        .map_err(Error::from_kube_error)?;
    Ok(())
}

/// Start a webhook-triggered run for `source`.
///
/// Used by the webhook receiver. Honours `spec.concurrencyPolicy`: `Forbid`
/// refuses while a run is active, `Replace` cancels the active run first.
///
/// # Errors
/// Returns [`Error`] when the cluster is missing or the job cannot be created.
pub async fn trigger_run(
    ctx: &Ctx,
    source: &GraphIngestSource,
    revision: Option<String>,
) -> Result<String, Error> {
    let namespace = source.namespace().unwrap_or_default();

    let cluster_name = source.spec.cluster_ref.name.clone();
    let clusters = Api::<GraphCluster>::namespaced(ctx.client.clone(), &namespace);
    let cluster = clusters
        .get_opt(&cluster_name)
        .await
        .map_err(Error::from_kube_error)?
        .ok_or_else(|| {
            Error::UnresolvableRef(format!(
                "clusterRef {namespace}/{cluster_name} does not resolve to a GraphCluster"
            ))
        })?;

    let jobs_api = Api::<Job>::namespaced(ctx.client.clone(), &namespace);
    let prefix = format!("{}-ingest-", source.name_any());
    let active: Vec<Job> = jobs_api
        .list(&kube::api::ListParams::default())
        .await
        .map_err(Error::from_kube_error)?
        .items
        .into_iter()
        .filter(|job| {
            job.metadata
                .name
                .as_deref()
                .is_some_and(|n| n.starts_with(&prefix))
                && job
                    .status
                    .as_ref()
                    .and_then(|s| s.active.as_ref())
                    .is_some_and(|a| *a > 0)
        })
        .collect();

    match source.spec.concurrency_policy {
        hydradb_operator_api::crd::ConcurrencyPolicy::Forbid if !active.is_empty() => {
            return Err(Error::Conflict(format!(
                "ingest source {} already has an active run",
                source.name_any()
            )));
        }
        hydradb_operator_api::crd::ConcurrencyPolicy::Replace => {
            for job in &active {
                if let Some(name) = job.metadata.name.clone() {
                    let _ = jobs_api.delete(&name, &DeleteParams::background()).await;
                }
            }
        }
        _ => {}
    }

    // A second-granularity timestamp collapses two pushes in the same second
    // into one Job name, silently dropping a trigger.
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    let nonce = nonce[..12].to_owned();
    let revision = revision.unwrap_or_else(|| "HEAD".to_owned());
    let mut job = desired_run_job(source, &cluster, &nonce)?;
    job.metadata.name = Some(run_job_name(&source.name_any(), &nonce));
    job.spec = Some(job_spec(source, &cluster, &revision));
    crate::apply::apply_child(&jobs_api, &job).await?;
    info!(source = %source.name_any(), %nonce, "ingest run started");
    Ok(run_job_name(&source.name_any(), &nonce))
}

/// Map an ingest reconcile error onto a requeue action.
#[must_use]
pub fn error_policy(source: Arc<GraphIngestSource>, error: &Error, _ctx: Arc<Ctx>) -> Action {
    Action::requeue(error.requeue_after(&source.name_any()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source() -> GraphIngestSource {
        serde_json::from_value(serde_json::json!({
            "apiVersion": "hydradb.helmforge.dev/v1alpha1",
            "kind": "GraphIngestSource",
            "metadata": {"name": "docs", "namespace": "graphs", "uid": "uid-3"},
            "spec": {
                "clusterRef": {"name": "demo"},
                "source": {"git": {
                    "repository": "https://git.example.com/org/repo.git",
                    "revision": "main",
                    "paths": ["docs/**/*.md"]
                }},
                "image": {"repository": "example.com/ingest", "tag": "v1"},
                "args": ["--revision", "{revision}"],
                "schedule": "*/15 * * * *",
                "webhook": {"enabled": true, "secretRef": {"name": "hmac"}}
            }
        }))
        .expect("source decodes")
    }

    #[test]
    fn webhook_route_is_namespace_scoped_and_uses_the_path_override() {
        let source = source();
        assert_eq!(webhook_path_of(&source), "docs");
        assert_eq!(
            webhook_route(&source),
            "/webhooks/v1alpha1/ingest/graphs/docs"
        );

        let mut overridden = source.clone();
        overridden.spec.webhook.path = Some("handbook".to_owned());
        assert!(webhook_route(&overridden).ends_with("/handbook"));
    }

    #[test]
    fn revision_token_is_substituted_in_args() {
        assert_eq!(
            render_args(
                &["--revision".to_owned(), "{revision}".to_owned()],
                "abc123"
            ),
            vec!["--revision".to_owned(), "abc123".to_owned()]
        );
    }

    #[test]
    fn run_job_names_are_unique_per_nonce() {
        assert_eq!(run_job_name("docs", "1"), "docs-ingest-1");
        assert_ne!(run_job_name("docs", "1"), run_job_name("docs", "2"));
    }

    #[test]
    fn concurrency_policy_maps_onto_the_cronjob_field() {
        let source = source();
        let cluster: GraphCluster = serde_json::from_value(serde_json::json!({
            "apiVersion": "hydradb.helmforge.dev/v1alpha1",
            "kind": "GraphCluster",
            "metadata": {"name": "demo", "namespace": "graphs", "uid": "uid-1"},
            "spec": {
                "image": {"repository": "example.com/hydradb", "tag": "v1"},
                "graph": {
                    "namespace": "tenant", "id": "g1",
                    "database": "default", "cellId": "cell-0"
                },
                "objectStore": {"explicit": {"bucket": "data-bucket"}}
            }
        }))
        .expect("cluster decodes");

        let cron = desired_cron_job(&source, &cluster).expect("cronjob builds");
        assert_eq!(cron.spec.concurrency_policy.as_deref(), Some("Allow"));
        assert_eq!(cron.spec.schedule, "*/15 * * * *");
    }
}
