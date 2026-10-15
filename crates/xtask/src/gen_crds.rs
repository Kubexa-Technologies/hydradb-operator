//! `gen-crds`: render the three CRDs into `deploy/crds/` and the chart's `crds/`.

use std::path::{Path, PathBuf};

use hydradb_operator_api::crd::{GraphCluster, GraphIndexer, GraphNodePool};
use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
use kube::CustomResourceExt;

/// Directories (relative to the workspace root) receiving the generated CRDs.
///
/// Helm's `crds/` directory is install-only and never upgraded, so the same
/// bytes are written to both paths.
const OUTPUT_DIRS: [&str; 2] = ["deploy/crds", "charts/hydradb-operator/crds"];

/// Workspace root derived from this crate's manifest location.
///
/// `CARGO_MANIFEST_DIR` is `crates/xtask` both for the binary and for
/// integration tests, so two ancestors up is the workspace root regardless of
/// the process working directory.
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// SPDX header required on every generated CRD; written here because a
/// hand-edit of the checked-in YAML is reverted by the next `gen-all`.
const SPDX_HEADER: &str = "# SPDX-License-Identifier: Apache-2.0\n";

/// Serialize one CRD with a stable field order and exactly one trailing newline.
fn render_crd(crd: &CustomResourceDefinition) -> anyhow::Result<String> {
    let mut yaml = String::from(SPDX_HEADER);
    yaml.push_str(&serde_saphyr::to_string(crd)?);
    if !yaml.ends_with('\n') {
        yaml.push('\n');
    }
    Ok(yaml)
}

/// Write `contents` to `path` only when the bytes differ (keeps runs idempotent
/// without churning mtimes).
fn write_if_different(path: &Path, contents: &str) -> anyhow::Result<()> {
    let current = std::fs::read_to_string(path).unwrap_or_default();
    if current != contents {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, contents)?;
    }
    Ok(())
}

/// File name derived from the CRD's own `spec.names.plural` and `spec.group`.
fn crd_filename(crd: &CustomResourceDefinition) -> anyhow::Result<String> {
    let plural = &crd.spec.names.plural;
    let group = &crd.spec.group;
    if plural.is_empty() || group.is_empty() {
        anyhow::bail!("CRD {} has empty plural or group", crd.spec.names.kind);
    }
    Ok(format!("{plural}.{group}.yaml"))
}

/// Render all three CRDs for both output directories.
///
/// Returns `(relative path, contents)` pairs so the `--check` drift gate can
/// diff without writing.
pub fn render_all() -> anyhow::Result<Vec<(String, String)>> {
    let crds: Vec<CustomResourceDefinition> = vec![
        GraphCluster::crd(),
        GraphNodePool::crd(),
        GraphIndexer::crd(),
    ];
    let mut out = Vec::new();
    for crd in &crds {
        let filename = crd_filename(crd)?;
        let yaml = render_crd(crd)?;
        for dir in OUTPUT_DIRS {
            out.push((format!("{dir}/{filename}"), yaml.clone()));
        }
    }
    Ok(out)
}

/// Generate all three CRDs into both output directories.
pub fn run() -> anyhow::Result<()> {
    let root = workspace_root();
    for (rel, contents) in render_all()? {
        let path = root.join(&rel);
        write_if_different(&path, &contents)?;
        println!("wrote {rel}");
    }
    Ok(())
}
