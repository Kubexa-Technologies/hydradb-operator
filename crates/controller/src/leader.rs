//! Lease-based leader election.
//!
//! kube-runtime 4.2 ships no leader election and the ecosystem crates target
//! older kube versions, so this is a deliberately small implementation over
//! `coordination.k8s.io/v1` `Lease` — the same primitive the upstream
//! controllers use.
//!
//! Semantics, and why each one matters:
//!
//! * **One writer.** Only the holder identity that won the lease starts the
//!   controllers. That is what stops two operator replicas from both creating
//!   `GraphBackup` jobs or both answering a webhook.
//! * **Expiry, not cooperation.** A leader that dies stops renewing; another
//!   replica takes over once `lease_duration` has passed. There is no
//!   dependency on graceful shutdown.
//! * **Conflict-safe.** Acquisition and renewal are single `replace`/`create`
//!   calls guarded by the object's `resourceVersion`, so two contenders can
//!   never both believe they won — the loser gets a 409 and retries.
//! * **Fails closed.** Any error while renewing is treated as "I may no longer
//!   be leader": the caller stops reconciling until it re-acquires. Continuing
//!   on a stale belief is the one failure mode that corrupts cluster state.

use std::time::Duration;

use k8s_openapi::api::coordination::v1::{Lease, LeaseSpec};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::MicroTime;
use kube::api::{Api, PostParams};

use crate::error::Error;

/// Environment variable carrying this replica's identity.
pub const IDENTITY_ENV: &str = "POD_NAME";
/// Lease duration: how long a leader may be silent before another takes over.
pub const LEASE_DURATION_SECS: u32 = 15;
/// How often the leader renews (a third of the duration, so two renewals may be
/// lost before another replica would take over).
pub const RENEW_INTERVAL: Duration = Duration::from_secs(5);

/// Identity to present in the lease.
///
/// Falls back to the hostname when `POD_NAME` is unset (local `cargo run`), and
/// to a constant of last resort so the operator can still elect a leader
/// outside a cluster rather than refusing to start.
#[must_use]
pub fn identity() -> String {
    if let Ok(value) = std::env::var(IDENTITY_ENV)
        && !value.trim().is_empty()
    {
        return value;
    }
    std::env::var("HOSTNAME")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "hydradb-operator".to_owned())
}

/// Whether `lease` may be taken over by `me` at `now`.
///
/// `true` when the lease is unheld, already ours, or held by somebody whose
/// `renew_time` is older than [`LEASE_DURATION_SECS`]. A missing `renewTime` on
/// a held lease is treated as expired: a leader that never wrote one has not
/// proven liveness.
#[must_use]
pub fn can_acquire(lease: Option<&Lease>, me: &str, now_secs: f64) -> bool {
    let Some(lease) = lease else {
        return true;
    };
    let Some(spec) = lease.spec.as_ref() else {
        return true;
    };
    match spec.holder_identity.as_deref() {
        None => true,
        Some(holder) if holder == me => true,
        Some(_) => {
            let Some(renew) = spec.renew_time.as_ref() else {
                return true;
            };
            now_secs - renew.0.as_second() as f64 > f64::from(LEASE_DURATION_SECS)
        }
    }
}

/// Build the lease body for `me` at `now`.
///
/// Kept pure so the acquisition rules are testable without an apiserver.
#[must_use]
pub fn lease_spec(me: &str, now: MicroTime) -> LeaseSpec {
    LeaseSpec {
        holder_identity: Some(me.to_owned()),
        lease_duration_seconds: Some(i32::try_from(LEASE_DURATION_SECS).unwrap_or(i32::MAX)),
        acquire_time: Some(now.clone()),
        renew_time: Some(now),
        lease_transitions: Some(0),
        ..LeaseSpec::default()
    }
}

/// Try once to acquire or renew the lease.
///
/// Returns `true` when this replica is the leader after the call.
///
/// # Errors
/// Returns [`Error::Api`] on any apiserver failure; the caller treats that as
/// "not leader" rather than exiting.
pub async fn try_acquire(api: &Api<Lease>, name: &str, me: &str) -> Result<bool, Error> {
    let now = MicroTime(k8s_openapi::jiff::Timestamp::now());
    let now_secs = now.0.as_second() as f64;
    let existing = api.get_opt(name).await.map_err(Error::from_kube_error)?;

    match existing {
        None => {
            let lease = Lease {
                metadata: kube::core::ObjectMeta {
                    name: Some(name.to_owned()),
                    ..Default::default()
                },
                spec: Some(lease_spec(me, now)),
            };
            match api.create(&PostParams::default(), &lease).await {
                Ok(_) => Ok(true),
                // Somebody else created it first; retry next tick.
                Err(kube::Error::Api(response)) if response.code == 409 => Ok(false),
                Err(error) => Err(Error::from_kube_error(error)),
            }
        }
        Some(mut lease) => {
            if !can_acquire(Some(&lease), me, now_secs) {
                return Ok(false);
            }
            let previous = lease
                .spec
                .as_ref()
                .and_then(|spec| spec.holder_identity.clone());
            let acquire_time = lease
                .spec
                .as_ref()
                .and_then(|spec| spec.acquire_time.clone())
                .unwrap_or_else(|| now.clone());
            let transitions = lease
                .spec
                .as_ref()
                .and_then(|spec| spec.lease_transitions)
                .unwrap_or(0)
                + i32::from(previous.as_deref() != Some(me));
            let mut spec = lease_spec(me, now);
            spec.acquire_time = Some(acquire_time);
            spec.lease_transitions = Some(transitions);
            lease.spec = Some(spec);
            match api.replace(name, &PostParams::default(), &lease).await {
                Ok(_) => Ok(true),
                // Lost the race (409) or the object changed shape (422).
                Err(kube::Error::Api(response)) if response.code == 409 || response.code == 422 => {
                    Ok(false)
                }
                Err(error) => Err(Error::from_kube_error(error)),
            }
        }
    }
}

/// Steady-state leadership loop: acquire, then renew until an error.
///
/// Returns only when this replica stops being able to renew — the caller then
/// goes back to campaigning. Deliberately does not return "leader forever": a
/// silent apiserver for one full lease duration means we must assume we were
/// replaced.
///
/// # Errors
/// Returns [`Error::Api`] when a renew attempt fails.
pub async fn hold_leadership(api: &Api<Lease>, name: &str, me: &str) -> Result<(), Error> {
    loop {
        tokio::time::sleep(RENEW_INTERVAL).await;
        if !try_acquire(api, name, me).await? {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn micro(secs: i64) -> MicroTime {
        MicroTime(k8s_openapi::jiff::Timestamp::from_second(secs).expect("timestamp in range"))
    }

    fn lease(holder: &str, renew: Option<i64>) -> Lease {
        Lease {
            metadata: Default::default(),
            spec: Some(LeaseSpec {
                holder_identity: Some(holder.to_owned()),
                renew_time: renew.map(micro),
                ..LeaseSpec::default()
            }),
        }
    }

    #[test]
    fn an_absent_lease_is_acquirable() {
        assert!(can_acquire(None, "me", 1000.0));
    }

    #[test]
    fn our_own_lease_is_renewable() {
        assert!(can_acquire(Some(&lease("me", Some(999))), "me", 1000.0));
    }

    #[test]
    fn a_live_foreign_lease_is_not_acquirable() {
        assert!(!can_acquire(Some(&lease("other", Some(999))), "me", 1000.0));
    }

    #[test]
    fn a_foreign_lease_expires_after_the_duration() {
        let expired = 1000 - i64::from(LEASE_DURATION_SECS) - 1;
        assert!(can_acquire(
            Some(&lease("other", Some(expired))),
            "me",
            1000.0
        ));
    }

    #[test]
    fn a_held_lease_without_a_renew_time_is_treated_as_expired() {
        assert!(can_acquire(Some(&lease("other", None)), "me", 1000.0));
    }

    #[test]
    fn an_unheld_lease_is_acquirable() {
        let mut empty = lease("ignored", Some(999));
        empty.spec.as_mut().expect("spec").holder_identity = None;
        assert!(can_acquire(Some(&empty), "me", 1000.0));
    }

    #[test]
    fn the_lease_body_carries_identity_duration_and_time() {
        let spec = lease_spec("me", micro(1000));
        assert_eq!(spec.holder_identity.as_deref(), Some("me"));
        assert_eq!(
            spec.lease_duration_seconds,
            Some(LEASE_DURATION_SECS as i32)
        );
        assert_eq!(spec.renew_time, Some(micro(1000)));
        assert_eq!(spec.lease_transitions, Some(0));
    }

    #[test]
    fn identity_falls_back_instead_of_failing() {
        // Whatever the environment, identity is never empty.
        assert!(!identity().is_empty());
    }
}
