//! S3-compatible [`ObjectSink`] backed by the `object_store` crate.
//!
//! Enabled by the `s3` cargo feature (on by default). Configuration follows the
//! same `AWS_*` variables the operator injects, so the binary works unchanged
//! inside the backup/restore Job.
//!
//! Only the operations the engine needs are implemented: list one prefix and
//! copy one object. The copy streams through memory a single object at a time,
//! never the whole prefix.

use object_store::ObjectStore;
use object_store::aws::AmazonS3Builder;
use object_store::path::Path;

use super::{BackupError, ObjectEntry, ObjectSink, optional};

/// One S3 bucket in one endpoint.
pub struct S3Sink {
    store: Box<dyn ObjectStore>,
}

impl std::fmt::Debug for S3Sink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3Sink").finish_non_exhaustive()
    }
}

impl S3Sink {
    /// Build a sink for `bucket` from the environment contract.
    ///
    /// Honours `AWS_ENDPOINT`, `AWS_DEFAULT_REGION`, `AWS_ALLOW_HTTP`,
    /// `AWS_ACCESS_KEY_ID` and `AWS_SECRET_ACCESS_KEY`; credentials may also be
    /// supplied by an instance profile, which `object_store` discovers itself.
    ///
    /// # Errors
    /// Returns [`BackupError::Copy`] when the builder rejects the configuration.
    pub fn from_env(bucket: &str) -> Result<Self, BackupError> {
        let mut builder = AmazonS3Builder::from_env().with_bucket_name(bucket);
        if let Some(endpoint) = optional("AWS_ENDPOINT") {
            builder = builder.with_endpoint(endpoint);
        }
        if let Some(region) = optional("AWS_DEFAULT_REGION") {
            builder = builder.with_region(region);
        }
        if optional("AWS_ALLOW_HTTP").as_deref() == Some("true") {
            builder = builder.with_allow_http(true);
        }
        if let Some(key) = optional("AWS_ACCESS_KEY_ID") {
            builder = builder.with_access_key_id(key);
        }
        if let Some(secret) = optional("AWS_SECRET_ACCESS_KEY") {
            builder = builder.with_secret_access_key(secret);
        }
        let store = builder.build().map_err(|error| BackupError::Copy {
            key: bucket.to_owned(),
            message: error.to_string(),
        })?;
        Ok(Self {
            store: Box::new(store),
        })
    }

    /// Path for a store-relative key.
    fn path(key: &str) -> Result<Path, BackupError> {
        Path::parse(key.trim_start_matches('/')).map_err(|error| BackupError::Copy {
            key: key.to_owned(),
            message: error.to_string(),
        })
    }
}

impl ObjectSink for S3Sink {
    async fn list(&self, prefix: &str) -> Result<Vec<ObjectEntry>, BackupError> {
        use futures::StreamExt as _;

        let prefix = prefix.trim_matches('/').to_owned();
        let path = if prefix.is_empty() {
            None
        } else {
            Some(Path::parse(&prefix).map_err(|error| BackupError::List {
                prefix: prefix.clone(),
                message: error.to_string(),
            })?)
        };
        let mut stream = self.store.list(path.as_ref());
        let mut entries = Vec::new();
        while let Some(item) = stream.next().await {
            let meta = item.map_err(|error| BackupError::List {
                prefix: prefix.clone(),
                message: error.to_string(),
            })?;
            entries.push(ObjectEntry {
                key: meta.location.to_string(),
                size: meta.size,
            });
        }
        Ok(entries)
    }

    async fn copy_between(
        source: &Self,
        target: &Self,
        entry: &ObjectEntry,
        target_key: &str,
    ) -> Result<(), BackupError> {
        let from = Self::path(&entry.key)?;
        let to = Self::path(target_key)?;
        // Server-side copy when both ends are the same bucket/endpoint;
        // otherwise stream through one object's bytes.
        let result = target.store.copy(&from, &to).await;
        if result.is_ok() {
            return Ok(());
        }
        let bytes = source
            .store
            .get(&from)
            .await
            .map_err(|error| BackupError::Copy {
                key: entry.key.clone(),
                message: error.to_string(),
            })?
            .bytes()
            .await
            .map_err(|error| BackupError::Copy {
                key: entry.key.clone(),
                message: error.to_string(),
            })?;
        target
            .store
            .put(&to, bytes.into())
            .await
            .map_err(|error| BackupError::Copy {
                key: entry.key.clone(),
                message: error.to_string(),
            })?;
        Ok(())
    }
}
