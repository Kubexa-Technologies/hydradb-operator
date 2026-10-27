//! Integration tests for `cargo xtask gen-crds`.
//!
//! Chosen location: `crates/xtask/tests/gen_crds.rs` (integration tests driving
//! the real binary via `CARGO_BIN_EXE_xtask`), not an inline `#[cfg(test)]`
//! module, so the tests exercise the shipped binary end to end.

use std::collections::BTreeSet;
use std::path::PathBuf;

use hydradb_operator_api::crd::{
    GraphBackup, GraphCluster, GraphIndexer, GraphIngestSource, GraphNodePool,
};
use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
use kube::CustomResourceExt;

const EXPECTED_GROUP: &str = "hydradb.kubexa.tech";

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .map(std::path::Path::to_path_buf)
        .expect("xtask lives two levels below the workspace root")
}

fn crd_dirs() -> [PathBuf; 2] {
    let root = workspace_root();
    [
        root.join("deploy/crds"),
        root.join("charts/hydradb-operator/crds"),
    ]
}

fn run_gen_crds() {
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_xtask"))
        .arg("gen-crds")
        .status()
        .expect("spawn xtask gen-crds");
    assert!(status.success(), "gen-crds exited non-zero: {status}");
}

fn yaml_files(dir: &std::path::Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|err| panic!("read {}: {err}", dir.display()))
        .map(|entry| entry.expect("dir entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "yaml"))
        .collect();
    files.sort();
    files
}

fn parse_crd(path: &std::path::Path) -> CustomResourceDefinition {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
    assert!(
        text.ends_with('\n') && !text.ends_with("\n\n"),
        "{} must end with exactly one newline",
        path.display()
    );
    serde_saphyr::from_str(&text)
        .unwrap_or_else(|err| panic!("parse {} as CRD: {err}", path.display()))
}

#[test]
fn gen_crds_writes_one_file_per_kind_to_both_directories() {
    run_gen_crds();
    for dir in crd_dirs() {
        assert_eq!(
            yaml_files(&dir).len(),
            5,
            "expected exactly 5 CRD files in {}",
            dir.display()
        );
    }
}

#[test]
fn gen_crds_is_idempotent() {
    run_gen_crds();
    let snapshot: Vec<(PathBuf, String)> = crd_dirs()
        .iter()
        .flat_map(|dir| yaml_files(dir))
        .map(|path| {
            let bytes = std::fs::read_to_string(&path)
                .unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
            (path, bytes)
        })
        .collect();
    run_gen_crds();
    for (path, before) in &snapshot {
        let after = std::fs::read_to_string(path)
            .unwrap_or_else(|err| panic!("re-read {}: {err}", path.display()));
        assert_eq!(*before, after, "{} changed between runs", path.display());
    }
}

#[test]
fn every_generated_file_parses_as_a_crd_with_the_expected_group_and_kind() {
    run_gen_crds();
    let mut kinds = BTreeSet::new();
    for dir in crd_dirs() {
        for path in yaml_files(&dir) {
            let crd = parse_crd(&path);
            assert_eq!(crd.spec.group, EXPECTED_GROUP, "{}", path.display());
            assert_eq!(crd.spec.scope, "Namespaced", "{}", path.display());
            kinds.insert(crd.spec.names.kind.clone());
        }
    }
    let expected: BTreeSet<String> = [
        "GraphCluster",
        "GraphNodePool",
        "GraphIndexer",
        "GraphBackup",
        "GraphIngestSource",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    // Each directory contributes the same kinds, so the union is exactly five.
    assert_eq!(kinds, expected);
}

#[test]
fn filename_comes_from_the_crd_plural_and_group() {
    run_gen_crds();
    let live: Vec<CustomResourceDefinition> = vec![
        GraphCluster::crd(),
        GraphNodePool::crd(),
        GraphIndexer::crd(),
        GraphBackup::crd(),
        GraphIngestSource::crd(),
    ];
    let expected: BTreeSet<String> = live
        .iter()
        .map(|crd| format!("{}.{}.yaml", crd.spec.names.plural, crd.spec.group))
        .collect();
    assert_eq!(expected.len(), 5, "live CRDs must disagree on plural");
    for dir in crd_dirs() {
        let actual: BTreeSet<String> = yaml_files(&dir)
            .iter()
            .map(|path| {
                path.file_name()
                    .expect("file name")
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(actual, expected, "{}", dir.display());
        // Cross-check each file's own content against its name (no hardcoded list).
        for path in yaml_files(&dir) {
            let crd = parse_crd(&path);
            let derived = format!("{}.{}.yaml", crd.spec.names.plural, crd.spec.group);
            assert_eq!(
                path.file_name().expect("file name").to_string_lossy(),
                derived,
                "content-derived name mismatch for {}",
                path.display()
            );
        }
    }
}
