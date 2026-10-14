//! HydraDB developer xtask binary.
//!
//! Tiny hand-rolled dispatcher over `std::env::args()`. Deliberately
//! dependency-light: no clap here (the CLI crate owns clap).
//!
//! Seams for plan todos 33-34: `gen-docs` and `gen-all --check`
//! are stub arms that exit non-zero until their todos land.

mod gen_crds;
mod gen_rbac;

fn usage() -> &'static str {
    "usage: cargo xtask <gen-crds|gen-rbac|gen-docs|gen-all> [--check]"
}

/// Stub for a not-yet-implemented subcommand (plan todos 32/33).
fn not_implemented(name: &str, todo: &str) -> ! {
    eprintln!("{name} is not implemented yet (plan {todo})");
    std::process::exit(1);
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
        "gen-docs" => not_implemented("gen-docs", "todo 33"),
        "gen-all" => not_implemented("gen-all", "todo 33"),
        _ => {
            eprintln!("unknown subcommand '{subcommand}'\n{}", usage());
            std::process::exit(1);
        }
    }
}
