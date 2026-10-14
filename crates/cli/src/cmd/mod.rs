//! Subcommand implementations for the `hydradb` CLI.
//!
//! Todo 27 wires every subcommand and fully implements `version`. Todo 28
//! implements the read commands (`status`, `endpoints`, `logs`) as file
//! modules; `doctor` (todo 29) and `scale` (todo 30) stay honest
//! [`crate::CliError::NotImplemented`] stubs. They never succeed silently
//! and never hit the network.

use crate::{Cli, CliError};

/// `hydradb status` — summarise `GraphCluster`/`GraphNodePool`/`GraphIndexer`
/// conditions as a kstatus string (todo 28).
pub mod status;

/// `hydradb endpoints` — print Bolt/HTTPS/admin endpoints from
/// `status.endpoints` (todo 28).
pub mod endpoints;

/// `hydradb logs` — stream a role's pod logs (todo 28).
pub mod logs;

/// `hydradb doctor` — run the documented readiness checklist (todo 29).
pub mod doctor {
    use super::*;

    /// Arguments for `hydradb doctor`.
    #[derive(Debug, Clone, Default, clap::Args)]
    pub struct DoctorArgs {}

    /// Run `hydradb doctor` (todo 29 owns the body).
    ///
    /// # Errors
    ///
    /// Always returns [`CliError::NotImplemented`] until todo 29 lands.
    pub async fn run(_cli: &Cli, _args: &DoctorArgs) -> Result<(), CliError> {
        Err(CliError::NotImplemented {
            command: "doctor",
            owning_todo: 29,
        })
    }
}

/// `hydradb scale` — patch a `GraphNodePool`/`GraphIndexer` replica count
/// (todo 30).
pub mod scale {
    use super::*;

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

    /// Run `hydradb scale` (todo 30 owns the body).
    ///
    /// # Errors
    ///
    /// Always returns [`CliError::NotImplemented`] until todo 30 lands.
    pub async fn run(_cli: &Cli, _args: &ScaleArgs) -> Result<(), CliError> {
        Err(CliError::NotImplemented {
            command: "scale",
            owning_todo: 30,
        })
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
