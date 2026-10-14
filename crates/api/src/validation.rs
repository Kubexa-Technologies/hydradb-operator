//! Cross-field validation for HydraDB specs.
//!
//! The CRD schema enforces per-field shape; the rules here span fields and
//! cannot be expressed in a schema, so the controller calls `validate()`
//! during reconcile. Every violation is collected into the returned `Vec`
//! (never stop-at-first) so one reconcile reports every problem at once.
//!
//! `GraphNodePoolSpec` and `GraphIndexerSpec` deliberately expose no
//! `validate()`: no real cross-field rule applies to them today, and inventing
//! one would reject clusters the operand accepts.

use crate::crd::GraphClusterSpec;
use crate::spec::ObjectStore;

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
    /// 1. `object_store` has exactly one source (delegates to
    ///    [`ObjectStore::validate`]).
    /// 2. `runtime.data_cache_max_open_files >= runtime.max_open_scopes * 2`
    ///    (the operand validates `maxOpenFiles >= maxOpenScopes * cells * 2`
    ///    and this operator is single-cell, so `cells == 1`).
    /// 3. `graph.cell_id` is non-empty.
    /// 4. `tls.additional_dns_names` is non-empty only when `tls.enabled`.
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
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}
