//! `hydradb logs` — stream a role's pod logs (plan todo 28).

use k8s_openapi::api::core::v1::Pod;
use kube::api::{Api, ListParams, LogParams};
use kube::{Client, ResourceExt};

use crate::{Cli, CliError, build_kube_config, effective_namespace};

/// Pod roles whose logs can be streamed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Graph worker nodes (`app.kubernetes.io/component=node`).
    Node,
    /// Index builders (`app.kubernetes.io/component=indexer`).
    Indexer,
}

impl Role {
    /// Label value used by the controller for this role's pods.
    #[must_use]
    pub const fn component_label(self) -> &'static str {
        match self {
            Role::Node => "node",
            Role::Indexer => "indexer",
        }
    }

    /// Role name as typed on the command line.
    #[must_use]
    pub const fn name(self) -> &'static str {
        self.component_label()
    }
}

/// Parsed `logs` target: a role plus an optional `role/cluster` qualifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogTarget {
    /// Selected role.
    pub role: Role,
    /// Owning cluster when the target was `role/cluster`.
    pub cluster: Option<String>,
}

/// Parse the `logs` target (`node`, `indexer` or `role/cluster`).
///
/// The target is validated before touching the cluster, so an unknown role
/// fails deterministically with a message naming the valid roles. A missing
/// target defaults to the `node` role.
///
/// # Errors
///
/// Returns [`CliError::Output`] for an unknown role.
pub fn parse_target(target: Option<&str>) -> Result<LogTarget, CliError> {
    let raw = target.unwrap_or("node");
    let (role_text, cluster) = match raw.split_once('/') {
        Some((role, cluster)) => (role, Some(cluster.to_string())),
        None => (raw, None),
    };
    let role = match role_text {
        "node" => Role::Node,
        "indexer" => Role::Indexer,
        other => {
            return Err(CliError::Output {
                message: format!("unknown logs role `{other}` (expected `node` or `indexer`)",),
            });
        }
    };
    Ok(LogTarget { role, cluster })
}

/// Build the pod label selector for a role, mirroring the labels the
/// controller sets (`reconcile/nodepool.rs` and `reconcile/indexer.rs`):
/// `app.kubernetes.io/name=hydradb` plus `component=<role>`, narrowed by
/// `instance=<cluster>` when the target named one.
#[must_use]
pub fn pod_selector(target: &LogTarget) -> String {
    let mut selector = format!(
        "app.kubernetes.io/name=hydradb,app.kubernetes.io/component={}",
        target.role.component_label()
    );
    if let Some(cluster) = &target.cluster {
        selector.push_str(&format!(",app.kubernetes.io/instance={cluster}"));
    }
    selector
}

/// Error for a role with no pods: always names the role.
#[must_use]
pub fn no_pods_error(target: &LogTarget, scope: &str) -> CliError {
    CliError::Output {
        message: format!("no pods found for role `{}` {scope}", target.role.name()),
    }
}

/// Arguments for `hydradb logs`.
#[derive(Debug, Clone, clap::Args)]
pub struct LogsArgs {
    /// Role (`node` or `indexer`) or `role/name` selector whose logs to stream.
    #[arg(value_name = "TARGET")]
    pub target: Option<String>,

    /// Follow the log stream.
    #[arg(long)]
    pub follow: bool,

    /// Lines of recent log to show.
    #[arg(long, value_name = "LINES")]
    pub tail: Option<i64>,

    /// Container to stream logs from.
    #[arg(long, value_name = "CONTAINER")]
    pub container: Option<String>,
}

/// Run `hydradb logs`.
///
/// # Errors
///
/// Returns [`CliError`] for an unknown role, a negative `--tail`, connection
/// or listing failures, when the role has no pods (message names the role),
/// or when the log stream fails.
pub async fn run(cli: &Cli, args: &LogsArgs) -> Result<(), CliError> {
    let target = parse_target(args.target.as_deref())?;
    if args.tail.is_some_and(|lines| lines < 0) {
        return Err(CliError::Output {
            message: format!(
                "invalid --tail value {} (expected >= 0)",
                args.tail.unwrap_or(0)
            ),
        });
    }
    let config = build_kube_config(cli).await?;
    let client = Client::try_from(config.clone())?;
    let namespace = effective_namespace(cli, &config);

    let pods = match &namespace {
        Some(ns) => Api::<Pod>::namespaced(client, ns),
        None => Api::<Pod>::all(client),
    };
    let params = ListParams {
        label_selector: Some(pod_selector(&target)),
        ..ListParams::default()
    };
    let mut pod_list = pods.list(&params).await?.items;
    pod_list.sort_by(|left, right| {
        (left.namespace(), left.name_any()).cmp(&(right.namespace(), right.name_any()))
    });
    let pod = pod_list.first().ok_or_else(|| {
        let scope = namespace.map_or_else(
            || "in any namespace".to_string(),
            |ns| format!("in namespace `{ns}`"),
        );
        no_pods_error(&target, &scope)
    })?;

    let log_params = LogParams {
        container: args.container.clone(),
        follow: args.follow,
        tail_lines: args.tail,
        ..LogParams::default()
    };
    let pod_name = pod.name_any();
    let pod_scope = pod
        .namespace()
        .map_or_else(|| pod_name.clone(), |ns| format!("{ns}/{pod_name}"));
    eprintln!(
        "hydradb: streaming logs from pod `{pod_scope}` (role `{}`)",
        target.role.name()
    );
    // `logs` collects the stream into a string; with `--follow` the call
    // stays open (like `tail -f`) until interrupted.
    let output = pods.logs(&pod_name, &log_params).await?;
    print!("{output}");
    if !output.ends_with('\n') {
        println!();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_target_defaults_to_node() {
        let target = parse_target(None).expect("bare target parses");
        assert_eq!(target.role, Role::Node);
        assert_eq!(target.cluster, None);
    }

    #[test]
    fn indexer_role_parses() {
        let target = parse_target(Some("indexer")).expect("indexer parses");
        assert_eq!(target.role, Role::Indexer);
    }

    #[test]
    fn role_slash_cluster_parses() {
        let target = parse_target(Some("node/demo")).expect("role/cluster parses");
        assert_eq!(target.role, Role::Node);
        assert_eq!(target.cluster.as_deref(), Some("demo"));
    }

    #[test]
    fn unknown_role_errors_naming_valid_roles() {
        let err = parse_target(Some("worker")).expect_err("unknown role must fail");
        let message = err.to_string();
        assert!(message.contains("worker"), "names the bad role: {message}");
        assert!(message.contains("node"), "names valid roles: {message}");
        assert!(message.contains("indexer"), "names valid roles: {message}");
    }

    #[test]
    fn selector_uses_controller_labels() {
        let target = parse_target(Some("node")).expect("node parses");
        assert_eq!(
            pod_selector(&target),
            "app.kubernetes.io/name=hydradb,app.kubernetes.io/component=node"
        );
        let scoped = parse_target(Some("indexer/demo")).expect("scoped parses");
        assert_eq!(
            pod_selector(&scoped),
            "app.kubernetes.io/name=hydradb,app.kubernetes.io/component=indexer,\
             app.kubernetes.io/instance=demo"
        );
    }

    #[test]
    fn no_pods_error_names_the_role() {
        let target = parse_target(Some("indexer")).expect("indexer parses");
        let err = no_pods_error(&target, "in namespace `demo`");
        let message = err.to_string();
        assert!(
            message.contains("indexer"),
            "message names the role: {message}"
        );
        assert!(
            message.contains("demo"),
            "message names the scope: {message}"
        );
    }

    #[test]
    fn no_pods_error_for_default_node_role() {
        let target = parse_target(None).expect("bare target parses");
        let err = no_pods_error(&target, "in any namespace");
        assert!(err.to_string().contains("node"), "names `node`: {err}");
    }
}
