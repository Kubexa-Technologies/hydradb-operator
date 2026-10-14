//! `GraphCluster` reconcilers.
//!
//! [`cluster`] (todo 18) owns the shared config `ConfigMap`, the auth
//! `Secret` and object-store resolution. [`indexer`] (todo 21) owns the
//! indexer `Deployment`. Todo 20 adds its module here.

pub mod cluster;
pub mod indexer;
