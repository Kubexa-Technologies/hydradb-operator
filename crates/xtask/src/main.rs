//! HydraDB developer xtask binary.
//!
//! Tiny hand-rolled dispatcher over `std::env::args()`. Deliberately
//! dependency-light: no clap here (the CLI crate owns clap).

mod check_phases;
mod check_wiring;
mod gen_crds;
mod gen_docs;
mod gen_rbac;

use std::path::{Path, PathBuf};

fn usage() -> &'static str {
    "usage: cargo xtask <gen-crds|gen-rbac|gen-docs|gen-all|check-wiring|check-phases|check-drift> [--check]"
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Diff rendered artifacts against the tree without writing.
///
/// Names every drifted file and fails; clean trees pass silently apart from a
/// one-line summary.
fn check_rendered(rendered: Vec<(String, String)>) -> anyhow::Result<()> {
    let root = workspace_root();
    let mut drifted: Vec<String> = Vec::new();
    for (rel, want) in &rendered {
        let path = root.join(rel);
        let have = std::fs::read_to_string(&path).unwrap_or_default();
        if have != *want {
            drifted.push(rel.clone());
        }
    }
    if drifted.is_empty() {
        println!("drift check clean ({} file(s) current)", rendered.len());
        return Ok(());
    }
    drifted.sort();
    for rel in &drifted {
        eprintln!("drift detected in {rel}: regenerate with `cargo xtask gen-all`");
    }
    anyhow::bail!(
        "drift check failed: {} generated file(s) differ from the tree:\n  {}",
        drifted.len(),
        drifted.join("\n  ")
    )
}

fn gen_all_rendered() -> anyhow::Result<Vec<(String, String)>> {
    let mut out = gen_crds::render_all()?;
    out.extend(gen_rbac::render_all()?);
    out.extend(gen_docs::render_all()?);
    Ok(out)
}

fn check_drift() -> anyhow::Result<()> {
    check_rendered(gen_all_rendered()?)
}

fn main() {
    let rest: Vec<String> = std::env::args().skip(1).collect();
    let Some((subcommand, flags)) = rest.split_first() else {
        eprintln!("{}", usage());
        std::process::exit(1);
    };
    let check = flags.iter().any(|flag| flag == "--check");
    if flags.iter().any(|flag| flag != "--check") {
        eprintln!("unknown flag(s)\n{}", usage());
        std::process::exit(1);
    }
    let result = match (subcommand.as_str(), check) {
        ("gen-crds", false) => gen_crds::run(),
        ("gen-crds", true) => gen_crds::render_all().and_then(check_rendered),
        ("gen-rbac", false) => gen_rbac::run(),
        ("gen-rbac", true) => gen_rbac::render_all().and_then(check_rendered),
        ("gen-docs", false) => gen_docs::run(),
        ("gen-docs", true) => gen_docs::render_all().and_then(check_rendered),
        ("gen-all", false) => gen_all_rendered().and_then(|rendered| {
            let root = workspace_root();
            write_all(&root, &rendered)
        }),
        ("gen-all", true) => check_drift(),
        ("check-wiring", false) => check_wiring::run(),
        ("check-phases", false) => check_phases::run(),
        ("check-drift", false) => check_drift()
            .and_then(|()| check_wiring::run())
            .and_then(|()| check_phases::run()),
        ("check-wiring" | "check-phases" | "check-drift", true) => {
            eprintln!("{subcommand} takes no --check flag\n{}", usage());
            std::process::exit(1);
        }
        _ => {
            eprintln!("unknown subcommand '{subcommand}'\n{}", usage());
            std::process::exit(1);
        }
    };
    if let Err(err) = result {
        eprintln!("{subcommand} failed: {err:?}");
        std::process::exit(1);
    }
}

/// Write every rendered artifact, keeping runs idempotent without churning
/// mtimes (same helper the per-kind `run()` fns inline).
fn write_all(root: &Path, rendered: &[(String, String)]) -> anyhow::Result<()> {
    for (rel, contents) in rendered {
        let path = root.join(rel);
        let current = std::fs::read_to_string(&path).unwrap_or_default();
        if current != *contents {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&path, contents)?;
        }
        println!("wrote {rel}");
    }
    Ok(())
}
