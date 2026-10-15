//! HydraDB developer xtask binary.
//!
//! Tiny hand-rolled dispatcher over `std::env::args()`. Deliberately
//! dependency-light: no clap here (the CLI crate owns clap).

mod gen_crds;
mod gen_docs;
mod gen_rbac;

fn usage() -> &'static str {
    "usage: cargo xtask <gen-crds|gen-rbac|gen-docs|gen-all> [--check]"
}

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(subcommand) = args.next() else {
        eprintln!("{}", usage());
        std::process::exit(1);
    };
    match subcommand.as_str() {
        "gen-crds" => {
            if let Err(err) = gen_crds::run() {
                eprintln!("gen-crds failed: {err:?}");
                std::process::exit(1);
            }
        }
        "gen-rbac" => {
            if let Err(err) = gen_rbac::run() {
                eprintln!("gen-rbac failed: {err:?}");
                std::process::exit(1);
            }
        }
        "gen-docs" => {
            if let Err(err) = gen_docs::run() {
                eprintln!("gen-docs failed: {err:?}");
                std::process::exit(1);
            }
        }
        "gen-all" => {
            if let Err(err) = gen_crds::run() {
                eprintln!("gen-all failed at gen-crds: {err:?}");
                std::process::exit(1);
            }
            if let Err(err) = gen_rbac::run() {
                eprintln!("gen-all failed at gen-rbac: {err:?}");
                std::process::exit(1);
            }
            if let Err(err) = gen_docs::run() {
                eprintln!("gen-all failed at gen-docs: {err:?}");
                std::process::exit(1);
            }
        }
        _ => {
            eprintln!("unknown subcommand '{subcommand}'\n{}", usage());
            std::process::exit(1);
        }
    }
}
