//! HydraDB custom resource definitions.
//!
//! One module per kind: [`cluster`] (`GraphCluster`), [`nodepool`]
//! (`GraphNodePool`), [`indexer`] (`GraphIndexer`).

pub mod cluster;
pub mod indexer;
pub mod nodepool;

pub use cluster::{
    AuthSpec, DeletionPolicy, Endpoints, GraphCluster, GraphClusterSpec, GraphClusterStatus,
    GraphIdentity, RuntimeSpec, ServiceSpec, TlsSpec,
};
pub use indexer::{GraphIndexer, GraphIndexerSpec, GraphIndexerStatus};
pub use nodepool::{
    CacheSpec, GraphNodePool, GraphNodePoolSpec, GraphNodePoolStatus, NodeServiceSpec,
    UpdateStrategy,
};
