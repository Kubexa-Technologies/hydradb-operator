//! Integration tests for `cargo xtask gen-docs`.
//!
//! Chosen location: `crates/xtask/tests/gen_docs.rs` (integration tests driving
//! the real binary via `CARGO_BIN_EXE_xtask`), mirroring
//! `tests/gen_crds.rs`, so the tests exercise the shipped binary end to end.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use hydradb_operator_api::crd::{GraphCluster, GraphIndexer, GraphNodePool};
use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
use kube::CustomResourceExt;

const EXPECTED_GROUP: &str = "hydradb.kubexa.tech";
const EXPECTED_VERSION: &str = "v1alpha1";

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .map(std::path::Path::to_path_buf)
        .expect("xtask lives two levels below the workspace root")
}

fn docs_dir() -> PathBuf {
    workspace_root().join("docs")
}

fn run_gen_docs() {
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_xtask"))
        .arg("gen-docs")
        .status()
        .expect("spawn xtask gen-docs");
    assert!(status.success(), "gen-docs exited non-zero: {status}");
}

fn md_files() -> Vec<PathBuf> {
    let dir = docs_dir();
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|err| panic!("read {}: {err}", dir.display()))
        .map(|entry| entry.expect("dir entry").path())
        .filter(|path| {
            path.extension().is_some_and(|ext| ext == "md")
                && path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with("crd-"))
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

/// The live CRD's `openAPIV3Schema` as JSON, reached the same way the
/// generator reaches it (JSON round-trip of the live object).
fn live_schema(crd: &CustomResourceDefinition) -> serde_json::Value {
    let value = serde_json::to_value(crd).expect("CRD serializes to JSON");
    value
        .get("spec")
        .and_then(|spec| spec.get("versions"))
        .and_then(|versions| versions.as_array())
        .and_then(|versions| versions.first())
        .and_then(|first| first.get("schema"))
        .and_then(|schema| schema.get("openAPIV3Schema"))
        .cloned()
        .unwrap_or_else(|| panic!("CRD {} has no openAPIV3Schema", crd.spec.names.kind))
}

/// Markdown-cell escaping, kept in sync with the generator: pipes would split
/// the row, raw newlines would split the table.
fn escape_cell(cell: &str) -> String {
    cell.replace('|', "\\|")
        .lines()
        .collect::<Vec<_>>()
        .join("<br/>")
}

fn schema_type(schema: &serde_json::Value) -> String {
    if let Some(marker) = schema.get("type").and_then(|kind| kind.as_str()) {
        return marker.to_owned();
    }
    if schema
        .get("x-kubernetes-int-or-string")
        .and_then(|flag| flag.as_bool())
        .unwrap_or(false)
    {
        return "integer or string".to_owned();
    }
    if schema.get("properties").is_some() {
        return "object".to_owned();
    }
    if schema.get("items").is_some() {
        return "array".to_owned();
    }
    String::new()
}

/// Independently flatten a schema node into `(path, type, required, default,
/// description)` rows: sorted keys, array items suffixed with `[]`, combiner
/// members contributing constraints without extending the path.
fn flatten(
    node: &serde_json::Value,
    prefix: &str,
    out: &mut Vec<(String, String, bool, String, String)>,
) {
    let required: BTreeSet<&str> = node
        .get("required")
        .and_then(|required| required.as_array())
        .map(|names| {
            names
                .iter()
                .filter_map(|name| name.as_str())
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();
    let Some(properties) = node.get("properties").and_then(|props| props.as_object()) else {
        return;
    };
    let mut keys: Vec<&str> = properties.keys().map(String::as_str).collect();
    keys.sort_unstable();
    for key in keys {
        let Some(field) = properties.get(key) else {
            continue;
        };
        let path = if prefix.is_empty() {
            key.to_owned()
        } else {
            format!("{prefix}.{key}")
        };
        out.push((
            path.clone(),
            schema_type(field),
            required.contains(key),
            field
                .get("default")
                .map(|default| serde_json::to_string(default).unwrap_or_default())
                .unwrap_or_default(),
            field
                .get("description")
                .and_then(|text| text.as_str())
                .unwrap_or_default()
                .to_owned(),
        ));
        descend(field, &path, out);
    }
}

fn descend(
    field: &serde_json::Value,
    path: &str,
    out: &mut Vec<(String, String, bool, String, String)>,
) {
    flatten(field, path, out);
    match field.get("items") {
        Some(serde_json::Value::Array(members)) => {
            for member in members {
                flatten(member, &format!("{path}[]"), out);
            }
        }
        Some(items) => flatten(items, &format!("{path}[]"), out),
        None => {}
    }
    for combiner in ["anyOf", "oneOf", "allOf"] {
        if let Some(serde_json::Value::Array(members)) = field.get(combiner) {
            for member in members {
                flatten(member, path, out);
            }
        }
    }
}

/// Split a markdown table row on unescaped pipes; `\|` stays inside the cell.
fn split_row(line: &str) -> Vec<String> {
    let mut cells = Vec::new();
    let mut current = String::new();
    let mut chars = line.chars().peekable();
    while let Some(char) = chars.next() {
        if char == '\\' && chars.peek() == Some(&'|') {
            current.push(char);
            current.push(chars.next().expect("peeked pipe"));
        } else if char == '|' {
            cells.push(current.trim().to_owned());
            current = String::new();
        } else {
            current.push(char);
        }
    }
    cells.push(current.trim().to_owned());
    cells
}

/// Body rows of a generated table: `(field, type, required, default,
/// description)`. Skips the header and separator rows.
fn table_rows(text: &str) -> Vec<(String, String, String, String, String)> {
    text.lines()
        .filter(|line| line.starts_with('|'))
        .filter(|line| !line.starts_with("| Field |") && !line.starts_with("| ---"))
        .map(|line| {
            let cells = split_row(line);
            assert_eq!(
                cells.len(),
                7,
                "every table row must have 5 cells plus 2 edge empties: {line}"
            );
            (
                cells[1].clone(),
                cells[2].clone(),
                cells[3].clone(),
                cells[4].clone(),
                cells[5].clone(),
            )
        })
        .collect()
}

fn live_crds() -> Vec<CustomResourceDefinition> {
    vec![
        GraphCluster::crd(),
        GraphNodePool::crd(),
        GraphIndexer::crd(),
    ]
}

#[test]
fn gen_docs_writes_one_table_per_kind() {
    run_gen_docs();
    let files = md_files();
    let expected: BTreeSet<String> = live_crds()
        .iter()
        .map(|crd| format!("crd-{}.md", crd.spec.names.kind.to_lowercase()))
        .collect();
    assert_eq!(
        expected.len(),
        3,
        "live CRDs must map to three distinct files"
    );
    let actual: BTreeSet<String> = files
        .iter()
        .map(|path| {
            path.file_name()
                .expect("file name")
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert_eq!(
        actual, expected,
        "docs/ must hold exactly one table per kind"
    );
    for path in &files {
        let text = read_raw(path);
        assert!(
            text.starts_with("<!-- GENERATED"),
            "{} must start with the generated-file header",
            path.display()
        );
        assert!(
            text.contains("cargo xtask gen-docs"),
            "{} header must name the regeneration command",
            path.display()
        );
        assert!(
            text.contains("do not hand-edit"),
            "{} header must forbid hand edits",
            path.display()
        );
        assert!(
            text.contains("| Field | Type | Required | Default | Description |"),
            "{} must carry the five-column table header",
            path.display()
        );
    }
}

#[test]
fn every_table_names_its_kind_group_and_version() {
    run_gen_docs();
    let by_kind: BTreeMap<String, PathBuf> = md_files()
        .into_iter()
        .map(|path| {
            let stem = path
                .file_stem()
                .expect("file stem")
                .to_string_lossy()
                .into_owned();
            (stem, path)
        })
        .collect();
    for crd in live_crds() {
        let kind = crd.spec.names.kind.clone();
        let key = format!("crd-{}", kind.to_lowercase());
        let path = by_kind
            .get(&key)
            .unwrap_or_else(|| panic!("missing table for kind {kind}"));
        let text = read_raw(path);
        assert!(
            text.contains(&format!("# {kind}")),
            "{kind} table must title itself with the kind",
        );
        assert!(
            text.contains(&format!("API group `{EXPECTED_GROUP}`")),
            "{kind} table must name the API group",
        );
        assert!(
            text.contains(&format!("version `{EXPECTED_VERSION}`")),
            "{kind} table must name the version",
        );
    }
}

#[test]
fn documented_field_yields_row_carrying_its_description() {
    run_gen_docs();
    for crd in live_crds() {
        let kind = crd.spec.names.kind.clone();
        let schema = live_schema(&crd);
        // First schema field carrying docs, found straight from the live schema.
        let mut expected = Vec::new();
        flatten(&schema, "", &mut expected);
        let (path, _, _, _, description) = expected
            .iter()
            .find(|(_, _, _, _, description)| !description.is_empty())
            .unwrap_or_else(|| panic!("{kind} schema has no documented field"))
            .clone();
        let path_in_table = format!("docs/crd-{}.md", kind.to_lowercase());
        let text = read_raw(&workspace_root().join(&path_in_table));
        let rows = table_rows(&text);
        let row = rows
            .iter()
            .find(|(field, _, _, _, _)| *field == escape_cell(&path))
            .unwrap_or_else(|| panic!("{kind} table drops documented field {path}"));
        assert_eq!(
            row.4,
            escape_cell(&description),
            "{kind} row for {path} must carry the schema description verbatim",
        );
    }
}

#[test]
fn undocumented_fields_gain_empty_cells_and_no_row_is_dropped() {
    run_gen_docs();
    for crd in live_crds() {
        let kind = crd.spec.names.kind.clone();
        let schema = live_schema(&crd);
        let mut expected = Vec::new();
        flatten(&schema, "", &mut expected);
        let text = read_raw(&workspace_root().join(format!("docs/crd-{}.md", kind.to_lowercase())));
        let rows = table_rows(&text);
        assert_eq!(
            rows.len(),
            expected.len(),
            "{kind} table must hold one row per schema field (no drops, no extras)",
        );
        // Every schema field without docs must still gain a row, with an EMPTY
        // description cell — never filler prose, never a dropped row.
        for (path, _, _, _, description) in &expected {
            let row = rows
                .iter()
                .find(|(field, _, _, _, _)| *field == escape_cell(path))
                .unwrap_or_else(|| panic!("{kind} table drops field {path}"));
            assert_eq!(
                row.4,
                escape_cell(description),
                "{kind} row for {path} must carry the schema description (empty when undocumented)",
            );
        }
        // The empty-cell count in the table must match the undocumented count
        // in the schema: filler prose would shrink it, dropped rows would too.
        let undocumented = expected
            .iter()
            .filter(|(_, _, _, _, description)| description.is_empty())
            .count();
        let empty_cells = rows
            .iter()
            .filter(|(_, _, _, _, description)| description.is_empty())
            .count();
        assert_eq!(
            empty_cells, undocumented,
            "{kind} table must have exactly one empty description cell per undocumented field",
        );
    }
}

#[test]
fn gen_docs_is_idempotent() {
    run_gen_docs();
    let snapshot: Vec<(PathBuf, String)> = md_files()
        .iter()
        .map(|path| {
            let bytes = std::fs::read_to_string(path)
                .unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
            (path.clone(), bytes)
        })
        .collect();
    assert_eq!(snapshot.len(), 3, "expected exactly 3 CRD reference");
    run_gen_docs();
    for (path, before) in &snapshot {
        let after = std::fs::read_to_string(path)
            .unwrap_or_else(|err| panic!("re-read {}: {err}", path.display()));
        assert_eq!(*before, after, "{} changed between runs", path.display());
    }
}
