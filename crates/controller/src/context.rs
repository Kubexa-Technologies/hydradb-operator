//! Shared reconciler context.
//!
//! [`Ctx`] carries the [`kube::Client`], the operator namespace, and one
//! cluster-scoped [`kube::Api`] per CRD kind. Every reconciler (todos
//! 18/20/21) takes this context by value (`Clone` is cheap: `Client` and
//! `Api` are `Arc`-backed handles).

use hydradb_operator_api::crd::{GraphCluster, GraphIndexer, GraphNodePool};
use kube::Api;

use crate::error::Error;

/// Default operator namespace when `OPERATOR_NAMESPACE` is unset.
pub const DEFAULT_NAMESPACE: &str = "hydradb-operator-system";

/// Environment variable selecting the namespace the operator is deployed in.
pub const OPERATOR_NAMESPACE_ENV: &str = "OPERATOR_NAMESPACE";

/// Shared context handed to every reconciler.
#[derive(Clone)]
// The `Api` handles and `client` are consumed by the reconcilers landing in
// todos 18/20/21; until then nothing reads them.
#[allow(dead_code)]
pub struct Ctx {
    /// Cluster client (also used for version probes and SSA in later todos).
    pub client: kube::Client,
    /// Namespace the operator itself runs in (for leader-election identity,
    /// metrics labels, etc.). Watch scopes stay cluster-wide regardless.
    pub namespace: String,
    /// Cluster-scoped handle for `GraphCluster` objects.
    pub clusters: Api<GraphCluster>,
    /// Cluster-scoped handle for `GraphNodePool` objects.
    pub pools: Api<GraphNodePool>,
    /// Cluster-scoped handle for `GraphIndexer` objects.
    pub indexers: Api<GraphIndexer>,
}

impl Ctx {
    /// Build a cluster-scoped context from a ready client.
    ///
    /// # Errors
    ///
    /// This constructor is infallible today; it returns `Result` so later
    /// todos (version probe, metrics registry) can fail construction without
    /// changing the signature.
    pub fn new(client: kube::Client) -> Result<Self, Error> {
        let namespace =
            std::env::var(OPERATOR_NAMESPACE_ENV).unwrap_or_else(|_| DEFAULT_NAMESPACE.to_owned());
        Ok(Self {
            clusters: Api::all(client.clone()),
            pools: Api::all(client.clone()),
            indexers: Api::all(client.clone()),
            namespace,
            client,
        })
    }
}
