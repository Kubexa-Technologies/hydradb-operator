//! HydraDB operator controller binary.
//!
//! The binary takes no flags: all configuration comes from the environment
//! (`KUBECONFIG` / in-cluster config, `OPERATOR_NAMESPACE`, `OPERATOR_SCOPE`,
//! `RUST_LOG`).
//! Unknown arguments (including `--help`) are ignored so the binary stays a
//! harmless no-op under `kubectl exec` probes and `--help` checks; there is
//! deliberately no CLI framework here (see `crates/cli` for the `hydradb`
//! command).
//!
//! All reconciler modules live in the library (`src/lib.rs`) so the
//! `tests/` suites can drive them; this binary only installs tracing and
//! calls [`hydradb_operator::run::run`].

use hydradb_operator::run;
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

/// Install tracing, then run until shutdown or a fatal startup error.
///
/// A top-level failure exits non-zero with a clear message on stderr.
/// This function never panics.
#[tokio::main(flavor = "multi_thread")]
async fn main() {
    init_tracing();
    if let Err(e) = run::run().await {
        eprintln!("hydradb-operator: error: {e}");
        std::process::exit(1);
    }
}

/// Install a `tracing` subscriber honoring `RUST_LOG` (default `info`).
///
/// When the `otlp` feature is compiled in and `OTEL_EXPORTER_OTLP_ENDPOINT`
/// is set, the OTLP layer from [`run::init_otlp`] is attached on top;
/// otherwise the subscriber is local-only. Either way there is exactly one
/// global subscriber install.
fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    match run::init_otlp() {
        Some(otlp_layer) => tracing_subscriber::registry()
            .with(otlp_layer)
            .with(filter)
            .with(fmt::layer())
            .init(),
        None => tracing_subscriber::registry()
            .with(filter)
            .with(fmt::layer())
            .init(),
    }
}
