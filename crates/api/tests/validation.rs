//! TDD tests for todo 11: cross-field validation + CRD schema attributes.
//!
//! Written BEFORE the implementation (plan mandates TDD): every `validate()`
//! call below must FAIL to compile until `validation.rs` and the per-kind
//! `validate()` bodies land. JSON payloads stand in for YAML (`serde_json` is
//! already an api dependency; no YAML crate may be added).

use hydradb_operator_api::crd::{
    AuthSpec, DeletionPolicy, GraphCluster, GraphClusterSpec, GraphIdentity, GraphIndexer,
    GraphNodePool, MonitoringSpec, RuntimeSpec, ServiceSpec, TlsSpec,
};
use hydradb_operator_api::spec::{Image, ObjectStore, S3};
use kube::CustomResourceExt;

fn valid_image() -> Image {
    Image {
        repository: "example.com/org/hydradb".to_string(),
        tag: Some("v0.1.0".to_string()),
        digest: None,
        pull_policy: None,
        pull_secrets: Vec::new(),
    }
}

fn valid_spec() -> GraphClusterSpec {
    GraphClusterSpec {
        image: valid_image(),
        graph: GraphIdentity {
            namespace: "production".to_string(),
            id: "default".to_string(),
            database: "default".to_string(),
            cell_id: "cell-0".to_string(),
        },
        object_store: ObjectStore {
            explicit: Some(S3 {
                bucket: "my-bucket".to_string(),
                endpoint: None,
                region: None,
                allow_http: false,
                credentials_secret_ref: None,
            }),
            object_bucket_claim: None,
        },
        auth: AuthSpec::default(),
        tls: TlsSpec::default(),
        deletion_policy: DeletionPolicy::Retain,
        service: ServiceSpec::default(),
        runtime: RuntimeSpec::default(),
        monitoring: MonitoringSpec::default(),
    }
}

#[test]
fn validate_rejects_object_store_with_zero_sources() {
    let mut spec = valid_spec();
    spec.object_store.explicit = None;
    spec.object_store.object_bucket_claim = None;
    let errs = spec
        .validate()
        .expect_err("object store with zero sources must fail");
    assert!(
        errs.iter().any(|e| e.field.contains("object_store")),
        "an error must name `object_store`, got: {errs:?}"
    );
}

#[test]
fn validate_rejects_object_store_with_two_sources() {
    use k8s_openapi::api::core::v1::LocalObjectReference;
    let mut spec = valid_spec();
    spec.object_store.object_bucket_claim = Some(LocalObjectReference {
        name: "my-claim".to_string(),
    });
    // Both `explicit` and `objectBucketClaim` are now set.
    assert!(spec.object_store.explicit.is_some());
    let errs = spec
        .validate()
        .expect_err("object store with two sources must fail");
    assert!(
        errs.iter().any(|e| e.field.contains("object_store")),
        "an error must name `object_store`, got: {errs:?}"
    );
}

#[test]
fn validate_rejects_data_cache_max_open_files_below_twice_max_open_scopes() {
    let mut spec = valid_spec();
    spec.runtime.max_open_scopes = 8;
    spec.runtime.data_cache_max_open_files = 15;
    let errs = spec
        .validate()
        .expect_err("data_cache_max_open_files < max_open_scopes * 2 must fail");
    assert!(
        errs.iter().any(|e| {
            let haystack = format!("{} {}", e.field, e.message);
            haystack.contains("data_cache_max_open_files") && haystack.contains("max_open_scopes")
        }),
        "an error must name both fields, got: {errs:?}"
    );
}

#[test]
fn validate_rejects_empty_cell_id() {
    let mut spec = valid_spec();
    spec.graph.cell_id = String::new();
    let errs = spec.validate().expect_err("empty cell_id must fail");
    assert!(
        errs.iter().any(|e| {
            let haystack = format!("{} {}", e.field, e.message);
            haystack.contains("cell_id")
        }),
        "an error must name `cell_id`, got: {errs:?}"
    );
}

#[test]
fn validate_rejects_additional_dns_names_when_tls_disabled() {
    let mut spec = valid_spec();
    spec.tls.enabled = false;
    spec.tls.additional_dns_names = vec!["extra.example.com".to_string()];
    let errs = spec
        .validate()
        .expect_err("dns names with tls disabled must fail");
    assert!(
        errs.iter().any(|e| {
            let haystack = format!("{} {}", e.field, e.message);
            haystack.contains("additional_dns_names")
        }),
        "an error must name `additional_dns_names`, got: {errs:?}"
    );
}

#[test]
fn validate_accepts_a_fully_specified_cluster() {
    let mut spec = valid_spec();
    spec.tls.enabled = true;
    spec.tls.additional_dns_names = vec!["extra.example.com".to_string()];
    spec.runtime.max_open_scopes = 8;
    spec.runtime.data_cache_max_open_files = 16;
    spec.validate()
        .expect("fully specified cluster must validate");
}

#[test]
fn validate_collects_every_violation() {
    let mut spec = valid_spec();
    // Break three independent rules at once.
    spec.object_store.explicit = None;
    spec.object_store.object_bucket_claim = None;
    spec.graph.cell_id = String::new();
    spec.tls.enabled = false;
    spec.tls.additional_dns_names = vec!["extra.example.com".to_string()];
    let errs = spec.validate().expect_err("broken cluster must fail");
    assert!(
        errs.len() >= 2,
        "one reconcile must report every problem (2+ errors), got: {errs:?}"
    );
    for needle in ["object_store", "cell_id", "additional_dns_names"] {
        assert!(
            errs.iter().any(|e| {
                let haystack = format!("{} {}", e.field, e.message);
                haystack.contains(needle)
            }),
            "an error must name `{needle}`, got: {errs:?}"
        );
    }
}

#[test]
fn crd_schema_denies_unknown_fields() {
    // kube's CustomResource derive strips `additionalProperties: false` from
    // CRD output (StructuralSchemaRewriter), so assert on the typed schemars
    // schema that feeds the CRD — never on YAML text.
    for (schema, name) in [
        (
            schemars::schema_for!(hydradb_operator_api::crd::GraphClusterSpec),
            "GraphClusterSpec",
        ),
        (
            schemars::schema_for!(hydradb_operator_api::crd::GraphNodePoolSpec),
            "GraphNodePoolSpec",
        ),
        (
            schemars::schema_for!(hydradb_operator_api::crd::GraphIndexerSpec),
            "GraphIndexerSpec",
        ),
    ] {
        assert_eq!(
            schema.get("additionalProperties"),
            Some(&serde_json::json!(false)),
            "{name} schema must deny unknown fields"
        );
    }
    for (crd, kind) in [
        (GraphCluster::crd(), "GraphCluster"),
        (GraphNodePool::crd(), "GraphNodePool"),
        (GraphIndexer::crd(), "GraphIndexer"),
    ] {
        let value = serde_json::to_value(&crd).expect("CRD must serialize");
        let spec_schema =
            &value["spec"]["versions"][0]["schema"]["openAPIV3Schema"]["properties"]["spec"];
        assert!(
            spec_schema.is_object(),
            "{kind} CRD must carry a spec schema object"
        );
    }
}
