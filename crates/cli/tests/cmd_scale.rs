//! Scale-command failure tests for the `hydradb` CLI (plan todo 30).
//!
//! These drive the real binary via `CARGO_BIN_EXE_hydradb`. Target parsing
//! and `--replicas` validation happen before any cluster contact, so both
//! fail deterministically with no cluster reachable. The happy-path patch
//! needs a live apiserver (absent on this box); it is covered by the
//! fixture-backed `scale_patch` unit test in `src/cmd/mod.rs`.

use std::process::Command;

fn hydradb() -> Command {
    Command::new(env!("CARGO_BIN_EXE_hydradb"))
}

#[test]
fn scale_negative_replicas_fails_at_parse_time() {
    let output = hydradb()
        .args(["-n", "demo", "scale", "graphnodepool/p", "--replicas=-1"])
        .env("KUBECONFIG", "/nonexistent-kubeconfig-for-tests")
        .output()
        .expect("run `hydradb scale --replicas -1`");
    assert!(
        !output.status.success(),
        "`scale --replicas -1` must exit non-zero, stdout: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8(output.stderr).expect("stderr is utf-8");
    assert!(
        stderr.contains("--replicas") || stderr.contains("replicas"),
        "error must name the flag, got:\n{stderr}"
    );
}

#[test]
fn scale_unknown_kind_fails_naming_valid_kinds() {
    let output = hydradb()
        .args(["-n", "demo", "scale", "graphcluster/p", "--replicas=5"])
        .env("KUBECONFIG", "/nonexistent-kubeconfig-for-tests")
        .output()
        .expect("run `hydradb scale graphcluster/p`");
    assert!(
        !output.status.success(),
        "`scale graphcluster/p` must exit non-zero, stdout: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8(output.stderr).expect("stderr is utf-8");
    assert!(
        stderr.contains("graphcluster"),
        "error must name the bad kind, got:\n{stderr}"
    );
    assert!(
        stderr.contains("graphnodepool") && stderr.contains("graphindexer"),
        "error must name the valid kinds, got:\n{stderr}"
    );
}

#[test]
fn scale_malformed_target_fails_with_shape_hint() {
    let output = hydradb()
        .args(["-n", "demo", "scale", "p", "--replicas=5"])
        .env("KUBECONFIG", "/nonexistent-kubeconfig-for-tests")
        .output()
        .expect("run `hydradb scale p`");
    assert!(
        !output.status.success(),
        "`scale p` must exit non-zero, stdout: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8(output.stderr).expect("stderr is utf-8");
    assert!(
        stderr.contains("graphnodepool"),
        "error must hint the target shape, got:\n{stderr}"
    );
}
