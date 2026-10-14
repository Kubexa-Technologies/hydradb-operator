//! CLI surface tests for the `hydradb` binary (plan todo 27).
//!
//! Integration-test target for the bin crate: the binary is driven via
//! `CARGO_BIN_EXE_hydradb` and no test touches the network.

use std::process::Command;

fn hydradb() -> Command {
    Command::new(env!("CARGO_BIN_EXE_hydradb"))
}

#[test]
fn help_lists_every_subcommand() {
    let output = hydradb()
        .arg("--help")
        .output()
        .expect("run `hydradb --help`");
    assert!(
        output.status.success(),
        "`--help` must exit 0, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let help = String::from_utf8(output.stdout).expect("help is utf-8");
    for subcommand in ["status", "endpoints", "logs", "doctor", "scale", "version"] {
        assert!(
            help.contains(subcommand),
            "`--help` must list `{subcommand}`, got:\n{help}"
        );
    }
}

#[test]
fn version_prints_the_crate_version() {
    let output = hydradb()
        .arg("version")
        .output()
        .expect("run `hydradb version`");
    assert!(
        output.status.success(),
        "`version` must exit 0, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let printed = String::from_utf8(output.stdout).expect("version is utf-8");
    assert_eq!(printed.trim(), env!("CARGO_PKG_VERSION"));
}

#[test]
fn output_rejects_an_unknown_format() {
    let output = hydradb()
        .args(["-o", "bogus", "version"])
        .output()
        .expect("run `hydradb -o bogus version`");
    assert!(
        !output.status.success(),
        "a bogus `-o` value must fail, stdout: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8(output.stderr).expect("stderr is utf-8");
    for choice in ["table", "wide", "yaml", "json", "name"] {
        assert!(
            stderr.contains(choice),
            "error must name valid choice `{choice}`, got:\n{stderr}"
        );
    }
}

#[test]
fn global_flags_parse_without_a_cluster() {
    // No cluster is reachable here; `version` performs no I/O, so success
    // proves the global flags parse without touching the network.
    let output = hydradb()
        .args(["-n", "demo", "-A", "-o", "json", "-vv", "version"])
        .env_remove("KUBECONFIG")
        .output()
        .expect("run version with global flags");
    assert!(
        output.status.success(),
        "global flags must parse without a cluster, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let printed = String::from_utf8(output.stdout).expect("version is utf-8");
    assert_eq!(printed.trim(), env!("CARGO_PKG_VERSION"));
}
