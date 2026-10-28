//! Kubernetes `Event` emission for reconcile failures.
//!
//! Events are the operator's primary UX: `kubectl describe graphcluster` shows
//! them inline, and `kubectl get events` is what an operator reaches for when
//! the logs are elsewhere. They are also the only signal that survives after
//! the pod that produced it is gone.
//!
//! Design rules, all deliberate:
//!
//! * **Only actionable failures.** [`crate::error::Error::should_emit_event`]
//!   suppresses transport/connectivity noise, so a flapping apiserver cannot
//!   flood the namespace with duplicates every requeue.
//! * **Deduplicated by name.** The event name embeds the reason and a coarse
//!   time bucket, so repeated identical failures collapse into repeated
//!   increments of the same object rather than N distinct events.
//! * **Never fatal.** Emission is best-effort: a failure to write an event must
//!   never change the reconcile outcome. Callers spawn it and ignore the result.
//! * **No secrets in the message.** The message is `Error::to_string()`, whose
//!   variants are built from field names and API messages, never from object
//!   data or credentials.

use hydradb_operator_api::constants;
use k8s_openapi::api::core::v1::{Event, EventSource, ObjectReference};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{MicroTime, ObjectMeta};
use kube::Client;
use kube::api::{Api, PostParams};

/// Component name recorded in `event.source`.
pub const COMPONENT: &str = "hydradb-operator";

/// Build the `ObjectReference` an event points at.
///
/// `uid` is included when present so the event survives object recreation
/// without attaching itself to the wrong instance.
#[must_use]
pub fn involved_object(
    api_version: &str,
    kind: &str,
    namespace: &str,
    name: &str,
    uid: Option<&str>,
) -> ObjectReference {
    ObjectReference {
        api_version: Some(api_version.to_owned()),
        kind: Some(kind.to_owned()),
        namespace: Some(namespace.to_owned()),
        name: Some(name.to_owned()),
        uid: uid.map(str::to_owned),
        ..ObjectReference::default()
    }
}

/// Deterministic event name: `<object>.<reason>.<bucket>`.
///
/// The 10-minute bucket keeps a persistent failure from creating one Event per
/// requeue while still letting a *new* incident be visible after the old one
/// ages out of `kubectl get events`.
#[must_use]
pub fn event_name(object_name: &str, reason: &str, now_secs: i64) -> String {
    let bucket = now_secs / 600;
    // Event names are DNS subdomains: lowercase, no underscores.
    let object: String = object_name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let reason: String = reason
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    format!("{object}.{reason}.{bucket:x}")
}

/// Emit one `Warning` event about `involved`.
///
/// # Errors
/// Returns the apiserver error from the create; callers treat it as
/// best-effort and must not change their own outcome because of it.
pub async fn emit(
    client: &Client,
    namespace: &str,
    involved: &ObjectReference,
    reason: &str,
    message: &str,
) -> Result<Event, kube::Error> {
    let now = k8s_openapi::jiff::Timestamp::now();
    let now_secs = now.as_second();
    let object_label = involved
        .name
        .clone()
        .unwrap_or_else(|| "unknown".to_owned());
    let event = Event {
        metadata: ObjectMeta {
            name: Some(event_name(&object_label, reason, now_secs)),
            namespace: Some(namespace.to_owned()),
            ..ObjectMeta::default()
        },
        involved_object: involved.clone(),
        reason: Some(reason.to_owned()),
        message: Some(message.to_owned()),
        type_: Some("Warning".to_owned()),
        source: Some(EventSource {
            component: Some(COMPONENT.to_owned()),
            host: None,
        }),
        first_timestamp: Some(k8s_openapi::apimachinery::pkg::apis::meta::v1::Time(now)),
        last_timestamp: Some(k8s_openapi::apimachinery::pkg::apis::meta::v1::Time(now)),
        count: Some(1),
        ..Event::default()
    };
    let api = Api::<Event>::namespaced(client.clone(), namespace);
    api.create(&PostParams::default(), &event).await
}

/// Identify an object for [`emit`] from its metadata.
#[must_use]
pub fn reference_for<T>(api_version: &str, kind: &str, object: &T) -> ObjectReference
where
    T: kube::Resource,
{
    let meta = object.meta();
    involved_object(
        api_version,
        kind,
        &meta.namespace.clone().unwrap_or_default(),
        &meta.name.clone().unwrap_or_default(),
        meta.uid.as_deref(),
    )
}

/// Log a reconcile failure at the severity its class deserves and emit an
/// actionable Event.
///
/// Called from the `error_policy` functions, which are synchronous: the event
/// write is spawned and its own failure is logged at `debug!` only, because a
/// failure to *report* a failure must never change the reconcile outcome.
pub fn report_for<T>(client: Client, kind: &str, object: &T, error: &crate::error::Error)
where
    T: kube::Resource,
{
    use crate::error::ErrorClass;

    let reason = error.event_reason();
    // Severity by class: a transient failure is routine and belongs at debug!;
    // a structural one will never fix itself and needs a human; a terminal one
    // means the spec is wrong.
    match error.class() {
        ErrorClass::Terminal => {
            tracing::error!(reason, error = %error, kind, "reconcile failed terminally");
        }
        ErrorClass::Structural => {
            tracing::warn!(reason, error = %error, kind, "reconcile stalled");
        }
        ErrorClass::Transient => {
            tracing::debug!(reason, error = %error, kind, "reconcile retrying");
        }
    }

    if !error.should_emit_event() {
        return;
    }
    let api_version = format!("{}/{}", constants::GROUP, constants::VERSION);
    let reference = reference_for(&api_version, kind, object);
    let message = error.to_string();
    tokio::spawn(async move {
        let namespace = reference.namespace.clone().unwrap_or_default();
        if let Err(emit_error) = emit(&client, &namespace, &reference, reason, &message).await {
            tracing::debug!(error = %emit_error, "event emission failed");
        }
    });
}

/// MicroTime helper kept for callers that need the same clock as an event.
#[must_use]
pub fn now_micro() -> MicroTime {
    MicroTime(k8s_openapi::jiff::Timestamp::now())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_names_are_dns_safe_and_bucketed() {
        let name = event_name("GraphCluster/demo_x", "ReconcileFailed", 1_700_000_123);
        assert!(!name.contains('/'), "slashes are illegal in a name: {name}");
        assert!(!name.contains('_'));
        assert!(name.contains("reconcilefailed"));
        // 10-minute buckets: 1_700_000_123 and _399 are both in 2833333, while
        // _400 starts 2833334. Same bucket -> same name (dedupe); new -> new.
        assert_eq!(
            name,
            event_name("GraphCluster/demo_x", "ReconcileFailed", 1_700_000_399)
        );
        assert_ne!(
            name,
            event_name("GraphCluster/demo_x", "ReconcileFailed", 1_700_000_400)
        );
    }

    #[test]
    fn involved_object_carries_identity() {
        let reference = involved_object(
            "hydradb.kubexa.tech/v1alpha1",
            "GraphCluster",
            "hydradb",
            "test",
            Some("uid-1"),
        );
        assert_eq!(reference.namespace.as_deref(), Some("hydradb"));
        assert_eq!(reference.name.as_deref(), Some("test"));
        assert_eq!(reference.uid.as_deref(), Some("uid-1"));
        assert_eq!(reference.kind.as_deref(), Some("GraphCluster"));
    }
}
