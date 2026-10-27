//! Shared watcher configuration for every controller.
//!
//! ONE builder owns every `.owns()` / `.watches()` / `Controller::new()`
//! watch in this operator. Callers take a [`WatcherSupport`] (probed once at
//! startup, see [`probe_watcher_support`]) and get back a config that always
//! carries [`watcher::ListSemantic::Any`]. No other module may build its own
//! watch config: kube-rs shares no informers between `Controller`s, so every
//! watch is its own LIST+WATCH, and a bare default config regresses cold
//! start into an etcd QUORUM read per watch (kube-runtime defaults to
//! `MostRecent`).
//!
//! # kube 4.2 API split
//!
//! Debounce and concurrency do NOT live on [`watcher::Config`] in kube 4.2:
//! they are [`controller::Config`] options applied to the `Controller` that
//! consumes the watch. This module therefore ships two builders that must be
//! used together: [`watcher_config`] (list semantics, streaming lists, label
//! filter) and [`controller_config`] (250 ms trailing-edge debounce, per-
//! controller concurrency cap).
//!
//! # Fail-open / fail-closed policy
//!
//! Streaming lists are version-gated on apiserver >= 1.32 (see kube issue
//! #1844: with streaming lists the watch stream re-sends every object on
//! each ~290 s reconnect, which our idempotent reconcilers absorb). The
//! gate decision is the pure function [`streaming_lists_enabled`]:
//!
//! - probe answers with a version >= 1.32 -> streaming ON.
//! - probe answers with a version < 1.32 -> streaming OFF (fail CLOSED: a
//!   real "too old" answer means the server cannot do streaming lists).
//! - probe answers with an unparsable version -> streaming OFF (fail
//!   CLOSED: a real answer we cannot confirm is treated like "too old").
//! - probe FAILS with a transport/timeout error -> streaming ON (fail OPEN:
//!   refusing streaming exactly during congestion would push every watcher
//!   onto the more expensive paged path when the apiserver is already
//!   struggling).
//!
//! The probe itself is bounded by [`PROBE_TIMEOUT`] so it can never hang
//! startup, runs exactly once, and the outcome is logged once via
//! `tracing::info!` inside [`probe_watcher_support`]. The resulting
//! [`WatcherSupport`] is passed by reference to every config builder below;
//! watches never re-probe.
//!
//! # Why `Any` is safe here
//!
//! Reconcilers are level-triggered and idempotent, and the watch stream
//! corrects any staleness from the cached read — the same trade client-go
//! informers make by default.

// Builders below are consumed by the reconcilers landing in todos 18/20/21
// and the controller wiring in run.rs; until then nothing calls them.
#![allow(dead_code)]

use std::time::Duration;

use kube::api::PartialObjectMeta;
use kube::runtime::{controller, watcher};
use kube::{Api, Client, Resource};

/// Trailing-edge debounce applied to every controller.
///
/// Deduplicates the reconcile-request burst each ~290 s watch reconnect
/// produces (kube issue #1844 resends every object on reconnect when
/// streaming lists are on). Carried from kopiur; validate on first load test.
pub const RECONCILE_DEBOUNCE: Duration = Duration::from_millis(250);

/// Maximum concurrent reconciliations per controller.
///
/// The controller still never reconciles the same object concurrently.
/// Carried from kopiur; validate on first load test.
pub const CONTROLLER_CONCURRENCY: u16 = 16;

/// Upper bound for the single apiserver version probe at startup.
///
/// The probe must never hang startup; on timeout we fail OPEN (see
/// [`streaming_lists_enabled`]).
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Minimum Kubernetes minor version with stable streaming lists.
///
/// Streaming lists went GA in 1.32; older servers get the paged LIST+WATCH
/// path.
pub const MIN_STREAMING_MINOR: u32 = 32;

/// Parsed apiserver server version (major.minor).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerVersion {
    /// Major version (1 for all Kubernetes releases to date).
    pub major: u32,
    /// Minor version (32 means 1.32).
    pub minor: u32,
}

/// How the single startup version probe can fail.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProbeError {
    /// The probe never got an answer: connection refused, TLS/DNS failure,
    /// or the [`PROBE_TIMEOUT`] bound hit. Fails OPEN.
    #[error("apiserver version probe transport failure: {0}")]
    Transport(String),
    /// The server answered but major/minor did not parse. Fails CLOSED.
    #[error("apiserver version probe returned unparsable version: {0}")]
    Unparsable(String),
}

/// Cached outcome of the startup version probe.
///
/// Built once by [`probe_watcher_support`], logged once there, then shared
/// by reference with every watch builder. Watches never re-probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WatcherSupport {
    /// Whether watches may use streaming lists.
    pub streaming_lists: bool,
    /// The parsed server version, when the probe got an answer.
    pub server: Option<ServerVersion>,
}

/// Pure gate: decide streaming lists from one probe outcome.
///
/// - `Ok` version >= 1.32 -> `true`.
/// - `Ok` version < 1.32 -> `false` (fail CLOSED on a real "too old").
/// - `Err(Transport)` -> `true` (fail OPEN: never push every watcher onto
///   the expensive paged path exactly during congestion).
/// - `Err(Unparsable)` -> `false` (fail CLOSED: real answer, unconfirmed).
pub fn streaming_lists_enabled(probe: &Result<ServerVersion, ProbeError>) -> bool {
    match probe {
        Ok(version) => (version.major, version.minor) >= (1, MIN_STREAMING_MINOR),
        Err(ProbeError::Transport(_)) => true,
        Err(ProbeError::Unparsable(_)) => false,
    }
}

/// Parse raw `/version` major/minor strings into a [`ServerVersion`].
///
/// Tolerates the `+` suffix real clusters report (e.g. minor `"32+"`) and
/// surrounding whitespace by reading the leading digit run. Anything else
/// is [`ProbeError::Unparsable`].
///
/// # Errors
///
/// Returns [`ProbeError::Unparsable`] when either component has no leading
/// digit run.
pub fn parse_server_version(major: &str, minor: &str) -> Result<ServerVersion, ProbeError> {
    fn leading_digits(s: &str) -> Option<u32> {
        let digits: String = s.trim().chars().take_while(|c| c.is_numeric()).collect();
        if digits.is_empty() {
            None
        } else {
            digits.parse().ok()
        }
    }
    match (leading_digits(major), leading_digits(minor)) {
        (Some(major), Some(minor)) => Ok(ServerVersion { major, minor }),
        _ => Err(ProbeError::Unparsable(format!(
            "major={major:?} minor={minor:?}"
        ))),
    }
}

/// Probe the apiserver version once, log the outcome once, cache it.
///
/// Bounded by [`PROBE_TIMEOUT`]; on timeout the probe fails OPEN. Call this
/// a single time at startup and hand the returned [`WatcherSupport`] to
/// every [`watcher_config`] / [`owned_watcher_config`] call.
pub async fn probe_watcher_support(client: &Client) -> WatcherSupport {
    let probe = bounded_probe(client).await;
    let streaming_lists = streaming_lists_enabled(&probe);
    match &probe {
        Ok(version) => tracing::info!(
            major = version.major,
            minor = version.minor,
            streaming_lists,
            "apiserver version probe succeeded"
        ),
        Err(error) => tracing::info!(
            error = %error,
            streaming_lists,
            "apiserver version probe failed; failing open to streaming lists"
        ),
    }
    WatcherSupport {
        streaming_lists,
        server: probe.ok(),
    }
}

/// Run the version probe under the [`PROBE_TIMEOUT`] bound.
async fn bounded_probe(client: &Client) -> Result<ServerVersion, ProbeError> {
    let info = tokio::time::timeout(PROBE_TIMEOUT, client.apiserver_version())
        .await
        .map_err(|_| ProbeError::Transport(format!("probe timed out after {PROBE_TIMEOUT:?}")))?
        .map_err(|error| ProbeError::Transport(error.to_string()))?;
    parse_server_version(&info.major, &info.minor)
}

/// Build the shared watch config: ALWAYS `ListSemantic::Any`.
///
/// Streaming lists follow the cached [`WatcherSupport`]; label filtering is
/// absent here (see [`owned_watcher_config`] for the label-filtered owned
/// variant). The struct is built field-by-field so `Any` can never be lost
/// to a bare default.
pub fn watcher_config(support: &WatcherSupport) -> watcher::Config {
    watcher::Config {
        list_semantic: watcher::ListSemantic::Any,
        initial_list_strategy: if support.streaming_lists {
            watcher::InitialListStrategy::StreamingList
        } else {
            watcher::InitialListStrategy::ListWatch
        },
        label_selector: None,
        field_selector: None,
        timeout: None,
        // Same page-size default client-go uses; only used on the paged path.
        page_size: Some(500),
        bookmarks: true,
    }
}

/// Build the label-filtered watch config for `.owns()` child watches.
///
/// `label_selector` is the child selector label every child carries (see the
/// apply layer); streaming follows the cached [`WatcherSupport`].
pub fn owned_watcher_config(label_selector: &str, support: &WatcherSupport) -> watcher::Config {
    let mut config = watcher_config(support);
    config.label_selector = Some(label_selector.to_owned());
    config
}

/// Build the shared controller config: 250 ms debounce + concurrency cap.
///
/// Pair with [`watcher_config`]: pass the watch config to
/// `Controller::new` / `.owns()` / `.watches()`, and this config to
/// `.with_config()`. Built from [`RECONCILE_DEBOUNCE`] and
/// [`CONTROLLER_CONCURRENCY`].
pub fn controller_config() -> controller::Config {
    let base: controller::Config = Default::default();
    base.debounce(RECONCILE_DEBOUNCE)
        .concurrency(CONTROLLER_CONCURRENCY)
}

/// Scope-aware metadata-only handle for watches whose reconciler does not
/// need the full object.
///
/// Backs `.watches()` over `Api<PartialObjectMeta<K>>`: the informer ships
/// only `ObjectMeta`, cutting LIST payload for large child sets. The caller
/// re-reads the full object inside `reconcile` when it needs the spec.
pub fn metadata_api<K>(
    client: &Client,
    scope: crate::context::WatchScope,
    namespace: &str,
) -> Api<PartialObjectMeta<K>>
where
    K: Resource<Scope = kube::core::NamespaceResourceScope>,
    K::DynamicType: Default,
{
    scope.api(client.clone(), namespace)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn streaming_on() -> WatcherSupport {
        WatcherSupport {
            streaming_lists: true,
            server: Some(ServerVersion {
                major: 1,
                minor: 32,
            }),
        }
    }

    fn streaming_off() -> WatcherSupport {
        WatcherSupport {
            streaming_lists: false,
            server: Some(ServerVersion {
                major: 1,
                minor: 31,
            }),
        }
    }

    #[test]
    fn watch_config_always_uses_any_semantic() {
        for support in [streaming_on(), streaming_off()] {
            assert_eq!(
                watcher_config(&support).list_semantic,
                watcher::ListSemantic::Any,
                "list semantic must stay Any regardless of streaming gate"
            );
            assert_eq!(
                owned_watcher_config("hydradb.kubexa.tech/cluster=x", &support).list_semantic,
                watcher::ListSemantic::Any,
                "owned watches must stay Any as well"
            );
        }
    }

    #[test]
    fn watch_config_follows_cached_streaming_gate() {
        assert_eq!(
            watcher_config(&streaming_on()).initial_list_strategy,
            watcher::InitialListStrategy::StreamingList
        );
        assert_eq!(
            watcher_config(&streaming_off()).initial_list_strategy,
            watcher::InitialListStrategy::ListWatch
        );
    }

    #[test]
    fn owned_watch_config_carries_label_selector() {
        let config = owned_watcher_config("hydradb.kubexa.tech/cluster=demo", &streaming_on());
        assert_eq!(
            config.label_selector.as_deref(),
            Some("hydradb.kubexa.tech/cluster=demo")
        );
        assert_eq!(watcher_config(&streaming_on()).label_selector, None);
    }

    #[test]
    fn gate_opens_for_new_servers() {
        for minor in [32, 33, 40] {
            let probe = Ok(ServerVersion { major: 1, minor });
            assert!(
                streaming_lists_enabled(&probe),
                "1.{minor} must enable streaming lists"
            );
        }
    }

    #[test]
    fn gate_closes_for_old_servers() {
        for minor in [27, 28, 31] {
            let probe = Ok(ServerVersion { major: 1, minor });
            assert!(
                !streaming_lists_enabled(&probe),
                "1.{minor} must NOT enable streaming lists"
            );
        }
    }

    #[test]
    fn gate_fails_open_on_transport_error() {
        let probe: Result<ServerVersion, ProbeError> =
            Err(ProbeError::Transport("connection refused".to_owned()));
        assert!(
            streaming_lists_enabled(&probe),
            "transport failure must fail OPEN to streaming lists"
        );
        let timed_out: Result<ServerVersion, ProbeError> =
            Err(ProbeError::Transport("probe timed out".to_owned()));
        assert!(
            streaming_lists_enabled(&timed_out),
            "probe timeout must fail OPEN to streaming lists"
        );
    }

    #[test]
    fn gate_fails_closed_on_unparsable_version() {
        let probe: Result<ServerVersion, ProbeError> =
            Err(ProbeError::Unparsable("major=\"\" minor=\"\"".to_owned()));
        assert!(
            !streaming_lists_enabled(&probe),
            "unparsable version must fail CLOSED"
        );
    }

    #[test]
    fn version_parser_tolerates_distro_suffix() {
        assert_eq!(
            parse_server_version("1", "32+"),
            Ok(ServerVersion {
                major: 1,
                minor: 32
            })
        );
        assert_eq!(
            parse_server_version(" 1 ", " 31 "),
            Ok(ServerVersion {
                major: 1,
                minor: 31
            })
        );
    }

    #[test]
    fn version_parser_rejects_garbage() {
        assert!(parse_server_version("", "").is_err());
        assert!(parse_server_version("one", "thirty-two").is_err());
    }

    #[test]
    fn debounce_is_250_ms() {
        assert_eq!(RECONCILE_DEBOUNCE, Duration::from_millis(250));
    }

    #[test]
    fn concurrency_cap_is_documented_constant() {
        assert_eq!(CONTROLLER_CONCURRENCY, 16);
        // The builder must accept the constant without further tuning.
        let _config = controller_config();
    }

    #[test]
    fn probe_timeout_bounds_startup() {
        assert_eq!(PROBE_TIMEOUT, Duration::from_secs(5));
    }
}
