//! Behavioural tests for the ingest webhook receiver.
//!
//! The handler is exercised through a real `axum` router over a mock apiserver,
//! so the status codes a caller actually sees are asserted — not just the
//! signature helper.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http::Response as HttpResponse;
use hydradb_operator::webhook::{self, WebhookState};
use kube::client::Body as KubeBody;
use tower::ServiceExt as _;

fn state(namespace: Option<&str>) -> WebhookState {
    let (service, mut handle) =
        tower_test::mock::pair::<Request<KubeBody>, HttpResponse<KubeBody>>();
    // Every request 404s: enough for the paths that must not reach the
    // apiserver's happy path.
    let responder = tokio::spawn(async move {
        while let Some((request, send)) = handle.next_request().await {
            let body = serde_json::json!({
                "kind": "Status",
                "status": "Failure",
                "message": "not found",
                "reason": "NotFound",
                "code": 404
            });
            drop(request);
            send.send_response(
                HttpResponse::builder()
                    .status(404)
                    .header("content-type", "application/json")
                    .body(KubeBody::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            );
        }
    });
    // Keep the responder alive for the test's lifetime.
    std::mem::forget(responder);
    let client = kube::Client::new(service, "default");
    WebhookState {
        client,
        allowed_namespace: namespace.map(str::to_owned),
    }
}

async fn call(state: WebhookState, namespace: &str, path: &str, body: Vec<u8>) -> StatusCode {
    let router = webhook::router(state);
    let request = Request::builder()
        .method("POST")
        .uri(format!("/webhooks/v1alpha1/ingest/{namespace}/{path}"))
        .header("content-type", "application/json")
        .body(Body::from(body))
        .expect("request builds");
    router
        .oneshot(request)
        .await
        .expect("router responds")
        .status()
}

#[tokio::test]
async fn a_namespace_outside_the_watch_scope_is_not_found() {
    let status = call(state(Some("allowed")), "other", "docs", b"{}".to_vec()).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn an_oversized_body_is_rejected_by_the_layer() {
    let big = vec![b'x'; webhook::MAX_BODY_BYTES + 1];
    let status = call(state(Some("allowed")), "allowed", "docs", big).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn an_unknown_source_answers_not_found_rather_than_erroring() {
    let status = call(state(Some("allowed")), "allowed", "missing", b"{}".to_vec()).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "a lookup miss must not surface as 500"
    );
}

#[tokio::test]
async fn the_router_only_accepts_post() {
    let router = webhook::router(state(Some("allowed")));
    let request = Request::builder()
        .method("GET")
        .uri("/webhooks/v1alpha1/ingest/allowed/docs")
        .body(Body::empty())
        .expect("request builds");
    let status = router
        .oneshot(request)
        .await
        .expect("router responds")
        .status();
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
}

// Keep `Arc` imported for future state sharing without a warning.
#[allow(dead_code)]
fn _unused(_: Arc<()>) {}
