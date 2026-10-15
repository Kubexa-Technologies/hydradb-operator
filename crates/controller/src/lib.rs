//! HydraDB operator controller library.
//!
//! The binary (`src/main.rs`) is a thin wrapper over [`run::run`]; every
//! reconciler module is public here so the `tests/` suites (`tower-test`
//! mock assertions and envtest integration) can drive them directly.

pub mod apply;
pub mod conditions;
pub mod context;
pub mod error;
pub mod finalizer;
pub mod metrics;
pub mod reconcile;
pub mod run;
pub mod status;
pub mod watcher;
