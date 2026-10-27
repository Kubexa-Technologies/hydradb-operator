//! HydraDB custom resource definitions.
//!
//! One module per kind: [`cluster`] (`GraphCluster`), [`nodepool`]
//! (`GraphNodePool`), [`indexer`] (`GraphIndexer`), [`backup`] (`GraphBackup`)
//! and [`ingest`] (`GraphIngestSource`).

pub mod backup;
pub mod cluster;
pub mod indexer;
pub mod ingest;
pub mod nodepool;

pub use backup::{BackupTarget, GraphBackup, GraphBackupSpec, GraphBackupStatus};
pub use cluster::{
    AuthSpec, DeletionPolicy, Endpoints, GraphCluster, GraphClusterSpec, GraphClusterStatus,
    GraphIdentity, IndexingStatus, MonitoringSpec, RestoreSource, RuntimeSpec, ServiceMonitorSpec,
    ServiceSpec, TlsSpec,
};
pub use indexer::{GraphIndexer, GraphIndexerSpec, GraphIndexerStatus};
pub use ingest::{
    ConcurrencyPolicy, GitSource, GraphIngestSource, GraphIngestSourceSpec,
    GraphIngestSourceStatus, HttpSource, IngestSourceSpec, WebhookSpec,
};
pub use nodepool::{
    CacheSpec, GraphNodePool, GraphNodePoolSpec, GraphNodePoolStatus, NodeServiceSpec,
    UpdateStrategy,
};
