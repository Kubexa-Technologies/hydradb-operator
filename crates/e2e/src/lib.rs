//! HydraDB end-to-end test helpers.
//!
//! Kind-based harness skeleton. Every cluster-dependent entry point degrades to
//! a graceful skip (`Ok(None)` / early return) when no cluster is reachable, so
//! the e2e suite passes as a no-op off-cluster and CI must opt in explicitly
//! with `cargo test -- --ignored`.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use kube::Client;

/// Name of the ephemeral kind cluster this harness creates.
pub const CLUSTER_NAME: &str = "hydradb-operator-e2e";

/// Default namespace the chart is installed into by [`Harness::helm_install`].
pub const DEFAULT_RELEASE_NAMESPACE: &str = "hydradb-operator-system";

/// Ephemeral kind-based e2e harness.
#[derive(Debug, Clone)]
pub struct Harness {
    cluster_name: String,
}

impl Default for Harness {
    fn default() -> Self {
        Self::new()
    }
}

impl Harness {
    /// Create a harness targeting the standard e2e cluster name.
    #[must_use]
    pub fn new() -> Self {
        Self {
            cluster_name: CLUSTER_NAME.to_owned(),
        }
    }

    /// Create a harness targeting a custom kind cluster name.
    #[must_use]
    pub fn with_cluster_name(name: impl Into<String>) -> Self {
        Self {
            cluster_name: name.into(),
        }
    }

    /// The kind cluster name this harness manages.
    #[must_use]
    pub fn cluster_name(&self) -> &str {
        &self.cluster_name
    }

    /// Resolve a kubeconfig path without hardcoding one location.
    ///
    /// Order: `KUBECONFIG` (first existing entry), then `~/.kube/config`.
    /// When `KUBECONFIG` is set it wins: a value pointing at nothing that
    /// exists resolves to `None` instead of falling back. Returns `None`
    /// when nothing usable is found instead of panicking.
    #[must_use]
    pub fn kubeconfig_path() -> Option<PathBuf> {
        if let Ok(raw) = std::env::var("KUBECONFIG") {
            if raw.is_empty() {
                return Self::default_kubeconfig_path().filter(|p| p.is_file());
            }
            for entry in std::env::split_paths(&raw) {
                if entry.is_file() {
                    return Some(entry);
                }
            }
            return None;
        }
        Self::default_kubeconfig_path().filter(|p| p.is_file())
    }

    fn default_kubeconfig_path() -> Option<PathBuf> {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .map(|home| home.join(".kube").join("config"))
    }

    /// Try to build a cluster client.
    ///
    /// Returns `Ok(None)` — never an error, never a panic — when no kubeconfig
    /// exists or the apiserver cannot be reached. Tests treat `None` as a
    /// graceful skip.
    pub async fn try_client() -> Result<Option<Client>> {
        if Self::kubeconfig_path().is_none() && std::env::var_os("KUBECONFIG").is_some() {
            // `KUBECONFIG` points at nothing that exists.
            return Ok(None);
        }
        let client = match Client::try_default().await {
            Ok(client) => client,
            Err(_) => return Ok(None),
        };
        match client.apiserver_version().await {
            Ok(_) => Ok(Some(client)),
            Err(_) => Ok(None),
        }
    }

    /// Create the kind cluster. Reports a clear error when the `kind` binary
    /// is absent instead of panicking.
    pub fn kind_create(&self) -> Result<()> {
        run_cmd(
            "kind",
            &[
                "create".to_owned(),
                "cluster".to_owned(),
                "--name".to_owned(),
                self.cluster_name.clone(),
            ],
        )
    }

    /// Delete the kind cluster. Succeeds silently when `kind` is absent or the
    /// cluster does not exist, so teardown is safe to call unconditionally.
    pub fn kind_delete(&self) -> Result<()> {
        if which("kind").is_none() {
            return Ok(());
        }
        match run_cmd(
            "kind",
            &[
                "delete".to_owned(),
                "cluster".to_owned(),
                "--name".to_owned(),
                self.cluster_name.clone(),
            ],
        ) {
            Ok(()) => Ok(()),
            Err(e) => {
                let msg = format!("{e:#}");
                if msg.contains("No nodes found for this cluster")
                    || msg.contains("failed to get nodes")
                {
                    Ok(())
                } else {
                    Err(e)
                }
            }
        }
    }

    /// `helm install` the chart from a local path into `namespace`.
    pub fn helm_install(&self, chart_path: &Path, namespace: &str) -> Result<()> {
        let chart = chart_path.display().to_string();
        run_cmd(
            "helm",
            &[
                "install".to_owned(),
                "hydradb-operator".to_owned(),
                chart,
                "--namespace".to_owned(),
                namespace.to_owned(),
                "--create-namespace".to_owned(),
            ],
        )
    }

    /// `helm uninstall` the release. Safe to call when setup never ran.
    pub fn helm_uninstall(&self, namespace: &str) -> Result<()> {
        if which("helm").is_none() {
            return Ok(());
        }
        match run_cmd(
            "helm",
            &[
                "uninstall".to_owned(),
                "hydradb-operator".to_owned(),
                "--namespace".to_owned(),
                namespace.to_owned(),
            ],
        ) {
            Ok(()) => Ok(()),
            Err(e) => {
                let msg = format!("{e:#}");
                if msg.contains("not found") || msg.contains("release: not found") {
                    Ok(())
                } else {
                    Err(e)
                }
            }
        }
    }

    /// Best-effort teardown of both helm release and kind cluster.
    pub fn teardown(&self, namespace: &str) -> Result<()> {
        let _ = self.helm_uninstall(namespace);
        self.kind_delete()
    }
}

/// Run `binary args`, capturing output. On failure the error carries the exact
/// command plus stderr so CI logs show what broke.
fn run_cmd(binary: &str, args: &[String]) -> Result<()> {
    let output = Command::new(binary)
        .args(args)
        .output()
        .with_context(|| format!("failed to execute `{binary}`; is it installed and on PATH?"))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let cmdline = std::iter::once(binary.to_owned())
        .chain(args.iter().cloned())
        .collect::<Vec<_>>()
        .join(" ");
    anyhow::bail!("command `{cmdline}` failed: {stderr}")
}

/// Minimal `which`: search `PATH` for an executable file.
fn which(binary: &str) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&paths) {
        let candidate = dir.join(binary);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cluster_name_defaults() {
        assert_eq!(Harness::new().cluster_name(), "hydradb-operator-e2e");
    }

    #[test]
    fn missing_kubeconfig_resolves_to_none() {
        let old = std::env::var_os("KUBECONFIG");
        // SAFETY: single-threaded unit test mutating process env for itself.
        unsafe {
            std::env::set_var("KUBECONFIG", "/nonexistent-kubeconfig-for-unit-test");
        }
        let path = Harness::kubeconfig_path();
        if let Some(old) = old {
            // SAFETY: restoring the previous value.
            unsafe {
                std::env::set_var("KUBECONFIG", old);
            }
        } else {
            // SAFETY: removing a value this test set.
            unsafe {
                std::env::remove_var("KUBECONFIG");
            }
        }
        assert!(path.is_none(), "KUBECONFIG pointing at nothing must win");
    }
}
