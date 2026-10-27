//! Integration tests for `cargo xtask gen-rbac`.
//!
//! Chosen location: `crates/xtask/tests/gen_rbac.rs` (integration tests
//! driving the real binary via `CARGO_BIN_EXE_xtask`), mirroring
//! `tests/gen_crds.rs`, so the tests exercise the shipped binary end to
//! end.

use std::collections::BTreeSet;
use std::path::PathBuf;

use k8s_openapi::api::rbac::v1::ClusterRole;

const EXPECTED_CRD_GROUP: &str = "hydradb.kubexa.tech";

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .map(std::path::Path::to_path_buf)
        .expect("xtask lives two levels below the workspace root")
}

fn rbac_dir() -> PathBuf {
    workspace_root().join("deploy/rbac")
}

fn run_gen_rbac() {
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_xtask"))
        .arg("gen-rbac")
        .status()
        .expect("spawn xtask gen-rbac");
    assert!(status.success(), "gen-rbac exited non-zero: {status}");
}

fn yaml_files(dir: &std::path::Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|err| panic!("read {}: {err}", dir.display()))
        .map(|entry| entry.expect("dir entry").path())
        .filter(|path| {
            path.extension().is_some_and(|ext| ext == "yaml")
                && path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with("operator-"))
        })
        .collect();
    files.sort();
    files
}

fn read_raw(path: &std::path::Path) -> String {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
    assert!(
        text.ends_with('\n') && !text.ends_with("\n\n"),
        "{} must end with exactly one newline",
        path.display()
    );
    text
}

fn parse_role(path: &std::path::Path) -> (serde_json::Value, ClusterRole) {
    let text = read_raw(path);
    let value: serde_json::Value = serde_saphyr::from_str(&text)
        .unwrap_or_else(|err| panic!("parse {} as YAML value: {err}", path.display()));
    assert_eq!(
        value
            .get("kind")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("<PII type=\"CASE_ID\" id=\"58\"/>"),
        "ClusterRole",
        "{} must be a ClusterRole",
        path.display()
    );
    let role: ClusterRole = serde_saphyr::from_str(&text)
        .unwrap_or_else(|err| panic!("parse {} as ClusterRole: {err}", path.display()));
    (value, role)
}

fn all_roles() -> Vec<(PathBuf, ClusterRole)> {
    run_gen_rbac();
    let dir = rbac_dir();
    let files = yaml_files(&dir);
    assert!(
        !files.is_empty(),
        "expected at least one operator-*.yaml in {}",
        dir.display()
    );
    files
        .iter()
        .map(|path| {
            let (_, role) = parse_role(path);
            (path.clone(), role)
        })
        .collect()
}

fn rule_index(roles: &[(PathBuf, ClusterRole)]) -> BTreeSet<(String, String, Vec<String>)> {
    let mut index = BTreeSet::new();
    for (path, role) in roles {
        let rules = role.rules.clone().unwrap_or_default();
        assert!(!rules.is_empty(), "{} carries no rules", path.display());
        for rule in rules {
            let groups = rule.api_groups.clone().unwrap_or_default();
            let resources = rule.resources.clone().unwrap_or_default();
            assert!(
                !resources.is_empty(),
                "{} has a rule with no resources",
                path.display()
            );
            assert!(
                !rule.verbs.is_empty(),
                "{} has a rule with no verbs",
                path.display()
            );
            for group in groups {
                for resource in &resources {
                    index.insert((group.clone(), resource.clone(), rule.verbs.clone()));
                }
            }
        }
    }
    index
}

#[test]
fn gen_rbac_emits_no_wildcard_resource_or_verb() {
    let roles = all_roles();
    for (path, role) in &roles {
        for rule in role.rules.clone().unwrap_or_default() {
            for group in rule.api_groups.clone().unwrap_or_default() {
                // `apiGroups: [""]` is the core group, not a wildcard.
                assert_ne!(group, "*", "{} grants wildcard apiGroup", path.display());
            }
            for resource in rule.resources.clone().unwrap_or_default() {
                assert_ne!(resource, "*", "{} grants wildcard resource", path.display());
            }
            for verb in &rule.verbs {
                assert_ne!(verb, "*", "{} grants wildcard verb", path.display());
            }
        }
    }
}

#[test]
fn gen_rbac_covers_the_three_crds_with_patchable_status() {
    let roles = all_roles();
    let index = rule_index(&roles);
    let crds = ["graphclusters", "graphnodepools", "graphindexers"];
    for plural in crds {
        let entry = index
            .iter()
            .find(|(group, resource, _)| group == EXPECTED_CRD_GROUP && resource == plural)
            .unwrap_or_else(|| panic!("missing CRD rule for {plural}"));
        for verb in ["get", "list", "watch", "update", "patch"] {
            assert!(
                entry.2.iter().any(|have| have == verb),
                "{plural} rule lacks verb {verb}"
            );
        }
        let status = format!("{plural}/status");
        let status_entry = index
            .iter()
            .find(|(group, resource, _)| group == EXPECTED_CRD_GROUP && *resource == status)
            .unwrap_or_else(|| panic!("missing status rule for {status}"));
        assert!(
            status_entry.2.iter().any(|have| have == "patch"),
            "{status} rule lacks verb patch"
        );
    }
}

#[test]
fn gen_rbac_covers_every_child_kind_in_its_api_group() {
    let roles = all_roles();
    let index = rule_index(&roles);
    let expected: &[(&str, &str)] = &[
        ("apps", "statefulsets"),
        ("apps", "deployments"),
        ("", "services"),
        ("", "configmaps"),
        ("", "secrets"),
        ("policy", "poddisruptionbudgets"),
        ("networking.k8s.io", "networkpolicies"),
        ("monitoring.coreos.com", "servicemonitors"),
        ("", "events"),
        ("coordination.k8s.io", "leases"),
        ("objectbucket.io", "objectbucketclaims"),
    ];
    for (group, resource) in expected {
        assert!(
            index
                .iter()
                .any(|(have_group, have_resource, _)| have_group == group
                    && have_resource == resource),
            "missing rule for {group:?}/{resource}"
        );
    }
}

#[test]
fn gen_rbac_files_carry_the_generated_header() {
    run_gen_rbac();
    for path in yaml_files(&rbac_dir()) {
        let text = read_raw(&path);
        assert!(
            text.starts_with("# SPDX-License-Identifier: Apache-2.0\n"),
            "{} must start with the SPDX license header",
            path.display()
        );
        assert!(
            text.contains("# GENERATED"),
            "{} must carry the generated-header",
            path.display()
        );
        assert!(
            text.contains("cargo xtask gen-rbac"),
            "{} header must name the regeneration command",
            path.display()
        );
        assert!(
            text.contains("do not hand-edit"),
            "{} header must forbid hand edits",
            path.display()
        );
    }
}

#[test]
fn gen_rbac_is_idempotent() {
    run_gen_rbac();
    let snapshot: Vec<(PathBuf, String)> = yaml_files(&rbac_dir())
        .iter()
        .map(|path| {
            let bytes = std::fs::read_to_string(path)
                .unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
            (path.clone(), bytes)
        })
        .collect();
    run_gen_rbac();
    for (path, before) in &snapshot {
        let after = std::fs::read_to_string(path)
            .unwrap_or_else(|err| panic!("re-read {}: {err}", path.display()));
        assert_eq!(*before, after, "{} changed between runs", path.display());
    }
}
