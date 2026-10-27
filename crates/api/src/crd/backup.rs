//! `GraphBackup` custom resource: durable copies of a cluster's object-store data.
//!
//! A backup is a physical copy of the graph's object-store prefix (`WAL`, `SST`s,
//! manifests and published index artifacts) into a second bucket or prefix. It is
//! taken by a Job, never in-process: the operator only owns scheduling, status and
//! the credential/env contract, so no credential ever enters the controller.
//!
//! One kind covers both shapes: with `spec.schedule` the operator maintains a
//! `CronJob`, without it a single `Job` is created and never recreated.

use std::collections::BTreeMap;

use k8s_openapi::api::core::v1::{EnvVar, LocalObjectReference, ResourceRequirements};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::spec::{Image, S3};

/// Destination of a backup copy.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(deny_unknown_fields)]
pub struct BackupTarget {
    /// Object-store location the copy is written to.
    ///
    /// Must differ from the cluster's own store: a backup into the same bucket
    /// and prefix as the source is rejected during validation, because it would
    /// both double the write amplification and provide no isolation.
    pub store: S3,
    /// Prefix inside the target bucket.
    ///
    /// Absent means `backups/<namespace>/<cluster-name>` computed by the
    /// controller, so two clusters in one namespace never collide.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
}

/// Desired state of a graph backup.
#[derive(CustomResource, Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(deny_unknown_fields)]
#[kube(
    group = "hydradb.helmforge.dev",
    version = "v1alpha1",
    kind = "GraphBackup",
    namespaced,
    status = "GraphBackupStatus",
    shortname = "hgb",
    category = "hydradb",
    printcolumn = "{\"name\":\"Schedule\",\"type\":\"string\",\"jsonPath\":\".spec.schedule\"}",
    printcolumn = "{\"name\":\"LastSuccess\",\"type\":\"string\",\"jsonPath\":\".status.lastSuccessfulTime\"}",
    printcolumn = "{\"name\":\"Ready\",\"type\":\"string\",\"jsonPath\":\".status.conditions[?(@.type==\\\"Ready\\\")].status\"}",
    printcolumn = "{\"name\":\"Age\",\"type\":\"date\",\"jsonPath\":\".metadata.creationTimestamp\"}"
)]
pub struct GraphBackupSpec {
    /// Reference to the owning `GraphCluster` (`.name` only).
    pub cluster_ref: LocalObjectReference,
    /// Cron schedule in standard five-field form.
    ///
    /// Absent means "take exactly one backup now": the controller creates a
    /// single `Job` and never replaces it, even after it fails (owner decides).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<String>,
    /// Suspend future scheduled runs; an in-flight run is left alone.
    #[serde(default)]
    pub suspend: bool,
    /// Image running the copy.
    ///
    /// Required: the operand image cannot copy objects, so the operator never
    /// guesses a default. Any image implementing the documented backup
    /// contract works; `rclone/rclone` is the reference.
    pub image: Image,
    /// Where the copy is written.
    pub target: BackupTarget,
    /// Number of successful backup copies kept in the target prefix.
    ///
    /// The operator prunes the oldest copies after a successful run; `1` keeps
    /// only the newest. Must be at least 1.
    #[serde(default = "default_retention")]
    #[schemars(range(min = 1))]
    pub retention: i32,
    /// `successfulJobsHistoryLimit` of the owned `CronJob` (default 3).
    #[serde(default = "default_history_limit")]
    pub successful_jobs_history_limit: Option<i32>,
    /// `failedJobsHistoryLimit` of the owned `CronJob` (default 1).
    #[serde(default = "default_failed_history_limit")]
    pub failed_jobs_history_limit: Option<i32>,
    /// Verify the copy before reporting success.
    #[serde(default = "default_verify")]
    pub verify: bool,
    /// Job deadline in seconds; absent means the Job's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1))]
    pub active_deadline_seconds: Option<i64>,
    /// Job retry budget (default 0: a backup either works or needs attention).
    #[serde(default = "default_backoff_limit")]
    #[schemars(range(min = 0))]
    pub backoff_limit: Option<i32>,
    /// Compute resources for the job pod.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resources: Option<ResourceRequirements>,
    /// Extra environment variables for the job pod.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_env: Vec<EnvVar>,
    /// Annotations added to job pods.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub pod_annotations: BTreeMap<String, String>,
    /// Labels added to job pods.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub pod_labels: BTreeMap<String, String>,
}

fn default_retention() -> i32 {
    3
}

fn default_history_limit() -> Option<i32> {
    Some(3)
}

fn default_failed_history_limit() -> Option<i32> {
    Some(1)
}

fn default_verify() -> bool {
    true
}

fn default_backoff_limit() -> Option<i32> {
    Some(0)
}

/// Observed state of a graph backup.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(deny_unknown_fields)]
pub struct GraphBackupStatus {
    /// Standard Kubernetes conditions (`Ready`, `Progressing`, `Failed`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<Condition>,
    /// Last reconciled generation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_generation: Option<i64>,
    /// <PII type="CASE_ID" id="423"/>RFC3339 time the schedule last fired.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_schedule_time: Option<String>,
    /// RFC3339 time of the last successful copy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_successful_time: Option<String>,
    /// Name of the job that performed the last run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run_name: Option<String>,
    /// Successful runs observed.
    #[serde(default)]
    pub successful_backups: i32,
    /// Failed runs observed.
    #[serde(default)]
    pub failed_backups: i32,
    /// Resolved destination (`bucket/prefix`) of the last run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_target: Option<String>,
}
