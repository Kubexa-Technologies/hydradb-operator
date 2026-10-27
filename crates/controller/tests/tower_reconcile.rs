//! Mock-apiserver suite for the three reconcilers (todo 26).
//!
//! Each test drives the real `reconcile()` entrypoint against a `tower-test`
//! 0.4 mock service and asserts the EXACT requests issued: HTTP method,
//! path and body shape. No cluster is needed.
//!
//! NOTE on plan vocabulary: `tower-test` 0.4 ships no `ApiServerVerifier` or
//! `Scenario` types (only the generic `mock::pair` / `Handle`). The
//! [`StubApi`] responder below is their equivalent: a scripted stub that
//! records every request and serves canned objects, so the assertions keep
//! the plan's intent (method + path + body shape per reconciler).

use std::sync::{Arc, Mutex};

use http::{Request, Response};
use hydradb_operator::context::{Ctx, WatchScope};
use hydradb_operator::reconcile::{cluster, indexer, nodepool};
use hydradb_operator_api::crd::{GraphCluster, GraphIndexer, GraphNodePool};
use kube::client::Body;
use tower_test::mock::Handle;

/// One request observed by the stub: method, path (no query) and bodies.
#[derive(Debug, Clone)]
struct Recorded {
    method: String,
    path: String,
    content_type: String,
    body: String,
}

/// Route closure: builds a stub response from `(method, path, body)`.
type Route = Box<dyn Fn(&str, &str, &str) -> Response<Body> + Send>;

/// Scripted stub apiserver over a `tower-test` handle.
///
/// Every request is recorded; the `route` closure builds the response from
/// `(method, path, body)`. Unknown routes get a 500 `Status` so a missing
/// stub fails the test loudly instead of hanging it.
struct StubApi {
    handle: Handle<Request<Body>, Response<Body>>,
    recorded: Arc<Mutex<Vec<Recorded>>>,
    route: Route,
}

impl StubApi {
    fn run(self) -> tokio::task::JoinHandle<Vec<Recorded>> {
        let Self {
            mut handle,
            recorded,
            route,
        } = self;
        tokio::spawn(async move {
            while let Some((request, send)) = handle.next_request().await {
                let (parts, body) = request.into_parts();
                let bytes = body
                    .collect_bytes()
                    .await
                    .expect("mock request body collects");
                let record = Recorded {
                    method: parts.method.to_string(),
                    path: parts.uri.path().to_owned(),
                    content_type: parts
                        .headers
                        .get(http::header::CONTENT_TYPE)
                        .and_then(|value| value.to_str().ok())
                        .unwrap_or_default()
                        .to_owned(),
                    body: String::from_utf8_lossy(&bytes).into_owned(),
                };
                let response = route(&record.method, &record.path, &record.body);
                recorded.lock().expect("record log unlocks").push(record);
                send.send_response(response);
            }
            recorded.lock().expect("record log unlocks").clone()
        })
    }
}

/// Build a [`Ctx`] over a mock-backed client.
fn mock_ctx(client: kube::Client) -> Arc<Ctx> {
    Arc::new(Ctx::new(client, WatchScope::Namespaced).expect("mock context builds without I/O"))
}

/// JSON object response for a known kind.
fn object_response(value: &serde_json::Value) -> Response<Body> {
    Response::new(Body::from(
        serde_json::to_vec(value).expect("fixture serializes"),
    ))
}

/// 404 `Status` response, which kube maps to `None` in `get_opt`.
fn not_found_response() -> Response<Body> {
    let status = serde_json::json!({
        "kind": "Status",
        "apiVersion": "v1",
        "metadata": {},
        "status": "Failure",
        "message": "not found",
        "reason": "NotFound",
        "code": 404,
    });
    Response::builder()
        .status(404)
        .body(Body::from(
            serde_json::to_vec(&status).expect("status serializes"),
        ))
        .expect("404 response builds")
}

/// Minimal object of the child kind a PATCH path names, so kube can
/// deserialize the apply response into the concrete type.
fn child_response(path: &str) -> Response<Body> {
    let (api_version, kind) = if path.contains("/configmaps/") {
        ("v1", "ConfigMap")
    } else if path.contains("/secrets/") {
        ("v1", "Secret")
    } else if path.contains("/services/") {
        ("v1", "Service")
    } else if path.contains("/statefulsets/") {
        ("apps/v1", "StatefulSet")
    } else if path.contains("/deployments/") {
        ("apps/v1", "Deployment")
    } else if path.contains("/poddisruptionbudgets/") {
        ("policy/v1", "PodDisruptionBudget")
    } else if path.contains("/networkpolicies/") {
        ("networking.k8s.io/v1", "NetworkPolicy")
    } else {
        ("v1", "ConfigMap")
    };
    object_response(&serde_json::json!({
        "apiVersion": api_version,
        "kind": kind,
        "metadata": {"name": "obj", "namespace": "default"},
    }))
}
/// 500 `Status` for an unstubbed route: loud failure, never a hang.
fn unstubbed_response(method: &str, path: &str) -> Response<Body> {
    let status = serde_json::json!({
        "kind": "Status",
        "apiVersion": "v1",
        "metadata": {},
        "status": "Failure",
        "message": format!("no stub for {method} {path}"),
        "code": 500,
    });
    Response::builder()
        .status(500)
        .body(Body::from(
            serde_json::to_vec(&status).expect("status serializes"),
        ))
        .expect("500 response builds")
}

fn empty_pod_list() -> serde_json::Value {
    serde_json::json!({
        "apiVersion": "v1",
        "kind": "PodList",
        "metadata": {"resourceVersion": "1"},
        "items": [],
    })
}

fn cluster_fixture(object_store: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "apiVersion": "hydradb.helmforge.dev/v1alpha1",
        "kind": "GraphCluster",
        "metadata": {
            "name": "demo",
            "namespace": "default",
            "uid": "abc-123",
            "finalizers": ["hydradb.helmforge.dev/finalizer"]
        },
        "spec": {
            "image": {"repository": "example.com/hydradb", "tag": "v0.1.0"},
            "graph": {"namespace": "ns", "id": "g", "database": "db", "cellId": "c0"},
            "objectStore": object_store,
            "auth": {"generate": true}
        }
    })
}

fn pool_fixture(cluster_ref_name: &str) -> serde_json::Value {
    serde_json::json!({
        "apiVersion": "hydradb.helmforge.dev/v1alpha1",
        "kind": "GraphNodePool",
        "metadata": {
            "name": "demo-pool",
            "namespace": "default",
            "uid": "pool-uid-1",
            "finalizers": ["hydradb.helmforge.dev/finalizer"]
        },
        "spec": {
            "clusterRef": {"name": cluster_ref_name},
            "replicas": 3,
            "cache": {"storage": {"emptyDir": {"sizeLimit": "20Gi"}}}
        }
    })
}

fn indexer_fixture(enabled: bool) -> serde_json::Value {
    serde_json::json!({
        "apiVersion": "hydradb.helmforge.dev/v1alpha1",
        "kind": "GraphIndexer",
        "metadata": {
            "name": "demo-indexer",
            "namespace": "default",
            "uid": "idx-uid-1",
            "finalizers": ["hydradb.helmforge.dev/finalizer"]
        },
        "spec": {
            "clusterRef": {"name": "demo"},
            "enabled": enabled,
            "replicas": 2
        }
    })
}

/// Paths only (query stripped): every request the cluster reconciler issues
/// on the happy path, in order.
///
/// The input object already carries the finalizer, so `with_finalizer`
/// dispatches straight to the apply body; the stamp itself is covered by
/// `finalizer_is_stamped_before_any_apply`.
fn cluster_happy_paths() -> Vec<(&'static str, &'static str)> {
    vec![
        ("GET", "/api/v1/namespaces/default/pods"),
        ("PATCH", "/api/v1/namespaces/default/configmaps/demo-config"),
        (
            "PATCH",
            "/api/v1/namespaces/default/services/demo-node-headless",
        ),
        ("PATCH", "/api/v1/namespaces/default/services/demo-bolt"),
        ("PATCH", "/api/v1/namespaces/default/services/demo-https"),
        ("PATCH", "/api/v1/namespaces/default/services/demo-metrics"),
        (
            "PATCH",
            "/api/v1/namespaces/default/services/demo-indexer-metrics",
        ),
        (
            "PATCH",
            "/apis/networking.k8s.io/v1/namespaces/default/networkpolicies/demo-netpol",
        ),
        (
            "GET",
            "/apis/monitoring.coreos.com/v1/namespaces/default/servicemonitors/demo-servicemonitor",
        ),
        (
            "GET",
            "/apis/cert-manager.io/v1/namespaces/default/certificates/demo-tls",
        ),
        (
            "GET",
            "/apis/monitoring.coreos.com/v1/namespaces/default/prometheusrules/demo-rules",
        ),
        ("GET", "/api/v1/namespaces/default/secrets/demo-auth"),
        ("PATCH", "/api/v1/namespaces/default/secrets/demo-auth"),
        (
            "GET",
            "/apis/hydradb.helmforge.dev/v1alpha1/namespaces/default/graphclusters/demo",
        ),
        (
            "GET",
            "/apis/hydradb.helmforge.dev/v1alpha1/namespaces/default/graphclusters/demo",
        ),
        (
            "PATCH",
            "/apis/hydradb.helmforge.dev/v1alpha1/namespaces/default/graphclusters/demo/status",
        ),
    ]
}

#[tokio::test]
async fn cluster_reconcile_issues_the_exact_child_and_status_requests() {
    let fixture = cluster_fixture(
        serde_json::json!({"explicit": {"bucket": "my-bucket", "region": "us-east-1"}}),
    );
    let cluster: GraphCluster =
        serde_json::from_value(fixture.clone()).expect("cluster fixture deserializes");
    let get_fixture = fixture.clone();
    let (service, handle) = tower_test::mock::pair::<Request<Body>, Response<Body>>();
    let client = kube::Client::new(service, "default");
    let ctx = mock_ctx(client);
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let stub = StubApi {
        handle,
        recorded: recorded.clone(),
        route: Box::new(
            move |method: &str, path: &str, _body: &str| -> Response<Body> {
                if path == "/api/v1/namespaces/default/pods" {
                    return object_response(&empty_pod_list());
                }
                if method == "GET"
                    && (path.ends_with("/servicemonitors/demo-servicemonitor")
                        || path.ends_with("/certificates/demo-tls")
                        || path.ends_with("/prometheusrules/demo-rules")
                        || path.ends_with("/horizontalpodautoscalers/demo-pool-hpa")
                        || path.ends_with("/secrets/demo-auth"))
                {
                    return not_found_response();
                }
                if method == "GET" && path.ends_with("/graphclusters/demo") {
                    return object_response(&get_fixture);
                }
                if method == "PATCH" {
                    if path.contains("/graphclusters/") {
                        return object_response(&get_fixture);
                    }
                    return child_response(path);
                }
                unstubbed_response(method, path)
            },
        ),
    };
    let responder = stub.run();
    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        cluster::reconcile(Arc::new(cluster), ctx.clone()),
    )
    .await
    .expect("reconcile settles against the stub");
    drop(ctx);
    let log = responder.await.expect("stub responder joins");

    outcome.expect("happy-path cluster reconcile succeeds");
    let seen: Vec<(String, String)> = log
        .iter()
        .map(|record| (record.method.clone(), record.path.clone()))
        .collect();
    let want: Vec<(String, String)> = cluster_happy_paths()
        .into_iter()
        .map(|(method, path)| (method.to_owned(), path.to_owned()))
        .collect();
    assert_eq!(
        seen, want,
        "cluster reconcile must issue exactly these requests"
    );

    let config_patch = log
        .iter()
        .find(|record| record.path.ends_with("/configmaps/demo-config"))
        .expect("config patch issued");
    assert_eq!(
        config_patch.content_type, "application/apply-patch+yaml",
        "child writes must be server-side apply"
    );
    assert!(
        config_patch.body.contains("AWS_BUCKET_NAME") && config_patch.body.contains("my-bucket"),
        "config body must carry the resolved bucket, got: {}",
        config_patch.body
    );
    assert!(
        !config_patch.body.contains("AWS_SECRET_ACCESS_KEY"),
        "credentials must never land in the ConfigMap, got: {}",
        config_patch.body
    );

    let bolt_patch = log
        .iter()
        .find(|record| record.path.ends_with("/services/demo-bolt"))
        .expect("bolt service patch issued");
    assert!(
        bolt_patch.body.contains("7687"),
        "bolt Service must expose 7687, got: {}",
        bolt_patch.body
    );
    let https_patch = log
        .iter()
        .find(|record| record.path.ends_with("/services/demo-https"))
        .expect("https service patch issued");
    assert!(
        https_patch.body.contains("8443"),
        "https Service must target 8443, got: {}",
        https_patch.body
    );
    let headless_patch = log
        .iter()
        .find(|record| record.path.ends_with("/services/demo-node-headless"))
        .expect("headless service patch issued");
    assert!(
        headless_patch.body.contains("publishNotReadyAddresses"),
        "headless Service must publish not-ready addresses, got: {}",
        headless_patch.body
    );
    let netpol_patch = log
        .iter()
        .find(|record| record.path.ends_with("/networkpolicies/demo-netpol"))
        .expect("networkpolicy patch issued");
    assert!(
        netpol_patch.body.contains("7687") && netpol_patch.body.contains("Egress"),
        "NetworkPolicy must gate bolt ingress and carry egress rules, got: {}",
        netpol_patch.body
    );

    let status_patch = log
        .iter()
        .find(|record| record.path.ends_with("/graphclusters/demo/status"))
        .expect("status patch issued");
    assert!(
        status_patch.content_type.contains("merge-patch"),
        "status writes must be merge patches, got: {}",
        status_patch.content_type
    );
    assert!(
        status_patch.body.contains("\"status\""),
        "status patch must wrap fields under `status` or the apiserver drops them, got: {}",
        status_patch.body
    );
    assert!(
        status_patch.body.contains("\"Ready\""),
        "status body must carry the Ready condition, got: {}",
        status_patch.body
    );
}

#[tokio::test]
async fn cluster_reconcile_without_object_store_stalls_instead_of_panicking() {
    let fixture = cluster_fixture(serde_json::json!({}));
    let cluster: GraphCluster =
        serde_json::from_value(fixture.clone()).expect("cluster fixture deserializes");
    let get_fixture = fixture.clone();
    let (service, handle) = tower_test::mock::pair::<Request<Body>, Response<Body>>();
    let client = kube::Client::new(service, "default");
    let ctx = mock_ctx(client);
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let stub = StubApi {
        handle,
        recorded: recorded.clone(),
        route: Box::new(
            move |method: &str, path: &str, _body: &str| -> Response<Body> {
                if method == "GET" && path.ends_with("/graphclusters/demo") {
                    return object_response(&get_fixture);
                }
                if method == "PATCH" {
                    return object_response(&get_fixture);
                }
                unstubbed_response(method, path)
            },
        ),
    };
    let responder = stub.run();
    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        cluster::reconcile(Arc::new(cluster), ctx.clone()),
    )
    .await
    .expect("reconcile settles against the stub");
    drop(ctx);
    let log = responder.await.expect("stub responder joins");

    outcome.expect("stalled reconcile still returns Ok (requeue), never panics");
    assert_eq!(
        log.len(),
        2,
        "stalled path must touch nothing but status, got: {log:?}"
    );
    assert_eq!(log[0].method, "GET");
    assert!(log[0].path.ends_with("/graphclusters/demo"));
    assert_eq!(log[1].method, "PATCH");
    assert!(log[1].path.ends_with("/graphclusters/demo/status"));
    assert!(
        log[1].body.contains("\"Stalled\"") && log[1].body.contains("\"True\""),
        "status body must set Stalled=True, got: {}",
        log[1].body
    );
    assert!(
        log.iter()
            .all(|record| !record.path.contains("/configmaps/")
                && !record.path.contains("/services/")),
        "stalled reconcile must create no children, got: {log:?}"
    );
}

#[tokio::test]
async fn nodepool_reconcile_issues_the_exact_statefulset_pdb_and_status_requests() {
    let cluster_value = cluster_fixture(
        serde_json::json!({"explicit": {"bucket": "my-bucket", "region": "us-east-1"}}),
    );
    let pool_value = pool_fixture("demo");
    let pool: GraphNodePool =
        serde_json::from_value(pool_value.clone()).expect("pool fixture deserializes");
    let (service, handle) = tower_test::mock::pair::<Request<Body>, Response<Body>>();
    let client = kube::Client::new(service, "default");
    let ctx = mock_ctx(client);
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let stub = StubApi {
        handle,
        recorded: recorded.clone(),
        route: Box::new(
            move |method: &str, path: &str, _body: &str| -> Response<Body> {
                if path == "/api/v1/namespaces/default/pods" {
                    return object_response(&empty_pod_list());
                }
                if method == "GET" && path.ends_with("/graphclusters/demo") {
                    return object_response(&cluster_value);
                }
                if method == "GET" && path.ends_with("/graphnodepools/demo-pool") {
                    return object_response(&pool_value);
                }
                if method == "GET" && path.ends_with("/horizontalpodautoscalers/demo-pool-hpa") {
                    return not_found_response();
                }
                if method == "GET" && path.ends_with("/statefulsets/demo-pool") {
                    return object_response(&serde_json::json!({
                        "apiVersion": "apps/v1",
                        "kind": "StatefulSet",
                        "metadata": {"name": "demo-pool", "namespace": "default"},
                        "status": {"readyReplicas": 0, "currentReplicas": 0},
                    }));
                }
                if method == "PATCH" {
                    if path.contains("/graphnodepools/") {
                        return object_response(&pool_value);
                    }
                    return child_response(path);
                }
                unstubbed_response(method, path)
            },
        ),
    };
    let responder = stub.run();
    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        nodepool::reconcile(Arc::new(pool), ctx.clone()),
    )
    .await
    .expect("reconcile settles against the stub");
    drop(ctx);
    let log = responder.await.expect("stub responder joins");

    outcome.expect("happy-path pool reconcile succeeds");
    let seen: Vec<(String, String)> = log
        .iter()
        .map(|record| (record.method.clone(), record.path.clone()))
        .collect();
    let want: Vec<(String, String)> = [
        (
            "GET",
            "/apis/hydradb.helmforge.dev/v1alpha1/namespaces/default/graphclusters/demo",
        ),
        (
            "PATCH",
            "/apis/apps/v1/namespaces/default/statefulsets/demo-pool",
        ),
        (
            "PATCH",
            "/apis/policy/v1/namespaces/default/poddisruptionbudgets/demo-pool",
        ),
        (
            "GET",
            "/apis/autoscaling/v2/namespaces/default/horizontalpodautoscalers/demo-pool-hpa",
        ),
        ("GET", "/apis/apps/v1/namespaces/default/statefulsets/demo-pool"),
        ("GET", "/api/v1/namespaces/default/pods"),
        (
            "GET",
            "/apis/hydradb.helmforge.dev/v1alpha1/namespaces/default/graphnodepools/demo-pool",
        ),
        (
            "GET",
            "/apis/hydradb.helmforge.dev/v1alpha1/namespaces/default/graphnodepools/demo-pool",
        ),
        (
            "PATCH",
            "/apis/hydradb.helmforge.dev/v1alpha1/namespaces/default/graphnodepools/demo-pool/status",
        ),
    ]
    .into_iter()
    .map(|(method, path)| (method.to_owned(), path.to_owned()))
    .collect();
    assert_eq!(
        seen, want,
        "pool reconcile must issue exactly these requests"
    );

    let statefulset_patch = log
        .iter()
        .find(|record| record.path.ends_with("/statefulsets/demo-pool"))
        .expect("statefulset patch issued");
    assert_eq!(
        statefulset_patch.content_type, "application/apply-patch+yaml",
        "child writes must be server-side apply"
    );
    assert!(
        statefulset_patch.body.contains("graph-node")
            && statefulset_patch.body.contains("demo-node-headless"),
        "statefulset body must run graph-node against the headless service, got: {}",
        statefulset_patch.body
    );
    let pdb_patch = log
        .iter()
        .find(|record| record.path.ends_with("/poddisruptionbudgets/demo-pool"))
        .expect("pdb patch issued");
    assert!(
        pdb_patch.body.contains("minAvailable"),
        "PDB body must pin minAvailable, got: {}",
        pdb_patch.body
    );
}

#[tokio::test]
async fn nodepool_reconcile_with_empty_cluster_ref_errors_without_touching_children() {
    let pool_value = pool_fixture("");
    let pool: GraphNodePool =
        serde_json::from_value(pool_value.clone()).expect("pool fixture deserializes");
    let (service, handle) = tower_test::mock::pair::<Request<Body>, Response<Body>>();
    let client = kube::Client::new(service, "default");
    let ctx = mock_ctx(client);
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let stub = StubApi {
        handle,
        recorded: recorded.clone(),
        route: Box::new(
            move |method: &str, path: &str, _body: &str| -> Response<Body> {
                if method == "PATCH" {
                    return object_response(&pool_value);
                }
                unstubbed_response(method, path)
            },
        ),
    };
    let responder = stub.run();
    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        nodepool::reconcile(Arc::new(pool), ctx.clone()),
    )
    .await
    .expect("reconcile settles against the stub");
    drop(ctx);
    let log = responder.await.expect("stub responder joins");

    let error = outcome.expect_err("empty clusterRef must fail the reconcile");
    assert!(
        error.to_string().contains("clusterRef"),
        "error must name clusterRef, got: {error}"
    );
    assert!(
        log.is_empty(),
        "failed reference must issue no apiserver request at all, got: {log:?}"
    );
}

#[tokio::test]
async fn indexer_reconcile_issues_the_exact_deployment_pdb_and_status_requests() {
    let cluster_value = cluster_fixture(
        serde_json::json!({"explicit": {"bucket": "my-bucket", "region": "us-east-1"}}),
    );
    let indexer_value = indexer_fixture(true);
    let indexer: GraphIndexer =
        serde_json::from_value(indexer_value.clone()).expect("indexer fixture deserializes");
    let (service, handle) = tower_test::mock::pair::<Request<Body>, Response<Body>>();
    let client = kube::Client::new(service, "default");
    let ctx = mock_ctx(client);
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let stub = StubApi {
        handle,
        recorded: recorded.clone(),
        route: Box::new(
            move |method: &str, path: &str, _body: &str| -> Response<Body> {
                if path == "/api/v1/namespaces/default/pods" {
                    return object_response(&empty_pod_list());
                }
                if method == "GET" && path.ends_with("/graphclusters/demo") {
                    return object_response(&cluster_value);
                }
                if method == "GET" && path.ends_with("/graphindexers/demo-indexer") {
                    return object_response(&indexer_value);
                }
                if method == "GET" && path.ends_with("/deployments/demo-indexer") {
                    return object_response(&serde_json::json!({
                        "apiVersion": "apps/v1",
                        "kind": "Deployment",
                        "metadata": {"name": "demo-indexer", "namespace": "default"},
                        "status": {"readyReplicas": 0, "replicas": 0},
                    }));
                }
                if method == "PATCH" {
                    if path.contains("/graphindexers/") {
                        return object_response(&indexer_value);
                    }
                    return child_response(path);
                }
                unstubbed_response(method, path)
            },
        ),
    };
    let responder = stub.run();
    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        indexer::reconcile(Arc::new(indexer), ctx.clone()),
    )
    .await
    .expect("reconcile settles against the stub");
    drop(ctx);
    let log = responder.await.expect("stub responder joins");

    outcome.expect("happy-path indexer reconcile succeeds");
    let seen: Vec<(String, String)> = log
        .iter()
        .map(|record| (record.method.clone(), record.path.clone()))
        .collect();
    let want: Vec<(String, String)> = [
        (
            "GET",
            "/apis/hydradb.helmforge.dev/v1alpha1/namespaces/default/graphclusters/demo",
        ),
        (
            "PATCH",
            "/apis/apps/v1/namespaces/default/deployments/demo-indexer",
        ),
        (
            "PATCH",
            "/apis/policy/v1/namespaces/default/poddisruptionbudgets/demo-indexer",
        ),
        (
            "GET",
            "/apis/apps/v1/namespaces/default/deployments/demo-indexer",
        ),
        (
            "GET",
            "/apis/hydradb.helmforge.dev/v1alpha1/namespaces/default/graphindexers/demo-indexer",
        ),
        ("GET", "/api/v1/namespaces/default/pods"),
        (
            "GET",
            "/apis/hydradb.helmforge.dev/v1alpha1/namespaces/default/graphindexers/demo-indexer",
        ),
        (
            "PATCH",
            "/apis/hydradb.helmforge.dev/v1alpha1/namespaces/default/graphindexers/demo-indexer/status",
        ),
    ]
    .into_iter()
    .map(|(method, path)| (method.to_owned(), path.to_owned()))
    .collect();
    assert_eq!(
        seen, want,
        "indexer reconcile must issue exactly these requests"
    );

    let deployment_patch = log
        .iter()
        .find(|record| record.path.ends_with("/deployments/demo-indexer"))
        .expect("deployment patch issued");
    assert_eq!(
        deployment_patch.content_type, "application/apply-patch+yaml",
        "child writes must be server-side apply"
    );
    assert!(
        deployment_patch.body.contains("graph-indexer"),
        "deployment body must run graph-indexer, got: {}",
        deployment_patch.body
    );
    assert!(
        !deployment_patch.body.contains("volumeClaimTemplates"),
        "stateless indexer must never declare a PVC, got: {}",
        deployment_patch.body
    );
}

#[tokio::test]
async fn finalizer_is_stamped_before_any_apply() {
    let mut fixture = cluster_fixture(
        serde_json::json!({"explicit": {"bucket": "my-bucket", "region": "us-east-1"}}),
    );
    fixture
        .get_mut("metadata")
        .expect("fixture carries metadata")
        .as_object_mut()
        .expect("metadata is an object")
        .remove("finalizers");
    let cluster: GraphCluster =
        serde_json::from_value(fixture.clone()).expect("cluster fixture deserializes");
    let (service, handle) = tower_test::mock::pair::<Request<Body>, Response<Body>>();
    let client = kube::Client::new(service, "default");
    let ctx = mock_ctx(client);
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let stub = StubApi {
        handle,
        recorded: recorded.clone(),
        route: Box::new(
            move |method: &str, path: &str, _body: &str| -> Response<Body> {
                if method == "PATCH" {
                    return object_response(&fixture);
                }
                unstubbed_response(method, path)
            },
        ),
    };
    let responder = stub.run();
    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        cluster::reconcile(Arc::new(cluster), ctx.clone()),
    )
    .await
    .expect("reconcile settles against the stub");
    drop(ctx);
    let log = responder.await.expect("stub responder joins");

    outcome.expect("finalizer stamp succeeds");
    assert_eq!(
        log.len(),
        1,
        "kube defers the apply body to the next reconcile after stamping, got: {log:?}"
    );
    assert_eq!(log[0].method, "PATCH");
    assert!(log[0].path.ends_with("/graphclusters/demo"));
    assert!(
        log[0].body.contains("hydradb.helmforge.dev/finalizer"),
        "the stamp must name the shared finalizer, got: {}",
        log[0].body
    );
}

#[tokio::test]
async fn disabled_indexer_scales_the_deployment_to_zero_and_reports_disabled() {
    let cluster_value = cluster_fixture(
        serde_json::json!({"explicit": {"bucket": "my-bucket", "region": "us-east-1"}}),
    );
    let indexer_value = indexer_fixture(false);
    let indexer: GraphIndexer =
        serde_json::from_value(indexer_value.clone()).expect("indexer fixture deserializes");
    let (service, handle) = tower_test::mock::pair::<Request<Body>, Response<Body>>();
    let client = kube::Client::new(service, "default");
    let ctx = mock_ctx(client);
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let stub = StubApi {
        handle,
        recorded: recorded.clone(),
        route: Box::new(
            move |method: &str, path: &str, _body: &str| -> Response<Body> {
                if method == "GET" && path.ends_with("/graphclusters/demo") {
                    return object_response(&cluster_value);
                }
                if method == "GET" && path.ends_with("/graphindexers/demo-indexer") {
                    return object_response(&indexer_value);
                }
                if method == "GET" && path.ends_with("/poddisruptionbudgets/demo-indexer") {
                    return not_found_response();
                }
                if method == "PATCH" {
                    if path.contains("/graphindexers/") {
                        return object_response(&indexer_value);
                    }
                    return child_response(path);
                }
                unstubbed_response(method, path)
            },
        ),
    };
    let responder = stub.run();
    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        indexer::reconcile(Arc::new(indexer), ctx.clone()),
    )
    .await
    .expect("reconcile settles against the stub");
    drop(ctx);
    let log = responder.await.expect("stub responder joins");

    outcome.expect("disabled indexer reconcile succeeds");
    let deployment_patch = log
        .iter()
        .find(|record| {
            record.method == "PATCH" && record.path.ends_with("/deployments/demo-indexer")
        })
        .expect("deployment patch issued");
    assert!(
        deployment_patch.body.contains("\"replicas\":0"),
        "disabled indexer must scale to 0, got: {}",
        deployment_patch.body
    );
    let status_patch = log
        .iter()
        .find(|record| record.path.ends_with("/graphindexers/demo-indexer/status"))
        .expect("status patch issued");
    assert!(
        status_patch.body.contains("Disabled"),
        "disabled status must carry the Disabled reason, got: {}",
        status_patch.body
    );
    assert!(
        log.iter().all(|record| !(record.method == "PATCH"
            && record.path.ends_with("/poddisruptionbudgets/demo-indexer"))),
        "disabled indexer must own no PDB, got: {log:?}"
    );
}
