//! HydraDB operator controller library.
//!
//! The binary (`src/main.rs`) is a thin wrapper over [`run::run`]; every
//! reconciler module is public here so the `tests/` suites (`tower-test`
//! mock assertions and envtest integration) can drive them directly.

/// Install a process-level rustls `CryptoProvider` exactly once.
///
/// With more than one provider in the dependency graph (the `object_store`
/// client pulls its own) rustls refuses to pick a default and panics the first
/// time a TLS connector is built. The operator therefore pins `ring` explicitly
/// at startup instead of relying on feature-unification to leave exactly one.
pub fn install_crypto_provider() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

pub mod apply;
pub mod children;
pub mod conditions;
pub mod context;
pub mod error;
pub mod finalizer;
pub mod metrics;
pub mod reconcile;
pub mod run;
pub mod scrape;
pub mod status;
pub mod watcher;
pub mod webhook;
