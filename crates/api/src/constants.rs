//! Shared string constants: API identity and operand env-var names.
//!
//! Every literal below is defined exactly once. Types in [`crate::spec`] and
//! the controller reference these constants instead of re-typing strings.

/// CRD API group for all HydraDB resources.
pub const GROUP: &str = "hydradb.helmforge.dev";
/// Current (and only) API version.
pub const VERSION: &str = "v1alpha1";
/// Server-side apply field manager used by the controller.
pub const FIELD_MANAGER: &str = "hydradb.helmforge.dev/controller";
/// Finalizer stranded on managed objects until cleanup completes.
pub const FINALIZER: &str = "hydradb.helmforge.dev/finalizer";

/// Object-store bucket name (S3 `bucketName`).
pub const AWS_BUCKET_NAME: &str = "AWS_BUCKET_NAME";
/// Object-store region (S3 `region`).
pub const AWS_DEFAULT_REGION: &str = "AWS_DEFAULT_REGION";
/// Custom S3-compatible endpoint override (empty means AWS default).
pub const AWS_ENDPOINT: &str = "AWS_ENDPOINT";
/// Allow plain-HTTP endpoints (`"true"`/`"false"`).
pub const AWS_ALLOW_HTTP: &str = "AWS_ALLOW_HTTP";
/// Object-store provider selector (`objectStore.cloudProvider`).
pub const CLOUD_PROVIDER: &str = "CLOUD_PROVIDER";
/// HydraDB graph namespace.
pub const GRAPH_NAMESPACE: &str = "GRAPH_NAMESPACE";
/// HydraDB graph id.
pub const GRAPH_ID: &str = "GRAPH_ID";
/// HydraDB database name.
pub const GRAPH_DATABASE: &str = "GRAPH_DATABASE";
/// Primary cell served by this node.
pub const GRAPH_CELL_ID: &str = "GRAPH_CELL_ID";
/// Comma-joined list of all cells in the graph.
pub const GRAPH_CELLS: &str = "GRAPH_CELLS";
/// Comma-joined `host:port` bolt addresses of every node.
pub const GRAPH_BOLT_NODE_ADDRESSES: &str = "GRAPH_BOLT_NODE_ADDRESSES";
/// Minimum stack size for spawned threads.
pub const RUST_MIN_STACK: &str = "RUST_MIN_STACK";
/// Tracing/logging filter directive.
pub const RUST_LOG: &str = "RUST_LOG";
