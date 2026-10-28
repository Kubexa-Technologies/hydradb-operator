//! Shared string constants: API identity and operand env-var names.
//!
//! Every literal below is defined exactly once. Types in [`crate::spec`] and
//! the controller reference these constants instead of re-typing strings.

/// CRD API group for all HydraDB resources.
pub const GROUP: &str = "hydradb.kubexa.tech";
/// Current (and only) API version.
pub const VERSION: &str = "v1alpha1";
/// Server-side apply field manager used by the controller.
pub const FIELD_MANAGER: &str = "hydradb.kubexa.tech/controller";
/// Finalizer stranded on managed objects until cleanup completes.
pub const FINALIZER: &str = "hydradb.kubexa.tech/finalizer";

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

// --- Query-tier runtime knobs (node runtime.* block) ------------------------

/// Maximum read-only scope handles retained between indexer cycles.
pub const GRAPH_MAX_OPEN_SCOPES: &str = "GRAPH_MAX_OPEN_SCOPES";
/// Process-wide SlateDB RAM cache budget in bytes.
pub const GRAPH_SLATE_DB_CACHE_BYTES: &str = "GRAPH_SLATE_DB_CACHE_BYTES";
/// Process-wide file-handle limit for SlateDB disk caches.
pub const GRAPH_DATA_CACHE_MAX_OPEN_FILES: &str = "GRAPH_DATA_CACHE_MAX_OPEN_FILES";
/// Parallel WAL reads used to establish a cold SlateDB reader.
pub const GRAPH_READER_WAL_REPLAY_CONCURRENCY: &str = "GRAPH_READER_WAL_REPLAY_CONCURRENCY";
/// Maximum concurrent queries served by one node.
pub const GRAPH_MAX_CONCURRENT_QUERIES: &str = "GRAPH_MAX_CONCURRENT_QUERIES";
/// Maximum edges a single query may scan.
pub const GRAPH_MAX_QUERY_SCAN_EDGES: &str = "GRAPH_MAX_QUERY_SCAN_EDGES";

// --- Typed `tuning.*` surface ----------------------------------------------
// Every knob below is read by the operand but was previously unreachable from
// the CRD. They are grouped exactly as `spec.tuning` groups them.

/// GraphBLAS matrix memory ceiling in bytes.
pub const GRAPH_MAX_GRAPHBLAS_BYTES: &str = "GRAPH_MAX_GRAPHBLAS_BYTES";
/// Maximum compiled GraphBLAS matrices held at once.
pub const GRAPH_MAX_GRAPHBLAS_MATRICES: &str = "GRAPH_MAX_GRAPHBLAS_MATRICES";
/// Maximum matrix adjacencies held at once.
pub const GRAPH_MAX_MATRIX_ADJACENCIES: &str = "GRAPH_MAX_MATRIX_ADJACENCIES";
/// Byte ceiling for a single matrix adjacency.
pub const GRAPH_MAX_MATRIX_ADJACENCY_BYTES: &str = "GRAPH_MAX_MATRIX_ADJACENCY_BYTES";
/// Concurrent matrix compilations allowed.
pub const GRAPH_MAX_CONCURRENT_MATRIX_COMPILATIONS: &str =
    "GRAPH_MAX_CONCURRENT_MATRIX_COMPILATIONS";
/// Wall-clock ceiling for one query, in milliseconds.
pub const GRAPH_MAX_QUERY_RUNTIME_MS: &str = "GRAPH_MAX_QUERY_RUNTIME_MS";
/// Default result page size.
pub const GRAPH_DEFAULT_PAGE_SIZE: &str = "GRAPH_DEFAULT_PAGE_SIZE";
/// Maximum live server-side cursors.
pub const GRAPH_MAX_SERVER_CURSORS: &str = "GRAPH_MAX_SERVER_CURSORS";
/// Byte ceiling per server-side cursor buffer.
pub const GRAPH_MAX_CURSOR_BUFFER_BYTES: &str = "GRAPH_MAX_CURSOR_BUFFER_BYTES";
/// Server-side cursor idle TTL in milliseconds.
pub const GRAPH_CURSOR_TTL_MS: &str = "GRAPH_CURSOR_TTL_MS";
/// Query duration above which a slow-query record is emitted.
pub const GRAPH_SLOW_QUERY_MS: &str = "GRAPH_SLOW_QUERY_MS";
/// Concurrent metadata hydrations allowed.
pub const GRAPH_MAX_CONCURRENT_HYDRATIONS: &str = "GRAPH_MAX_CONCURRENT_HYDRATIONS";
/// Bookmark wait ceiling in milliseconds.
pub const GRAPH_MAX_BOOKMARK_WAIT_MS: &str = "GRAPH_MAX_BOOKMARK_WAIT_MS";
/// Transaction retry ceiling.
pub const GRAPH_TXN_MAX_RETRIES: &str = "GRAPH_TXN_MAX_RETRIES";
/// Maximum concurrent Bolt connections.
pub const GRAPH_MAX_BOLT_CONNECTIONS: &str = "GRAPH_MAX_BOLT_CONNECTIONS";
/// Bolt connection idle timeout in milliseconds.
pub const GRAPH_BOLT_IDLE_TIMEOUT_MS: &str = "GRAPH_BOLT_IDLE_TIMEOUT_MS";
/// Bolt connection maximum age in milliseconds.
pub const GRAPH_BOLT_MAX_CONNECTION_AGE_MS: &str = "GRAPH_BOLT_MAX_CONNECTION_AGE_MS";
/// Bolt authentication handshake timeout in milliseconds.
pub const GRAPH_BOLT_AUTHENTICATION_TIMEOUT_MS: &str = "GRAPH_BOLT_AUTHENTICATION_TIMEOUT_MS";
/// Graceful shutdown drain window in milliseconds.
pub const GRAPH_GRACEFUL_SHUTDOWN_MS: &str = "GRAPH_GRACEFUL_SHUTDOWN_MS";
/// Object-store cache chunk size in bytes.
pub const GRAPH_DATA_CACHE_PART_BYTES: &str = "GRAPH_DATA_CACHE_PART_BYTES";
/// Parallel L0 flush tasks.
pub const GRAPH_L0_FLUSH_PARALLELISM: &str = "GRAPH_L0_FLUSH_PARALLELISM";
/// Target L0 SST size in bytes.
pub const GRAPH_L0_SST_SIZE_BYTES: &str = "GRAPH_L0_SST_SIZE_BYTES";
/// Key batch size for maintenance compaction work.
pub const GRAPH_MAINTENANCE_BATCH_KEYS: &str = "GRAPH_MAINTENANCE_BATCH_KEYS";
/// Key batch size for index-artifact garbage collection.
pub const GRAPH_ARTIFACT_GC_BATCH_KEYS: &str = "GRAPH_ARTIFACT_GC_BATCH_KEYS";
/// Key batch size for index-artifact writes.
pub const GRAPH_ARTIFACT_WRITE_BATCH_KEYS: &str = "GRAPH_ARTIFACT_WRITE_BATCH_KEYS";
/// WAL flush interval in milliseconds.
pub const GRAPH_WAL_FLUSH_INTERVAL_MS: &str = "GRAPH_WAL_FLUSH_INTERVAL_MS";
/// Return hydration scratch memory to the OS (`"true"`/`"false"`).
pub const GRAPH_TRIM_MEMORY_AFTER_HYDRATION: &str = "GRAPH_TRIM_MEMORY_AFTER_HYDRATION";
/// OpenCypher engine selector.
pub const GRAPH_CYPHER_ENGINE: &str = "GRAPH_CYPHER_ENGINE";
/// Compiled-kernel selector.
pub const GRAPH_COMPILED_KERNEL: &str = "GRAPH_COMPILED_KERNEL";
/// Sparse-kernel selector.
pub const GRAPH_SPARSE_KERNEL: &str = "GRAPH_SPARSE_KERNEL";
/// Emit matrix profiling telemetry (`"true"`/`"false"`).
pub const GRAPH_PROFILE_MATRIX: &str = "GRAPH_PROFILE_MATRIX";
/// Placement heartbeat interval in milliseconds.
pub const GRAPH_HEARTBEAT_INTERVAL_MS: &str = "GRAPH_HEARTBEAT_INTERVAL_MS";
/// Placement heartbeat expiry in milliseconds.
pub const GRAPH_HEARTBEAT_TIMEOUT_MS: &str = "GRAPH_HEARTBEAT_TIMEOUT_MS";
/// Durable writer-lease duration in milliseconds.
pub const GRAPH_WRITER_LEASE_MS: &str = "GRAPH_WRITER_LEASE_MS";
/// Number of concurrent write lanes.
pub const GRAPH_WRITE_LANES: &str = "GRAPH_WRITE_LANES";
/// Bolt read-routing policy selector.
pub const GRAPH_READ_ROUTING: &str = "GRAPH_READ_ROUTING";
/// Manifest poll interval for follower readers, in milliseconds.
pub const GRAPH_READER_MANIFEST_POLL_INTERVAL: &str = "GRAPH_READER_MANIFEST_POLL_INTERVAL";
/// Index discovery interval in milliseconds.
pub const GRAPH_INDEX_DISCOVERY_INTERVAL_MS: &str = "GRAPH_INDEX_DISCOVERY_INTERVAL_MS";

/// Allow serving without TLS (`"true"`/`"false"`).
///
/// The operand refuses to start with neither a certificate nor this flag:
/// "graph runtime requires GRAPH_TLS_CERTIFICATE and GRAPH_TLS_PRIVATE_KEY
/// unless GRAPH_ALLOW_PLAINTEXT=true".
pub const GRAPH_ALLOW_PLAINTEXT: &str = "GRAPH_ALLOW_PLAINTEXT";
/// Path to the serving certificate the operand presents.
pub const GRAPH_TLS_CERTIFICATE: &str = "GRAPH_TLS_CERTIFICATE";
/// Path to the serving certificate's private key.
pub const GRAPH_TLS_PRIVATE_KEY: &str = "GRAPH_TLS_PRIVATE_KEY";

// --- Node role and multi-tenancy -------------------------------------------

/// Stable per-pod node identity used by placement heartbeats.
pub const GRAPH_NODE_ID: &str = "GRAPH_NODE_ID";

// --- Backup / restore job contract ----------------------------------------

/// Source bucket of a backup or restore job.
pub const HYDRADB_BACKUP_SOURCE_BUCKET: &str = "HYDRADB_BACKUP_SOURCE_BUCKET";
/// Source object prefix of a backup or restore job.
pub const HYDRADB_BACKUP_SOURCE_PREFIX: &str = "HYDRADB_BACKUP_SOURCE_PREFIX";
/// Target bucket of a backup or restore job.
pub const HYDRADB_BACKUP_TARGET_BUCKET: &str = "HYDRADB_BACKUP_TARGET_BUCKET";
/// Target object prefix of a backup or restore job.
pub const HYDRADB_BACKUP_TARGET_PREFIX: &str = "HYDRADB_BACKUP_TARGET_PREFIX";
/// Direction of the job: `backup` or `restore`.
pub const HYDRADB_BACKUP_DIRECTION: &str = "HYDRADB_BACKUP_DIRECTION";
/// How many backup copies to keep in the target prefix.
pub const HYDRADB_BACKUP_RETENTION: &str = "HYDRADB_BACKUP_RETENTION";
/// Whether the job must verify the copy before exiting.
pub const HYDRADB_BACKUP_VERIFY: &str = "HYDRADB_BACKUP_VERIFY";

// --- Ingest contract -------------------------------------------------------

/// Graph scope the ingest job writes into.
pub const HYDRADB_INGEST_NAMESPACE: &str = "HYDRADB_INGEST_NAMESPACE";
/// Graph id the ingest job writes into.
pub const HYDRADB_INGEST_GRAPH_ID: &str = "HYDRADB_INGEST_GRAPH_ID";
/// Bolt endpoint the ingest job writes through.
pub const HYDRADB_INGEST_BOLT_URL: &str = "HYDRADB_INGEST_BOLT_URL";
/// Source revision (commit SHA / tag) resolved for this run.
pub const HYDRADB_INGEST_REVISION: &str = "HYDRADB_INGEST_REVISION";
/// Source URL for this run.
pub const HYDRADB_INGEST_SOURCE_URL: &str = "HYDRADB_INGEST_SOURCE_URL";
/// Comma-joined path globs selected from the source.
pub const HYDRADB_INGEST_PATHS: &str = "HYDRADB_INGEST_PATHS";
