//! Todo 12: CRD emission smoke tests for all three kinds plus the
//! table-driven `validate()` accept/reject matrix.
//!
//! The typed `CustomResourceDefinition` returned by `::crd()` is asserted on
//! directly — never on serialized YAML text. Complements (never duplicates)
//! the `crd_emission_smoke` test in `tests/crd_types.rs`, which owns the
//! group/kind/version/printer-column basics; this file additionally pins the
//! plural names, short names, and categories.

use hydradb_operator_api::constants::{GROUP, VERSION};
use hydradb_operator_api::crd::{GraphCluster, GraphIndexer, GraphNodePool};
use kube::CustomResourceExt;
use serde_json::json;

// --- 5. CRD emission smoke tests ---------------------------------------------

#[test]
fn crd_emission_pins_group_version_kind_plural_shortnames_scope_and_columns() {
    for (crd, kind, plural, short) in [
        (GraphCluster::crd(), "GraphCluster", "graphclusters", "hgc"),
        (
            GraphNodePool::crd(),
            "GraphNodePool",
            "graphnodepools",
            "hgnp",
        ),
        (GraphIndexer::crd(), "GraphIndexer", "graphindexers", "hgi"),
    ] {
        assert_eq!(crd.spec.group, GROUP, "{kind} group mismatch");
        assert_eq!(crd.spec.names.kind, kind, "{kind} kind name mismatch");
        assert_eq!(crd.spec.names.plural, plural, "{kind} plural name mismatch");
        let shorts = crd
            .spec
            .names
            .short_names
            .as_ref()
            .expect("short_names must be present so `kubectl get {short}` keeps working");
        assert!(
            shorts.iter().any(|s| s == short),
            "{kind} short_names {shorts:?} must contain {short}"
        );
        let categories = crd
            .spec
            .names
            .categories
            .as_ref()
            .expect("categories must be present so `kubectl get hydradb` keeps working");
        assert!(
            categories.iter().any(|c| c == "hydradb"),
            "{kind} categories {categories:?} must contain hydradb"
        );
        assert_eq!(crd.spec.scope, "Namespaced", "{kind} must be namespaced");
        let versions = &crd.spec.versions;
        assert_eq!(versions.len(), 1, "{kind} must expose exactly one version");
        assert_eq!(versions[0].name, VERSION, "{kind} version mismatch");
        assert!(versions[0].served, "{kind} version must be served");
        assert!(versions[0].storage, "{kind} version must be storage");
        assert!(
            versions[0]
                .additional_printer_columns
                .as_ref()
                .is_some_and(|c| !c.is_empty()),
            "{kind} must print at least one column"
        );
    }
}

// --- 4. validate() accept/reject matrix ---------------------------------------

fn valid_cluster_value() -> serde_json::Value {
    json!({
        "apiVersion": "hydradb.helmforge.dev/v1alpha1",
        "kind": "GraphCluster",
        "metadata": { "name": "example", "namespace": "default" },
        "spec": {
            "image": { "repository": "example.com/org/hydradb" },
            "graph": {
                "namespace": "production",
                "id": "default",
                "database": "default",
                "cellId": "cell-0"
            },
            "objectStore": { "explicit": { "bucket": "my-bucket" } }
        }
    })
}

#[test]
fn validate_accept_reject_matrix() {
    struct Case {
        name: &'static str,
        mutate: fn(&mut serde_json::Value),
        expect_ok: bool,
        expect_field: Option<&'static str>,
    }

    fn no_mutation(_: &mut serde_json::Value) {}

    fn zero_sources(value: &mut serde_json::Value) {
        value
            .pointer_mut("/spec/objectStore")
            .expect("objectStore must exist")
            .as_object_mut()
            .expect("objectStore must be an object")
            .remove("explicit");
    }

    fn two_sources(value: &mut serde_json::Value) {
        value["spec"]["objectStore"]["objectBucketClaim"] = json!({ "name": "my-claim" });
    }

    fn files_below_floor(value: &mut serde_json::Value) {
        value["spec"]["runtime"] = json!({ "maxOpenScopes": 8, "dataCacheMaxOpenFiles": 15 });
    }

    fn empty_cell(value: &mut serde_json::Value) {
        value["spec"]["graph"]["cellId"] = json!("");
    }

    fn dns_without_tls(value: &mut serde_json::Value) {
        value["spec"]["tls"] =
            json!({ "enabled": false, "additionalDnsNames": ["extra.example.com"] });
    }

    fn dns_with_tls(value: &mut serde_json::Value) {
        value["spec"]["tls"] = json!({
            "enabled": true,
            "secretName": "cluster-serving-cert",
            "additionalDnsNames": ["extra.example.com"]
        });
    }

    let cases = [
        Case {
            name: "valid minimal cluster",
            mutate: no_mutation,
            expect_ok: true,
            expect_field: None,
        },
        Case {
            name: "zero object-store sources",
            mutate: zero_sources,
            expect_ok: false,
            expect_field: Some("object_store"),
        },
        Case {
            name: "two object-store sources",
            mutate: two_sources,
            expect_ok: false,
            expect_field: Some("object_store"),
        },
        Case {
            name: "dataCacheMaxOpenFiles below maxOpenScopes * 2",
            mutate: files_below_floor,
            expect_ok: false,
            expect_field: Some("runtime.data_cache_max_open_files"),
        },
        Case {
            name: "empty cellId",
            mutate: empty_cell,
            expect_ok: false,
            expect_field: Some("graph.cell_id"),
        },
        Case {
            name: "additionalDnsNames with tls disabled",
            mutate: dns_without_tls,
            expect_ok: false,
            expect_field: Some("tls.additional_dns_names"),
        },
        Case {
            name: "additionalDnsNames with tls enabled",
            mutate: dns_with_tls,
            expect_ok: true,
            expect_field: None,
        },
    ];
    assert!(
        cases.len() >= 6,
        "matrix must hold at least six inputs, got {}",
        cases.len()
    );

    for case in cases {
        let mut value = valid_cluster_value();
        (case.mutate)(&mut value);
        let cluster: GraphCluster = serde_json::from_value(value)
            .expect("matrix fixture must parse before validate() runs");
        match cluster.spec.validate() {
            Ok(()) => assert!(
                case.expect_ok,
                "case {:?} must be rejected but validate() returned Ok",
                case.name
            ),
            Err(errs) => {
                assert!(
                    !case.expect_ok,
                    "case {:?} must validate but got errors: {errs:?}",
                    case.name
                );
                let needle = case
                    .expect_field
                    .expect("rejecting matrix case must name the expected field");
                assert!(
                    errs.iter()
                        .any(|e| { format!("{} {}", e.field, e.message).contains(needle) }),
                    "case {:?} error must name field `{needle}`, got: {errs:?}",
                    case.name
                );
            }
        }
    }
}
