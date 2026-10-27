//! `GraphIngestSource` custom resource: turn an external source into graph writes.
//!
//! HydraDB's indexer only indexes the graph's *own* WAL, so nothing is indexed
//! until something writes into the graph. This resource owns that "something":
//! it declares where the data lives and which ingester turns it into Cypher
//! writes, then the operator provides the two triggers that matter —
//!
//! * a `CronJob` when `spec.schedule` is set (periodic reconciliation), and
//! * a webhook receiver that starts a one-off `Job` the moment an upstream
//!   pushes (see the operator's `/webhooks/v1alpha1/ingest/...` endpoint).
//!
//! The ingester itself is user-chosen (`spec.image` + `command`/`args`): the
//! operator injects the source location, the resolved Bolt endpoint and the
//! graph scope, and requires only that the process exit 0 on success. That keeps
//! the operator free of a proprietary ingest format while still owning
//! scheduling, concurrency, retries and status.

use std::collections::BTreeMap;

use k8s_openapi::api::core::v1::{EnvVar, LocalObjectReference, ResourceRequirements, Toleration};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::spec::Image;

/// A git-hosted source.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(deny_unknown_fields)]
pub struct GitSource {
    /// Clone URL (`https://` or `ssh://`).
    pub repository: String,
    /// Branch, tag or commit to read. Absent means the remote default branch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    /// Path globs to read from the checkout, relative to the repository root.
    /// Absent means every file the ingester considers in scope.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
    /// Secret holding git credentials (keys `username`/`password` or `token`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credentials_secret_ref: Option<LocalObjectReference>,
}

/// An HTTP-hosted source fetched as a single document.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(deny_unknown_fields)]
pub struct HttpSource {
    /// Absolute `http(s)://` URL of the document.
    pub url: String,
    /// Secret holding a bearer token, if the endpoint needs one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credentials_secret_ref: Option<LocalObjectReference>,
}

/// Exactly one source: `git` or `http`.
#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(deny_unknown_fields)]
pub struct IngestSourceSpec {
    /// Git repository source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git: Option<GitSource>,
    /// Single-document HTTP source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http: Option<HttpSource>,
}

/// push-driven trigger for this source.
#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(deny_unknown_fields)]
pub struct WebhookSpec {
    /// Accept webhook-triggered runs.
    #[serde(default)]
    pub enabled: bool,
    /// Secret holding the HMAC-SHA256 key used to verify the request body.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret_ref: Option<LocalObjectReference>,
    /// Key inside `secret_ref` holding the shared secret.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret_key: Option<String>,
    /// Path segment identifying this source.
    ///
    /// Defaults to the resource name. The receiver URL is
    /// `/webhooks/v1alpha1/ingest/<namespace>/<path>`; changing it invalidates
    /// the URL configured upstream.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// What the operator does when a run is requested while one is still active.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "PascalCase")]
pub enum ConcurrencyPolicy {
    /// Start the new run alongside the active one (default).
    #[default]
    Allow,
    /// Skip the new run while one is active.
    Forbid,
    /// Cancel the active run and start the new one.
    Replace,
}

/// Desired state of an ingest source.
#[derive(CustomResource, Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(deny_unknown_fields)]
#[kube(
    group = "hydradb.kubexa.tech",
    version = "v1alpha1",
    kind = "GraphIngestSource",
    namespaced,
    status = "GraphIngestSourceStatus",
    shortname = "hgis",
    category = "hydradb",
    printcolumn = "{\"name\":\"Schedule\",\"type\":\"string\",\"jsonPath\":\".spec.schedule\"}",
    printcolumn = "{\"name\":\"Webhook\",\"type\":\"boolean\",\"jsonPath\":\".spec.webhook.enabled\"}",
    printcolumn = "{\"name\":\"Runs\",\"type\":\"integer\",\"jsonPath\":\".status.runsStarted\"}",
    printcolumn = "{\"name\":\"Age\",\"type\":\"date\",\"jsonPath\":\".metadata.creationTimestamp\"}"
)]
pub struct GraphIngestSourceSpec {
    /// Reference to the target `GraphCluster` (`.name` only).
    pub cluster_ref: LocalObjectReference,
    /// Where the data comes from (exactly one of `git` / `http`).
    pub source: IngestSourceSpec,
    /// Image implementing the ingest contract.
    pub image: Image,
    /// Entrypoint override; absent means the image's own entrypoint.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub command: Vec<String>,
    /// Arguments for the entrypoint; a `{revision}` token is substituted with
    /// the revision resolved for the run.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// Extra environment variables for the ingest pod.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<EnvVar>,
    /// Periodic reconciliation schedule (five-field cron). Absent means the
    /// source is only ever triggered by its webhook.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<String>,
    /// Suspend scheduled (not webhook) runs.
    #[serde(default)]
    pub suspend: bool,
    /// Behaviour when a run is requested while one is active.
    #[serde(default)]
    pub concurrency_policy: ConcurrencyPolicy,
    /// `successfulJobsHistoryLimit` of the owned `CronJob` (default 3).
    #[serde(default = "default_history_limit")]
    pub successful_jobs_history_limit: Option<i32>,
    /// `failedJobsHistoryLimit` of the owned `CronJob` (default 1).
    #[serde(default = "default_failed_history_limit")]
    pub failed_jobs_history_limit: Option<i32>,
    /// Run deadline in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1))]
    pub active_deadline_seconds: Option<i64>,
    /// Job retry budget (default 2).
    #[serde(default = "default_backoff_limit")]
    #[schemars(range(min = 0))]
    pub backoff_limit: Option<i32>,
    /// Compute resources for the ingest pod.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resources: Option<ResourceRequirements>,
    /// Service account for the ingest pod.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_account: Option<String>,
    /// Pod tolerations.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tolerations: Vec<Toleration>,
    /// Annotations added to ingest pods.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub pod_annotations: BTreeMap<String, String>,
    /// Labels added to ingest pods.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub pod_labels: BTreeMap<String, String>,
    /// Webhook trigger configuration.
    #[serde(default)]
    pub webhook: WebhookSpec,
}

fn default_history_limit() -> Option<i32> {
    Some(3)
}

fn default_failed_history_limit() -> Option<i32> {
    Some(1)
}

fn default_backoff_limit() -> Option<i32> {
    Some(2)
}

/// Observed state of an ingest source.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(deny_unknown_fields)]
pub struct GraphIngestSourceStatus {
    /// Standard Kubernetes conditions (`Ready`, `Progressing`, `Failed`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<Condition>,
    /// Last reconciled generation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_generation: Option<i64>,
    /// Name of the most recently started run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run_name: Option<String>,
    /// `RRFC3339` time of the last successful run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_successful_time: Option<String>,
    /// Source revision used by the last run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_revision: Option<String>,
    /// Runs started since the resource was created.
    #[serde(default)]
    pub runs_started: i32,
    /// Runs that ended in failure.
    #[serde(default)]
    pub runs_failed: i32,
    /// Runs currently active.
    #[serde(default)]
    pub active_runs: i32,
    /// Canonical webhook URL, when enabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webhook_path: Option<String>,
}
