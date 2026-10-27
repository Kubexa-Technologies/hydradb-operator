//! Hand-rolled kstatus-compatible status condition helpers.
//!
//! kube-rs ships NO `kube::runtime::conditions` helper (only
//! `kube::runtime::wait::conditions` + `await_condition` test helpers exist),
//! so everything here is built directly on
//! [`k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition`].
//!
//! Contract (consumed by Flux `wait`/kstatus health, Argo CD health, and
//! `kubectl wait --for=condition=Ready`):
//!
//! - [`upsert_condition`] is ORDER-STABLE: updating a condition of the same
//!   `type` happens in place (its index never moves); a new `type` is
//!   appended. `last_transition_time` changes ONLY when `status` changes, so
//!   re-upserting an identical condition leaves the serialized status
//!   byte-identical and never churns the apiserver.
//! - [`upsert_gate`] is driven by the [`STRUCTURAL_GATES`] registry: adding a
//!   gate is a one-line table change and every gate is reachable from one place.
//! - [`set_ready`]/[`set_degraded`] emit the kstatus-compatible `Ready`
//!   condition (`True`/`Available`, `False`/`Reconciling`,
//!   `False`/`Degraded`), reusing
//!   [`hydradb_operator_api::spec::ConditionReason`].
//! - [`live_conditions`] re-reads the in-cluster object's conditions and
//!   compares against the desired set BEFORE writing, so an unchanged set
//!   produces NO patch and the reconcile never hot-loops.
//! - Every condition carries `observed_generation` copied from the object's
//!   `metadata.generation` by the caller.
//! - The three standard conditions are [`READY`], [`RECONCILING`] and
//!   [`STALLED`].

use hydradb_operator_api::spec::{ConditionReason, PhaseLabel};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, Time};

// The reconcilers landing in todos 18/20/21 own every `pub` item below;
// until then nothing outside `#[cfg(test)]` reads them.
#[allow(dead_code)]
/// Condition `type` identifier (`Ready`, `Reconciling`, `Stalled`, ...).
pub type ConditionType = &'static str;

#[allow(dead_code)]
/// kstatus `Ready` condition type; what `kubectl wait --for=condition=Ready`
/// and the Flux/Argo health checks key off.
pub const READY: ConditionType = "Ready";

#[allow(dead_code)]
/// kstatus `Reconciling` condition type: `True` while the controller is still
/// working toward the desired state.
pub const RECONCILING: ConditionType = "Reconciling";

#[allow(dead_code)]
/// kstatus `Stalled` condition type: `True` when reconciliation cannot proceed
/// until a human fixes something (e.g. unresolvable `objectStore`).
pub const STALLED: ConditionType = "Stalled";

#[allow(dead_code)]
/// Registry of every structural gate condition and the reason it carries.
///
/// Shape is `&'static [(ConditionType, &str)]` on purpose: adding a gate is a
/// one-line table change, [`upsert_gate`] resolves its reason from here, and
/// the registry test iterates this table so an unreachable entry is a test
/// failure. Every reason must be a canonical
/// [`ConditionReason`] label (asserted by the registry test).
pub static STRUCTURAL_GATES: &[(ConditionType, &str)] = &[
    (READY, "Available"),
    (RECONCILING, "Reconciling"),
    (STALLED, "Available"),
];

#[allow(dead_code)]
/// Explicit no-change signal so the status writer can skip the patch.
///
/// Returned by [`live_conditions`]: [`StatusUpdate::NoChange`] means the
/// in-cluster condition set already equals the desired set and NO write must
/// happen; [`StatusUpdate::Patch`] carries the desired set to write.
pub enum StatusUpdate {
    /// Live and desired condition sets are equal: write nothing.
    NoChange,
    /// Desired conditions to write to the `/status` subresource.
    Patch(Vec<Condition>),
}

#[allow(dead_code)]
/// Build a condition stamped with the current time.
///
/// `observed_generation` must be the object's `metadata.generation`.
#[must_use]
pub fn new_condition(
    condition_type: impl Into<String>,
    status: impl Into<String>,
    reason: &str,
    message: impl Into<String>,
    observed_generation: Option<i64>,
) -> Condition {
    Condition {
        last_transition_time: Time(k8s_openapi::jiff::Timestamp::now()),
        message: message.into(),
        observed_generation,
        reason: reason.to_owned(),
        status: status.into(),
        type_: condition_type.into(),
    }
}

#[allow(dead_code)]
/// Order-stable upsert of a single condition.
///
/// - Same `type` present: updated IN PLACE (index never changes).
///   `last_transition_time` is preserved unless `status` changed, so a
///   re-upserted identical condition stays byte-identical.
/// - New `type`: appended at the end.
pub fn upsert_condition(conditions: &mut Vec<Condition>, new: Condition) {
    match conditions.iter().position(|c| c.type_ == new.type_) {
        Some(index) => {
            let mut merged = new;
            if merged.status == conditions[index].status {
                merged.last_transition_time = conditions[index].last_transition_time.clone();
            }
            conditions[index] = merged;
        }
        None => conditions.push(new),
    }
}

#[allow(dead_code)]
/// Upsert a structural gate condition, resolving its reason from
/// [`STRUCTURAL_GATES`].
///
/// Returns `true` when `gate` is a registered gate (condition upserted with
/// the registry reason), `false` when it is unknown (nothing upserted, never
/// a panic: a typo must not crash the reconcile loop).
pub fn upsert_gate(
    conditions: &mut Vec<Condition>,
    gate: ConditionType,
    status: &str,
    message: impl Into<String>,
    observed_generation: Option<i64>,
) -> bool {
    let Some(reason) = STRUCTURAL_GATES
        .iter()
        .find(|(name, _)| *name == gate)
        .map(|(_, reason)| *reason)
    else {
        return false;
    };
    upsert_condition(
        conditions,
        new_condition(gate, status, reason, message, observed_generation),
    );
    true
}

#[allow(dead_code)]
/// Emit the kstatus-compatible `Ready` condition.
///
/// - `ready == true`: `Ready=True`/`Available` (serving), `Reconciling=False`.
/// - `ready == false`: `Ready=False`/`Reconciling` (still converging),
///   `Reconciling=True`.
///
/// A `Stalled=False`/`Available` entry is ensured when absent so the three
/// standard conditions are always present; an existing `Stalled=True` is
/// never clobbered here (clearing a stall is the blocker's job, via
/// [`upsert_gate`]).
/// [`upsert_condition`] in owned form, for callers that build the vector.
#[must_use]
pub fn upsert_condition_returning(
    mut conditions: Vec<Condition>,
    new: Condition,
) -> Vec<Condition> {
    upsert_condition(&mut conditions, new);
    conditions
}

pub fn set_ready(conditions: &mut Vec<Condition>, ready: bool, observed_generation: Option<i64>) {
    if ready {
        upsert_condition(
            conditions,
            new_condition(
                READY,
                "True",
                ConditionReason::Available.label(),
                "all desired state observed",
                observed_generation,
            ),
        );
        upsert_condition(
            conditions,
            new_condition(
                RECONCILING,
                "False",
                ConditionReason::Available.label(),
                "reconcile converged",
                observed_generation,
            ),
        );
    } else {
        upsert_condition(
            conditions,
            new_condition(
                READY,
                "False",
                ConditionReason::Reconciling.label(),
                "reconcile in progress",
                observed_generation,
            ),
        );
        upsert_condition(
            conditions,
            new_condition(
                RECONCILING,
                "True",
                ConditionReason::Reconciling.label(),
                "reconcile in progress",
                observed_generation,
            ),
        );
    }
    if conditions.iter().all(|c| c.type_ != STALLED) {
        upsert_condition(
            conditions,
            new_condition(
                STALLED,
                "False",
                ConditionReason::Available.label(),
                "no structural blockers",
                observed_generation,
            ),
        );
    }
}

#[allow(dead_code)]
/// Emit `Ready=False`/`Degraded` for reduced-capacity serving.
///
/// Also flips `Reconciling` to `False` (the controller is not converging; it
/// is serving degraded) and ensures `Stalled=False` like [`set_ready`].
pub fn set_degraded(
    conditions: &mut Vec<Condition>,
    message: impl Into<String>,
    observed_generation: Option<i64>,
) {
    upsert_condition(
        conditions,
        new_condition(
            READY,
            "False",
            ConditionReason::Degraded.label(),
            message,
            observed_generation,
        ),
    );
    upsert_condition(
        conditions,
        new_condition(
            RECONCILING,
            "False",
            ConditionReason::Degraded.label(),
            "serving in a reduced-capacity state",
            observed_generation,
        ),
    );
    if conditions.iter().all(|c| c.type_ != STALLED) {
        upsert_condition(
            conditions,
            new_condition(
                STALLED,
                "False",
                ConditionReason::Available.label(),
                "no structural blockers",
                observed_generation,
            ),
        );
    }
}

#[allow(dead_code)]
/// Compare the live (re-read, in-cluster) condition set against the desired
/// set BEFORE writing.
///
/// Returns [`StatusUpdate::NoChange`] when they match (caller must skip the
/// patch: writing an identical status would bump `resourceVersion` and
/// hot-loop the reconcile), else [`StatusUpdate::Patch`] with the desired
/// set. Comparison is order-insensitive but field-exact: any `status`,
/// `reason`, `message`, `observed_generation` or `last_transition_time`
/// difference is a real change.
#[must_use]
pub fn live_conditions(live: &[Condition], desired: &[Condition]) -> StatusUpdate {
    if same_condition_set(live, desired) {
        StatusUpdate::NoChange
    } else {
        StatusUpdate::Patch(desired.to_vec())
    }
}

/// Order-insensitive, field-exact condition-set equality.
fn same_condition_set(left: &[Condition], right: &[Condition]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut sorted_left = left.to_vec();
    let mut sorted_right = right.to_vec();
    sorted_left.sort_by(|a, b| a.type_.cmp(&b.type_));
    sorted_right.sort_by(|a, b| a.type_.cmp(&b.type_));
    sorted_left == sorted_right
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixed_time(secs: i64) -> Time {
        Time(
            k8s_openapi::jiff::Timestamp::from_second(secs)
                .expect("fixed test timestamp must be in range"),
        )
    }

    fn fixed_condition(condition_type: &str, status: &str) -> Condition {
        Condition {
            last_transition_time: fixed_time(1_700_000_000),
            message: "test".to_owned(),
            observed_generation: Some(7),
            reason: "Reconciling".to_owned(),
            status: status.to_owned(),
            type_: condition_type.to_owned(),
        }
    }

    fn condition_type_order(conditions: &[Condition]) -> Vec<String> {
        conditions.iter().map(|c| c.type_.clone()).collect()
    }

    /// Deterministic LCG Fisher-Yates shuffle (no extra dependency).
    fn shuffled<T>(items: &mut [T], seed: &mut u64) {
        for i in (1..items.len()).rev() {
            *seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let j = (*seed % (i as u64 + 1)) as usize;
            items.swap(i, j);
        }
    }

    #[test]
    fn upsert_same_condition_twice_keeps_last_transition_time() {
        let mut conditions = Vec::new();
        upsert_condition(&mut conditions, fixed_condition(READY, "True"));

        // Re-upsert an identical condition carrying a DIFFERENT timestamp:
        // the stored `lastTransitionTime` must stay byte-identical.
        let mut second = fixed_condition(READY, "True");
        second.last_transition_time = fixed_time(1_800_000_000);
        upsert_condition(&mut conditions, second);

        assert_eq!(conditions.len(), 1);
        assert_eq!(
            conditions[0].last_transition_time,
            fixed_time(1_700_000_000),
            "identical re-upsert must not move lastTransitionTime"
        );
    }

    #[test]
    fn upsert_status_change_moves_last_transition_time() {
        let mut conditions = Vec::new();
        upsert_condition(&mut conditions, fixed_condition(READY, "False"));

        let mut flipped = fixed_condition(READY, "True");
        flipped.last_transition_time = fixed_time(1_800_000_000);
        upsert_condition(&mut conditions, flipped);

        assert_eq!(conditions.len(), 1);
        assert_eq!(conditions[0].status, "True");
        assert_eq!(
            conditions[0].last_transition_time,
            fixed_time(1_800_000_000),
            "status transition must adopt the new lastTransitionTime"
        );
    }

    #[test]
    fn condition_order_is_stable_across_100_random_upsert_orders() {
        let mut conditions = vec![
            fixed_condition(READY, "False"),
            fixed_condition(RECONCILING, "True"),
            fixed_condition(STALLED, "False"),
        ];
        let baseline = condition_type_order(&conditions);
        let baseline_time = conditions[0].last_transition_time.clone();

        let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
        for _ in 0..100 {
            // Random re-upsert order of the EXISTING types (message-only
            // change, status identical): order must never move.
            let mut order = [0_usize, 1, 2];
            shuffled(&mut order, &mut seed);
            for slot in order {
                let mut next =
                    fixed_condition(&baseline[slot].clone(), &conditions[slot].status.clone());
                next.message = format!("round update {seed}");
                next.last_transition_time = fixed_time(1_800_000_000);
                upsert_condition(&mut conditions, next);
            }
            assert_eq!(
                condition_type_order(&conditions),
                baseline,
                "in-place updates must never reorder conditions"
            );
        }
        assert_eq!(
            conditions[0].last_transition_time, baseline_time,
            "message-only re-upserts must preserve lastTransitionTime"
        );
    }

    #[test]
    fn set_ready_true_yields_ready_true() {
        let mut conditions = Vec::new();
        set_ready(&mut conditions, true, Some(3));

        let ready = conditions
            .iter()
            .find(|c| c.type_ == READY)
            .expect("set_ready(true) must emit a Ready condition");
        assert_eq!(ready.status, "True");
        assert_eq!(ready.reason, ConditionReason::Available.label());
        assert_eq!(ready.observed_generation, Some(3));

        // The three standard conditions are all present.
        for want in [READY, RECONCILING, STALLED] {
            assert!(
                conditions.iter().any(|c| c.type_ == want),
                "missing standard condition {want}"
            );
        }
    }

    #[test]
    fn set_ready_false_yields_ready_false_reconciling() {
        let mut conditions = Vec::new();
        set_ready(&mut conditions, false, Some(3));

        let ready = conditions
            .iter()
            .find(|c| c.type_ == READY)
            .expect("set_ready(false) must emit a Ready condition");
        assert_eq!(ready.status, "False");
        assert_eq!(ready.reason, ConditionReason::Reconciling.label());
    }

    #[test]
    fn identical_upsert_produces_no_patch() {
        let mut conditions = Vec::new();
        set_ready(&mut conditions, true, Some(5));
        let before = serde_json::to_vec(&conditions).expect("conditions must serialize");

        // Re-upsert every condition identically (fresh timestamps on the new
        // copies, same status): the serialized status must not change.
        for existing in conditions.clone() {
            let mut same = existing;
            same.last_transition_time = Time(k8s_openapi::jiff::Timestamp::now());
            upsert_condition(&mut conditions, same);
        }
        let after = serde_json::to_vec(&conditions).expect("conditions must serialize");
        assert_eq!(
            before, after,
            "identical upsert must leave the serialized status byte-identical"
        );

        match live_conditions(&conditions, &conditions.clone()) {
            StatusUpdate::NoChange => {}
            StatusUpdate::Patch(_) => {
                panic!("identical sets must produce NO patch");
            }
        }
    }

    #[test]
    fn upsert_gate_uses_the_structural_gates_registry() {
        assert!(
            !STRUCTURAL_GATES.is_empty(),
            "registry must name at least one gate"
        );
        for (gate, reason) in STRUCTURAL_GATES {
            // Every registry reason must be a canonical ConditionReason label.
            assert!(
                ConditionReason::parse(reason).is_some(),
                "registry reason {reason} for gate {gate} must be canonical"
            );

            let mut conditions = Vec::new();
            assert!(
                upsert_gate(&mut conditions, gate, "True", "gate probe", Some(1)),
                "registered gate {gate} must be reachable via upsert_gate"
            );
            let emitted = conditions
                .iter()
                .find(|c| c.type_ == *gate)
                .expect("upsert_gate must emit the registered gate");
            assert_eq!(
                emitted.reason, *reason,
                "gate {gate} must carry its registry reason"
            );
        }

        // Unknown gate: no upsert, no panic.
        let mut conditions = Vec::new();
        assert!(
            !upsert_gate(&mut conditions, "NoSuchGate", "True", "x", Some(1)),
            "unknown gate must report unreachable"
        );
        assert!(
            conditions.is_empty(),
            "unknown gate must not emit any condition"
        );
    }

    #[test]
    fn live_conditions_detects_an_unchanged_set() {
        let mut desired = Vec::new();
        set_ready(&mut desired, true, Some(9));

        // Re-read live state equals desired: caller must skip the write.
        let live = desired.clone();
        match live_conditions(&live, &desired) {
            StatusUpdate::NoChange => {}
            StatusUpdate::Patch(_) => {
                panic!("unchanged set must produce NO patch");
            }
        }
    }

    #[test]
    fn live_conditions_changed_set_yields_patch() {
        let mut live = Vec::new();
        set_ready(&mut live, true, Some(9));

        let mut desired = live.clone();
        desired[0].message = "progress update".to_owned();
        match live_conditions(&live, &desired) {
            StatusUpdate::NoChange => {
                panic!("changed message must yield a patch");
            }
            StatusUpdate::Patch(next) => assert_eq!(next, desired),
        }
    }
}
