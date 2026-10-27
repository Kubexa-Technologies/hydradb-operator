//! Operator metrics registry and Prometheus text exposition.
//!
//! Hand-rolled counters backed by a process-global [`Mutex`]: the plan calls
//! this "quick", so no external metrics dependency is pulled in. The
//! [`render`] output backs the `/metrics` endpoint served by [`crate::run`]
//! and stays valid Prometheus text exposition (`# HELP`/`# TYPE` + samples).
//!
//! INSTRUMENTATION CALL SITES (todos 18-24 own these; the reconcilers do not
//! exist yet): each reconciler calls [`record_reconcile`] once per
//! reconciliation with its `kind`, the elapsed [`Duration`], and the outcome
//! mapped through [`ErrorClass`](crate::error::ErrorClass); each watcher
//! calls [`record_watcher_event`] per received event; every reconcile path
//! opens [`reconcile_span`] so logs carry `kind`/`namespace`/`name`.

use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use crate::error::ErrorClass;

/// Upper bounds (seconds) of the reconcile-duration histogram buckets.
const DURATION_BUCKETS: [f64; 11] = [
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];

/// Per-kind reconcile counters and duration observations.
#[derive(Debug, Default)]
struct KindStats {
    reconciles: u64,
    errors: BTreeMap<&'static str, u64>,
    duration_bucket_counts: [u64; 11],
    duration_sum: f64,
    duration_count: u64,
    watcher_events: BTreeMap<&'static str, u64>,
}

/// Process-global registry. Keys are leaked to `&'static str` so recording
/// never allocates on the hot path beyond the map node itself.
#[derive(Debug, Default)]
struct Registry {
    kinds: BTreeMap<&'static str, KindStats>,
    /// Gauge families: metric name -> rendered label set -> value.
    ///
    /// Gauges are absolute observations (index staleness, last-backup
    /// timestamp), so they are overwritten rather than accumulated.
    gauges: BTreeMap<String, BTreeMap<String, f64>>,
}

fn registry() -> &'static Mutex<Registry> {
    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(Registry::default()))
}

fn lock() -> std::sync::MutexGuard<'static, Registry> {
    registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Record an absolute gauge value.
///
/// `labels` is rendered in the order given; callers pass a stable order so two
/// reconciles of the same object produce byte-identical exposition.
pub fn set_gauge(name: &str, labels: &[(&str, &str)], value: f64) {
    let mut reg = lock();
    let rendered = labels
        .iter()
        .map(|(key, value)| format!("{key}=\"{}\"", escape_label(value)))
        .collect::<Vec<_>>()
        .join(",");
    reg.gauges
        .entry(name.to_owned())
        .or_default()
        .insert(rendered, value);
}

/// Read a gauge sample back, keyed by the same label set used to set it.
#[must_use]
pub fn gauge_value(name: &str, labels: &[(&str, &str)]) -> Option<f64> {
    let rendered = labels
        .iter()
        .map(|(key, value)| format!("{key}=\"{}\"", escape_label(value)))
        .collect::<Vec<_>>()
        .join(",");
    let reg = lock();
    reg.gauges
        .get(name)
        .and_then(|samples| samples.get(&rendered))
        .copied()
}

/// Drop every gauge whose family name starts with `prefix`.
///
/// Called when a child disappears so a stale sample cannot keep an alert firing
/// after the object it described was deleted.
pub fn clear_gauges_with_prefix(prefix: &str) {
    let mut reg = lock();
    reg.gauges.retain(|name, _| !name.starts_with(prefix));
}

// Called by the reconcilers landing in todos 18-24; until then only tests
// record outcomes.
#[allow(dead_code)]
fn intern(s: &str) -> &'static str {
    Box::leak(s.to_owned().into_boxed_str())
}

// Called by the reconcilers landing in todos 18-24; until then only tests
// record outcomes.
#[allow(dead_code)]
fn class_label(class: &ErrorClass) -> &'static str {
    match class {
        ErrorClass::Transient => "transient",
        ErrorClass::Structural => "structural",
        ErrorClass::Terminal => "terminal",
    }
}

/// Escape a Prometheus label value (`\`, `"`, newline).
fn escape_label(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            _ => out.push(c),
        }
    }
    out
}

/// Record one finished reconciliation.
///
/// `outcome` is `Ok(())` on success or `Err(class)` with the
/// [`ErrorClass`](crate::error::ErrorClass) of the failure, so the
/// `hydradb_operator_reconcile_errors_total{kind,class}` counter carries the
/// class label. Called once per reconcile by the reconcilers landing in
/// todos 18-24; until then only tests and examples call it.
// Called once per reconcile by the reconcilers landing in todos 18-24;
// until then only tests and examples call it.
#[allow(dead_code)]
pub fn record_reconcile(kind: &str, duration: Duration, outcome: Result<(), ErrorClass>) {
    let mut reg = lock();
    let stats = reg.kinds.entry(intern(kind)).or_default();
    stats.reconciles = stats.reconciles.saturating_add(1);
    if let Err(class) = outcome {
        let counter = stats.errors.entry(class_label(&class)).or_insert(0);
        *counter = counter.saturating_add(1);
    }
    let secs = duration.as_secs_f64();
    for (i, bound) in DURATION_BUCKETS.iter().enumerate() {
        if secs <= *bound {
            stats.duration_bucket_counts[i] = stats.duration_bucket_counts[i].saturating_add(1);
        }
    }
    stats.duration_sum += secs;
    stats.duration_count = stats.duration_count.saturating_add(1);
}

/// Record one watcher event (e.g. `applied`, `deleted`, `resync`) for `kind`.
///
/// Called by the watch handlers; todos 18-24 own the call sites.
// Called by the watch handlers; todos 18-24 own the call sites.
#[allow(dead_code)]
pub fn record_watcher_event(kind: &str, event: &str) {
    let mut reg = lock();
    let stats = reg.kinds.entry(intern(kind)).or_default();
    let counter = stats.watcher_events.entry(intern(event)).or_insert(0);
    *counter = counter.saturating_add(1);
}

/// Reconcile-scoped span carrying `kind`/`namespace`/`name`.
///
/// Todos 18-24 open this span at the top of every reconcile so structured
/// `tracing` logs instrument consistently.
// Todos 18-24 open this span at the top of every reconcile so structured
// `tracing` logs instrument consistently.
#[allow(dead_code)]
pub fn reconcile_span(kind: &str, namespace: &str, name: &str) -> tracing::Span {
    tracing::info_span!(
        "reconcile",
        kind = %kind,
        namespace = %namespace,
        name = %name,
    )
}

/// Render the registry as Prometheus text exposition.
///
/// Always carries `hydradb_operator_up 1` (the process is serving while this
/// can run) plus every counter/observation recorded so far.
#[must_use]
pub fn render() -> String {
    let reg = lock();
    let mut out = String::new();
    out.push_str("# HELP hydradb_operator_up Whether the operator process is up.\n");
    out.push_str("# TYPE hydradb_operator_up gauge\n");
    out.push_str("hydradb_operator_up 1\n");
    out.push_str("# HELP hydradb_operator_reconciles_total Total reconciliations by kind.\n");
    out.push_str("# TYPE hydradb_operator_reconciles_total counter\n");
    for (kind, stats) in &reg.kinds {
        out.push_str(&format!(
            "hydradb_operator_reconciles_total{{kind=\"{}\"}} {}\n",
            escape_label(kind),
            stats.reconciles
        ));
    }
    out.push_str(
        "# HELP hydradb_operator_reconcile_errors_total Reconcile errors by kind and error class.\n",
    );
    out.push_str("# TYPE hydradb_operator_reconcile_errors_total counter\n");
    for (kind, stats) in &reg.kinds {
        for (class, count) in &stats.errors {
            out.push_str(&format!(
                "hydradb_operator_reconcile_errors_total{{kind=\"{}\",class=\"{}\"}} {}\n",
                escape_label(kind),
                escape_label(class),
                count
            ));
        }
    }
    out.push_str(
        "# HELP hydradb_operator_reconcile_duration_seconds Reconcile duration in seconds.\n",
    );
    out.push_str("# TYPE hydradb_operator_reconcile_duration_seconds histogram\n");
    for (kind, stats) in &reg.kinds {
        // Each observation bumps every bucket it fits, so the stored
        // counts are already cumulative and render directly.
        for (i, bound) in DURATION_BUCKETS.iter().enumerate() {
            out.push_str(&format!(
                "hydradb_operator_reconcile_duration_seconds_bucket{{kind=\"{}\",le=\"{}\"}} {}\n",
                escape_label(kind),
                bound,
                stats.duration_bucket_counts[i]
            ));
        }
        out.push_str(&format!(
            "hydradb_operator_reconcile_duration_seconds_bucket{{kind=\"{}\",le=\"+Inf\"}} {}\n",
            escape_label(kind),
            stats.duration_count
        ));
        out.push_str(&format!(
            "hydradb_operator_reconcile_duration_seconds_sum{{kind=\"{}\"}} {}\n",
            escape_label(kind),
            stats.duration_sum
        ));
        out.push_str(&format!(
            "hydradb_operator_reconcile_duration_seconds_count{{kind=\"{}\"}} {}\n",
            escape_label(kind),
            stats.duration_count
        ));
    }
    for (family, samples) in &reg.gauges {
        out.push_str(&format!("# TYPE {family} gauge\n"));
        for (labels, value) in samples {
            if labels.is_empty() {
                out.push_str(&format!("{family} {value}\n"));
            } else {
                out.push_str(&format!("{family}{{{labels}}} {value}\n"));
            }
        }
    }
    out.push_str(
        "# HELP hydradb_operator_watcher_events_total Watcher events by kind and event.\n",
    );
    out.push_str("# TYPE hydradb_operator_watcher_events_total counter\n");
    for (kind, stats) in &reg.kinds {
        for (event, count) in &stats.watcher_events {
            out.push_str(&format!(
                "hydradb_operator_watcher_events_total{{kind=\"{}\",event=\"{}\"}} {}\n",
                escape_label(kind),
                escape_label(event),
                count
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconcile_counter_increments() {
        record_reconcile("GraphClusterCounter", Duration::from_millis(5), Ok(()));
        let body = render();
        assert!(
            body.contains("hydradb_operator_reconciles_total{kind=\"GraphClusterCounter\"} 1"),
            "reconcile counter carries the kind, got:\n{body}"
        );
    }

    #[test]
    fn error_counter_carries_the_error_class() {
        record_reconcile(
            "GraphClusterErrClass",
            Duration::from_millis(5),
            Err(ErrorClass::Transient),
        );
        let body = render();
        assert!(
            body.contains(
                "hydradb_operator_reconcile_errors_total{kind=\"GraphClusterErrClass\",class=\"transient\"} 1"
            ),
            "error counter carries kind + class, got:\n{body}"
        );
    }

    #[test]
    fn render_is_valid_prometheus_text() {
        record_reconcile("GraphClusterValidity", Duration::from_millis(5), Ok(()));
        record_watcher_event("GraphClusterValidity", "applied");
        let body = render();
        let mut type_lines: Vec<&str> = Vec::new();
        let mut sample_families: Vec<String> = Vec::new();
        for line in body.lines() {
            if line.starts_with("# HELP ") {
                continue;
            }
            if let Some(rest) = line.strip_prefix("# TYPE ") {
                let name = rest.split_whitespace().next().unwrap_or_default();
                assert!(!name.is_empty(), "TYPE line names a metric: {line}");
                type_lines.push(Box::leak(name.to_owned().into_boxed_str()));
                continue;
            }
            if line.starts_with('#') || line.trim().is_empty() {
                continue;
            }
            let (name, value) = line.split_once(' ').unwrap_or_default();
            assert!(!name.is_empty(), "sample line has a metric name: {line}");
            assert!(!value.is_empty(), "sample line has a value: {line}");
            value.parse::<f64>().unwrap_or_else(|_| {
                panic!("sample value parses as f64: {line}");
            });
            let family = name
                .split_once('{')
                .map_or(name, |(base, _)| base)
                .trim_end_matches("_bucket")
                .trim_end_matches("_sum")
                .trim_end_matches("_count")
                .to_owned();
            sample_families.push(family);
        }
        assert!(!sample_families.is_empty(), "render emits samples");
        for family in sample_families {
            assert!(
                type_lines.contains(&family.as_str()),
                "metric {family} has a # TYPE line, got:\n{body}"
            );
        }
    }
}
