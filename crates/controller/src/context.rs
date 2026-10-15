//! Shared reconciler context.
//!
//! [`Ctx`] carries the [`kube::Client`], the operator namespace, the watch
//! [`WatchScope`] parsed once at startup from `OPERATOR_SCOPE` (mirroring the
//! chart's `installScope`), and one [`kube::Api`] per CRD kind scoped
//! accordingly. Every reconciler (todos 18/20/21) takes this context by value
//! (`Clone` is cheap: `Client` and `Api` are `Arc`-backed handles).

use hydradb_operator_api::crd::{GraphCluster, GraphIndexer, GraphNodePool};
use kube::{Api, Resource};

use crate::error::Error;

/// Default operator namespace when `OPERATOR_NAMESPACE` is unset.
pub const DEFAULT_NAMESPACE: &str = "hydradb-operator-system";

/// Environment variable selecting the namespace the operator is deployed in.
pub const OPERATOR_NAMESPACE_ENV: &str = "OPERATOR_NAMESPACE";

/// Environment variable selecting the watch scope. The chart sets it from
/// `installScope`, whose schema allows exactly `namespaced` and `cluster`.
pub const OPERATOR_SCOPE_ENV: &str = "OPERATOR_SCOPE";

/// Watch scope parsed once at startup from [`OPERATOR_SCOPE_ENV`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchScope {
    /// Watch only the operator's own namespace (`installScope=namespaced`,
    /// the chart default and the safer choice).
    Namespaced,
    /// Watch all namespaces (`installScope=cluster`).
    Cluster,
}

impl WatchScope {
    /// Read the scope from [`OPERATOR_SCOPE_ENV`].
    ///
    /// Unset or empty means [`WatchScope::Namespaced`] (the chart default).
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidScope`] naming the valid values when the
    /// variable holds anything else; the caller (`run`) propagates it so the
    /// process exits non-zero before touching the cluster.
    pub fn from_env() -> Result<Self, Error> {
        match std::env::var(OPERATOR_SCOPE_ENV)
            .unwrap_or_default()
            .as_str()
        {
            "" | "namespaced" => Ok(Self::Namespaced),
            "cluster" => Ok(Self::Cluster),
            other => Err(Error::InvalidScope(other.to_owned())),
        }
    }

    /// Build an `Api` for `K` honoring the scope: the operator's own
    /// namespace when [`WatchScope::Namespaced`], all namespaces otherwise.
    #[must_use]
    pub fn api<K>(&self, client: kube::Client, namespace: &str) -> Api<K>
    where
        K: Resource<Scope = kube::core::NamespaceResourceScope>,
        K::DynamicType: Default,
    {
        match self {
            Self::Namespaced => Api::namespaced(client, namespace),
            Self::Cluster => Api::all(client),
        }
    }
}

/// Shared context handed to every reconciler.
#[derive(Clone)]
// The `Api` handles and `client` are consumed by the reconcilers landing in
// todos 18/20/21; until then nothing reads them.
#[allow(dead_code)]
pub struct Ctx {
    /// Cluster client (also used for version probes and SSA in later todos).
    pub client: kube::Client,
    /// Namespace the operator itself runs in (the chart's `OPERATOR_NAMESPACE`
    /// downward-API env). Watches stay inside it unless [`WatchScope::Cluster`].
    pub namespace: String,
    /// Watch scope parsed once at startup; drives every watch in `run`.
    pub scope: WatchScope,
    /// Handle for `GraphCluster` objects, scoped by [`WatchScope`].
    pub clusters: Api<GraphCluster>,
    /// Handle for `GraphNodePool` objects, scoped by [`WatchScope`].
    pub pools: Api<GraphNodePool>,
    /// Handle for `GraphIndexer` objects, scoped by [`WatchScope`].
    pub indexers: Api<GraphIndexer>,
}

impl Ctx {
    /// Build a scope-aware context from a ready client and a parsed scope.
    ///
    /// # Errors
    ///
    /// This constructor is infallible today; it returns `Result` so later
    /// todos (version probe, metrics registry) can fail construction without
    /// changing the signature.
    pub fn new(client: kube::Client, scope: WatchScope) -> Result<Self, Error> {
        let namespace =
            std::env::var(OPERATOR_NAMESPACE_ENV).unwrap_or_else(|_| DEFAULT_NAMESPACE.to_owned());
        Ok(Self {
            clusters: scope.api(client.clone(), &namespace),
            pools: scope.api(client.clone(), &namespace),
            indexers: scope.api(client.clone(), &namespace),
            namespace,
            scope,
            client,
        })
    }

    /// Scope-aware handle for any watched kind: the CRD `Api`s above and every
    /// owned-child watch in `run` go through here so `installScope` moves the
    /// RBAC and the runtime watches together.
    #[must_use]
    pub fn api<K>(&self) -> Api<K>
    where
        K: Resource<Scope = kube::core::NamespaceResourceScope>,
        K::DynamicType: Default,
    {
        self.scope.api(self.client.clone(), &self.namespace)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use k8s_openapi::api::core::v1::ConfigMap;

    /// Serializes the scope env-var tests: the environment is process-global.
    static SCOPE_ENV_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Offline client: `try_from` builds the connector without any I/O, so no
    /// cluster is needed to assert the `Api` shapes below.
    fn test_client() -> kube::Client {
        let config = kube::Config::new("http://127.0.0.1:1".parse().expect("loopback URI parses"));
        kube::Client::try_from(config).expect("client builds without I/O")
    }

    /// Set `OPERATOR_SCOPE` (and the operator namespace) under the guard, run
    /// `f`, then restore a clean environment.
    fn with_env(scope: Option<&str>, f: impl FnOnce()) {
        let _guard = SCOPE_ENV_GUARD
            .lock()
            .expect("scope env guard is not poisoned");
        // SAFETY: the guard serializes every env mutation in this test
        // binary, and no other thread reads these vars concurrently.
        unsafe {
            match scope {
                Some(value) => std::env::set_var(OPERATOR_SCOPE_ENV, value),
                None => std::env::remove_var(OPERATOR_SCOPE_ENV),
            }
            std::env::set_var(OPERATOR_NAMESPACE_ENV, "operator-ns");
        }
        f();
        // SAFETY: same serialization as above; restores the pre-test state.
        unsafe {
            std::env::remove_var(OPERATOR_SCOPE_ENV);
            std::env::remove_var(OPERATOR_NAMESPACE_ENV);
        }
    }

    #[tokio::test]
    async fn namespaced_scope_builds_a_namespaced_api() {
        with_env(Some("namespaced"), || {
            let scope = WatchScope::from_env().expect("namespaced parses");
            assert_eq!(scope, WatchScope::Namespaced);
            let api: Api<ConfigMap> = scope.api(test_client(), "operator-ns");
            assert_eq!(
                api.namespace(),
                Some("operator-ns"),
                "namespaced scope must watch the operator's own namespace"
            );
            let ctx = Ctx::new(test_client(), scope).expect("context builds");
            assert_eq!(ctx.scope, WatchScope::Namespaced);
            assert_eq!(ctx.namespace, "operator-ns");
            assert_eq!(ctx.clusters.namespace(), Some("operator-ns"));
            assert_eq!(ctx.pools.namespace(), Some("operator-ns"));
            assert_eq!(ctx.indexers.namespace(), Some("operator-ns"));
        });
    }

    #[tokio::test]
    async fn unset_scope_defaults_to_the_namespaced_chart_default() {
        with_env(None, || {
            let scope = WatchScope::from_env().expect("unset scope defaults");
            assert_eq!(
                scope,
                WatchScope::Namespaced,
                "unset OPERATOR_SCOPE must match the chart default installScope=namespaced"
            );
            let api: Api<ConfigMap> = scope.api(test_client(), "operator-ns");
            assert_eq!(api.namespace(), Some("operator-ns"));
        });
    }

    #[tokio::test]
    async fn cluster_scope_builds_an_all_namespaces_api() {
        with_env(Some("cluster"), || {
            let scope = WatchScope::from_env().expect("cluster parses");
            assert_eq!(scope, WatchScope::Cluster);
            let api: Api<ConfigMap> = scope.api(test_client(), "operator-ns");
            assert_eq!(
                api.namespace(),
                None,
                "cluster scope must watch all namespaces"
            );
            let ctx = Ctx::new(test_client(), scope).expect("context builds");
            assert_eq!(ctx.clusters.namespace(), None);
            assert_eq!(ctx.pools.namespace(), None);
            assert_eq!(ctx.indexers.namespace(), None);
        });
    }

    #[test]
    fn invalid_scope_fails_naming_the_valid_values() {
        with_env(Some("bogus"), || {
            let error = WatchScope::from_env().expect_err("bogus scope must fail");
            let message = error.to_string();
            assert!(
                message.contains("OPERATOR_SCOPE"),
                "error must name the variable, got: {message}"
            );
            assert!(
                message.contains("namespaced") && message.contains("cluster"),
                "error must name the valid values, got: {message}"
            );
            assert_eq!(error.class(), crate::error::ErrorClass::Structural);
        });
    }
}
