//! Tests for the feature-expansion API surface: tuning, TLS issuers,
//! subnamespaces, autoscaling, backup and ingest validation.

use hydradb_operator_api::constants;
use hydradb_operator_api::crd::{
    AuthSpec, BackupTarget, ConcurrencyPolicy, DeletionPolicy, GitSource, GraphBackup,
    GraphBackupSpec, GraphCluster, GraphClusterSpec, GraphIdentity, GraphIngestSource,
    GraphIngestSourceSpec, GraphNodePool, GraphNodePoolSpec, HttpSource, IngestSourceSpec,
    MonitoringSpec, RestoreSource, RuntimeSpec, ServiceSpec, TlsSpec, WebhookSpec,
};
use hydradb_operator_api::spec::{
    AutoscalingSpec, Image, IssuerRef, NodeRole, ObjectStore, PhaseLabel, S3, StorageTuning,
    TuningSpec,
};
use k8s_openapi::api::core::v1::{EnvVar, EnvVarSource, ObjectFieldSelector};
use kube::CustomResourceExt;

fn image() -> Image {
    Image {
        repository: "example.com/org/hydradb".to_string(),
        tag: Some("v0.1.0".to_string()),
        digest: None,
        pull_policy: None,
        pull_secrets: Vec::new(),
    }
}

fn store() -> S3 {
    S3 {
        bucket: "data-bucket".to_string(),
        endpoint: Some("http://rgw:80".to_string()),
        region: Some("us-east-1".to_string()),
        allow_http: true,
        credentials_secret_ref: None,
    }
}

fn cluster_spec() -> GraphClusterSpec {
    GraphClusterSpec {
        image: image(),
        graph: GraphIdentity {
            namespace: "tenant".to_string(),
            subnamespaces: Vec::new(),
            id: "graph".to_string(),
            database: "default".to_string(),
            cell_id: "cell-0".to_string(),
        },
        object_store: ObjectStore {
            explicit: Some(store()),
            object_bucket_claim: None,
        },
        auth: AuthSpec::default(),
        tls: TlsSpec::default(),
        deletion_policy: DeletionPolicy::Retain,
        service: ServiceSpec::default(),
        runtime: RuntimeSpec::default(),
        tuning: TuningSpec::default(),
        extra_env: Vec::new(),
        restore_from: None,
        monitoring: MonitoringSpec::default(),
    }
}

fn fields(errs: &[hydradb_operator_api::validation::ValidationError]) -> Vec<String> {
    errs.iter().map(|e| e.field.clone()).collect()
}

// --- subnamespaces ---------------------------------------------------------

#[test]
fn subnamespace_depth_above_the_operand_limit_is_rejected() {
    let mut spec = cluster_spec();
    spec.graph.subnamespaces = (0..8).map(|i| format!("level{i}")).collect();
    let errs = spec.validate().expect_err("eight nested levels must fail");
    assert!(fields(&errs).contains(&"graph.subnamespaces".to_string()));
}

#[test]
fn subnamespace_components_must_be_single_lowercase_labels() {
    let mut spec = cluster_spec();
    spec.graph.subnamespaces = vec!["team.team".to_string()];
    let errs = spec.validate().expect_err("a dotted component must fail");
    assert!(fields(&errs).contains(&"graph.subnamespaces[0]".to_string()));

    let mut spec = cluster_spec();
    spec.graph.subnamespaces = vec!["Team".to_string()];
    assert!(spec.validate().is_err(), "uppercase must fail");

    let mut spec = cluster_spec();
    spec.graph.subnamespaces = (0..7).map(|i| format!("level{i}")).collect();
    spec.validate().expect("seven nested levels are legal");
}

// --- TLS / cert-manager ----------------------------------------------------

#[test]
fn tls_enabled_requires_exactly_one_certificate_source() {
    let mut spec = cluster_spec();
    spec.tls.enabled = true;
    assert!(
        spec.validate().is_err(),
        "tls.enabled without secretName/issuerRef must fail"
    );

    let mut spec = cluster_spec();
    spec.tls.enabled = true;
    spec.tls.secret_name = Some("cluster-serving-cert".to_string());
    spec.tls.issuer_ref = Some(IssuerRef {
        name: "letsencrypt".to_string(),
        kind: "ClusterIssuer".to_string(),
        group: "cert-manager.io".to_string(),
    });
    let errs = spec
        .validate()
        .expect_err("secretName + issuerRef must fail");
    assert!(fields(&errs).contains(&"tls".to_string()));

    let mut spec = cluster_spec();
    spec.tls.enabled = true;
    spec.tls.issuer_ref = Some(IssuerRef {
        name: "letsencrypt".to_string(),
        kind: "Issuer".to_string(),
        group: "cert-manager.io".to_string(),
    });
    spec.validate().expect("issuerRef alone is legal");
}

#[test]
fn tls_issuer_ref_is_validated_and_requires_tls_enabled() {
    let mut spec = cluster_spec();
    spec.tls.issuer_ref = Some(IssuerRef {
        name: String::new(),
        kind: "Bogus".to_string(),
        group: String::new(),
    });
    let errs = spec.validate().expect_err("bad issuer must fail");
    let names = fields(&errs);
    assert!(names.contains(&"tls.issuerRef.name".to_string()));
    assert!(names.contains(&"tls.issuerRef.kind".to_string()));
    assert!(names.contains(&"tls.issuerRef.group".to_string()));
    assert!(names.contains(&"tls.issuerRef".to_string()));
}

// --- tuning / env ----------------------------------------------------------

#[test]
fn tuning_rejects_a_cache_part_size_that_is_not_a_1024_multiple() {
    let mut spec = cluster_spec();
    spec.tuning = TuningSpec {
        storage: StorageTuning {
            cache_part_bytes: Some(1000),
            ..StorageTuning::default()
        },
        ..TuningSpec::default()
    };
    let errs = spec.validate().expect_err("1000 is not a 1024 multiple");
    assert!(fields(&errs).contains(&"tuning.storage.cachePartBytes".to_string()));

    spec.tuning.storage.cache_part_bytes = Some(4096);
    spec.validate().expect("4096 is legal");
}

#[test]
fn extra_env_may_not_shadow_operator_owned_variables() {
    let mut spec = cluster_spec();
    spec.extra_env = vec![EnvVar {
        name: constants::GRAPH_ID.to_string(),
        value: Some("hijacked".to_string()),
        ..EnvVar::default()
    }];
    let errs = spec.validate().expect_err("shadowing must fail");
    assert!(fields(&errs).contains(&format!("extra_env[{}]", constants::GRAPH_ID)));

    spec.extra_env = vec![EnvVar {
        name: "GRAPH_SOMETHING_ELSE".to_string(),
        value: Some("ok".to_string()),
        ..EnvVar::default()
    }];
    spec.validate().expect("a non-owned variable is fine");
}

#[test]
fn tuning_serialises_only_the_leaves_that_are_set() {
    let mut tuning = TuningSpec::default();
    tuning.query.max_runtime_ms = Some(30_000);
    let json = serde_json::to_value(&tuning).expect("serialises");
    assert_eq!(
        json["query"],
        serde_json::json!({ "maxRuntimeMs": 30_000 }),
        "unset leaves must disappear entirely"
    );
    assert_eq!(json["graphblas"], serde_json::json!({}));
}

// --- restore ---------------------------------------------------------------

#[test]
fn restore_source_must_not_be_the_clusters_own_store() {
    let mut spec = cluster_spec();
    spec.restore_from = Some(RestoreSource {
        store: store(),
        prefix: None,
        image: image(),
        verify: true,
    });
    let errs = spec.validate().expect_err("same store must fail");
    assert!(fields(&errs).contains(&"restoreFrom.store".to_string()));

    let mut other = store();
    other.bucket = "backup-bucket".to_string();
    spec.restore_from = Some(RestoreSource {
        store: other,
        prefix: Some("clusters/prod".to_owned()),
        image: image(),
        verify: true,
    });
    spec.validate().expect("a distinct backup store is legal");
}

// --- node role -------------------------------------------------------------

#[test]
fn node_role_decodes_unknown_values_verbatim() {
    let known: NodeRole = serde_json::from_str("\"writer\"").expect("decodes");
    assert_eq!(known, NodeRole::Writer);
    assert_eq!(known.label(), "writer");

    let future: NodeRole = serde_json::from_str("\"observer\"").expect("decodes");
    assert_eq!(future.label(), "observer");
    assert!(future.is_unknown());
    assert_eq!(
        serde_json::to_string(&future).expect("encodes"),
        "\"observer\"",
        "an unknown role must round-trip untouched"
    );
}

#[test]
fn node_pool_defaults_to_the_all_role() {
    let pool: GraphNodePoolSpec = serde_json::from_value(serde_json::json!({
        "clusterRef": { "name": "cluster" },
        "replicas": 3,
        "cache": { "storage": { "emptyDir": {} } }
    }))
    .expect("decodes");
    assert_eq!(pool.role, Some(NodeRole::All));
}

// --- autoscaling -----------------------------------------------------------

#[test]
fn nodepool_autoscaling_bounds_must_be_coherent() {
    let mut pool = GraphNodePoolSpec {
        cluster_ref: k8s_openapi::api::core::v1::LocalObjectReference {
            name: "cluster".to_string(),
        },
        role: Some(NodeRole::Reader),
        replicas: 1,
        autoscaling: Some(AutoscalingSpec {
            min_replicas: 5,
            max_replicas: 2,
            target_cpu_utilization_percentage: Some(70),
            behavior: None,
        }),
        image: None,
        resources: None,
        cache: hydradb_operator_api::crd::CacheSpec {
            storage: hydradb_operator_api::spec::CacheStorage::EmptyDir(
                hydradb_operator_api::spec::EmptyDirCache { size_limit: None },
            ),
            memory_limit_bytes: None,
        },
        tmp_size_limit: None,
        service_account: None,
        priority_class_name: None,
        termination_grace_period_seconds: None,
        update_strategy: None,
        affinity: None,
        topology_spread_constraints: Vec::new(),
        tolerations: Vec::new(),
        node_selector: None,
        extra_env: Vec::new(),
        pod_annotations: Default::default(),
        pod_labels: Default::default(),
        service: None,
    };
    let errs = pool.validate().expect_err("max < min must fail");
    assert!(fields(&errs).contains(&"autoscaling.maxReplicas".to_string()));

    pool.autoscaling = Some(AutoscalingSpec {
        min_replicas: 2,
        max_replicas: 6,
        target_cpu_utilization_percentage: Some(70),
        behavior: None,
    });
    pool.validate().expect("coherent bounds are legal");
}

// --- backup ----------------------------------------------------------------

fn backup_spec() -> GraphBackupSpec {
    GraphBackupSpec {
        cluster_ref: k8s_openapi::api::core::v1::LocalObjectReference {
            name: "cluster".to_string(),
        },
        schedule: Some("0 3 * * *".to_string()),
        suspend: false,
        image: image(),
        target: BackupTarget {
            store: S3 {
                bucket: "backup-bucket".to_string(),
                ..store()
            },
            prefix: None,
        },
        retention: 3,
        successful_jobs_history_limit: Some(3),
        failed_jobs_history_limit: Some(1),
        verify: true,
        active_deadline_seconds: None,
        backoff_limit: Some(0),
        resources: None,
        extra_env: Vec::new(),
        pod_annotations: Default::default(),
        pod_labels: Default::default(),
    }
}

#[test]
fn backup_validation_covers_cron_retention_and_bucket() {
    let mut spec = backup_spec();
    spec.schedule = Some("not a cron".to_string());
    spec.retention = 0;
    spec.target.store.bucket = String::new();
    let errs = spec.validate().expect_err("three broken fields");
    let names = fields(&errs);
    assert!(names.contains(&"schedule".to_string()));
    assert!(names.contains(&"retention".to_string()));
    assert!(names.contains(&"target.store.bucket".to_string()));

    backup_spec().validate().expect("the baseline is legal");
}

#[test]
fn backup_detects_a_target_on_the_source_store() {
    let spec = backup_spec();
    assert!(
        !spec.targets(&store()),
        "a distinct bucket must not be reported as the source"
    );
    assert!(spec.targets(&S3 {
        bucket: "backup-bucket".to_string(),
        ..store()
    }));
}

// --- ingest ----------------------------------------------------------------

fn ingest_spec() -> GraphIngestSourceSpec {
    GraphIngestSourceSpec {
        cluster_ref: k8s_openapi::api::core::v1::LocalObjectReference {
            name: "cluster".to_string(),
        },
        source: IngestSourceSpec {
            git: Some(GitSource {
                repository: "https://git.example.com/org/repo.git".to_string(),
                revision: Some("main".to_string()),
                paths: vec!["docs/**/*.md".to_string()],
                credentials_secret_ref: None,
            }),
            http: None,
        },
        image: image(),
        command: vec!["/ingest".to_string()],
        args: vec!["--revision".to_string(), "{revision}".to_string()],
        env: Vec::new(),
        schedule: Some("*/15 * * * *".to_string()),
        suspend: false,
        concurrency_policy: ConcurrencyPolicy::Forbid,
        successful_jobs_history_limit: Some(3),
        failed_jobs_history_limit: Some(1),
        active_deadline_seconds: Some(1800),
        backoff_limit: Some(2),
        resources: None,
        service_account: None,
        tolerations: Vec::new(),
        pod_annotations: Default::default(),
        pod_labels: Default::default(),
        webhook: WebhookSpec {
            enabled: true,
            secret_ref: Some(k8s_openapi::api::core::v1::LocalObjectReference {
                name: "ingest-hmac".to_string(),
            }),
            secret_key: Some("token".to_string()),
            path: Some("docs".to_string()),
        },
    }
}

#[test]
fn ingest_requires_exactly_one_source() {
    let mut spec = ingest_spec();
    spec.source.git = None;
    let errs = spec.validate().expect_err("no source must fail");
    assert!(fields(&errs).contains(&"source".to_string()));

    spec.source.git = Some(GitSource {
        repository: "https://git.example.com/org/repo.git".to_string(),
        revision: None,
        paths: Vec::new(),
        credentials_secret_ref: None,
    });
    spec.source.http = Some(HttpSource {
        url: "https://example.com/doc.json".to_string(),
        credentials_secret_ref: None,
    });
    let errs = spec.validate().expect_err("both sources must fail");
    assert!(fields(&errs).contains(&"source".to_string()));
}

#[test]
fn ingest_validates_urls_schedules_and_the_webhook_contract() {
    let mut spec = ingest_spec();
    spec.source.git = Some(GitSource {
        repository: "ftp://nope".to_string(),
        revision: None,
        paths: Vec::new(),
        credentials_secret_ref: None,
    });
    spec.schedule = Some("every minute".to_string());
    spec.webhook.secret_ref = None;
    spec.webhook.path = Some("a/b".to_string());
    let errs = spec.validate().expect_err("four broken fields");
    let names = fields(&errs);
    assert!(names.contains(&"source.git.repository".to_string()));
    assert!(names.contains(&"schedule".to_string()));
    assert!(names.contains(&"webhook.secretRef".to_string()));
    assert!(names.contains(&"webhook.path".to_string()));

    ingest_spec().validate().expect("the baseline is legal");
}

// --- CRD emission ----------------------------------------------------------

#[test]
fn new_kinds_emit_crds_with_stable_identity() {
    let backup = GraphBackup::crd();
    assert_eq!(backup.spec.group, constants::GROUP);
    assert_eq!(backup.spec.names.kind, "GraphBackup");
    assert_eq!(backup.spec.names.plural, "graphbackups");
    assert_eq!(backup.spec.names.singular.as_deref(), Some("graphbackup"));
    assert_eq!(backup.spec.scope, "Namespaced");

    let ingest = GraphIngestSource::crd();
    assert_eq!(ingest.spec.names.kind, "GraphIngestSource");
    assert_eq!(ingest.spec.names.plural, "graphingestsources");
    assert_eq!(ingest.spec.scope, "Namespaced");

    // The three original kinds keep their identity.
    assert_eq!(GraphCluster::crd().spec.names.kind, "GraphCluster");
    assert_eq!(GraphNodePool::crd().spec.names.kind, "GraphNodePool");
}

#[test]
fn cluster_crd_exposes_the_new_spec_surface() {
    let crd = GraphCluster::crd();
    let versions = crd.spec.versions;
    let version = versions.first().expect("one version");
    let root = version
        .schema
        .as_ref()
        .and_then(|s| s.open_api_v3_schema.as_ref())
        .and_then(|s| s.properties.as_ref())
        .expect("root properties");
    let props = root
        .get("spec")
        .and_then(|spec| spec.properties.as_ref())
        .expect("spec properties");
    for key in ["tuning", "extraEnv", "restoreFrom", "monitoring"] {
        assert!(props.contains_key(key), "spec.{key} must be in the CRD");
    }
}

#[test]
fn env_field_selector_is_representable_for_node_id() {
    // GRAPH_NODE_ID is produced from a downward-API fieldRef, so the type must
    // accept a valueFrom source (compile-time shape check).
    let var = EnvVar {
        name: constants::GRAPH_NODE_ID.to_string(),
        value_from: Some(EnvVarSource {
            field_ref: Some(ObjectFieldSelector {
                field_path: "metadata.name".to_string(),
                api_version: Some("v1".to_string()),
            }),
            ..EnvVarSource::default()
        }),
        ..EnvVar::default()
    };
    assert!(var.value_from.is_some());
}
