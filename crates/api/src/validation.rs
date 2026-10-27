//! Cross-field validation for HydraDB specs.
//!
//! The CRD schema enforces per-field shape; the rules here span fields and
//! cannot be expressed in a schema, so the controller calls `validate()`
//! during reconcile. Every violation is collected into the returned `Vec`
//! (never stop-at-first) so one reconcile reports every problem at once.

use k8s_openapi::api::core::v1::EnvVar;

use crate::constants;
use crate::crd::{GraphBackupSpec, GraphClusterSpec, GraphIngestSourceSpec, GraphNodePoolSpec};
use crate::spec::{IssuerRef, ObjectStore, S3};

/// A single cross-field validation failure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidationError {
    /// Dotted path of the offending field (e.g. `object_store`).
    pub field: String,
    /// Why the value is rejected.
    pub message: String,
}

impl ValidationError {
    /// Build an error for `field` with `message`.
    #[must_use]
    pub fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid value for {}: {}", self.field, self.message)
    }
}

impl std::error::Error for ValidationError {}

/// Result of validating a HydraDB spec: `Ok` when clean, every violation when not.
pub type ValidationResult<T = ()> = Result<T, Vec<ValidationError>>;

/// Maximum number of nested subnamespaces below the root tenant namespace.
///
/// Mirrors the operand's `MAX_NAMESPACE_DEPTH == 8` counted *including* the root
/// namespace, so at most seven comp nodes hang below it.
pub const MAX_SUBNAMESPACE_DEPTH: usize = 7;

/// Env vars the operator owns. A user-supplied value for any of these is
/// rejected instead of silently losing to (or silently winning over) the value
/// the operator computes from the spec.
pub const OPERATOR_OWNED_ENV: &[&str] = &[
    constants::GRAPH_NAMESPACE,
    constants::GRAPH_ID,
    constants::GRAPH_DATABASE,
    constants::GRAPH_CELL_ID,
    constants::GRAPH_CELLS,
    constants::GRAPH_BOLT_NODE_ADDRESSES,
    constants::GRAPH_SUBNAMESPACE_PATH,
    constants::GRAPH_NODE_ROLE,
    constants::GRAPH_NODE_ID,
    constants::AWS_BUCKET_NAME,
    constants::AWS_DEFAULT_REGION,
    constants::AWS_ENDPOINT,
    constants::AWS_ALLOW_HTTP,
    constants::CLOUD_PROVIDER,
    constants::RUST_LOG,
    constants::RUST_MIN_STACK,
    constants::GRAPH_READ_ROUTING,
    // Backup/restore contract: a user value could redirect a copy to another
    // bucket (exfiltration) or flip a restore into a backup.
    constants::HYDRADB_BACKUP_SOURCE_BUCKET,
    constants::HYDRADB_BACKUP_SOURCE_PREFIX,
    constants::HYDRADB_BACKUP_TARGET_BUCKET,
    constants::HYDRADB_BACKUP_TARGET_PREFIX,
    constants::HYDRADB_BACKUP_DIRECTION,
    constants::HYDRADB_BACKUP_VERIFY,
    // Ingest contract: a user value could repoint the Bolt URL or the revision.
    constants::HYDRADB_INGEST_NAMESPACE,
    constants::HYDRADB_INGEST_GRAPH_ID,
    constants::HYDRADB_INGEST_BOLT_URL,
    constants::HYDRADB_INGEST_REVISION,
    constants::HYDRADB_INGEST_SOURCE_URL,
    constants::HYDRADB_INGEST_PATHS,
    // Typed tuning surface: emitted from the ConfigMap, so a shadowing env var
    // would silently contradict `spec.tuning`.
    constants::GRAPH_MAX_BOLT_CONNECTIONS,
    constants::GRAPH_BOLT_IDLE_TIMEOUT_MS,
    constants::GRAPH_BOLT_MAX_CONNECTION_AGE_MS,
    constants::GRAPH_BOLT_AUTHENTICATION_TIMEOUT_MS,
    constants::GRAPH_GRACEFUL_SHUTDOWN_MS,
    constants::GRAPH_READER_MANIFEST_POLL_INTERVAL,
];

/// Whether `value` is a legal single namespace component.
///
/// The operand's `validate_component` accepts lowercase alphanumerics plus `-`
/// and `_`, and rejects empty input; dots are separators and must not appear.
#[must_use]
fn is_namespace_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 63
        && value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
        && !value.starts_with('-')
        && !value.ends_with('-')
}

/// Validate a five-field cron expression without pulling a cron parser in.
///
/// Kubernetes rejects a bad schedule at apply time anyway; this exists so the
/// CR reports a clear condition instead of a wedge of opaque child errors.
fn check_cron(field: &str, expr: &str, errors: &mut Vec<ValidationError>) {
    let fields: Vec<&str> = expr.split_whitespace().collect();
    if fields.len() != 5 {
        errors.push(ValidationError::new(
            field,
            format!(
                "must be a five-field cron expression; got {} fields",
                fields.len()
            ),
        ));
    }
}

/// Validate a cert-manager issuer reference.
fn check_issuer(field: &str, issuer: &IssuerRef, errors: &mut Vec<ValidationError>) {
    if issuer.name.trim().is_empty() {
        errors.push(ValidationError::new(
            format!("{field}.name"),
            "must be non-empty",
        ));
    }
    if issuer.kind != "Issuer" && issuer.kind != "ClusterIssuer" {
        errors.push(ValidationError::new(
            format!("{field}.kind"),
            format!("must be `Issuer` or `ClusterIssuer`; got `{}`", issuer.kind),
        ));
    }
    if issuer.group.trim().is_empty() {
        errors.push(ValidationError::new(
            format!("{field}.group"),
            "must be non-empty",
        ));
    }
}

/// Reject user env vars that collide with operator-owned keys.
fn check_env_shadowing(field: &str, env: &[EnvVar], errors: &mut Vec<ValidationError>) {
    for var in env {
        if OPERATOR_OWNED_ENV.contains(&var.name.as_str()) {
            errors.push(ValidationError::new(
                format!("{field}[{}]", var.name),
                "is owned by the operator and cannot be overridden",
            ));
        }
    }
}

/// Validate a non-empty repository/bucket style string.
fn check_non_empty(field: &str, value: &str, errors: &mut Vec<ValidationError>) {
    if value.trim().is_empty() {
        errors.push(ValidationError::new(field, "must be non-empty"));
    }
}

impl ObjectStore {
    /// Exactly one object-store source must be set.
    ///
    /// Zero sources and two sources are both errors; the failure names the
    /// field `object_store` and both sub-fields in the message.
    pub fn validate(&self) -> ValidationResult {
        match (&self.explicit, &self.object_bucket_claim) {
            (Some(_), None) | (None, Some(_)) => Ok(()),
            (None, None) => Err(vec![ValidationError::new(
                "object_store",
                "exactly one of `explicit` / `object_bucket_claim` must be set (got none)",
            )]),
            (Some(_), Some(_)) => Err(vec![ValidationError::new(
                "object_store",
                "exactly one of `explicit` / `object_bucket_claim` must be set (got both)",
            )]),
        }
    }
}

impl GraphClusterSpec {
    /// Cross-field rules the CRD schema cannot express.
    ///
    /// 1. `object_store` has exactly one source.
    /// 2. `runtime.data_cache_max_open_files >= runtime.max_open_scopes * 2`.
    /// 3. `graph.cell_id` is non-empty.
    /// 4. `tls.additional_dns_names` only when `tls.enabled`.
    /// 5. `graph.subnamespaces` holds at most [`MAX_SUBNAMESPACE_DEPTH`] legal
    ///    single components.
    /// 6. when `tls.enabled`, exactly one of `secret_name` / `issuer_ref`.
    /// 7. `tuning.storage.cachePartBytes` is a positive multiple of 1024.
    /// 8. `extraEnv` never shadows an operator-owned variable.
    /// 9. `restoreFrom.store` never points at the cluster's own bucket+path.
    pub fn validate(&self) -> ValidationResult {
        let mut errors: Vec<ValidationError> = Vec::new();
        if let Err(mut store_errors) = self.object_store.validate() {
            errors.append(&mut store_errors);
        }
        let required_files = u64::from(self.runtime.max_open_scopes) * 2;
        if u64::from(self.runtime.data_cache_max_open_files) < required_files {
            errors.push(ValidationError::new(
                "runtime.data_cache_max_open_files",
                format!(
                    "must be >= runtime.max_open_scopes * 2 ({required_files}); got {}",
                    self.runtime.data_cache_max_open_files
                ),
            ));
        }
        if self.graph.cell_id.is_empty() {
            errors.push(ValidationError::new("graph.cell_id", "must be non-empty"));
        }
        if !self.tls.enabled && !self.tls.additional_dns_names.is_empty() {
            errors.push(ValidationError::new(
                "tls.additional_dns_names",
                "may be non-empty only when tls.enabled == true",
            ));
        }
        if self.graph.subnamespaces.len() > MAX_SUBNAMESPACE_DEPTH {
            errors.push(ValidationError::new(
                "graph.subnamespaces",
                format!(
                    "at most {MAX_SUBNAMESPACE_DEPTH} nested levels are allowed below the root \
                     namespace; got {}",
                    self.graph.subnamespaces.len()
                ),
            ));
        }
        for (index, component) in self.graph.subnamespaces.iter().enumerate() {
            if !is_namespace_component(component) {
                errors.push(ValidationError::new(
                    format!("graph.subnamespaces[{index}]"),
                    format!(
                        "`{component}` is not a legal namespace component: use lowercase \
                         alphanumerics, `-` or `_`, with no dots"
                    ),
                ));
            }
        }
        if self.tls.enabled {
            match (&self.tls.secret_name, &self.tls.issuer_ref) {
                (Some(_), None) | (None, Some(_)) => {}
                (None, None) => errors.push(ValidationError::new(
                    "tls",
                    "when tls.enabled is true, set exactly one of `secretName` / `issuerRef`",
                )),
                (Some(_), Some(_)) => errors.push(ValidationError::new(
                    "tls",
                    "`secretName` and `issuerRef` are mutually exclusive: with an issuer the \
                     operator names the secret itself",
                )),
            }
        }
        if let Some(issuer) = &self.tls.issuer_ref {
            check_issuer("tls.issuerRef", issuer, &mut errors);
            if !self.tls.enabled {
                errors.push(ValidationError::new(
                    "tls.issuerRef",
                    "may be set only when tls.enabled == true",
                ));
            }
        }
        if let Some(part) = self.tuning.storage.cache_part_bytes
            && (part == 0 || part % 1024 != 0)
        {
            errors.push(ValidationError::new(
                "tuning.storage.cachePartBytes",
                format!("must be a positive multiple of 1024; got {part}"),
            ));
        }
        check_env_shadowing("extra_env", &self.extra_env, &mut errors);
        if let Some(restore) = &self.restore_from {
            check_non_empty(
                "restoreFrom.store.bucket",
                &restore.store.bucket,
                &mut errors,
            );
            if let Some(explicit) = &self.object_store.explicit
                && explicit.bucket == restore.store.bucket
                && explicit.endpoint == restore.store.endpoint
            {
                errors.push(ValidationError::new(
                    "restoreFrom.store",
                    "must not be the cluster's own object store: restore into a fresh \
                     location and point objectStore at it afterwards",
                ));
            }
        }
        collect(errors)
    }
}

impl GraphNodePoolSpec {
    /// Cross-field rules for a pool: the autoscaling bounds must be coherent.
    pub fn validate(&self) -> ValidationResult {
        let mut errors: Vec<ValidationError> = Vec::new();
        if let Some(auto) = &self.autoscaling
            && auto.max_replicas < auto.min_replicas
        {
            errors.push(ValidationError::new(
                "autoscaling.maxReplicas",
                format!(
                    "must be >= autoscaling.minReplicas ({}); got {}",
                    auto.min_replicas, auto.max_replicas
                ),
            ));
        }
        collect(errors)
    }
}

impl GraphBackupSpec {
    /// Cross-field rules for a backup.
    ///
    /// A `target` that equals the source store would double write amplification
    /// and give no isolation, so it is rejected when both are explicit.
    pub fn validate(&self) -> ValidationResult {
        let mut errors: Vec<ValidationError> = Vec::new();
        check_non_empty(
            "target.store.bucket",
            &self.target.store.bucket,
            &mut errors,
        );
        if let Some(schedule) = &self.schedule {
            check_cron("schedule", schedule, &mut errors);
        }
        if self.retention < 1 {
            errors.push(ValidationError::new(
                "retention",
                format!("must be at least 1; got {}", self.retention),
            ));
        }
        check_non_empty("image.repository", &self.image.repository, &mut errors);
        check_env_shadowing("extra_env", &self.extra_env, &mut errors);
        collect(errors)
    }

    /// Whether this backup targets the same location as `source`.
    ///
    /// Kept separate from [`Self::validate`] because the comparison needs the
    /// owning cluster's resolved store, which the CR does not carry.
    #[must_use]
    pub fn targets(&self, source: &S3) -> bool {
        self.target.store.bucket == source.bucket && self.target.store.endpoint == source.endpoint
    }
}

impl GraphIngestSourceSpec {
    /// Cross-field rules for an ingest source.
    pub fn validate(&self) -> ValidationResult {
        let mut errors: Vec<ValidationError> = Vec::new();
        match (&self.source.git, &self.source.http) {
            (Some(git), None) => {
                check_non_empty("source.git.repository", &git.repository, &mut errors);
                if !(git.repository.starts_with("https://")
                    || git.repository.starts_with("http://")
                    || git.repository.starts_with("ssh://")
                    || git.repository.starts_with("git@"))
                {
                    errors.push(ValidationError::new(
                        "source.git.repository",
                        "must be an `https://`, `http://`, `ssh://` or `git@` URL",
                    ));
                }
            }
            (None, Some(http)) => {
                check_non_empty("source.http.url", &http.url, &mut errors);
                if !(http.url.starts_with("https://") || http.url.starts_with("http://")) {
                    errors.push(ValidationError::new(
                        "source.http.url",
                        "must be an absolute `http://` or `https://` URL",
                    ));
                }
            }
            (None, None) => errors.push(ValidationError::new(
                "source",
                "exactly one of `git` / `http` must be set (got none)",
            )),
            (Some(_), Some(_)) => errors.push(ValidationError::new(
                "source",
                "exactly one of `git` / `http` must be set (got both)",
            )),
        }
        check_non_empty("image.repository", &self.image.repository, &mut errors);
        if let Some(schedule) = &self.schedule {
            check_cron("schedule", schedule, &mut errors);
        }
        if self.webhook.enabled {
            if self.webhook.secret_ref.is_none() {
                errors.push(ValidationError::new(
                    "webhook.secretRef",
                    "is required when webhook.enabled is true: unauthenticated triggers are \
                     never accepted",
                ));
            }
            if let Some(path) = &self.webhook.path
                && (path.is_empty()
                    || path.contains('/')
                    || !path
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.'))
            {
                errors.push(ValidationError::new(
                    "webhook.path",
                    "must be a single URL path segment (alphanumerics, `-`, `.`)",
                ));
            }
        }
        check_env_shadowing("env", &self.env, &mut errors);
        collect(errors)
    }
}

/// Fold the collected errors into the crate's [`ValidationResult`].
fn collect(errors: Vec<ValidationError>) -> ValidationResult {
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}
