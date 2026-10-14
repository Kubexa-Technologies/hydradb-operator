//! `hydradb status` — list `GraphCluster`/`GraphNodePool`/`GraphIndexer`
//! with conditions summarised as a kstatus string (plan todo 28).

use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
use kube::api::{Api, ListParams};
use kube::{Client, Resource};
use serde_json::Value;

use crate::output::{OutputFormat, Printer, list_value, print_yaml, render_table, with_kind};
use crate::{Cli, CliError, build_kube_config, effective_namespace};
use hydradb_operator_api::constants::{GROUP, VERSION};
use hydradb_operator_api::crd::{GraphCluster, GraphIndexer, GraphNodePool};

/// Arguments for `hydradb status`.
#[derive(Debug, Clone, Default, clap::Args)]
pub struct StatusArgs {
    /// Show only the named object.
    #[arg(value_name = "NAME")]
    pub name: Option<String>,
}

/// Summarise standard Kubernetes conditions as a kstatus-style string.
///
/// Precedence: `Terminating` (object deleting) beats `Failed` (`Stalled`
/// true, `Failed` true, `Degraded` true), which beats `Ready` (`Ready`
/// true), which beats `InProgress` (reconciling or `Ready` false); anything
/// else, including no conditions at all, is `Unknown`.
#[must_use]
pub fn kstatus(conditions: &[Condition], deleting: bool) -> &'static str {
    if deleting {
        return "Terminating";
    }
    if conditions.is_empty() {
        return "Unknown";
    }
    let is_true = |want_type: &str| {
        conditions
            .iter()
            .any(|c| c.type_ == want_type && c.status == "True")
    };
    if is_true("Stalled") {
        return "Failed";
    }
    if conditions
        .iter()
        .any(|c| c.status == "True" && (c.type_ == "Failed" || c.reason == "Failed"))
    {
        return "Failed";
    }
    if is_true("Degraded") {
        return "Failed";
    }
    if is_true("Ready") {
        return "Ready";
    }
    if is_true("Reconciling") {
        return "InProgress";
    }
    if conditions
        .iter()
        .any(|c| c.type_ == "Ready" && c.status == "False")
    {
        return "InProgress";
    }
    "Unknown"
}

/// Summarise the `status.conditions` array inside a serialised object.
fn kstatus_of(value: &Value) -> &'static str {
    let deleting = value
        .pointer("/metadata/deletionTimestamp")
        .is_some_and(|v| !v.is_null());
    let conditions: Vec<Condition> = value
        .pointer("/status/conditions")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| serde_json::from_value(item.clone()).ok())
                .collect()
        })
        .unwrap_or_default();
    kstatus(&conditions, deleting)
}

/// One printable status row derived from a serialised object.
struct StatusRow {
    kind: String,
    namespace: String,
    name: String,
    status: &'static str,
    age: String,
    ready: String,
}

/// Build a [`StatusRow`] from a serialised object in a `v1/List`.
fn status_row(value: &Value) -> StatusRow {
    let text = |pointer: &str| {
        value
            .pointer(pointer)
            .and_then(Value::as_str)
            .unwrap_or("<unknown>")
            .to_string()
    };
    let ready = value
        .pointer("/status/conditions")
        .and_then(Value::as_array)
        .and_then(|items| {
            items.iter().find_map(|item| {
                let is_ready = item.get("type").and_then(Value::as_str) == Some("Ready");
                is_ready.then(|| {
                    item.get("status")
                        .and_then(Value::as_str)
                        .unwrap_or("Unknown")
                        .to_string()
                })
            })
        })
        .unwrap_or_else(|| "Unknown".to_string());
    StatusRow {
        kind: text("/kind"),
        namespace: value
            .pointer("/metadata/namespace")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        name: text("/metadata/name"),
        status: kstatus_of(value),
        age: text("/metadata/creationTimestamp"),
        ready,
    }
}

/// List one namespaced CRD kind, honouring the effective namespace.
///
/// A `Some` namespace lists only that namespace; `None` (`-A`) lists across
/// all namespaces. Never pages the whole cluster when a namespace is given.
async fn list_clusters(
    client: &Client,
    namespace: &Option<String>,
) -> Result<Vec<GraphCluster>, CliError> {
    let api = match namespace {
        Some(ns) => Api::<GraphCluster>::namespaced(client.clone(), ns),
        None => Api::<GraphCluster>::all(client.clone()),
    };
    Ok(api.list(&ListParams::default()).await?.items)
}

/// List `GraphNodePool` objects in scope (see [`list_clusters`]).
async fn list_pools(
    client: &Client,
    namespace: &Option<String>,
) -> Result<Vec<GraphNodePool>, CliError> {
    let api = match namespace {
        Some(ns) => Api::<GraphNodePool>::namespaced(client.clone(), ns),
        None => Api::<GraphNodePool>::all(client.clone()),
    };
    Ok(api.list(&ListParams::default()).await?.items)
}

/// List `GraphIndexer` objects in scope (see [`list_clusters`]).
async fn list_indexers(
    client: &Client,
    namespace: &Option<String>,
) -> Result<Vec<GraphIndexer>, CliError> {
    let api = match namespace {
        Some(ns) => Api::<GraphIndexer>::namespaced(client.clone(), ns),
        None => Api::<GraphIndexer>::all(client.clone()),
    };
    Ok(api.list(&ListParams::default()).await?.items)
}

/// Serialise one object, ensuring `apiVersion`/`kind` survive the round trip.
///
/// The CLI crate does not depend on `serde` directly; the trait bounds use
/// the `k8s-openapi` re-export (`k8s_openapi::serde`).
fn to_object_value<K>(object: &K, api_version: &str, kind: &str) -> Result<Value, CliError>
where
    K: Resource + k8s_openapi::serde::Serialize,
{
    serde_json::to_value(object)
        .map(|value| with_kind(value, api_version, kind))
        .map_err(|err| CliError::Output {
            message: format!("cannot serialise {kind}: {err}"),
        })
}

/// Run `hydradb status`.
///
/// # Errors
///
/// Returns [`CliError`] when the cluster cannot be reached, when objects
/// cannot be listed or rendered, or when `--name` matches nothing.
pub async fn run(cli: &Cli, args: &StatusArgs) -> Result<(), CliError> {
    let config = build_kube_config(cli).await?;
    let client = Client::try_from(config.clone())?;
    let namespace = effective_namespace(cli, &config);

    let mut items: Vec<Value> = Vec::new();
    let api_version = format!("{GROUP}/{VERSION}");
    for cluster in list_clusters(&client, &namespace).await? {
        items.push(to_object_value(
            &cluster,
            &api_version,
            &GraphCluster::kind(&()),
        )?);
    }
    for pool in list_pools(&client, &namespace).await? {
        items.push(to_object_value(
            &pool,
            &api_version,
            &GraphNodePool::kind(&()),
        )?);
    }
    for indexer in list_indexers(&client, &namespace).await? {
        items.push(to_object_value(
            &indexer,
            &api_version,
            &GraphIndexer::kind(&()),
        )?);
    }
    if let Some(name) = &args.name {
        items.retain(|item| {
            item.pointer("/metadata/name").and_then(Value::as_str) == Some(name.as_str())
        });
        if items.is_empty() {
            return Err(CliError::Output {
                message: format!(
                    "no GraphCluster/GraphNodePool/GraphIndexer named `{name}`{}",
                    namespace.map_or_else(
                        || " in any namespace".to_string(),
                        |ns| format!(" in namespace `{ns}`"),
                    )
                ),
            });
        }
    }

    let list = list_value(items);
    match cli.output {
        OutputFormat::Table => {
            let rows: Vec<Vec<String>> = list["items"]
                .as_array()
                .map(|objects| {
                    objects
                        .iter()
                        .map(|object| {
                            let row = status_row(object);
                            vec![row.kind, row.namespace, row.name, row.status.to_string()]
                        })
                        .collect()
                })
                .unwrap_or_default();
            print!(
                "{}",
                render_table(&["KIND", "NAMESPACE", "NAME", "STATUS"], &rows)
            );
            Ok(())
        }
        OutputFormat::Wide => {
            let rows: Vec<Vec<String>> = list["items"]
                .as_array()
                .map(|objects| {
                    objects
                        .iter()
                        .map(|object| {
                            let row = status_row(object);
                            vec![
                                row.kind,
                                row.namespace,
                                row.name,
                                row.status.to_string(),
                                row.ready,
                                row.age,
                            ]
                        })
                        .collect()
                })
                .unwrap_or_default();
            print!(
                "{}",
                render_table(
                    &["KIND", "NAMESPACE", "NAME", "STATUS", "READY", "AGE"],
                    &rows
                )
            );
            Ok(())
        }
        OutputFormat::Yaml => print_yaml(&list),
        OutputFormat::Json => Printer::new(OutputFormat::Json).print(&list),
        OutputFormat::Name => Printer::new(OutputFormat::Name).print(&list),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn condition(type_: &str, status: &str, reason: &str) -> Condition {
        serde_json::from_value(json!({
            "type": type_,
            "status": status,
            "reason": reason,
            "message": "",
            "lastTransitionTime": "2026-10-01T00:00:00Z",
        }))
        .expect("fixture condition deserialises")
    }

    #[test]
    fn ready_true_is_ready() {
        let conditions = vec![
            condition("Reconciling", "False", "Reconciling"),
            condition("Ready", "True", "Available"),
        ];
        assert_eq!(kstatus(&conditions, false), "Ready");
    }

    #[test]
    fn reconciling_true_is_in_progress() {
        let conditions = vec![
            condition("Reconciling", "True", "Reconciling"),
            condition("Ready", "False", "Reconciling"),
        ];
        assert_eq!(kstatus(&conditions, false), "InProgress");
    }

    #[test]
    fn ready_false_without_reconciling_is_in_progress() {
        let conditions = vec![condition("Ready", "False", "Reconciling")];
        assert_eq!(kstatus(&conditions, false), "InProgress");
    }

    #[test]
    fn failed_reason_is_failed() {
        let conditions = vec![
            condition("Ready", "False", "Failed"),
            condition("Failed", "True", "Failed"),
        ];
        assert_eq!(kstatus(&conditions, false), "Failed");
    }

    #[test]
    fn stalled_true_is_failed() {
        let conditions = vec![
            condition("Ready", "True", "Available"),
            condition("Stalled", "True", "Degraded"),
        ];
        assert_eq!(kstatus(&conditions, false), "Failed");
    }

    #[test]
    fn degraded_true_is_failed() {
        let conditions = vec![condition("Degraded", "True", "Degraded")];
        assert_eq!(kstatus(&conditions, false), "Failed");
    }

    #[test]
    fn empty_conditions_are_unknown() {
        assert_eq!(kstatus(&[], false), "Unknown");
    }

    #[test]
    fn deleting_object_is_terminating() {
        let conditions = vec![condition("Ready", "True", "Available")];
        assert_eq!(kstatus(&conditions, true), "Terminating");
    }

    #[test]
    fn json_list_shape_matches_v1_list() {
        let items = vec![
            json!({
                "apiVersion": "hydradb.helmforge.dev/v1alpha1",
                "kind": "GraphCluster",
                "metadata": {"name": "demo", "namespace": "demo"},
                "spec": {},
                "status": {"conditions": []},
            }),
            json!({
                "apiVersion": "hydradb.helmforge.dev/v1alpha1",
                "kind": "GraphIndexer",
                "metadata": {"name": "demo", "namespace": "demo"},
                "spec": {},
                "status": {"conditions": []},
            }),
        ];
        let list = list_value(items);
        assert_eq!(list["apiVersion"], json!("v1"));
        assert_eq!(list["kind"], json!("List"));
        assert_eq!(list["metadata"], json!({"resourceVersion": ""}));
        let listed = list["items"].as_array().expect("items is an array");
        assert_eq!(listed.len(), 2);
        for item in listed {
            assert!(item.get("apiVersion").and_then(Value::as_str).is_some());
            assert!(item.get("kind").and_then(Value::as_str).is_some());
            assert!(
                item.pointer("/metadata/name")
                    .and_then(Value::as_str)
                    .is_some()
            );
        }
        // Deterministic order: sorted by (kind, namespace, name).
        assert_eq!(listed[0]["kind"], json!("GraphCluster"));
        assert_eq!(listed[1]["kind"], json!("GraphIndexer"));
    }

    #[test]
    fn yaml_renders_the_same_list_document() {
        let items = vec![json!({
            "apiVersion": "hydradb.helmforge.dev/v1alpha1",
            "kind": "GraphCluster",
            "metadata": {"name": "demo", "namespace": "demo"},
            "status": {"conditions": []},
        })];
        let list = list_value(items);
        let yaml = crate::output::to_yaml(&list);
        assert!(
            yaml.contains("kind: List"),
            "yaml names the List kind:\n{yaml}"
        );
        assert!(yaml.contains("apiVersion: v1"), "yaml names v1:\n{yaml}");
        assert!(
            yaml.contains("kind: GraphCluster"),
            "yaml carries the item:\n{yaml}"
        );
        assert!(
            yaml.contains("name: demo"),
            "yaml carries the name:\n{yaml}"
        );
    }

    #[test]
    fn name_mode_uses_lowercase_kind_slash_name() {
        let list = list_value(vec![json!({
            "apiVersion": "hydradb.helmforge.dev/v1alpha1",
            "kind": "GraphNodePool",
            "metadata": {"name": "workers", "namespace": "demo"},
        })]);
        let lines = crate::output::name_lines(&list).expect("name lines render");
        assert_eq!(lines, vec!["graphnodepool/workers".to_string()]);
    }

    #[test]
    fn status_row_summarises_ready_conditions() {
        let object = json!({
            "kind": "GraphCluster",
            "metadata": {"name": "demo", "namespace": "demo"},
            "status": {"conditions": [
                {"type": "Ready", "status": "True", "reason": "Available",
                 "message": "", "lastTransitionTime": "2026-10-01T00:00:00Z"},
            ]},
        });
        let row = status_row(&object);
        assert_eq!(row.status, "Ready");
        assert_eq!(row.ready, "True");
        assert_eq!(row.name, "demo");
    }
}
