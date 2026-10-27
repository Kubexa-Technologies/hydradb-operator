//! Object-store copy engine behind `hydradb backup` / `hydradb restore`.
//!
//! The operator schedules the run; this module performs it. Both halves share
//! one contract (see `docs/backup.md`): the process reads its whole
//! configuration from `HYDRADB_BACKUP_*` / `AWS_*` environment variables and
//! exits non-zero on any failure.
//!
//! Ordering is the safety property that matters: every data object is copied
//! **before** the manifest that makes them addressable. A partial run therefore
//! leaves a target that HydraDB will not adopt, instead of a manifest pointing
//! at objects that were never written.
//!
//! All decision logic ([`plan_copy`], [`verify`]) is pure and tested against an
//! in-memory sink; the S3 binding lives behind the `s3` cargo feature so the
//! default build stays dependency-light.

#[cfg(feature = "s3")]
pub mod s3;

use std::collections::BTreeMap;
use std::env;

use thiserror::Error;

/// Object key that marks a complete, adoptable copy.
pub const MANIFEST: &str = "manifest.json";

/// One object in a store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjectEntry {
    /// Key relative to the store root.
    pub key: String,
    /// Size in bytes, used for post-copy verification.
    pub size: u64,
}

/// Errors the copy engine can return.
#[derive(Debug, Error)]
pub enum BackupError {
    /// A required environment variable is missing or empty.
    #[error("missing required environment variable `{0}`")]
    MissingEnv(&'static str),
    /// `HYDRADB_BACKUP_DIRECTION` held something other than backup/restore.
    #[error("HYDRADB_BACKUP_DIRECTION must be `backup` or `restore`, got `{0}`")]
    BadDirection(String),
    /// Directory listing failed.
    #[error("listing `{prefix}` failed: {message}")]
    List {
        /// Prefix that failed.
        prefix: String,
        /// Underlying message.
        message: String,
    },
    /// Copying one object failed.
    #[error("copying `{key}` failed: {message}")]
    Copy {
        /// Object that failed.
        key: String,
        /// Underlying message.
        message: String,
    },
    /// The copy completed but verification disagreed.
    #[error(
        "verification failed: source has {source_objects} object(s)/{source_bytes} bytes, \
         target has {target_objects} object(s)/{target_bytes} bytes"
    )]
    Verify {
        /// Objects seen at the source.
        source_objects: usize,
        /// Bytes seen at the source.
        source_bytes: u64,
        /// Objects seen at the target.
        target_objects: usize,
        /// Bytes seen at the target.
        target_bytes: u64,
    },
    /// The source was empty, which is never a successful backup.
    #[error("source prefix `{0}` contains no objects")]
    EmptySource(String),
}

/// Fully resolved configuration for one run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunConfig {
    /// `backup` or `restore`.
    pub direction: Direction,
    /// Source bucket.
    pub source_bucket: String,
    /// Source prefix (may be empty).
    pub source_prefix: String,
    /// Target bucket.
    pub target_bucket: String,
    /// Target prefix (may be empty).
    pub target_prefix: String,
    /// Verify the copy.
    pub verify: bool,
}

/// Copy direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    /// Cluster store → backup store.
    Backup,
    /// Backup store → cluster store.
    Restore,
}

impl Direction {
    /// Parse the wire value.
    ///
    /// # Errors
    /// Returns [`BackupError::BadDirection`] for anything else.
    pub fn parse(value: &str) -> Result<Self, BackupError> {
        match value {
            "backup" => Ok(Self::Backup),
            "restore" => Ok(Self::Restore),
            other => Err(BackupError::BadDirection(other.to_owned())),
        }
    }
}

/// Read one required environment variable.
fn required(name: &'static str) -> Result<String, BackupError> {
    match env::var(name) {
        Ok(value) if !value.trim().is_empty() => Ok(value),
        _ => Err(BackupError::MissingEnv(name)),
    }
}

/// Read one optional environment variable (`None` when unset or empty).
#[must_use]
pub fn optional(name: &str) -> Option<String> {
    env::var(name).ok().filter(|value| !value.trim().is_empty())
}

impl RunConfig {
    /// Build the configuration from the documented environment contract.
    ///
    /// # Errors
    /// Returns [`BackupError::MissingEnv`] naming the first missing variable.
    pub fn from_env() -> Result<Self, BackupError> {
        let direction = Direction::parse(&required("HYDRADB_BACKUP_DIRECTION")?)?;
        Ok(Self {
            direction,
            source_bucket: required("HYDRADB_BACKUP_SOURCE_BUCKET")?,
            source_prefix: optional("HYDRADB_BACKUP_SOURCE_PREFIX").unwrap_or_default(),
            target_bucket: required("HYDRADB_BACKUP_TARGET_BUCKET")?,
            target_prefix: optional("HYDRADB_BACKUP_TARGET_PREFIX").unwrap_or_default(),
            verify: optional("HYDRADB_BACKUP_VERIFY").as_deref() == Some("true"),
        })
    }
}

/// A readable and writable object store.
///
/// Deliberately small: list one prefix, copy one object. Everything else
/// (ordering, verification, reporting) is pure code above this trait.
#[allow(async_fn_in_trait)]
pub trait ObjectSink {
    /// List every object under `prefix`.
    ///
    /// # Errors
    /// Returns [`BackupError::List`] when the store refuses the listing.
    async fn list(&self, prefix: &str) -> Result<Vec<ObjectEntry>, BackupError>;

    /// Copy a single object between two sinks.
    ///
    /// Taking both sinks in one call keeps the operation streaming: the
    /// implementation never materialises a whole object in memory.
    ///
    /// # Errors
    /// Returns [`BackupError::Copy`] when either side fails.
    async fn copy_between(
        source: &Self,
        target: &Self,
        entry: &ObjectEntry,
        target_key: &str,
    ) -> Result<(), BackupError>
    where
        Self: Sized;
}

/// Normalise a prefix to `<prefix>/<key>` with no doubled or trailing slashes.
#[must_use]
pub fn join_key(prefix: &str, key: &str) -> String {
    let prefix = prefix.trim_matches('/');
    let key = key.trim_start_matches('/');
    if prefix.is_empty() {
        key.to_owned()
    } else {
        format!("{prefix}/{key}")
    }
}

/// Strip `prefix` from `key`, leaving the store-relative path.
#[must_use]
pub fn strip_prefix<'a>(prefix: &str, key: &'a str) -> &'a str {
    let prefix = prefix.trim_matches('/');
    // Leading slashes are stripped from the key first: providers disagree on
    // whether keys are returned with one, and a mismatch would silently keep
    // the whole path and land objects under `<target>/<source-prefix>/...`.
    let key = key.trim_start_matches('/');
    if prefix.is_empty() {
        return key;
    }
    key.strip_prefix(prefix)
        .map(|rest| rest.trim_start_matches('/'))
        .unwrap_or(key)
}

/// Compute the copy plan for a listing.
///
/// Returns `(source_key, target_key)` pairs with **every non-manifest object
/// first and the manifest last**, which is what makes an interrupted run
/// harmless: the target never looks adoptable until all data is present.
#[must_use]
pub fn plan_copy(
    entries: &[ObjectEntry],
    source_prefix: &str,
    target_prefix: &str,
) -> Vec<(String, String)> {
    let mut data: Vec<(String, String)> = Vec::with_capacity(entries.len());
    let mut manifests: Vec<(String, String)> = Vec::new();
    for entry in entries {
        let relative = strip_prefix(source_prefix, &entry.key);
        let target = join_key(target_prefix, relative);
        if relative == MANIFEST || relative.ends_with(&format!("/{MANIFEST}")) {
            manifests.push((entry.key.clone(), target));
        } else {
            data.push((entry.key.clone(), target));
        }
    }
    // Deterministic order keeps logs and retries reproducible.
    data.sort();
    manifests.sort();
    data.extend(manifests);
    data
}

/// Compare the source listing with the target listing.
///
/// # Errors
/// Returns [`BackupError::Verify`] with both sides' totals.
pub fn verify(
    source: &[ObjectEntry],
    source_prefix: &str,
    target: &[ObjectEntry],
    target_prefix: &str,
) -> Result<(), BackupError> {
    let expected: BTreeMap<String, u64> = source
        .iter()
        .map(|entry| {
            (
                strip_prefix(source_prefix, &entry.key).to_owned(),
                entry.size,
            )
        })
        .collect();
    let actual: BTreeMap<String, u64> = target
        .iter()
        .map(|entry| {
            (
                strip_prefix(target_prefix, &entry.key).to_owned(),
                entry.size,
            )
        })
        .collect();
    if expected == actual {
        return Ok(());
    }
    Err(BackupError::Verify {
        source_objects: source.len(),
        source_bytes: source.iter().map(|entry| entry.size).sum(),
        target_objects: target.len(),
        target_bytes: target.iter().map(|entry| entry.size).sum(),
    })
}

/// Run a copy: list, plan, copy in order, optionally verify.
///
/// Returns the number of objects copied.
///
/// # Errors
/// Returns [`BackupError::EmptySource`] when there is nothing to copy (an empty
/// backup is always a configuration mistake, never a success), plus any
/// listing, copy or verification error.
pub async fn run<S: ObjectSink>(
    source: &S,
    target: &S,
    config: &RunConfig,
) -> Result<usize, BackupError> {
    let entries = source.list(&config.source_prefix).await?;
    if entries.is_empty() {
        return Err(BackupError::EmptySource(config.source_prefix.clone()));
    }
    let plan = plan_copy(&entries, &config.source_prefix, &config.target_prefix);
    for (from, to) in &plan {
        let size = entries
            .iter()
            .find(|entry| &entry.key == from)
            .map_or(0, |entry| entry.size);
        let item = ObjectEntry {
            key: from.clone(),
            size,
        };
        S::copy_between(source, target, &item, to).await?;
    }
    if config.verify {
        let copied = target.list(&config.target_prefix).await?;
        verify(
            &entries,
            &config.source_prefix,
            &copied,
            &config.target_prefix,
        )?;
    }
    Ok(plan.len())
}

#[cfg(test)]
mod tests;
