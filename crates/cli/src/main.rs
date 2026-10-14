//! `hydradb` — CLI for the HydraDB operator (`kubectl hydradb` plugin).
//!
//! The binary is named `hydradb` so krew installs it as `kubectl-hydradb`;
//! the standalone binary and the plugin coexist via the krew symlink.
//!
//! Todo 27 provides the clap surface, global flags, logging and the shared
//! client helper. Command bodies land in todos 28-30.

use std::path::PathBuf;

use clap::{ArgAction, Parser, Subcommand};

pub mod cmd;
pub mod output;

/// Re-exported for todos 28-30, which need the API group/version.
pub use hydradb_operator_api::constants;

use output::OutputFormat;

/// HydraDB operator CLI.
#[derive(Debug, Parser)]
#[command(
    name = "hydradb",
    version,
    about = "Manage HydraDB graph clusters on Kubernetes",
    long_about = "Manage HydraDB graph clusters (GraphCluster, GraphNodePool, \
        GraphIndexer) on Kubernetes.\n\nReads kubeconfig via the standard \
        KUBECONFIG rules; --kubeconfig/--context override the file and the \
        context. Never talks to the cluster at parse time."
)]
pub struct Cli {
    /// Path to a kubeconfig file (defaults to the standard KUBECONFIG rules).
    #[arg(long, value_name = "PATH", env = "KUBECONFIG")]
    pub kubeconfig: Option<PathBuf>,

    /// Kubeconfig context to use.
    #[arg(long, value_name = "NAME")]
    pub context: Option<String>,

    /// Namespace to operate in.
    #[arg(short = 'n', long, value_name = "NS")]
    pub namespace: Option<String>,

    /// Operate across all namespaces, ignoring `--namespace`.
    #[arg(short = 'A', long)]
    pub all_namespaces: bool,

    /// Output format.
    #[arg(short = 'o', long, value_enum, default_value_t = OutputFormat::Table)]
    pub output: OutputFormat,

    /// Verbosity: `-v` for debug, `-vv` for trace.
    #[arg(short = 'v', action = ArgAction::Count)]
    pub verbose: u8,

    /// Subcommand to run.
    #[command(subcommand)]
    pub command: Command,
}

/// Subcommands of the `hydradb` CLI.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Show cluster/node-pool/indexer conditions as a kstatus string.
    Status(cmd::status::StatusArgs),
    /// Print the Bolt/HTTPS/admin endpoints from `status.endpoints`.
    Endpoints(cmd::endpoints::EndpointsArgs),
    /// Stream a role's pod logs.
    Logs(cmd::logs::LogsArgs),
    /// Run the readiness checklist, exiting 1 on any FAIL.
    Doctor(cmd::doctor::DoctorArgs),
    /// Patch a `GraphNodePool`/`GraphIndexer` replica count.
    Scale(cmd::scale::ScaleArgs),
    /// Print the CLI crate version.
    Version(cmd::version::VersionArgs),
}

/// Errors the CLI can return.
#[derive(Debug, thiserror::Error)]
pub enum CliError {
    /// Command body lives in a later plan todo; wiring exists, logic does not.
    #[error("command `{command}` is not implemented yet (see plan todo {owning_todo})")]
    NotImplemented {
        /// Subcommand (or `output:<mode>`) that is not implemented yet.
        command: &'static str,
        /// Plan todo that owns the implementation.
        owning_todo: u32,
    },

    /// Kubernetes client or API error.
    #[error("kubernetes error: {0}")]
    Kube(#[from] kube::Error),

    /// Kubeconfig loading error.
    #[error("kubeconfig error: {0}")]
    Kubeconfig(#[from] kube::config::KubeconfigError),

    /// Infer-config error (no kubeconfig and no in-cluster config).
    #[error("cannot load kubernetes config: {0}")]
    InferConfig(#[from] kube::config::InferConfigError),

    /// Filesystem error (e.g. reading `--kubeconfig`).
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// Output rendering error.
    #[error("{message}")]
    Output {
        /// Human-readable description of what went wrong.
        message: String,
    },

    /// JSON rendering error.
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

/// Build a [`kube::Config`] from the global flags.
///
/// Respects `--kubeconfig` (explicit file) and `--context`, falling back to
/// the standard kubeconfig rules and then to the in-cluster config. Never
/// called at parse time — only by commands that need the cluster (todos
/// 28-30 share this helper).
///
/// # Errors
///
/// Returns [`CliError`] when no usable config is found or the file cannot
/// be loaded.
pub async fn build_kube_config(cli: &Cli) -> Result<kube::Config, CliError> {
    let options = kube::config::KubeConfigOptions {
        context: cli.context.clone(),
        ..Default::default()
    };
    if let Some(path) = &cli.kubeconfig {
        let kubeconfig = kube::config::Kubeconfig::read_from(path)?;
        kube::Config::from_custom_kubeconfig(kubeconfig, &options)
            .await
            .map_err(CliError::from)
    } else if cli.context.is_some() {
        kube::Config::from_kubeconfig(&options)
            .await
            .map_err(CliError::from)
    } else {
        kube::Config::infer().await.map_err(CliError::from)
    }
}

/// Build a [`kube::Client`] from the global flags (see [`build_kube_config`]).
///
/// # Errors
///
/// Returns [`CliError`] when the config cannot be loaded.
pub async fn build_client(cli: &Cli) -> Result<kube::Client, CliError> {
    let config = build_kube_config(cli).await?;
    Ok(kube::Client::try_from(config)?)
}

/// Resolve the effective namespace: `--all-namespaces` wins (meaning "all",
/// represented as [`None`]), then `--namespace`, then the config default.
#[must_use]
pub fn effective_namespace(cli: &Cli, config: &kube::Config) -> Option<String> {
    if cli.all_namespaces {
        None
    } else if let Some(ns) = &cli.namespace {
        Some(ns.clone())
    } else {
        Some(config.default_namespace.clone())
    }
}

/// Initialise tracing from verbosity and env.
///
/// Precedence: `HYDRADB_LOG`, then `RUST_LOG`, then `-v`/`-vv` (debug/trace,
/// default info).
fn init_logging(verbose: u8) {
    let default_level: &str = match verbose {
        0 => "info",
        1 => "debug",
        _ => "trace",
    };
    let filter = std::env::var("HYDRADB_LOG")
        .or_else(|_| std::env::var("RUST_LOG"))
        .unwrap_or_else(|_| default_level.to_string());
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(filter)),
        )
        .try_init();
}

async fn run() -> Result<(), CliError> {
    let cli = Cli::parse();
    init_logging(cli.verbose);
    match &cli.command {
        Command::Status(args) => cmd::status::run(&cli, args).await,
        Command::Endpoints(args) => cmd::endpoints::run(&cli, args).await,
        Command::Logs(args) => cmd::logs::run(&cli, args).await,
        Command::Doctor(args) => cmd::doctor::run(&cli, args).await,
        Command::Scale(args) => cmd::scale::run(&cli, args).await,
        Command::Version(args) => cmd::version::run(args).await,
    }
}

#[tokio::main]
async fn main() {
    if let Err(err) = run().await {
        eprintln!("hydradb: error: {err}");
        std::process::exit(1);
    }
}
