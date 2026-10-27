//! Subcommand implementations for the `hydradb` CLI.
//!
//! Todo 27 wires every subcommand and fully implements `version`. Todo 28
//! implements the read commands (`status`, `endpoints`, `logs`) as file
//! modules, todo 29 implements `doctor`, todo 30 implements `scale` plus the
//! shared output renderers. Stubs never succeed silently
//! and never hit the network.

use crate::{Cli, CliError};

/// `hydradb backup` / `hydradb restore` — object-store copies.
pub mod backup;

/// `hydradb status` — summarise `GraphCluster`/`GraphNodePool`/`GraphIndexer`
/// conditions as a kstatus string (todo 28).
pub mod status;

/// `hydradb endpoints` — print Bolt/HTTPS/admin endpoints from
/// `status.endpoints` (todo 28).
pub mod endpoints;

/// `hydradb logs` — stream a role's pod logs (todo 28).
pub mod logs;

/// `hydradb doctor` — run the documented readiness checklist (todo 29).
pub mod doctor;

/// `hydradb scale` — patch a `GraphNodePool`/`GraphIndexer` replica count
/// (todo 30).
pub mod scale {
    use super::*;
    use crate::{build_client, build_kube_config, effective_namespace};
    use kube::api::{Api, Patch, PatchParams};
    use serde_json::json;

    /// Which CRD kind a `scale` target names.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum ScaleKind {
        /// A `GraphNodePool`.
        NodePool,
        /// A `GraphIndexer`.
        Indexer,
    }

    impl ScaleKind {
        /// Kubernetes kind as stored on the object.
        #[must_use]
        pub const fn kind(self) -> &'static str {
            match self {
                ScaleKind::NodePool => "GraphNodePool",
                ScaleKind::Indexer => "GraphIndexer",
            }
        }
    }

    /// A parsed `scale` target: a scalable kind plus the object name.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct ScaleTarget {
        /// Which CRD kind to patch.
        pub kind: ScaleKind,
        /// Name of the object to patch.
        pub name: String,
    }

    /// Parse a `scale` target (`graphnodepool/<name>` or
    /// `graphindexer/<name>`).
    ///
    /// Parsing happens before touching the cluster, so an unknown kind
    /// fails deterministically with a message naming the valid kinds.
    ///
    /// # Errors
    ///
    /// Returns [`CliError::Output`] for a malformed target or an unknown kind.
    pub fn parse_target(raw: &str) -> Result<ScaleTarget, CliError> {
        let (kind_text, name) = raw.split_once('/').ok_or_else(|| CliError::Output {
            message: format!(
                "invalid scale target `{raw}` \
                 (expected `graphnodepool/<name>` or `graphindexer/<name>`)"
            ),
        })?;
        let kind = match kind_text.to_lowercase().as_str() {
            "graphnodepool" | "graphnodepools" => ScaleKind::NodePool,
            "graphindexer" | "graphindexers" => ScaleKind::Indexer,
            other => {
                return Err(CliError::Output {
                    message: format!(
                        "unknown scale kind `{other}` \
                         (expected `graphnodepool` or `graphindexer`)"
                    ),
                });
            }
        };
        if name.is_empty() {
            return Err(CliError::Output {
                message: format!("invalid scale target `{raw}` (object name is empty)"),
            });
        }
        Ok(ScaleTarget {
            kind,
            name: name.to_string(),
        })
    }

    /// Build the merge-patch body for a replica-count change.
    ///
    /// The patch touches `spec.replicas` only — the CR is the API, so the
    /// StatefulSet/Deployment owned by the controller is never scaled
    /// directly.
    #[must_use]
    pub fn scale_patch(replicas: u32) -> serde_json::Value {
        json!({"spec": {"replicas": replicas}})
    }

    /// Arguments for `hydradb scale`.
    #[derive(Debug, Clone, clap::Args)]
    pub struct ScaleArgs {
        /// Scale target as `graphnodepool/<name>` or `graphindexer/<name>`.
        #[arg(value_name = "TARGET")]
        pub target: String,

        /// Desired replica count. Parsed as unsigned so `--replicas -1`
        /// fails at parse time, before touching the cluster.
        #[arg(long, value_name = "N")]
        pub replicas: u32,
    }

    /// Run `hydradb scale`: merge-patch `spec.replicas` on the targeted
    /// `GraphNodePool` or `GraphIndexer`, then print the new replica count.
    ///
    /// # Errors
    ///
    /// Returns [`CliError`] for a malformed target, when the cluster cannot
    /// be reached, or when the patch is rejected.
    pub async fn run(cli: &Cli, args: &ScaleArgs) -> Result<(), CliError> {
        let target = parse_target(&args.target)?;
        let config = build_kube_config(cli).await?;
        let client = build_client(cli).await?;
        let namespace = effective_namespace(cli, &config);
        let params = PatchParams::default();
        let body = scale_patch(args.replicas);
        match target.kind {
            ScaleKind::NodePool => {
                use hydradb_operator_api::crd::GraphNodePool;
                let api = match &namespace {
                    Some(ns) => Api::<GraphNodePool>::namespaced(client, ns),
                    None => Api::<GraphNodePool>::all(client),
                };
                api.patch(&target.name, &params, &Patch::Merge(&body))
                    .await?;
            }
            ScaleKind::Indexer => {
                use hydradb_operator_api::crd::GraphIndexer;
                let api = match &namespace {
                    Some(ns) => Api::<GraphIndexer>::namespaced(client, ns),
                    None => Api::<GraphIndexer>::all(client),
                };
                api.patch(&target.name, &params, &Patch::Merge(&body))
                    .await?;
            }
        }
        println!(
            "{}/{} scaled to {}",
            target.kind.kind().to_lowercase(),
            target.name,
            args.replicas
        );
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn nodepool_target_parses() {
            let target = parse_target("graphnodepool/p").expect("pool target parses");
            assert_eq!(target.kind, ScaleKind::NodePool);
            assert_eq!(target.name, "p");
        }

        #[test]
        fn indexer_target_parses() {
            let target = parse_target("graphindexer/i").expect("indexer target parses");
            assert_eq!(target.kind, ScaleKind::Indexer);
            assert_eq!(target.name, "i");
        }

        #[test]
        fn kind_matching_is_case_insensitive() {
            let target = parse_target("GraphNodePool/p").expect("mixed case parses");
            assert_eq!(target.kind, ScaleKind::NodePool);
        }

        #[test]
        fn unknown_kind_errors_naming_valid_kinds() {
            let err = parse_target("graphcluster/p").expect_err("unknown kind must fail");
            let message = err.to_string();
            assert!(
                message.contains("graphcluster"),
                "names bad kind: {message}"
            );
            assert!(
                message.contains("graphnodepool"),
                "names valid kinds: {message}"
            );
            assert!(
                message.contains("graphindexer"),
                "names valid kinds: {message}"
            );
        }

        #[test]
        fn missing_slash_errors() {
            let err = parse_target("p").expect_err("bare name must fail");
            assert!(
                err.to_string().contains("graphnodepool"),
                "hints shape: {err}"
            );
        }

        #[test]
        fn empty_name_errors() {
            let err = parse_target("graphnodepool/").expect_err("empty name must fail");
            assert!(err.to_string().contains("empty"), "names problem: {err}");
        }

        #[test]
        fn patch_body_touches_spec_replicas_only() {
            let body = scale_patch(5);
            assert_eq!(body, json!({"spec": {"replicas": 5}}));
        }
    }
}

/// `hydradb version` — print the CLI crate version.
pub mod version {
    use super::CliError;

    /// Arguments for `hydradb version`.
    #[derive(Debug, Clone, Default, clap::Args)]
    pub struct VersionArgs {}

    /// Run `hydradb version`.
    ///
    /// # Errors
    ///
    /// This command performs no I/O and cannot fail; the `Result` keeps the
    /// dispatch surface uniform.
    pub async fn run(_args: &VersionArgs) -> Result<(), CliError> {
        println!("{}", env!("CARGO_PKG_VERSION"));
        Ok(())
    }
}
