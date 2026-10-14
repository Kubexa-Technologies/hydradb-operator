//! `GraphCluster` reconcilers.
//!
//! [`cluster`] (todo 18) owns the shared config `ConfigMap`, the auth
//! `Secret` and object-store resolution. [`indexer`] (todo 21) owns the
//! indexer `Deployment`. [`nodepool`] (todo 20) owns the node `StatefulSet`.

pub mod cluster;
pub mod indexer;
pub mod nodepool;
