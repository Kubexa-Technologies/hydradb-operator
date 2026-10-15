//! Controller error type with exhaustive classification and requeue policy.
//!
//! Every reconciler returns [`Error`]. [`Error::class`] maps each variant to
//! an [`ErrorClass`] without a wildcard arm, so the compiler forces this
//! module to grow a mapping whenever a variant is added. The requeue
//! intervals are overridable `pub const`s carried from kopiur (validate on
//! the first load test):
//!
//! - [`ErrorClass::Transient`] (apiserver blip, conflict, rate limit):
//!   base 30 s, FNV-1a jittered into 30-60 s.
//! - [`ErrorClass::Structural`] (user must fix the spec or a referenced
//!   object): 300 s.
//! - [`ErrorClass::Terminal`] (retry cannot fix, e.g. RBAC denial):
//!   1800 s heartbeat.
//!
//! [`Error::should_emit_event`] suppresses Kubernetes Events for transport
//! failures so a flapping apiserver does not spam Events, while surfacing
//! the failures a human must act on.
//!
//! This module never imports `anyhow`: the [`ErrorClass`] must stay
//! reachable from the reconcile path as a concrete type, not an erased
//! `anyhow::Error`.

use std::io;
use std::time::Duration;

/// Base requeue delay for [`ErrorClass::Transient`] failures, in seconds.
// Overridable at the reconcile call sites landing in todos 18/20/21;
// until then nothing reads it.
#[allow(dead_code)]
pub const TRANSIENT_REQUEUE_BASE_SECS: u64 = 30;

/// Jitter band width for [`ErrorClass::Transient`] failures, in seconds.
///
/// The jittered delay lands in
/// `[TRANSIENT_REQUEUE_BASE_SECS, TRANSIENT_REQUEUE_BASE_SECS + TRANSIENT_REQUEUE_JITTER_SECS]`.
// Overridable at the reconcile call sites landing in todos 18/20/21;
// until then nothing reads it.
#[allow(dead_code)]
pub const TRANSIENT_REQUEUE_JITTER_SECS: u64 = 30;

/// Requeue delay for [`ErrorClass::Structural`] failures, in seconds.
// Overridable at the reconcile call sites landing in todos 18/20/21;
// until then nothing reads it.
#[allow(dead_code)]
pub const STRUCTURAL_REQUEUE_SECS: u64 = 300;

/// Requeue delay for [`ErrorClass::Terminal`] failures, in seconds
/// (slow heartbeat, never a hot loop).
// Overridable at the reconcile call sites landing in todos 18/20/21;
// until then nothing reads it.
#[allow(dead_code)]
pub const TERMINAL_REQUEUE_SECS: u64 = 1800;

/// Failure kind driving the requeue policy.
// Consumed by the reconcilers landing in todos 18/20/21; until then
// nothing reads it.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorClass {
    /// Transport/IO/apiserver-unavailable/timeout/conflict/rate-limit.
    /// Backs off gently with jitter.
    Transient,
    /// The user must fix something: validation, spec shape, unresolvable
    /// reference, missing Secret/ConfigMap. Backs off hard.
    Structural,
    /// Retry cannot fix (e.g. permission denial). Slow heartbeat only.
    Terminal,
}

/// Top-level controller error.
// New variants are constructed by the reconcilers landing in todos 18/20/21;
// until then most variants are unread outside tests.
#[allow(dead_code)]
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Kubernetes client construction failed (no usable kubeconfig or
    /// in-cluster config).
    #[error("failed to build Kubernetes client: {0}")]
    Client(#[source] kube::Error),

    /// The health/metrics HTTP server failed (bind or serve error on `:8080`).
    #[error("health server failure: {0}")]
    Server(#[source] io::Error),

    /// A Kubernetes API call made during reconciliation failed. The class
    /// is derived from the inner [`kube::Error`] (HTTP status when
    /// available, transport kind otherwise).
    #[error("kubernetes api error: {0}")]
    Api(#[source] kube::Error),

    /// The spec is structurally invalid (fails controller-side validation).
    #[error("invalid spec: {0}")]
    InvalidSpec(String),

    /// `OPERATOR_SCOPE` carries an unknown value.
    #[error("invalid OPERATOR_SCOPE {0:?}: valid values are `namespaced`, `cluster`")]
    InvalidScope(String),

    /// A value failed validation.
    #[error("validation failed: {0}")]
    Validation(String),

    /// A required spec field is missing.
    #[error("missing required field: {0}")]
    MissingField(String),

    /// A referenced object cannot be resolved.
    #[error("unresolvable reference: {0}")]
    UnresolvableRef(String),

    /// A Secret the user must create is missing.
    #[error("missing secret: {0}")]
    MissingSecret(String),

    /// A ConfigMap the user must create is missing.
    #[error("missing configmap: {0}")]
    MissingConfigMap(String),

    /// Optimistic-concurrency conflict; retrying is expected to succeed.
    #[error("conflict: {0}")]
    Conflict(String),

    /// The apiserver rate-limited the request.
    #[error("rate limited: {0}")]
    RateLimited(String),

    /// The request timed out or the connection failed.
    #[error("operation timed out: {0}")]
    Timeout(String),

    /// The controller is not allowed to do this (RBAC denial or terminal
    /// capability refusal). Retrying fast cannot fix it.
    #[error("permission denied: {0}")]
    PermissionDenied(String),

    /// The requested operation is not supported by the cluster.
    #[error("unsupported: {0}")]
    Unsupported(String),
}

// Called by the reconcilers landing in todos 18/20/21; until then the
// methods are exercised only by the unit tests below.
#[allow(dead_code)]
impl Error {
    /// Exhaustive classification. No wildcard arm: adding a variant is a
    /// compile error until it is mapped here.
    #[must_use]
    pub fn class(&self) -> ErrorClass {
        match self {
            Error::Client(inner) => classify_kube_error(inner),
            Error::Server(_) => ErrorClass::Transient,
            Error::Api(inner) => classify_kube_error(inner),
            Error::InvalidSpec(_) => ErrorClass::Structural,
            Error::InvalidScope(_) => ErrorClass::Structural,
            Error::Validation(_) => ErrorClass::Structural,
            Error::MissingField(_) => ErrorClass::Structural,
            Error::UnresolvableRef(_) => ErrorClass::Structural,
            Error::MissingSecret(_) => ErrorClass::Structural,
            Error::MissingConfigMap(_) => ErrorClass::Structural,
            Error::Conflict(_) => ErrorClass::Transient,
            Error::RateLimited(_) => ErrorClass::Transient,
            Error::Timeout(_) => ErrorClass::Transient,
            Error::PermissionDenied(_) => ErrorClass::Terminal,
            Error::Unsupported(_) => ErrorClass::Terminal,
        }
    }

    /// Requeue delay for this error.
    ///
    /// `key` must be a stable per-object key (e.g. `namespace/name`); it
    /// seeds the FNV-1a jitter so the same object always draws the same
    /// [`ErrorClass::Transient`] delay while different objects spread
    /// across the 30-60 s band instead of thundering-herding.
    #[must_use]
    pub fn requeue_after(&self, key: &str) -> Duration {
        match self.class() {
            ErrorClass::Transient => {
                let jitter = fnv1a_64(key) % (TRANSIENT_REQUEUE_JITTER_SECS + 1);
                Duration::from_secs(TRANSIENT_REQUEUE_BASE_SECS + jitter)
            }
            ErrorClass::Structural => Duration::from_secs(STRUCTURAL_REQUEUE_SECS),
            ErrorClass::Terminal => Duration::from_secs(TERMINAL_REQUEUE_SECS),
        }
    }

    /// Whether reconciliation should emit a Kubernetes Event for this error.
    ///
    /// `false` for transport/connectivity failures so a flapping apiserver
    /// does not spam Events; `true` for the failures a human must see.
    #[must_use]
    pub fn should_emit_event(&self) -> bool {
        !matches!(self.class(), ErrorClass::Transient)
    }

    /// Wrap a [`kube::Error`] from the reconcile path so it carries the
    /// correct class via [`Error::Api`].
    #[must_use]
    pub fn from_kube_error(err: kube::Error) -> Self {
        Error::Api(err)
    }
}

impl From<kube::Error> for Error {
    fn from(err: kube::Error) -> Self {
        Error::from_kube_error(err)
    }
}

/// Classify a [`kube::Error`] by HTTP status when it carries one
/// (`kube::Error::Api`), otherwise by transport kind.
///
/// Status mapping:
/// - 408 (timeout), 409 (conflict), 429 (rate limit), 5xx, and 0/unset
///   (fail open) -> [`ErrorClass::Transient`].
/// - 400/422 (rejected payload), 404/405/410-expired-shape mismatches the
///   user or the controller build must fix -> [`ErrorClass::Structural`],
///   except 410 Gone (stale `resourceVersion`), which is [`ErrorClass::Transient`].
/// - 401/403 (who you are / what you may do) -> [`ErrorClass::Terminal`].
/// - Anything else with a status -> [`ErrorClass::Transient`] (fail open,
///   back off gently rather than hot-loop or stall for 30 min).
// Called from `Error::class`; consumed by the reconcilers landing in
// todos 18/20/21.
#[allow(dead_code)]
#[must_use]
pub fn classify_kube_error(err: &kube::Error) -> ErrorClass {
    match err {
        kube::Error::Api(status) => class_for_status(status.code),
        kube::Error::HyperError(_)
        | kube::Error::Service(_)
        | kube::Error::ReadEvents(_)
        | kube::Error::UpgradeConnection(_)
        | kube::Error::RustlsTls(_)
        | kube::Error::TlsRequired
        | kube::Error::FromUtf8(_)
        | kube::Error::LinesCodecMaxLineLengthExceeded => ErrorClass::Transient,
        kube::Error::HttpError(_)
        | kube::Error::SerdeError(_)
        | kube::Error::BuildRequest(_)
        | kube::Error::InferConfig(_)
        | kube::Error::InferKubeconfig(_)
        | kube::Error::Discovery(_)
        | kube::Error::ProxyProtocolUnsupported { .. }
        | kube::Error::ProxyProtocolDisabled { .. } => ErrorClass::Structural,
        kube::Error::Auth(_) => ErrorClass::Terminal,
    }
}

/// Classify an apiserver HTTP status code. See [`classify_kube_error`].
// Consumed via `classify_kube_error` by the reconcilers landing in
// todos 18/20/21.
#[allow(dead_code)]
#[must_use]
pub fn class_for_status(code: u16) -> ErrorClass {
    match code {
        401 | 403 => ErrorClass::Terminal,
        400 | 404 | 405 | 415 | 422 => ErrorClass::Structural,
        408 | 409 | 410 | 429 => ErrorClass::Transient,
        500..=599 => ErrorClass::Transient,
        _ => ErrorClass::Transient,
    }
}

/// FNV-1a 64-bit hash. Hand-rolled so the jitter is stable across runs,
// builds, and platforms; `std::collections::hash_map::DefaultHasher` is
// deliberately NOT used (its output is not stable).
// Called from `Error::requeue_after`.
#[allow(dead_code)]
fn fnv1a_64(key: &str) -> u64 {
    const OFFSET_BASIS: u64 = 14_695_981_039_346_656_037;
    const PRIME: u64 = 1_099_511_628_211;
    let mut hash = OFFSET_BASIS;
    for byte in key.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    fn api_error_with_code(code: u16) -> Error {
        Error::from_kube_error(kube::Error::Api(Box::new(kube::core::Status {
            code,
            ..Default::default()
        })))
    }

    #[test]
    fn every_variant_maps_to_its_documented_class() {
        let io_err = || io::Error::new(io::ErrorKind::AddrInUse, "test bind");
        let cases: Vec<(Error, ErrorClass)> = vec![
            (Error::Server(io_err()), ErrorClass::Transient),
            (
                Error::InvalidScope("bogus".to_owned()),
                ErrorClass::Structural,
            ),
            (Error::InvalidSpec("bad".to_owned()), ErrorClass::Structural),
            (Error::Validation("bad".to_owned()), ErrorClass::Structural),
            (
                Error::MissingField("spec.x".to_owned()),
                ErrorClass::Structural,
            ),
            (
                Error::UnresolvableRef("ns/name".to_owned()),
                ErrorClass::Structural,
            ),
            (
                Error::MissingSecret("ns/name".to_owned()),
                ErrorClass::Structural,
            ),
            (
                Error::MissingConfigMap("ns/name".to_owned()),
                ErrorClass::Structural,
            ),
            (
                Error::Conflict("rv clash".to_owned()),
                ErrorClass::Transient,
            ),
            (
                Error::RateLimited("slow down".to_owned()),
                ErrorClass::Transient,
            ),
            (Error::Timeout("deadline".to_owned()), ErrorClass::Transient),
            (
                Error::PermissionDenied("rbac".to_owned()),
                ErrorClass::Terminal,
            ),
            (
                Error::Unsupported("old apiserver".to_owned()),
                ErrorClass::Terminal,
            ),
        ];
        for (err, want) in cases {
            assert_eq!(err.class(), want, "wrong class for {err:?}");
        }
    }

    #[test]
    fn api_status_mapping_table() {
        // (status, expected class)
        let cases: Vec<(u16, ErrorClass)> = vec![
            (408, ErrorClass::Transient),
            (409, ErrorClass::Transient),
            (410, ErrorClass::Transient),
            (429, ErrorClass::Transient),
            (500, ErrorClass::Transient),
            (502, ErrorClass::Transient),
            (503, ErrorClass::Transient),
            (400, ErrorClass::Structural),
            (404, ErrorClass::Structural),
            (405, ErrorClass::Structural),
            (415, ErrorClass::Structural),
            (422, ErrorClass::Structural),
            (401, ErrorClass::Terminal),
            (403, ErrorClass::Terminal),
            // Unset/unknown codes fail open to gentle backoff.
            (0, ErrorClass::Transient),
            (418, ErrorClass::Transient),
        ];
        for (code, want) in cases {
            let err = api_error_with_code(code);
            assert_eq!(err.class(), want, "wrong class for HTTP {code}");
            assert_eq!(
                Error::from(kube::Error::Api(Box::new(kube::core::Status {
                    code,
                    ..Default::default()
                })))
                .class(),
                want,
                "From<kube::Error> must preserve class for HTTP {code}"
            );
        }
    }

    #[test]
    fn transient_requeue_stays_in_band_and_is_deterministic() {
        let err = Error::Timeout("flap".to_owned());
        assert_eq!(err.class(), ErrorClass::Transient);
        let lo = Duration::from_secs(TRANSIENT_REQUEUE_BASE_SECS);
        let hi = Duration::from_secs(TRANSIENT_REQUEUE_BASE_SECS + TRANSIENT_REQUEUE_JITTER_SECS);
        assert_eq!((lo, hi), (Duration::from_secs(30), Duration::from_secs(60)));
        for i in 0..200 {
            let key = format!("default/obj-{i}");
            let delay = err.requeue_after(&key);
            assert!(
                (lo..=hi).contains(&delay),
                "jitter out of [30s, 60s] for {key}: {delay:?}"
            );
            assert_eq!(
                delay,
                err.requeue_after(&key),
                "same key must draw the same delay: {key}"
            );
        }
        // Distinct keys spread across the band (no single-point collapse).
        let mut distinct = std::collections::HashSet::new();
        for i in 0..200 {
            distinct.insert(err.requeue_after(&format!("default/obj-{i}")));
        }
        assert!(
            distinct.len() > 1,
            "jitter must spread distinct keys across the band"
        );
    }

    #[test]
    fn structural_and_terminal_requeues_are_exact() {
        let structural = Error::Validation("needs a human".to_owned());
        let terminal = Error::PermissionDenied("rbac".to_owned());
        for key in ["a", "default/x", "kube-system/y"] {
            assert_eq!(
                structural.requeue_after(key),
                Duration::from_secs(STRUCTURAL_REQUEUE_SECS)
            );
            assert_eq!(structural.requeue_after(key), Duration::from_secs(300));
            assert_eq!(
                terminal.requeue_after(key),
                Duration::from_secs(TERMINAL_REQUEUE_SECS)
            );
            assert_eq!(terminal.requeue_after(key), Duration::from_secs(1800));
        }
    }

    #[test]
    fn event_suppression_hides_transport_and_shows_human_actionable() {
        // Transport/connectivity: no Event spam.
        for err in [
            Error::Timeout("flap".to_owned()),
            Error::Conflict("rv clash".to_owned()),
            Error::RateLimited("slow down".to_owned()),
            Error::Server(io::Error::new(io::ErrorKind::AddrInUse, "bind")),
            api_error_with_code(500),
            api_error_with_code(503),
        ] {
            assert_eq!(err.class(), ErrorClass::Transient);
            assert!(
                !err.should_emit_event(),
                "transport error must not emit an Event: {err:?}"
            );
        }
        // Human-actionable: Events surface.
        for err in [
            Error::Validation("bad value".to_owned()),
            Error::InvalidSpec("bad shape".to_owned()),
            Error::MissingSecret("ns/name".to_owned()),
            Error::PermissionDenied("rbac".to_owned()),
            api_error_with_code(422),
            api_error_with_code(403),
        ] {
            assert!(
                err.should_emit_event(),
                "actionable error must emit an Event: {err:?}"
            );
        }
    }

    #[tokio::test]
    async fn client_display_names_the_client() {
        let result = kube::Config::from_custom_kubeconfig(
            kube::config::Kubeconfig::default(),
            &kube::config::KubeConfigOptions::default(),
        )
        .await
        .map_err(kube::Error::InferKubeconfig)
        .and_then(kube::Client::try_from);
        let kube_err = match result {
            Ok(_) => panic!("empty kubeconfig must fail client construction"),
            Err(e) => e,
        };
        let err = Error::Client(kube_err);
        let msg = err.to_string();
        assert!(
            msg.contains("Kubernetes client"),
            "client Display must name the client, got: {msg}"
        );
    }

    #[test]
    fn server_display_names_the_server() {
        let err = Error::Server(io::Error::new(io::ErrorKind::AddrInUse, "test bind"));
        let msg = err.to_string();
        assert!(
            msg.contains("health server"),
            "server Display must name the server, got: {msg}"
        );
    }
}
