//! Server-side apply helpers for controller-owned child objects.
//!
//! Todo 17 fills this module: the SSA patch path (field manager from
//! [`hydradb_operator_api::constants::FIELD_MANAGER`], force conflicts,
//! owned-object labelling) used to build every child manifest in Rust from
//! the CRD spec. The controller never shells out to `helm template` and never
//! resolves `:latest` at runtime.

// The reconcilers landing in todos 18/20/21 consume this module; until then
// nothing outside `#[cfg(test)]` reads most of it.
#![allow(dead_code)]

use std::collections::BTreeMap;

use hydradb_operator_api::constants;
use hydradb_operator_api::crd::GraphCluster;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{ObjectMeta, OwnerReference};
use kube::Resource as _;
use kube::api::{Api, Patch, PatchParams};

use crate::error::Error;

// Selector label carried by every child so `.owns()` can label-filter.
pub const CLUSTER_SELECTOR_LABEL: &str = "hydradb.helmforge.dev/cluster";

/// Child role for the shared operand `ConfigMap`.
pub const ROLE_CONFIG: &str = "config";
/// Child role for the client-auth `Secret`.
pub const ROLE_AUTH: &str = "auth";
/// Child role for the Bolt `Service` (port 7687).
pub const ROLE_BOLT: &str = "bolt";
/// Child role for the HTTPS `Service` (port 443 -> 8443).
pub const ROLE_HTTPS: &str = "https";
/// Child role for the metrics `Service` (port 9090).
pub const ROLE_METRICS: &str = "metrics";
/// Child role for the headless `Service` (`publishNotReadyAddresses: true`).
pub const ROLE_HEADLESS: &str = "headless";
/// Child role for the `PodDisruptionBudget`.
pub const ROLE_PDB: &str = "pdb";
/// Child role for the `NetworkPolicy`.
pub const ROLE_NETPOL: &str = "netpol";

/// SSA patch parameters shared by every child write: the pinned field
/// manager with conflict forcing, so the controller owns every field it
/// manages and adopts fields other managers (e.g. a previous release)
/// still claim.
#[must_use]
pub fn apply_params() -> PatchParams {
    PatchParams::apply(constants::FIELD_MANAGER).force()
}

/// `OwnerReference` pointing at the owning [`GraphCluster`].
///
/// `controller` is `true` so garbage collection cascades from the cluster;
/// `block_owner_deletion` stays `false` so a stuck child can never block
/// cluster deletion (kopiur precedent).
///
/// # Errors
///
/// Returns [`Error::MissingField`] when the owner has no name or uid yet
/// (not yet persisted to the apiserver).
pub fn owner_ref_for(owner: &GraphCluster) -> Result<OwnerReference, Error> {
    let meta = owner.meta();
    let name = meta
        .name
        .clone()
        .ok_or_else(|| Error::MissingField("GraphCluster has no metadata.name".to_owned()))?;
    let uid = meta
        .uid
        .clone()
        .ok_or_else(|| Error::MissingField("GraphCluster has no metadata.uid".to_owned()))?;
    Ok(OwnerReference {
        api_version: GraphCluster::api_version(&()).into_owned(),
        kind: GraphCluster::kind(&()).into_owned(),
        name,
        uid,
        controller: Some(true),
        block_owner_deletion: Some(false),
    })
}

/// Deterministic name for a cluster-owned child: `<cluster>-<role>`.
#[must_use]
pub fn cluster_child_name(cluster_name: &str, role: &str) -> String {
    format!("{cluster_name}-{role}")
}

/// Deterministic name for a pool-owned child: the pool name itself.
#[must_use]
pub fn pool_child_name(pool_name: &str) -> String {
    pool_name.to_owned()
}

/// Deterministic name for an indexer-owned child: the indexer name itself.
#[must_use]
pub fn indexer_child_name(indexer_name: &str) -> String {
    indexer_name.to_owned()
}

/// Standard label set applied to EVERY child: the `app.kubernetes.io/*`
/// suite plus the [`CLUSTER_SELECTOR_LABEL`] selector label so `.owns()`
/// can label-filter children by cluster.
#[must_use]
pub fn standard_labels(cluster_name: &str, role: &str) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("app.kubernetes.io/name".to_owned(), "hydradb".to_owned()),
        (
            "app.kubernetes.io/instance".to_owned(),
            cluster_name.to_owned(),
        ),
        ("app.kubernetes.io/component".to_owned(), role.to_owned()),
        (
            "app.kubernetes.io/managed-by".to_owned(),
            "hydradb-operator".to_owned(),
        ),
        ("app.kubernetes.io/part-of".to_owned(), "hydradb".to_owned()),
        (CLUSTER_SELECTOR_LABEL.to_owned(), cluster_name.to_owned()),
    ])
}

/// Pure builder for a cluster-owned child's `ObjectMeta`: deterministic
/// name, standard labels, and the controller owner references No API call,
/// so the whole write path stays unit-testable without a cluster.
///
/// # Errors
///
/// Returns [`Error::MissingField`] when the owner has no name or uid yet.
pub fn child_object_meta(
    cluster_name: &str,
    role: &str,
    owner: &GraphCluster,
) -> Result<ObjectMeta, Error> {
    Ok(ObjectMeta {
        name: Some(cluster_child_name(cluster_name, role)),
        labels: Some(standard_labels(cluster_name, role)),
        owner_references: Some(vec![owner_ref_for(owner)?]),
        ..ObjectMeta::default()
    })
}

/// Server-side-apply `obj` into the apiserver (create-or-patch in one
/// call) with the pinned field manager and `force`.
///
/// # Errors
///
/// Returns [`Error::MissingField`] when `obj` has no name, or
/// [`Error::Api`] when the apiserver rejects the patch.
pub async fn apply_child<K>(api: &Api<K>, obj: &K) -> Result<K, Error>
where
    K: kube::Resource + serde::Serialize + serde::de::DeserializeOwned + Clone + std::fmt::Debug,
{
    let name = obj
        .meta()
        .name
        .clone()
        .ok_or_else(|| Error::MissingField("child object has no metadata.name".to_owned()))?;
    api.patch(&name, &apply_params(), &Patch::Apply(obj))
        .await
        .map_err(Error::from_kube_error)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_owner() -> GraphCluster {
        serde_json::from_value(serde_json::json!({
            "apiVersion": "hydradb.helmforge.dev/v1alpha1",
            "kind": "GraphCluster",
            "metadata": {"name": "demo", "namespace": "default", "uid": "abc-123"},
            "spec": {
                "image": {"repository": "example.com/hydradb"},
                "graph": {"namespace": "ns", "id": "g", "database": "db", "cellId": "c0"},
                "objectStore": {}
            }
        }))
        .expect("test owner must deserialize")
    }

    #[test]
    fn field_manager_is_exactly_the_pinned_string() {
        assert_eq!(constants::FIELD_MANAGER, "hydradb.helmforge.dev/controller");
        assert!(
            !constants::FIELD_MANAGER.contains("hydradb.hydradb."),
            "field manager must not be double-prefixed, got: {}",
            constants::FIELD_MANAGER
        );
        assert_eq!(
            apply_params().field_manager.as_deref(),
            Some(constants::FIELD_MANAGER)
        );
    }

    #[test]
    fn apply_params_force_is_true() {
        let params = apply_params();
        assert!(params.force, "SSA apply must set force == true");
    }

    #[test]
    fn owner_ref_is_controller_true_and_block_owner_deletion_false() {
        let owner = test_owner();
        let owner_ref = owner_ref_for(&owner).expect("owner has name and uid");
        assert_eq!(owner_ref.controller, Some(true));
        assert_eq!(owner_ref.block_owner_deletion, Some(false));
        assert_eq!(owner_ref.api_version, "hydradb.helmforge.dev/v1alpha1");
        assert_eq!(owner_ref.kind, "GraphCluster");
        assert_eq!(owner_ref.name, "demo");
        assert_eq!(owner_ref.uid, "abc-123");
    }

    #[test]
    fn child_names_are_deterministic_and_prefixed_with_the_cluster_name() {
        for role in [
            "config", "auth", "bolt", "https", "metrics", "headless", "pdb", "netpol",
        ] {
            let first = cluster_child_name("demo", role);
            let second = cluster_child_name("demo", role);
            assert_eq!(first, second, "child name must be deterministic");
            assert_eq!(first, format!("demo-{role}"));
            assert!(
                first.starts_with("demo-"),
                "child name must be prefixed with the cluster name, got: {first}"
            );
        }
        assert_eq!(pool_child_name("demo-pool"), "demo-pool");
        assert_eq!(indexer_child_name("demo-indexer"), "demo-indexer");
    }

    #[test]
    fn every_child_carries_the_standard_label_set_and_the_cluster_selector_label() {
        let labels = standard_labels("demo", "bolt");
        for (key, value) in [
            ("app.kubernetes.io/name", "hydradb"),
            ("app.kubernetes.io/instance", "demo"),
            ("app.kubernetes.io/component", "bolt"),
            ("app.kubernetes.io/managed-by", "hydradb-operator"),
            ("app.kubernetes.io/part-of", "hydradb"),
            ("hydradb.helmforge.dev/cluster", "demo"),
        ] {
            assert_eq!(
                labels.get(key).map(String::as_str),
                Some(value),
                "missing or wrong label {key}"
            );
        }
    }

    #[test]
    fn two_builds_of_the_same_desired_child_are_byte_identical() {
        let owner = test_owner();
        let first =
            child_object_meta("demo", "config", &owner).expect("meta builds without a cluster");
        let second =
            child_object_meta("demo", "config", &owner).expect("meta builds without a cluster");
        let first_json = serde_json::to_string(&first).expect("ObjectMeta must serialize to JSON");
        let second_json =
            serde_json::to_string(&second).expect("ObjectMeta must serialize to JSON");
        assert_eq!(first_json, second_json, "no churn between builds");
    }
}
