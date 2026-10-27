//! The chart's RBAC must grant exactly what the controller applies.
//!
//! This is a regression guard: the rendered `deploy/rbac/operator-clusterrole.yaml`
//! is generated, but `charts/hydradb-operator/templates/rbac.yaml` is a separate
//! file that has silently drifted before. A missing rule here means every apply
//! of that kind is `Forbidden` at runtime, which classifies as transient and
//! retries forever.

use std::fs;
use std::path::PathBuf;

fn chart_rbac() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("charts/hydradb-operator/templates/rbac.yaml");
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

#[test]
fn chart_rbac_grants_every_child_kind_the_controller_applies() {
    let rbac = chart_rbac();
    for resource in [
        // CRDs (and their status subresource).
        "graphclusters",
        "graphnodepools",
        "graphindexers",
        "graphbackups",
        "graphingestsources",
        // Core children.
        "pods",
        "secrets",
        "configmaps",
        "services",
        "persistentvolumeclaims",
        "events",
        // Workload + policy children.
        "statefulsets",
        "deployments",
        "poddisruptionbudgets",
        "networkpolicies",
        // Optional ecosystem children.
        "servicemonitors",
        "prometheusrules",
        "certificates",
        "horizontalpodautoscalers",
        // Batch children of GraphBackup / GraphIngestSource.
        "jobs",
        "cronjobs",
        "leases",
    ] {
        assert!(
            rbac.contains(resource),
            "chart rbac.yaml is missing `{resource}`: the controller applies it and would get Forbidden"
        );
    }
}

#[test]
fn chart_rbac_names_every_api_group_it_uses() {
    let rbac = chart_rbac();
    for group in [
        "hydradb.helmforge.dev",
        "apps",
        "policy",
        "networking.k8s.io",
        "monitoring.coreos.com",
        "cert-manager.io",
        "autoscaling",
        "batch",
        "coordination.k8s.io",
    ] {
        assert!(
            rbac.contains(group),
            "chart rbac.yaml is missing group `{group}`"
        );
    }
}
