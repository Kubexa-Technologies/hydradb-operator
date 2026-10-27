//! `hydradb endpoints` — print Bolt/HTTPS/admin endpoints from
//! `status.endpoints` (plan todo 28).

use kube::api::{Api, ListParams};
use kube::{Client, ResourceExt};
use serde_json::Value;

use crate::output::{OutputFormat, Printer, list_value, print_yaml, render_table, with_kind};
use crate::{Cli, CliError, build_kube_config, effective_namespace};
use hydradb_operator_api::constants::{GROUP, VERSION};
use hydradb_operator_api::crd::GraphCluster;

/// Arguments for `hydradb endpoints`.
#[derive(Debug, Clone, Default, clap::Args)]
pub struct EndpointsArgs {
    /// Show endpoints for only the named `GraphCluster`.
    #[arg(value_name = "NAME")]
    pub name: Option<String>,
}

/// One printable endpoint row: empty fields render as `<none>` and never as
/// secrets (only the advertised `status.endpoints` strings are shown).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointRow {
    /// Cluster name.
    pub cluster: String,
    /// Namespace of the cluster.
    pub namespace: String,
    /// Bolt endpoint (`host:port`, default port 7687).
    pub bolt: String,
    /// HTTPS endpoint (default port 443).
    pub https: String,
    /// Admin/metrics endpoint (node 9090, indexer 9091).
    pub admin: String,
    /// True when `status.endpoints` carried nothing at all.
    pub empty: bool,
}

/// Build an [`EndpointRow`] from a serialised `GraphCluster`.
///
/// Reads only `status.endpoints` (`bolt`/`https`/`admin`); missing fields
/// become `<none>`. Never touches `spec` credentials or `Secret` data.
#[must_use]
pub fn endpoint_row(cluster: &Value) -> EndpointRow {
    let endpoint = |field: &str| {
        cluster
            .pointer(&format!("/status/endpoints/{field}"))
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
            .unwrap_or("<none>")
            .to_string()
    };
    let bolt = endpoint("bolt");
    let https = endpoint("https");
    let admin = endpoint("admin");
    EndpointRow {
        cluster: cluster
            .pointer("/metadata/name")
            .and_then(Value::as_str)
            .unwrap_or("<unknown>")
            .to_string(),
        namespace: cluster
            .pointer("/metadata/namespace")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        empty: bolt == "<none>" && https == "<none>" && admin == "<none>",
        bolt,
        https,
        admin,
    }
}

/// Run `hydradb endpoints`.
///
/// # Errors
///
/// Returns [`CliError`] when the cluster cannot be reached or rendered, or
/// when the selected scope (or `--name`) matches no `GraphCluster`.
pub async fn run(cli: &Cli, args: &EndpointsArgs) -> Result<(), CliError> {
    let config = build_kube_config(cli).await?;
    let client = Client::try_from(config.clone())?;
    let namespace = effective_namespace(cli, &config);

    let api = match &namespace {
        Some(ns) => Api::<GraphCluster>::namespaced(client, ns),
        None => Api::<GraphCluster>::all(client),
    };
    let mut clusters = api.list(&ListParams::default()).await?.items;
    if let Some(name) = &args.name {
        clusters.retain(|cluster| cluster.name_any() == *name);
        if clusters.is_empty() {
            return Err(CliError::Output {
                message: format!("no GraphCluster named `{name}` in scope"),
            });
        }
    }
    if clusters.is_empty() {
        let scope = namespace.map_or_else(
            || "in any namespace".to_string(),
            |ns| format!("in namespace `{ns}`"),
        );
        return Err(CliError::Output {
            message: format!("no GraphClusters found {scope}"),
        });
    }

    let api_version = format!("{GROUP}/{VERSION}");
    let items: Vec<Value> = clusters
        .iter()
        .map(|cluster| {
            serde_json::to_value(cluster)
                .map(|value| with_kind(value, &api_version, "GraphCluster"))
                .map_err(|err| CliError::Output {
                    message: format!("cannot serialise GraphCluster: {err}"),
                })
        })
        .collect::<Result<_, _>>()?;
    let rows: Vec<EndpointRow> = items.iter().map(endpoint_row).collect();

    match cli.output {
        OutputFormat::Table | OutputFormat::Wide => {
            let table: Vec<Vec<String>> = rows
                .iter()
                .map(|row| {
                    vec![
                        row.cluster.clone(),
                        row.namespace.clone(),
                        row.bolt.clone(),
                        row.https.clone(),
                        row.admin.clone(),
                    ]
                })
                .collect();
            print!(
                "{}",
                render_table(&["CLUSTER", "NAMESPACE", "BOLT", "HTTPS", "ADMIN"], &table)
            );
            for row in &rows {
                if row.empty {
                    eprintln!(
                        "hydradb: note: cluster `{}` has no endpoints yet (status.endpoints is empty)",
                        row.cluster
                    );
                }
            }
            Ok(())
        }
        OutputFormat::Yaml => print_yaml(&list_value(items)),
        OutputFormat::Json => Printer::new(OutputFormat::Json).print(&list_value(items)),
        OutputFormat::Name => Printer::new(OutputFormat::Name).print(&list_value(items)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn cluster_fixture() -> Value {
        json!({
            "apiVersion": "hydradb.kubexa.tech/v1alpha1",
            "kind": "GraphCluster",
            "metadata": {"name": "demo", "namespace": "demo"},
            "spec": {},
            "status": {"endpoints": {
                "bolt": "demo-bolt.demo.svc:7687",
                "https": "demo-https.demo.svc:443",
                "admin": "demo-admin.demo.svc:9090",
            }},
        })
    }

    #[test]
    fn prints_all_three_endpoints_from_status() {
        let row = endpoint_row(&cluster_fixture());
        assert_eq!(row.cluster, "demo");
        assert_eq!(row.bolt, "demo-bolt.demo.svc:7687");
        assert_eq!(row.https, "demo-https.demo.svc:443");
        assert_eq!(row.admin, "demo-admin.demo.svc:9090");
        assert!(!row.empty);
    }

    #[test]
    fn empty_endpoints_degrade_to_placeholders() {
        let cluster = json!({
            "kind": "GraphCluster",
            "metadata": {"name": "demo", "namespace": "demo"},
            "status": {},
        });
        let row = endpoint_row(&cluster);
        assert!(row.empty);
        assert_eq!(row.bolt, "<none>");
        assert_eq!(row.https, "<none>");
        assert_eq!(row.admin, "<none>");
    }

    #[test]
    fn partial_endpoints_keep_what_exists() {
        let cluster = json!({
            "kind": "GraphCluster",
            "metadata": {"name": "demo", "namespace": "demo"},
            "status": {"endpoints": {"bolt": "demo-bolt.demo.svc:7687"}},
        });
        let row = endpoint_row(&cluster);
        assert!(!row.empty);
        assert_eq!(row.bolt, "demo-bolt.demo.svc:7687");
        assert_eq!(row.https, "<none>");
        assert_eq!(row.admin, "<none>");
    }

    #[test]
    fn table_lists_the_three_endpoints() {
        let rows = [endpoint_row(&cluster_fixture())];
        let table: Vec<Vec<String>> = rows
            .iter()
            .map(|row| {
                vec![
                    row.cluster.clone(),
                    row.namespace.clone(),
                    row.bolt.clone(),
                    row.https.clone(),
                    row.admin.clone(),
                ]
            })
            .collect();
        let rendered = render_table(&["CLUSTER", "NAMESPACE", "BOLT", "HTTPS", "ADMIN"], &table);
        assert!(rendered.contains("CLUSTER"), "header present:\n{rendered}");
        assert!(
            rendered.contains("demo-bolt.demo.svc:7687"),
            "bolt present:\n{rendered}"
        );
        assert!(
            rendered.contains("demo-https.demo.svc:443"),
            "https present:\n{rendered}"
        );
        assert!(
            rendered.contains("demo-admin.demo.svc:9090"),
            "admin present:\n{rendered}"
        );
    }

    #[test]
    fn row_never_carries_spec_credentials() {
        let cluster = json!({
            "kind": "GraphCluster",
            "metadata": {"name": "demo", "namespace": "demo"},
            "spec": {"auth": {"token": "super-secret"}},
            "status": {"endpoints": {"bolt": "demo-bolt.demo.svc:7687"}},
        });
        let row = endpoint_row(&cluster);
        let rendered = format!("{row:?}");
        assert!(
            !rendered.contains("super-secret"),
            "no secret leaks: {rendered}"
        );
    }
}
