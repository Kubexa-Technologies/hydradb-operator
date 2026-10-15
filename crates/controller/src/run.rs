//! Operator entrypoint: client, health server, graceful shutdown.
//!
//! [`run`] builds the client and [`Ctx`](crate::context::Ctx`), serves
//! `/healthz`, `/readyz` and `/metrics` on `:8080` via an axum
//! [`Router`](axum::Router), then blocks until SIGTERM/SIGINT and stops the
//! server gracefully.
//!
//! Controllers are registered here as their reconcilers land (todos 18/20/21).
//! `GraphCluster` (todo 18), `GraphNodePool` (todo 20) and `GraphIndexer`
//! (todo 21) spawn sites exist. Wiring them in must not change this module's
//! shutdown contract (signal -> stop server -> return `Ok(())`).

use axum::{Router, http::StatusCode, http::header, response::IntoResponse, routing::get};

use crate::context::Ctx;
use crate::error::Error;
use crate::metrics;
use crate::{reconcile, watcher};

/// Address the health/metrics server binds.
pub const HEALTH_ADDR: &str = "0.0.0.0:8080";

/// Run the operator until SIGTERM or SIGINT.
///
/// # Errors
///
/// Returns [`Error::Client`] when no usable kubeconfig/in-cluster config
/// exists, or [`Error::Server`] when the health server cannot bind `:8080`.
pub async fn run() -> Result<(), Error> {
    // Parse the scope BEFORE building the client: an invalid OPERATOR_SCOPE
    // must fail fast at startup even when no cluster is reachable.
    let scope = crate::context::WatchScope::from_env()?;
    let client = kube::Client::try_default().await.map_err(Error::Client)?;
    let ctx = Ctx::new(client, scope)?;
    tracing::info!(
        namespace = %ctx.namespace,
        addr = HEALTH_ADDR,
        "starting hydradb-operator"
    );

    // Controllers are registered here as their reconcilers land (todos 18/20/21).
    let cluster_handle = spawn_graph_cluster_controller(&ctx);
    let pool_handle = spawn_graph_nodepool_controller(&ctx);
    let indexer_handle = spawn_graph_indexer_controller(&ctx);

    // The OTLP layer (if any) is attached in `main::init_tracing` while the
    // subscriber is built; nothing OTLP-related happens here.

    let listener = tokio::net::TcpListener::bind(HEALTH_ADDR)
        .await
        .map_err(Error::Server)?;
    axum::serve(listener, app())
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(Error::Server)?;
    cluster_handle.abort();
    pool_handle.abort();
    indexer_handle.abort();
    tracing::info!("shutdown signal received, health server stopped");
    Ok(())
}

/// Spawn the `GraphCluster` controller (todo 18) as a background task.
///
/// Watches `GraphCluster` objects plus the owned `<cluster>-config`
/// ConfigMaps and `<cluster>-auth` Secrets (label-filtered by
/// [`crate::apply::CLUSTER_SELECTOR_LABEL`]); todos 20/21 extend the
/// registration block with their reconcilers. The returned handle is aborted
/// after the health server stops so the shutdown contract stays
/// `signal -> stop server -> return Ok(())`.
fn spawn_graph_cluster_controller(ctx: &Ctx) -> tokio::task::JoinHandle<()> {
    use futures::StreamExt as _;
    use k8s_openapi::api::core::v1::{ConfigMap, Secret};
    use k8s_openapi::api::networking::v1::NetworkPolicy;
    use kube::runtime::Controller;

    use crate::apply::CLUSTER_SELECTOR_LABEL;
    use crate::reconcile::cluster::ServiceMonitorObject;

    let client = ctx.client.clone();
    let clusters = ctx.clusters.clone();
    let context = std::sync::Arc::new(ctx.clone());
    tokio::spawn(async move {
        // Version probe runs once here (not per watch); every watch below
        // shares the cached outcome via the config builders.
        let support = watcher::probe_watcher_support(&client).await;
        let owned_selector = CLUSTER_SELECTOR_LABEL;
        let running = Controller::new(clusters, watcher::watcher_config(&support))
            .owns(
                context.api::<ConfigMap>(),
                watcher::owned_watcher_config(owned_selector, &support),
            )
            .owns(
                context.api::<Secret>(),
                watcher::owned_watcher_config(owned_selector, &support),
            )
            .owns(
                context.api::<NetworkPolicy>(),
                watcher::owned_watcher_config(owned_selector, &support),
            )
            .owns(
                context.api::<ServiceMonitorObject>(),
                watcher::owned_watcher_config(owned_selector, &support),
            )
            .with_config(watcher::controller_config())
            .shutdown_on_signal()
            .run(
                reconcile::cluster::reconcile,
                reconcile::cluster::error_policy,
                context,
            )
            .for_each(|result| async move {
                if let Err(error) = result {
                    tracing::warn!(%error, "GraphCluster reconcile stream error");
                }
            });
        running.await;
    })
}

/// Spawn the `GraphNodePool` controller (todo 20) as a background task.
///
/// Watches `GraphNodePool` objects plus the owned node `StatefulSet`s
/// (label-filtered by [`crate::apply::CLUSTER_SELECTOR_LABEL`]). Shutdown
/// mirrors [`spawn_graph_cluster_controller`]: the returned handle is aborted
/// after the health server stops.
fn spawn_graph_nodepool_controller(ctx: &Ctx) -> tokio::task::JoinHandle<()> {
    use futures::StreamExt as _;
    use k8s_openapi::api::apps::v1::StatefulSet;
    use k8s_openapi::api::policy::v1::PodDisruptionBudget;
    use kube::runtime::Controller;

    use crate::apply::CLUSTER_SELECTOR_LABEL;

    let client = ctx.client.clone();
    let pools = ctx.pools.clone();
    let context = std::sync::Arc::new(ctx.clone());
    tokio::spawn(async move {
        // Version probe runs once here (not per watch); every watch below
        // shares the cached outcome via the config builders.
        let support = watcher::probe_watcher_support(&client).await;
        let owned_selector = CLUSTER_SELECTOR_LABEL;
        let running = Controller::new(pools, watcher::watcher_config(&support))
            .owns(
                context.api::<StatefulSet>(),
                watcher::owned_watcher_config(owned_selector, &support),
            )
            .owns(
                context.api::<PodDisruptionBudget>(),
                watcher::owned_watcher_config(owned_selector, &support),
            )
            .with_config(watcher::controller_config())
            .shutdown_on_signal()
            .run(
                reconcile::nodepool::reconcile,
                reconcile::nodepool::error_policy,
                context,
            )
            .for_each(|result| async move {
                if let Err(error) = result {
                    tracing::warn!(%error, "GraphNodePool reconcile stream error");
                }
            });
        running.await;
    })
}

/// Spawn the `GraphIndexer` controller (todo 21) as a background task.
///
/// Watches `GraphIndexer` objects plus the owned indexer `Deployment`s
/// (label-filtered by [`crate::apply::CLUSTER_SELECTOR_LABEL`]). Shutdown
/// mirrors [`spawn_graph_cluster_controller`]: the returned handle is aborted
/// after the health server stops.
fn spawn_graph_indexer_controller(ctx: &Ctx) -> tokio::task::JoinHandle<()> {
    use futures::StreamExt as _;
    use k8s_openapi::api::apps::v1::Deployment;
    use k8s_openapi::api::policy::v1::PodDisruptionBudget;
    use kube::runtime::Controller;

    use crate::apply::CLUSTER_SELECTOR_LABEL;

    let client = ctx.client.clone();
    let indexers = ctx.indexers.clone();
    let context = std::sync::Arc::new(ctx.clone());
    tokio::spawn(async move {
        // Version probe runs once here (not per watch); every watch below
        // shares the cached outcome via the config builders.
        let support = watcher::probe_watcher_support(&client).await;
        let owned_selector = CLUSTER_SELECTOR_LABEL;
        let running = Controller::new(indexers, watcher::watcher_config(&support))
            .owns(
                context.api::<Deployment>(),
                watcher::owned_watcher_config(owned_selector, &support),
            )
            .owns(
                context.api::<PodDisruptionBudget>(),
                watcher::owned_watcher_config(owned_selector, &support),
            )
            .with_config(watcher::controller_config())
            .shutdown_on_signal()
            .run(
                reconcile::indexer::reconcile,
                reconcile::indexer::error_policy,
                context,
            )
            .for_each(|result| async move {
                if let Err(error) = result {
                    tracing::warn!(%error, "GraphIndexer reconcile stream error");
                }
            });
        running.await;
    })
}

/// Wait for SIGTERM or SIGINT.
///
/// Never panics: if the SIGTERM handler cannot install, SIGINT alone is
/// awaited instead of crashing the process.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut terminate) => {
                tokio::select! {
                    () = async {
                        let _ = tokio::signal::ctrl_c().await;
                    } => {},
                    _ = terminate.recv() => {},
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "SIGTERM handler unavailable, waiting for SIGINT");
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// axum router for `/healthz`, `/readyz` and `/metrics`.
///
/// Unmatched paths fall through to [`fallback_404`]. `/metrics` serves the
/// live registry from [`metrics::render`].
fn app() -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/metrics", get(metrics))
        .fallback(fallback_404)
}

async fn healthz() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/plain")], "ok\n")
}

async fn readyz() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/plain")], "ok\n")
}

async fn metrics() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/plain; version=0.0.4")],
        metrics::render(),
    )
}

async fn fallback_404() -> impl IntoResponse {
    (
        StatusCode::NOT_FOUND,
        [(header::CONTENT_TYPE, "text/plain")],
        "not found\n",
    )
}

/// Enable OpenTelemetry export when the `otlp` feature is compiled in.
///
/// Reads `OTEL_EXPORTER_OTLP_ENDPOINT`: unset or empty means tracing stays
/// local-only and this returns `None`. Otherwise it builds a real OTLP/gRPC
/// (`grpc-tonic`) span exporter, installs its batching provider as the global
/// tracer provider, and returns the `tracing-opentelemetry` layer so the
/// caller (`main::init_tracing`) can attach it while building the subscriber.
///
/// Startup is never blocked or failed here: the tonic client connects lazily
/// (an unreachable collector only surfaces as dropped batches on the
/// background thread), and every build failure degrades to a logged warning
/// plus `None`. The endpoint value is never logged — it may carry auth
/// material in userinfo or query parameters.
#[cfg(feature = "otlp")]
pub fn init_otlp()
-> Option<Box<dyn tracing_subscriber::layer::Layer<tracing_subscriber::Registry> + Send + Sync>> {
    use opentelemetry::trace::TracerProvider as _;
    use opentelemetry_otlp::WithExportConfig as _;

    let endpoint = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT").unwrap_or_default();
    if endpoint.is_empty() {
        return None;
    }
    let exporter = match opentelemetry_otlp::SpanExporter::builder()
        .with_tonic()
        .with_endpoint(endpoint)
        .build()
    {
        Ok(exporter) => exporter,
        Err(error) => {
            tracing::warn!(%error, "OTLP exporter build failed, spans stay local-only");
            return None;
        }
    };
    let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .build();
    opentelemetry::global::set_tracer_provider(provider.clone());
    let tracer = provider.tracer("hydradb-operator");
    Some(Box::new(tracing_opentelemetry::layer().with_tracer(tracer)))
}

/// No-op without the `otlp` feature: no OTLP code is linked.
#[cfg(not(feature = "otlp"))]
pub fn init_otlp()
-> Option<Box<dyn tracing_subscriber::layer::Layer<tracing_subscriber::Registry> + Send + Sync>> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Raw HTTP/1.1 round-trip against the real axum server over TCP.
    async fn raw_get(port: u16, path: &str) -> String {
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .expect("test server accepts connections");
        let request =
            format!("GET {path} HTTP/1.1\r\nhost: 127.0.0.1\r\nconnection: close\r\n\r\n");
        stream
            .write_all(request.as_bytes())
            .await
            .expect("test request writes");
        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .await
            .expect("test response reads");
        String::from_utf8(response).expect("test response is UTF-8")
    }

    fn status_line(response: &str) -> &str {
        response.lines().next().unwrap_or_default()
    }

    fn body(response: &str) -> &str {
        response.split("\r\n\r\n").nth(1).unwrap_or_default()
    }

    #[tokio::test]
    async fn health_server_answers_all_endpoints_over_tcp() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("ephemeral port binds");
        let port = listener
            .local_addr()
            .expect("listener has a local addr")
            .port();
        let server = tokio::spawn(async move {
            axum::serve(listener, app())
                .await
                .expect("test server serves");
        });

        let healthz = raw_get(port, "/healthz").await;
        assert!(
            status_line(&healthz).contains("200"),
            "healthz status is 200, got: {healthz}"
        );
        assert_eq!(body(&healthz), "ok\n", "healthz body is ok");
        assert!(
            healthz.contains("content-type: text/plain"),
            "healthz is text/plain, got: {healthz}"
        );

        let readyz = raw_get(port, "/readyz").await;
        assert!(
            status_line(&readyz).contains("200"),
            "readyz status is 200, got: {readyz}"
        );
        assert_eq!(body(&readyz), "ok\n", "readyz body is ok");

        let metrics = raw_get(port, "/metrics").await;
        assert!(
            status_line(&metrics).contains("200"),
            "metrics status is 200, got: {metrics}"
        );
        assert!(
            body(&metrics).contains("hydradb_operator_up 1"),
            "metrics body carries the up gauge, got: {metrics}"
        );
        assert!(
            metrics.contains("text/plain; version=0.0.4"),
            "metrics content-type is the Prometheus exposition type, got: {metrics}"
        );

        let unknown = raw_get(port, "/nope").await;
        assert!(
            status_line(&unknown).contains("404"),
            "unknown path is 404, got: {unknown}"
        );

        server.abort();
    }

    #[tokio::test]
    async fn metrics_endpoint_serves_the_rendered_registry() {
        crate::metrics::record_reconcile(
            "GraphClusterEndpoint",
            std::time::Duration::from_millis(5),
            Ok(()),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("ephemeral port binds");
        let port = listener
            .local_addr()
            .expect("listener has a local addr")
            .port();
        let server = tokio::spawn(async move {
            axum::serve(listener, app())
                .await
                .expect("test server serves");
        });

        let response = raw_get(port, "/metrics").await;
        assert!(
            status_line(&response).contains("200"),
            "metrics status is 200, got: {response}"
        );
        let response_body = body(&response);
        assert!(
            response_body.contains("hydradb_operator_up 1"),
            "metrics carries the up gauge, got: {response_body}"
        );
        assert!(
            response_body
                .contains("hydradb_operator_reconciles_total{kind=\"GraphClusterEndpoint\"} 1"),
            "metrics carries the recorded reconcile, got: {response_body}"
        );

        server.abort();
    }

    /// Serializes the OTLP env-var tests: the endpoint switch is process-global.
    #[cfg(feature = "otlp")]
    static OTLP_ENV_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Unset endpoint returns `None` without panicking or hanging.
    #[cfg(feature = "otlp")]
    #[tokio::test]
    async fn otlp_init_without_endpoint_returns_none() {
        let _guard = OTLP_ENV_GUARD
            .lock()
            .expect("OTLP env guard is not poisoned");
        // SAFETY: guard serializes all env mutation in this test binary, and
        // no other thread reads this var concurrently.
        unsafe {
            std::env::remove_var("OTEL_EXPORTER_OTLP_ENDPOINT");
        }
        let started = std::time::Instant::now();
        assert!(
            super::init_otlp().is_none(),
            "unset endpoint must stay local-only"
        );
        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "unset endpoint must return quickly"
        );
    }

    /// Unreachable collector still returns a layer quickly: the tonic client
    /// connects lazily, so startup is never blocked.
    #[cfg(feature = "otlp")]
    #[tokio::test]
    async fn otlp_init_with_unreachable_endpoint_returns_quickly() {
        let _guard = OTLP_ENV_GUARD
            .lock()
            .expect("OTLP env guard is not poisoned");
        // SAFETY: same serialization as above; 127.0.0.1:9 is the discard
        // port and loopback-only, so this is hermetic.
        unsafe {
            std::env::set_var("OTEL_EXPORTER_OTLP_ENDPOINT", "http://127.0.0.1:9");
        }
        let started = std::time::Instant::now();
        let layer = super::init_otlp();
        let elapsed = started.elapsed();
        // SAFETY: restoring the pre-test state under the same guard.
        unsafe {
            std::env::remove_var("OTEL_EXPORTER_OTLP_ENDPOINT");
        }
        assert!(
            layer.is_some(),
            "configured endpoint must yield a layer even when unreachable"
        );
        assert!(
            elapsed < std::time::Duration::from_secs(10),
            "unreachable collector must not block startup, took {elapsed:?}"
        );
    }

    /// Without the `otlp` feature there is no layer, ever.
    #[cfg(not(feature = "otlp"))]
    #[tokio::test]
    async fn otlp_init_is_always_none_without_feature() {
        assert!(
            super::init_otlp().is_none(),
            "no-feature build must stay local-only"
        );
    }
}
