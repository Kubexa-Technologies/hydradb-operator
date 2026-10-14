//! Read-command failure tests for the `hydradb` CLI (plan todo 28).
//!
//! These drive the real binary via `CARGO_BIN_EXE_hydradb`. Role validation
//! happens before any cluster contact, so unknown roles fail deterministically
//! with no cluster reachable.

use std::process::Command;

fn hydradb() -> Command {
    Command::new(env!("CARGO_BIN_EXE_hydradb"))
}

#[test]
fn logs_unknown_role_fails_naming_the_role() {
    let output = hydradb()
        .args(["-n", "demo", "logs", "worker"])
        .env("KUBECONFIG", "/nonexistent-kubeconfig-for-tests")
        .output()
        .expect("run `hydradb logs worker`");
    assert!(
        !output.status.success(),
        "`logs worker` must exit non-zero, stdout: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8(output.stderr).expect("stderr is utf-8");
    assert!(
        stderr.contains("worker"),
        "error must name the bad role, got:\n{stderr}"
    );
    assert!(
        stderr.contains("node") && stderr.contains("indexer"),
        "error must name the valid roles, got:\n{stderr}"
    );
}

#[test]
fn logs_negative_tail_fails_before_touching_the_cluster() {
    let output = hydradb()
        .args(["-n", "demo", "logs", "node", "--tail=-1"])
        .env("KUBECONFIG", "/nonexistent-kubeconfig-for-tests")
        .output()
        .expect("run `hydradb logs --tail -1`");
    assert!(
        !output.status.success(),
        "negative `--tail` must exit non-zero, stdout: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8(output.stderr).expect("stderr is utf-8");
    assert!(
        stderr.contains("--tail"),
        "error must name the flag, got:\n{stderr}"
    );
}
