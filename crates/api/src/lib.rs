//! HydraDB operator API types.
//!
//! Shared CRD primitives for the three `hydradb.helmforge.dev` kinds
//! (`GraphCluster`, `GraphNodePool`, `GraphIndexer`). The per-kind specs live
//! in [`crd`]; everything reusable across kinds lives in [`spec`].

#![warn(missing_docs)]

pub mod constants;
pub mod crd;
pub mod spec;
pub mod status;
pub mod validation;
