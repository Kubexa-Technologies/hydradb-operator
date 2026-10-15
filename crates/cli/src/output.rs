//! Output-mode plumbing for the `hydradb` CLI.
//!
//! All five [`OutputFormat`] modes are finalised here (plan todo 30):
//! `json` emits the exact Kubernetes object, `yaml` the same document as
//! YAML, `name` emits lowercase `kind/name` lines, `table` a compact
//! `KIND/NAMESPACE/NAME/STATUS` layout and `wide` the same plus
//! `READY`/`AGE` columns. Read commands with richer rows (`status`,
//! `endpoints`) build their own tables via [`render_table`] and route only
//! `json`/`name` through [`Printer`]; the generic [`Printer`] table/wide
//! paths below cover every other shape (notably `scale` output).

use crate::CliError;
use serde_json::{Value, json};

/// Sort key for list output: `(kind, namespace, name)` as strings.
fn sort_key(value: &Value) -> (String, String, String) {
    let text = |pointer: &str| {
        value
            .pointer(pointer)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    (
        text("/kind"),
        text("/metadata/namespace"),
        text("/metadata/name"),
    )
}

/// Wrap serialised objects in the exact `v1/List` shape (`-o json`/`-o yaml`
/// emit this so output pipes to `apply`/`jq`/`yq`).
///
/// Items are sorted by `(kind, namespace, name)` for deterministic output.
#[must_use]
pub fn list_value(mut items: Vec<Value>) -> Value {
    items.sort_by_key(sort_key);
    json!({
        "apiVersion": "v1",
        "kind": "List",
        "items": items,
        "metadata": {"resourceVersion": ""},
    })
}

/// Ensure a serialised object carries its `apiVersion`/`kind` (kube emits
/// them, but list output must not silently drop them if a shape ever omits
/// one).
#[must_use]
pub fn with_kind(mut value: Value, api_version: &str, kind: &str) -> Value {
    if value
        .get("apiVersion")
        .and_then(Value::as_str)
        .is_none_or(str::is_empty)
    {
        value["apiVersion"] = Value::String(api_version.to_string());
    }
    if value
        .get("kind")
        .and_then(Value::as_str)
        .is_none_or(str::is_empty)
    {
        value["kind"] = Value::String(kind.to_string());
    }
    value
}

/// Output mode selected by the global `-o/--output` flag.
///
/// Mirrors `kubectl -o`: `-o name` emits `kind/name` lines, `-o json` emits
/// the exact Kubernetes object so it can be piped to `apply`/`jq`/`yq`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum OutputFormat {
    /// Human-readable table.
    #[default]
    Table,
    /// Wider human-readable table (adds `READY`/`AGE`).
    Wide,
    /// Exact Kubernetes object as YAML.
    Yaml,
    /// Exact Kubernetes object as JSON.
    Json,
    /// `kind/name` per object, one per line.
    Name,
}

/// Renders a [`Value`] in the selected [`OutputFormat`].
#[derive(Debug, Clone, Copy)]
pub struct Printer {
    /// Format to render in.
    pub format: OutputFormat,
}

impl Printer {
    /// Create a printer for the given format.
    #[must_use]
    pub const fn new(format: OutputFormat) -> Self {
        Self { format }
    }

    /// Render `value` to a string in the printer's format (pure, testable).
    ///
    /// # Errors
    ///
    /// Returns [`CliError::Output`] when the value cannot be rendered, and
    /// [`CliError::Json`] when JSON serialisation fails.
    pub fn render(&self, value: &Value) -> Result<String, CliError> {
        match self.format {
            OutputFormat::Json => {
                let mut rendered = serde_json::to_string_pretty(value).map_err(CliError::Json)?;
                rendered.push('\n');
                Ok(rendered)
            }
            OutputFormat::Name => {
                let mut out = String::new();
                for line in name_lines(value)? {
                    out.push_str(&line);
                    out.push('\n');
                }
                Ok(out)
            }
            OutputFormat::Yaml => Ok(to_yaml(value)),
            OutputFormat::Table => Ok(render_table(
                &["KIND", "NAMESPACE", "NAME", "STATUS"],
                &generic_rows(value, false),
            )),
            OutputFormat::Wide => Ok(render_table(
                &["KIND", "NAMESPACE", "NAME", "STATUS", "READY", "AGE"],
                &generic_rows(value, true),
            )),
        }
    }

    /// Render `value` to stdout in the printer's format.
    ///
    /// # Errors
    ///
    /// Returns [`CliError::Output`] when the value cannot be rendered, and
    /// [`CliError::Json`] when JSON serialisation fails.
    pub fn print(&self, value: &Value) -> Result<(), CliError> {
        print!("{}", self.render(value)?);
        Ok(())
    }
}

/// One generic row per object in `value` (single object or `v1/List`).
///
/// Columns are `KIND`/`NAMESPACE`/`NAME` plus a kstatus-style `STATUS`
/// derived from `status.conditions` (same precedence family as the `status`
/// command: `Failed` beats `Ready` beats `InProgress`, else `Unknown`).
/// With `wide`, `READY` (the `Ready` condition status) and `AGE`
/// (`.metadata.creationTimestamp`) are appended.
fn generic_rows(value: &Value, wide: bool) -> Vec<Vec<String>> {
    let objects: Vec<&Value> = if value.get("items").is_some() {
        value["items"]
            .as_array()
            .map_or_else(Vec::new, |items| items.iter().collect())
    } else {
        vec![value]
    };
    objects
        .iter()
        .map(|object| {
            let text = |pointer: &str| {
                object
                    .pointer(pointer)
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string()
            };
            let mut row = vec![
                text("/kind"),
                text("/metadata/namespace"),
                text("/metadata/name"),
                generic_status(object),
            ];
            if wide {
                row.push(generic_ready(object));
                row.push(text("/metadata/creationTimestamp"));
            }
            row
        })
        .collect()
}

/// Best-effort status string from `status.conditions` for generic tables.
fn generic_status(object: &Value) -> String {
    let conditions = object
        .pointer("/status/conditions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let is_true = |want_type: &str| {
        conditions.iter().any(|c| {
            c.get("type").and_then(Value::as_str) == Some(want_type)
                && c.get("status").and_then(Value::as_str) == Some("True")
        })
    };
    if object
        .pointer("/metadata/deletionTimestamp")
        .is_some_and(|v| !v.is_null())
    {
        return "Terminating".to_string();
    }
    if is_true("Stalled") || is_true("Degraded") || is_true("Failed") {
        return "Failed".to_string();
    }
    if is_true("Ready") {
        return "Ready".to_string();
    }
    if is_true("Reconciling")
        || conditions.iter().any(|c| {
            c.get("type").and_then(Value::as_str) == Some("Ready")
                && c.get("status").and_then(Value::as_str) == Some("False")
        })
    {
        return "InProgress".to_string();
    }
    "Unknown".to_string()
}

/// Value of the `Ready` condition's `status`, or `Unknown` when absent.
fn generic_ready(object: &Value) -> String {
    object
        .pointer("/status/conditions")
        .and_then(Value::as_array)
        .and_then(|items| {
            items.iter().find_map(|item| {
                (item.get("type").and_then(Value::as_str) == Some("Ready")).then(|| {
                    item.get("status")
                        .and_then(Value::as_str)
                        .unwrap_or("Unknown")
                        .to_string()
                })
            })
        })
        .unwrap_or_else(|| "Unknown".to_string())
}

/// Collect `kind/name` lines for a single object or a `v1/List`.
pub fn name_lines(value: &Value) -> Result<Vec<String>, CliError> {
    if value.get("items").is_some() {
        let items = value["items"].as_array().ok_or_else(|| CliError::Output {
            message: "expected `items` to be an array for `-o name`".to_string(),
        })?;
        items.iter().map(name_line).collect()
    } else {
        Ok(vec![name_line(value)?])
    }
}

/// Render one object as lowercase `kind/name`, e.g. `graphcluster/example`.
fn name_line(value: &Value) -> Result<String, CliError> {
    let kind = value
        .get("kind")
        .and_then(Value::as_str)
        .ok_or_else(|| CliError::Output {
            message: "object is missing `.kind`, cannot render `-o name`".to_string(),
        })?;
    let name = value
        .pointer("/metadata/name")
        .and_then(Value::as_str)
        .ok_or_else(|| CliError::Output {
            message: "object is missing `.metadata.name`, cannot render `-o name`".to_string(),
        })?;
    Ok(format!("{}/{}", kind.to_lowercase(), name))
}

/// Render a real column layout with headers (todo 28 read commands).
///
/// Widths come from the widest cell per column; rows shorter than the header
/// list are padded with empty cells. Pure function so tests assert layout
/// without touching stdout.
#[must_use]
pub fn render_table(headers: &[&str], rows: &[Vec<String>]) -> String {
    let width = headers.len();
    let mut widths: Vec<usize> = headers.iter().map(|h| h.len()).collect();
    for row in rows {
        for (index, cell) in row.iter().enumerate().take(width) {
            if cell.len() > widths[index] {
                widths[index] = cell.len();
            }
        }
    }
    let mut out = String::new();
    out.push_str(&render_row(headers, &widths));
    out.push('\n');
    for row in rows {
        let cells: Vec<&str> = (0..width)
            .map(|index| row.get(index).map_or("", String::as_str))
            .collect();
        out.push_str(&render_row(&cells, &widths));
        out.push('\n');
    }
    out
}

/// Render one padded row for [`render_table`].
fn render_row(cells: &[&str], widths: &[usize]) -> String {
    cells
        .iter()
        .enumerate()
        .map(|(index, cell)| {
            let pad = widths.get(index).copied().unwrap_or(cell.len());
            format!("{cell:pad$}")
        })
        .collect::<Vec<_>>()
        .join("  ")
}

/// Render `value` as YAML (todo 28 read commands).
///
/// Hand-rolled JSON-to-YAML emitter: the CLI crate must not gain a YAML
/// dependency (workspace pins `serde-saphyr` for xtask/controller only), and
/// double-quoted scalars are valid YAML 1.2, so quoting follows JSON string
/// syntax via `serde_json`.
#[must_use]
pub fn to_yaml(value: &Value) -> String {
    let mut out = String::new();
    write_yaml(value, 0, &mut out);
    out
}

/// Print `value` as YAML to stdout.
///
/// # Errors
///
/// Returns [`CliError::Output`] when the value cannot be serialised (never
/// for a [`Value`]; the `Result` keeps the surface uniform with [`Printer`]).
pub fn print_yaml(value: &Value) -> Result<(), CliError> {
    print!("{}", to_yaml(value));
    Ok(())
}

/// Write one YAML node at `indent` spaces into `out`.
///
/// Block collections emit their own leading indent on every line; scalars
/// and empty collections render inline. Sequence items holding objects use
/// the compact `- key: value` form.
fn write_yaml(value: &Value, indent: usize, out: &mut String) {
    match value {
        Value::Null => out.push_str("null\n"),
        Value::Bool(true) => out.push_str("true\n"),
        Value::Bool(false) => out.push_str("false\n"),
        Value::Number(number) => {
            out.push_str(&number.to_string());
            out.push('\n');
        }
        Value::String(text) => {
            out.push_str(&yaml_scalar(text));
            out.push('\n');
        }
        Value::Array(items) => {
            if items.is_empty() {
                out.push_str("[]\n");
            } else {
                for item in items {
                    out.push_str(&" ".repeat(indent));
                    out.push('-');
                    if is_inline(item) {
                        out.push(' ');
                        write_yaml(item, indent + 2, out);
                    } else if let Value::Object(map) = item {
                        let mut pairs = map.iter();
                        if let Some((key, first)) = pairs.next() {
                            out.push(' ');
                            out.push_str(&yaml_scalar(key));
                            out.push(':');
                            write_pair_value(first, indent + 2, out);
                        }
                        for (key, rest) in pairs {
                            write_pair(key, rest, indent + 2, out);
                        }
                    } else {
                        out.push('\n');
                        write_yaml(item, indent + 2, out);
                    }
                }
            }
        }
        Value::Object(map) => {
            if map.is_empty() {
                out.push_str("{}\n");
            } else {
                for (key, item) in map {
                    write_pair(key, item, indent, out);
                }
            }
        }
    }
}

/// Write one `key: value` mapping line at `indent` spaces; nested values
/// sit two spaces deeper.
fn write_pair(key: &str, value: &Value, indent: usize, out: &mut String) {
    out.push_str(&" ".repeat(indent));
    out.push_str(&yaml_scalar(key));
    out.push(':');
    write_pair_value(value, indent + 2, out);
}

/// Write the `: value` half of a mapping: inline values continue the line,
/// collections start on the next line at `indent`.
fn write_pair_value(value: &Value, indent: usize, out: &mut String) {
    if is_inline(value) {
        out.push(' ');
        write_yaml(value, indent, out);
    } else {
        out.push('\n');
        write_yaml(value, indent, out);
    }
}

/// True for scalars and empty collections (fit on one line).
fn is_inline(value: &Value) -> bool {
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => true,
        Value::Array(items) => items.is_empty(),
        Value::Object(map) => map.is_empty(),
    }
}

/// Render a string as a YAML plain scalar, JSON-quoting when it carries
/// characters that would change meaning (`: `, `#`, leading specials...).
fn yaml_scalar(text: &str) -> String {
    if is_plain_scalar(text) {
        text.to_string()
    } else {
        serde_json::to_string(text).unwrap_or_else(|_| format!("{text:?}"))
    }
}

/// True when `text` is safe to emit as a YAML plain scalar.
fn is_plain_scalar(text: &str) -> bool {
    if text.is_empty() {
        return false;
    }
    let mut chars = text.chars();
    let first = chars.next().unwrap_or(' ');
    if "-?:,[]{}#&*!|>'\"%@`".contains(first) {
        return false;
    }
    if text.contains(": ") || text.ends_with(':') {
        return false;
    }
    if text.contains(" #") {
        return false;
    }
    text.chars().all(|c| {
        !c.is_control()
            && !matches!(c, '\n' | '\r')
            && (c.is_alphanumeric() || "-_./:+".contains(c))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn name_renders_single_object() {
        let value = json!({"kind": "GraphCluster", "metadata": {"name": "example"}});
        let lines = name_lines(&value).expect("single object renders");
        assert_eq!(lines, vec!["graphcluster/example".to_string()]);
    }

    #[test]
    fn name_renders_list_items() {
        let value = json!({
            "kind": "List",
            "items": [
                {"kind": "GraphCluster", "metadata": {"name": "a"}},
                {"kind": "GraphNodePool", "metadata": {"name": "b"}},
            ],
        });
        let lines = name_lines(&value).expect("list renders");
        assert_eq!(lines, vec!["graphcluster/a", "graphnodepool/b"]);
    }

    #[test]
    fn all_five_modes_render_the_same_fixture() {
        let fixture = json!({
            "apiVersion": "hydradb.helmforge.dev/v1alpha1",
            "kind": "GraphNodePool",
            "metadata": {
                "name": "p",
                "namespace": "demo",
                "creationTimestamp": "2026-10-01T00:00:00Z",
            },
            "spec": {"replicas": 5},
            "status": {"conditions": [
                {"type": "Ready", "status": "True", "reason": "Available",
                 "message": "", "lastTransitionTime": "2026-10-01T00:00:00Z"},
            ]},
        });

        let name = Printer::new(OutputFormat::Name)
            .render(&fixture)
            .expect("name renders");
        assert_eq!(name, "graphnodepool/p\n");

        let wide = Printer::new(OutputFormat::Wide)
            .render(&fixture)
            .expect("wide renders");
        for fragment in ["GraphNodePool", "demo", "p", "Ready", "True"] {
            assert!(
                wide.contains(fragment),
                "wide carries `{fragment}`:\n{wide}"
            );
        }

        let json = Printer::new(OutputFormat::Json)
            .render(&fixture)
            .expect("json renders");
        let parsed: Value = serde_json::from_str(&json).expect("json parses back");
        assert_eq!(parsed, fixture);

        let yaml = Printer::new(OutputFormat::Yaml)
            .render(&fixture)
            .expect("yaml renders");
        for fragment in ["kind: GraphNodePool", "name: p", "namespace: demo"] {
            assert!(
                yaml.contains(fragment),
                "yaml carries `{fragment}`:\n{yaml}"
            );
        }

        let table = Printer::new(OutputFormat::Table)
            .render(&list_value(vec![fixture]))
            .expect("table renders");
        assert!(table.contains("GraphNodePool"), "table kind:\n{table}");
        assert!(table.contains("Ready"), "table status:\n{table}");
    }

    #[test]
    fn table_renders_headers_and_aligned_columns() {
        let rendered = render_table(
            &["KIND", "NAME", "STATUS"],
            &[
                vec![
                    "GraphCluster".to_string(),
                    "demo".to_string(),
                    "Ready".to_string(),
                ],
                vec![
                    "GraphIndexer".to_string(),
                    "demo".to_string(),
                    "InProgress".to_string(),
                ],
            ],
        );
        let mut lines = rendered.lines();
        let header = lines.next().expect("header line renders");
        assert!(header.starts_with("KIND"), "header first:\n{rendered}");
        assert!(header.contains("STATUS"), "header last:\n{rendered}");
        let first = lines.next().expect("first row renders");
        assert!(first.contains("GraphCluster"), "row content:\n{rendered}");
        assert!(first.contains("Ready"), "row status:\n{rendered}");
        assert_eq!(header.len(), first.len(), "columns align:\n{rendered}");
    }

    #[test]
    fn table_tolerates_ragged_rows() {
        let rendered = render_table(&["A", "B"], &[vec!["only-a".to_string()]]);
        assert!(
            rendered.contains("only-a"),
            "short row renders:\n{rendered}"
        );
    }

    #[test]
    fn yaml_round_trips_scalars_and_nesting() {
        let value = json!({
            "apiVersion": "v1",
            "kind": "List",
            "count": 2,
            "ready": true,
            "missing": null,
            "items": [{"kind": "GraphCluster", "metadata": {"name": "demo"}}],
        });
        let yaml = to_yaml(&value);
        for fragment in [
            "apiVersion: v1",
            "kind: List",
            "count: 2",
            "ready: true",
            "missing: null",
            "- kind: GraphCluster",
            "name: demo",
        ] {
            assert!(
                yaml.contains(fragment),
                "yaml carries `{fragment}`:\n{yaml}"
            );
        }
    }

    #[test]
    fn yaml_quotes_strings_that_would_change_meaning() {
        let value = json!({"key: with colon": "value # hash", "plain": "demo-0"});
        let yaml = to_yaml(&value);
        assert!(yaml.contains("\"key: with colon\""), "key quoted:\n{yaml}");
        assert!(yaml.contains("\"value # hash\""), "value quoted:\n{yaml}");
        assert!(yaml.contains("plain: demo-0"), "plain stays bare:\n{yaml}");
    }

    #[test]
    fn list_value_sorts_by_kind_namespace_name() {
        let item = |kind: &str, ns: &str, name: &str| json!({"kind": kind, "metadata": {"namespace": ns, "name": name}});
        let list = list_value(vec![
            item("GraphIndexer", "demo", "b"),
            item("GraphCluster", "demo", "b"),
            item("GraphCluster", "aaa", "z"),
            item("GraphCluster", "demo", "a"),
        ]);
        let names: Vec<String> = list["items"]
            .as_array()
            .expect("items array")
            .iter()
            .map(|item| {
                format!(
                    "{}/{}/{}",
                    item["kind"].as_str().unwrap_or("?"),
                    item["metadata"]["namespace"].as_str().unwrap_or("?"),
                    item["metadata"]["name"].as_str().unwrap_or("?"),
                )
            })
            .collect();
        assert_eq!(
            names,
            vec![
                "GraphCluster/aaa/z",
                "GraphCluster/demo/a",
                "GraphCluster/demo/b",
                "GraphIndexer/demo/b",
            ]
        );
        assert_eq!(list["metadata"], json!({"resourceVersion": ""}));
    }

    #[test]
    fn with_kind_fills_only_missing_fields() {
        let filled = with_kind(json!({}), "hydradb.helmforge.dev/v1alpha1", "GraphCluster");
        assert_eq!(
            filled["apiVersion"],
            json!("hydradb.helmforge.dev/v1alpha1")
        );
        assert_eq!(filled["kind"], json!("GraphCluster"));
        let kept = with_kind(
            json!({"apiVersion": "kept", "kind": "Kept"}),
            "other",
            "Other",
        );
        assert_eq!(kept["apiVersion"], json!("kept"));
        assert_eq!(kept["kind"], json!("Kept"));
    }
}
