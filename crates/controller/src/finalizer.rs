//! Finalizer and `deletionPolicy` handling for HydraDB reconcilers.
//!
//! Contract (todos 18/20/21 call into this module; nothing here wires into a
//! reconciler):
//!
//! - [`with_finalizer`] is the ONLY top-level action inside a reconcile: it
//!   delegates to `kube::runtime::finalizer` with
//!   [`hydradb_operator_api::constants::FINALIZER`], dispatching `Apply` to
//!   the apply closure and `Cleanup` to the cleanup closure.
//! - [`cleanup_plan`] is the PURE destructive-decision function: given the
//!   `deletionPolicy`, whether operator-owned children exist, and whether the
//!   object store is reachable, it returns the ORDERED cleanup actions. The
//!   ordering contract is unit-testable without a cluster.
//! - `Retain` (default) removes operator-owned children but LEAVES
//!   object-store data intact. `Delete` removes children AND the cluster's
//!   own data prefix (see [`data_prefix_for_cluster`]).
//! - The `pruned-by` stamp ([`PRUNED_BY_ANNOTATION`]) is action zero on EVERY
//!   plan, on both policies, before the finalizer is released, so an
//!   operator pruning stays auditable.
//! - Cleanup NEVER blocks deletion on an unreachable object store: the plan
//!   omits the data-prefix step when the store is unreachable, and
//!   [`execute_cleanup`] additionally warns-and-continues on store errors.
//! - The operator NEVER deletes a bucket it did not create: only the
//!   cluster-scoped data prefix is ever removed, never the bucket itself.
//! - Cleanup tolerates a CR that was never applied (no children to delete),
//!   missing children (404 is success), an already-removed finalizer, and an
//!   unreachable store.

// The reconcilers landing in todos 18/20/21 consume this module; until then
// nothing outside `#[cfg(test)]` reads the cluster-bound items.
#![allow(dead_code)]

use std::fmt::Debug;
use std::future::Future;
use std::sync::Arc;

use hydradb_operator_api::constants;
use hydradb_operator_api::crd::DeletionPolicy;
use kube::api::{Api, Patch, PatchParams};
use kube::runtime::controller::Action;
use kube::runtime::finalizer::Event;
use serde::{Serialize, de::DeserializeOwned};

use crate::error::Error;

/// Annotation stamped on the CR before it goes away, carrying the deletion
/// reason and timestamp so an operator pruning stays auditable.
pub const PRUNED_BY_ANNOTATION: &str = "hydradb.kubexa.tech/pruned-by";

/// The finalizer string every reconciler must use: the shared
/// [`hydradb_operator_api::constants::FINALIZER`] constant, never a literal.
#[must_use]
pub fn finalizer_name() -> &'static str {
    constants::FINALIZER
}

/// One ordered cleanup step. Produced by [`cleanup_plan`], executed by
/// [`execute_cleanup`]. The enum has NO bucket-deletion variant on purpose:
/// the operator only ever removes the cluster-scoped data prefix, never a
/// bucket it did not create.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CleanupAction {
    /// Patch [`PRUNED_BY_ANNOTATION`] with the deletion reason/timestamp.
    /// Always first, on both policies, before the finalizer is released.
    StampPrunedBy {
        /// Human-readable deletion reason (e.g. `"Retain"` / `"Delete"`).
        reason: String,
    },
    /// Remove operator-owned child objects. Tolerates missing children
    /// (404 is success) and a never-applied CR (nothing to delete).
    DeleteChildren,
    /// Remove the cluster's own object-store data prefix. Present ONLY on
    /// `Delete` with a reachable store; NEVER a whole bucket.
    DeleteDataPrefix {
        /// Cluster-scoped prefix from [`data_prefix_for_cluster`].
        prefix: String,
    },
}

/// Cluster-scoped object-store data prefix owned by this cluster.
///
/// Only this prefix is ever removed on `Delete`; the bucket itself is never
/// touched, so a bucket the operator did not create can never be deleted.
#[must_use]
pub fn data_prefix_for_cluster(cluster_name: &str) -> String {
    format!("hydradb/clusters/{cluster_name}/")
}

/// Deletion reason/timestamp value written to [`PRUNED_BY_ANNOTATION`].
///
/// Shape is `{reason}@{RFC3339-UTC}` so the reason stays greppable and the
/// stamp stays auditable.
#[must_use]
pub fn pruned_by_value(reason: &str) -> String {
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    format!("{reason}@{now}")
}

/// PURE destructive-decision function: ordered cleanup actions for
/// (`deletion_policy`, whether operator-owned children exist, whether the
/// object store is reachable).
///
/// Ordering contract (deterministic for the same inputs):
///
/// 1. `StampPrunedBy` is ALWAYS first, on both policies.
/// 2. `DeleteChildren` follows iff `children_exist`.
/// 3. `DeleteDataPrefix` is last and ONLY when the policy is `Delete` AND
///    the store is reachable. `Retain` never carries it; an unreachable
///    store omits it so deletion is never blocked on the store.
#[must_use]
pub fn cleanup_plan(
    policy: &DeletionPolicy,
    children_exist: bool,
    store_reachable: bool,
    cluster_name: &str,
) -> Vec<CleanupAction> {
    let reason = match policy {
        DeletionPolicy::Retain => "Retain",
        DeletionPolicy::Delete => "Delete",
    };
    let mut plan = vec![CleanupAction::StampPrunedBy {
        reason: reason.to_owned(),
    }];
    if children_exist {
        plan.push(CleanupAction::DeleteChildren);
    }
    if *policy == DeletionPolicy::Delete && store_reachable {
        plan.push(CleanupAction::DeleteDataPrefix {
            prefix: data_prefix_for_cluster(cluster_name),
        });
    }
    plan
}

/// True when `err` is an apiserver 404 (object already gone).
///
/// Missing children and already-removed objects are success, never failure.
#[must_use]
pub fn is_not_found(err: &Error) -> bool {
    matches!(
        err,
        Error::Api(kube::Error::Api(status)) if status.code == 404
    )
}

/// True when `err` means the object store cannot be reached.
///
/// Store errors must NEVER block deletion: [`execute_cleanup`] warns and
/// continues on these.
#[must_use]
pub fn is_store_unreachable(err: &Error) -> bool {
    matches!(
        err,
        Error::Timeout(_) | Error::UnresolvableRef(_) | Error::MissingField(_)
    )
}

/// Patch [`PRUNED_BY_ANNOTATION`] onto the named object.
///
/// Tolerates a missing object (404 → `Ok(())`): an already-removed object
/// needs no stamp. Maps any other apiserver failure to [`Error::Api`].
/// Never panics.
///
/// # Errors
///
/// Returns [`Error::MissingField`] when `name` is empty, or [`Error::Api`]
/// when the apiserver rejects the patch for a reason other than 404.
pub async fn stamp_pruned_by<K>(api: &Api<K>, name: &str, reason: &str) -> Result<(), Error>
where
    K: kube::Resource + Clone + Debug + Serialize + DeserializeOwned,
{
    if name.is_empty() {
        return Err(Error::MissingField(
            "object has no metadata.name".to_owned(),
        ));
    }
    let value = pruned_by_value(reason);
    let patch = serde_json::json!({
        "metadata": { "annotations": { PRUNED_BY_ANNOTATION: value } },
    });
    match api
        .patch(name, &PatchParams::default(), &Patch::Merge(&patch))
        .await
    {
        Ok(_) => Ok(()),
        Err(err) => {
            let err = Error::from_kube_error(err);
            if is_not_found(&err) {
                tracing::debug!(
                    annotation = PRUNED_BY_ANNOTATION,
                    name,
                    "stamp target already gone; treating as stamped"
                );
                Ok(())
            } else {
                Err(err)
            }
        }
    }
}

/// Execute a [`cleanup_plan`] with caller-injected side effects.
///
/// - `stamp` performs the [`CleanupAction::StampPrunedBy`] step
///   (call [`stamp_pruned_by`]); a 404 from it is tolerated.
/// - `delete_children` performs [`CleanupAction::DeleteChildren`]; a 404 is
///   tolerated (never-applied CR, already-removed children).
/// - `delete_data_prefix` performs [`CleanupAction::DeleteDataPrefix`] for
///   the given prefix; store-unreachable errors ([`is_store_unreachable`])
///   are logged and SKIPPED so deletion is never blocked on the store.
///
/// Steps run strictly in plan order. Unknown plan shapes cannot occur: the
/// match is exhaustive over [`CleanupAction`].
///
/// # Errors
///
/// Returns the first non-tolerated step error: a failing stamp (other than
/// 404), failing child deletion (other than 404), or a non-store failure
/// from prefix deletion.
pub async fn execute_cleanup<StampFut, ChildrenFut, PrefixFut>(
    plan: &[CleanupAction],
    stamp: impl FnOnce() -> StampFut,
    delete_children: impl FnOnce() -> ChildrenFut,
    delete_data_prefix: impl Fn(String) -> PrefixFut,
) -> Result<(), Error>
where
    StampFut: Future<Output = Result<(), Error>>,
    ChildrenFut: Future<Output = Result<(), Error>>,
    PrefixFut: Future<Output = Result<(), Error>>,
{
    // `FnOnce` hooks must fire at most once each even if the plan (built by
    // a third party, not `cleanup_plan`) repeats a step: wrap them so a
    // repeated step reuses the first outcome instead of failing to compile
    // or double-running a destructive effect.
    let mut stamp = Some(stamp);
    let mut delete_children = Some(delete_children);
    for action in plan {
        match action {
            CleanupAction::StampPrunedBy { reason } => {
                let outcome = match stamp.take() {
                    Some(stamp) => stamp().await,
                    None => {
                        tracing::debug!(
                            reason,
                            "duplicate stamp step; first outcome already applied"
                        );
                        Ok(())
                    }
                };
                match outcome {
                    Ok(()) => {}
                    Err(err) if is_not_found(&err) => {
                        tracing::debug!(reason, "stamp target already gone; treating as stamped");
                    }
                    Err(err) => return Err(err),
                }
            }
            CleanupAction::DeleteChildren => {
                let outcome = match delete_children.take() {
                    Some(delete_children) => delete_children().await,
                    None => {
                        tracing::debug!("duplicate child-deletion step; already applied");
                        Ok(())
                    }
                };
                match outcome {
                    Ok(()) => {}
                    Err(err) if is_not_found(&err) => {
                        tracing::debug!("children already gone; treating as deleted");
                    }
                    Err(err) => return Err(err),
                }
            }
            CleanupAction::DeleteDataPrefix { prefix } => {
                match delete_data_prefix(prefix.clone()).await {
                    Ok(()) => {}
                    Err(err) if is_store_unreachable(&err) => {
                        tracing::warn!(
                            prefix,
                            error = %err,
                            "object store unreachable; leaving data prefix and proceeding \
                             so deletion is never blocked on the store"
                        );
                    }
                    Err(err) => return Err(err),
                }
            }
        }
    }
    Ok(())
}

/// Wrap a reconcile in the shared finalizer as the ONLY top-level action.
///
/// Dispatches `Event::Apply` to `apply` and `Event::Cleanup` to `cleanup`,
/// stamping [`constants::FINALIZER`] via `kube::runtime::finalizer`.
/// Reconcilers in todos 18/20/21 call this instead of touching
/// `kube::runtime::finalizer` directly. Never panics.
///
/// # Errors
///
/// - Apply/cleanup closure failures pass through unwrapped.
/// - Finalizer add/remove failures surface as [`Error::Api`].
/// - An unnamed object surfaces as [`Error::MissingField`]; an internal
///   finalizer-path failure surfaces as [`Error::Unsupported`].
pub async fn with_finalizer<K, ApplyFn, ApplyFut, CleanupFn, CleanupFut>(
    api: &Api<K>,
    obj: Arc<K>,
    apply: ApplyFn,
    cleanup: CleanupFn,
) -> Result<Action, Error>
where
    K: kube::Resource + Clone + Debug + Serialize + DeserializeOwned,
    ApplyFn: FnOnce(Arc<K>) -> ApplyFut,
    ApplyFut: Future<Output = Result<Action, Error>>,
    CleanupFn: FnOnce(Arc<K>) -> CleanupFut,
    CleanupFut: Future<Output = Result<Action, Error>>,
{
    kube::runtime::finalizer(api, constants::FINALIZER, obj, |event| async move {
        match event {
            Event::Apply(obj) => apply(obj).await,
            Event::Cleanup(obj) => cleanup(obj).await,
        }
    })
    .await
    .map_err(|err| match err {
        kube::runtime::finalizer::Error::ApplyFailed(inner)
        | kube::runtime::finalizer::Error::CleanupFailed(inner) => inner,
        kube::runtime::finalizer::Error::AddFinalizer(inner)
        | kube::runtime::finalizer::Error::RemoveFinalizer(inner) => Error::Api(inner),
        kube::runtime::finalizer::Error::UnnamedObject => {
            Error::MissingField("object has no metadata.name".to_owned())
        }
        kube::runtime::finalizer::Error::InvalidFinalizer => {
            Error::Unsupported("invalid finalizer path".to_owned())
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stamp_reason(plan: &[CleanupAction]) -> Option<&str> {
        match plan.first() {
            Some(CleanupAction::StampPrunedBy { reason }) => Some(reason),
            _ => None,
        }
    }

    fn has_data_prefix(plan: &[CleanupAction]) -> bool {
        plan.iter()
            .any(|a| matches!(a, CleanupAction::DeleteDataPrefix { .. }))
    }

    fn has_delete_children(plan: &[CleanupAction]) -> bool {
        plan.iter()
            .any(|a| matches!(a, CleanupAction::DeleteChildren))
    }

    fn not_found() -> Error {
        Error::Api(kube::Error::Api(Box::new(kube::core::Status {
            code: 404,
            ..Default::default()
        })))
    }

    #[test]
    fn finalizer_name_is_exactly_the_shared_constant() {
        assert_eq!(finalizer_name(), constants::FINALIZER);
        assert_eq!(finalizer_name(), "hydradb.kubexa.tech/finalizer");
    }

    #[test]
    fn retain_keeps_data_but_removes_children() {
        let plan = cleanup_plan(&DeletionPolicy::Retain, true, true, "demo");
        assert!(has_delete_children(&plan), "Retain must remove children");
        assert!(
            !has_data_prefix(&plan),
            "Retain must LEAVE object-store data intact"
        );
    }

    #[test]
    fn delete_removes_children_and_the_data_prefix() {
        let plan = cleanup_plan(&DeletionPolicy::Delete, true, true, "demo");
        assert!(has_delete_children(&plan), "Delete must remove children");
        let prefix = plan
            .iter()
            .find_map(|a| match a {
                CleanupAction::DeleteDataPrefix { prefix } => Some(prefix.clone()),
                _ => None,
            })
            .expect("Delete with a reachable store must remove the data prefix");
        assert!(
            prefix.contains("demo"),
            "data prefix must be scoped to the cluster, got: {prefix}"
        );
        assert_eq!(prefix, data_prefix_for_cluster("demo"));
    }

    #[test]
    fn pruned_by_stamp_is_first_on_both_policies_before_release() {
        for policy in [DeletionPolicy::Retain, DeletionPolicy::Delete] {
            for children in [false, true] {
                for reachable in [false, true] {
                    let plan = cleanup_plan(&policy, children, reachable, "demo");
                    let reason = stamp_reason(&plan).expect("stamp must lead every plan");
                    let want = match policy {
                        DeletionPolicy::Retain => "Retain",
                        DeletionPolicy::Delete => "Delete",
                    };
                    assert_eq!(reason, want, "stamp must name the policy");
                    assert!(
                        !plan[1..]
                            .iter()
                            .any(|a| matches!(a, CleanupAction::StampPrunedBy { .. })),
                        "stamp must appear exactly once, first"
                    );
                }
            }
        }
    }

    #[test]
    fn unreachable_store_omits_the_data_prefix_but_keeps_child_deletion() {
        let plan = cleanup_plan(&DeletionPolicy::Delete, true, false, "demo");
        assert!(
            !has_data_prefix(&plan),
            "unreachable store must omit the data prefix so deletion never blocks"
        );
        assert!(
            has_delete_children(&plan),
            "child deletion needs no store and must stay"
        );
        let bare = cleanup_plan(&DeletionPolicy::Delete, false, false, "demo");
        assert_eq!(bare.len(), 1, "nothing to delete means stamp only");
        assert!(stamp_reason(&bare).is_some());
    }

    #[test]
    fn ordered_action_list_is_deterministic_for_the_same_inputs() {
        let first = cleanup_plan(&DeletionPolicy::Delete, true, true, "demo");
        let second = cleanup_plan(&DeletionPolicy::Delete, true, true, "demo");
        assert_eq!(
            first, second,
            "same inputs must yield the same ordered plan"
        );
        let order: Vec<&str> = first
            .iter()
            .map(|a| match a {
                CleanupAction::StampPrunedBy { .. } => "stamp",
                CleanupAction::DeleteChildren => "children",
                CleanupAction::DeleteDataPrefix { .. } => "prefix",
            })
            .collect();
        assert_eq!(order, vec!["stamp", "children", "prefix"]);
        let retain = cleanup_plan(&DeletionPolicy::Retain, true, true, "demo");
        let retain_order: Vec<&str> = retain
            .iter()
            .map(|a| match a {
                CleanupAction::StampPrunedBy { .. } => "stamp",
                CleanupAction::DeleteChildren => "children",
                CleanupAction::DeleteDataPrefix { .. } => "prefix",
            })
            .collect();
        assert_eq!(retain_order, vec!["stamp", "children"]);
    }

    #[test]
    fn pruned_by_value_carries_reason_and_timestamp() {
        let value = pruned_by_value("Retain");
        assert!(
            value.starts_with("Retain@"),
            "stamp must lead with the reason, got: {value}"
        );
        assert!(
            value.len() > "Retain@".len(),
            "stamp must carry a timestamp, got: {value}"
        );
    }

    #[test]
    fn data_prefix_is_scoped_to_the_cluster_never_a_bucket() {
        let prefix = data_prefix_for_cluster("demo");
        assert!(prefix.contains("demo"), "prefix must name the cluster");
        assert!(
            prefix.ends_with('/'),
            "prefix must be a prefix, got: {prefix}"
        );
        assert!(!prefix.is_empty());
        assert_ne!(
            data_prefix_for_cluster("a"),
            data_prefix_for_cluster("b"),
            "different clusters must own different prefixes"
        );
    }

    #[test]
    fn is_not_found_classifies_only_404() {
        assert!(is_not_found(&not_found()));
        assert!(!is_not_found(&Error::Timeout("flap".to_owned())));
        assert!(!is_not_found(&Error::Validation("bad".to_owned())));
    }

    #[test]
    fn is_store_unreachable_classifies_store_failures() {
        assert!(is_store_unreachable(&Error::Timeout("flap".to_owned())));
        assert!(is_store_unreachable(&Error::UnresolvableRef(
            "no endpoint".to_owned()
        )));
        assert!(!is_store_unreachable(&Error::PermissionDenied(
            "rbac".to_owned()
        )));
        assert!(!is_store_unreachable(&not_found()));
    }

    #[tokio::test]
    async fn cleanup_of_never_applied_cr_succeeds_without_touching_children_or_store() {
        // Never applied: no children, unreachable store → stamp-only plan.
        let plan = cleanup_plan(&DeletionPolicy::Retain, false, false, "demo");
        assert_eq!(plan.len(), 1);
        let mut stamped = false;
        execute_cleanup(
            &plan,
            || {
                stamped = true;
                async { Ok(()) }
            },
            || async {
                panic!("no children exist; the child hook must never fire");
                #[allow(unreachable_code)]
                Ok::<(), Error>(())
            },
            |_: String| async {
                panic!("no prefix step exists; the store hook must never fire");
                #[allow(unreachable_code)]
                Ok::<(), Error>(())
            },
        )
        .await
        .expect("never-applied cleanup must succeed");
        assert!(stamped, "the pruned-by stamp must still fire");
    }

    #[tokio::test]
    async fn cleanup_tolerates_missing_children() {
        let plan = cleanup_plan(&DeletionPolicy::Retain, true, true, "demo");
        execute_cleanup(
            &plan,
            || async { Ok(()) },
            || async { Err(not_found()) },
            |_: String| async { Ok(()) },
        )
        .await
        .expect("missing children (404) must not fail cleanup");
    }

    #[tokio::test]
    async fn cleanup_with_unreachable_store_still_terminates_successfully() {
        // Backstop: even if a prefix step IS planned and the store fails,
        // deletion proceeds.
        let plan = cleanup_plan(&DeletionPolicy::Delete, true, true, "demo");
        assert!(has_data_prefix(&plan));
        execute_cleanup(
            &plan,
            || async { Ok(()) },
            || async { Ok(()) },
            |_: String| async { Err(Error::Timeout("endpoint down".to_owned())) },
        )
        .await
        .expect("unreachable store must not block deletion");
    }

    #[tokio::test]
    async fn cleanup_propagates_non_tolerated_errors() {
        let plan = cleanup_plan(&DeletionPolicy::Retain, true, true, "demo");
        let err = execute_cleanup(
            &plan,
            || async { Ok(()) },
            || async { Err(Error::PermissionDenied("rbac".to_owned())) },
            |_: String| async { Ok(()) },
        )
        .await
        .expect_err("non-404 child errors must surface");
        assert!(matches!(err, Error::PermissionDenied(_)));
    }
}
