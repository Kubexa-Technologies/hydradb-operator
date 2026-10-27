//! Push-driven ingest receiver.
//!
//! Serves one endpoint:
//!
//! ```text
//! POST /webhooks/v1alpha1/ingest/{namespace}/{path}
//! X-HydraDB-Signature: sha256=<hex hmac of the raw body>
//! X-HydraDB-Revision:  <optional revision override>
//! ```
//!
//! Design rules, all of them deliberate:
//!
//! * **No unauthenticated triggers.** A source without `webhook.enabled` or
//!   without a `secretRef` answers `404`/`409`; the endpoint never starts a run
//!   it cannot authenticate.
//! * **The raw body is what is signed.** Parsing before verification would let
//!   a crafted document change the semantic meaning of a signed payload.
//! * **Constant-time comparison** via `hmac`'s `verify_slice`, never `==`.
//! * **No secret material in logs**, not even at debug level; only the source
//!   name, status code and run name are recorded.
//! * **Bounded body** (1 MiB) so a large push cannot exhaust operator memory.

use axum::Router;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use hmac::{Hmac, KeyInit, Mac};
use hydradb_operator_api::crd::GraphIngestSource;
use k8s_openapi::api::core::v1::Secret;
use kube::{Api, Client, ResourceExt};
use sha2::Sha256;
use tracing::{info, warn};

use crate::error::Error;

type HmacSha256 = Hmac<Sha256>;

/// Header carrying the lowercase hex HMAC-SHA256 of the request body.
pub const SIGNATURE_HEADER: &str = "x-hydradb-signature";
/// Header carrying the unix timestamp the signature covers.
pub const TIMESTAMP_HEADER: &str = "x-hydradb-timestamp";
/// Maximum accepted clock skew, in seconds, between signer and receiver.
pub const MAX_SKEW_SECS: u64 = 300;
/// Default key inside the webhook Secret.
pub const DEFAULT_WEBHOOK_KEY: &str = "token";
/// Largest body the receiver accepts.
pub const MAX_BODY_BYTES: usize = 1024 * 1024;

/// Shared state for the receiver.
#[derive(Clone)]
pub struct WebhookState {
    /// Kubernetes client used to read sources and start runs.
    pub client: Client,
    /// Namespace the operator is allowed to act in.
    ///
    /// `Some(ns)` under `installScope=namespaced`: any other namespace answers
    /// 404, so the receiver can never reach beyond what the operator watches.
    /// `None` under `installScope=cluster`.
    pub allowed_namespace: Option<String>,
}

/// Verify a signature over `timestamp.body`.
///
/// Signing the timestamp (not just the body) is what makes a captured request
/// non-replayable: the receiver rejects anything outside
/// [`MAX_SKEW_SECS`]. The digest comparison itself is constant-time.
#[must_use]
pub fn verify_signature(secret: &[u8], body: &[u8], timestamp: &str, header: &str) -> bool {
    let Some(hex) = header.strip_prefix("sha256=") else {
        return false;
    };
    let Ok(expected) = hex_decode(hex) else {
        return false;
    };
    let Ok(mut mac) = HmacSha256::new_from_slice(secret) else {
        return false;
    };
    mac.update(timestamp.as_bytes());
    mac.update(b".");
    mac.update(body);
    mac.verify_slice(&expected).is_ok()
}

/// Whether `timestamp` is within [`MAX_SKEW_SECS`] of now.
#[must_use]
pub fn timestamp_is_fresh(timestamp: &str) -> bool {
    let Ok(seconds) = timestamp.parse::<u64>() else {
        return false;
    };
    let Ok(now) = u64::try_from(chrono::Utc::now().timestamp()) else {
        return false;
    };
    now.abs_diff(seconds) <= MAX_SKEW_SECS
}

/// Compute the `sha256=<hex>` signature over `timestamp.body`.
#[must_use]
pub fn sign(secret: &[u8], body: &[u8], timestamp: &str) -> String {
    let mut mac = match HmacSha256::new_from_slice(secret) {
        Ok(mac) => mac,
        Err(_) => return String::new(),
    };
    mac.update(timestamp.as_bytes());
    mac.update(b".");
    mac.update(body);
    format!("sha256={}", hex_encode(&mac.finalize().into_bytes()))
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn hex_decode(input: &str) -> Result<Vec<u8>, ()> {
    if !input.len().is_multiple_of(2) {
        return Err(());
    }
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(input.len() / 2);
    for pair in bytes.as_chunks::<2>().0 {
        let high = hex_value(pair[0]).ok_or(())?;
        let low = hex_value(pair[1]).ok_or(())?;
        out.push((high << 4) | low);
    }
    Ok(out)
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Build the router for the receiver.
pub fn router(state: WebhookState) -> Router {
    Router::new()
        .route(
            "/webhooks/v1alpha1/ingest/{namespace}/{path}",
            post(handle_ingest),
        )
        // Enforced by the extractor layer: the length check in the handler ran
        // after axum had already buffered the body.
        .layer(axum::extract::DefaultBodyLimit::max(MAX_BODY_BYTES))
        .with_state(state)
}

/// Handle one ingest webhook.
async fn handle_ingest(
    State(state): State<WebhookState>,
    Path((namespace, path)): Path<(String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if body.len() > MAX_BODY_BYTES {
        return (StatusCode::PAYLOAD_TOO_LARGE, "body too large").into_response();
    }

    // installScope=namespaced means the operator must not touch another
    // namespace, even though its ClusterRole could.
    if let Some(allowed) = &state.allowed_namespace
        && &namespace != allowed
    {
        return (StatusCode::NOT_FOUND, "unknown ingest source").into_response();
    }

    let sources = Api::<GraphIngestSource>::namespaced(state.client.clone(), &namespace);
    let source = match find_source(&sources, &path).await {
        Ok(Some(source)) => source,
        Ok(None) => return (StatusCode::NOT_FOUND, "unknown ingest source").into_response(),
        Err(error) => {
            warn!(%namespace, %path, error = %error, "ingest source lookup failed");
            return (StatusCode::INTERNAL_SERVER_ERROR, "lookup failed").into_response();
        }
    };

    if !source.spec.webhook.enabled {
        // Deliberately indistinguishable from "does not exist": an unauthenticated
        // caller learns nothing about which sources exist.
        return (StatusCode::NOT_FOUND, "unknown ingest source").into_response();
    }
    let Some(secret_ref) = source.spec.webhook.secret_ref.clone() else {
        return (StatusCode::CONFLICT, "webhook enabled without a secretRef").into_response();
    };

    let signature = headers
        .get(SIGNATURE_HEADER)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    let timestamp = headers
        .get(TIMESTAMP_HEADER)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    let key = match read_secret_key(
        &state.client,
        &namespace,
        &secret_ref.name,
        source
            .spec
            .webhook
            .secret_key
            .as_deref()
            .unwrap_or(DEFAULT_WEBHOOK_KEY),
    )
    .await
    {
        Ok(Some(key)) => key,
        Ok(None) => {
            warn!(%namespace, source = %source.name_any(), "webhook secret key missing");
            return (StatusCode::INTERNAL_SERVER_ERROR, "webhook secret missing").into_response();
        }
        Err(error) => {
            warn!(%namespace, error = %error, "webhook secret read failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "webhook secret unreadable",
            )
                .into_response();
        }
    };

    if !verify_signature(&key, &body, timestamp, signature) {
        warn!(%namespace, %path, "webhook signature rejected");
        return (StatusCode::UNAUTHORIZED, "invalid signature").into_response();
    }
    if !timestamp_is_fresh(timestamp) {
        warn!(%namespace, %path, "webhook timestamp outside the allowed skew");
        return (StatusCode::UNAUTHORIZED, "stale request").into_response();
    }

    // The revision comes from the SIGNED body only: a header would sit outside
    // the MAC and let a replayed-but-valid request choose any revision.
    let revision = revision_from_body(&body);

    let ctx = match crate::context::Ctx::new(
        state.client.clone(),
        crate::context::WatchScope::Namespaced,
    ) {
        Ok(ctx) => ctx,
        Err(error) => {
            warn!(error = %error, "webhook context build failed");
            return (StatusCode::INTERNAL_SERVER_ERROR, "context failed").into_response();
        }
    };

    match crate::reconcile::ingest::trigger_run(&ctx, &source, revision).await {
        Ok(run) => {
            info!(%namespace, source = %source.name_any(), %run, "ingest run triggered");
            (StatusCode::ACCEPTED, run).into_response()
        }
        Err(Error::Conflict(message)) => (StatusCode::CONFLICT, message).into_response(),
        Err(error) => {
            warn!(%namespace, error = %error, "ingest run failed to start");
            (StatusCode::INTERNAL_SERVER_ERROR, "run failed").into_response()
        }
    }
}

/// Find a source by its webhook `path` segment (falling back to its name).
async fn find_source(
    api: &Api<GraphIngestSource>,
    path: &str,
) -> Result<Option<GraphIngestSource>, Error> {
    if let Some(source) = api.get_opt(path).await.map_err(Error::from_kube_error)? {
        return Ok(Some(source));
    }
    let all = api
        .list(&kube::api::ListParams::default())
        .await
        .map_err(Error::from_kube_error)?;
    Ok(all
        .items
        .into_iter()
        .find(|source| crate::reconcile::ingest::webhook_path_of(source) == path))
}

/// Read one key from a Secret, returning `None` when the key is absent.
async fn read_secret_key(
    client: &Client,
    namespace: &str,
    name: &str,
    key: &str,
) -> Result<Option<Vec<u8>>, Error> {
    let secrets = Api::<Secret>::namespaced(client.clone(), namespace);
    let Some(secret) = secrets
        .get_opt(name)
        .await
        .map_err(Error::from_kube_error)?
    else {
        return Ok(None);
    };
    Ok(secret
        .data
        .as_ref()
        .and_then(|data| data.get(key))
        .map(|bytes| bytes.0.clone()))
}

/// Best-effort revision extraction from a push payload.
///
/// Git providers disagree on the field name, so the three common shapes are
/// tried in order. A body without any of them simply yields `None`, which makes
/// the run fall back to the configured revision.
#[must_use]
pub fn revision_from_body(body: &[u8]) -> Option<String> {
    let value: serde_json::Value = serde_json::from_slice(body).ok()?;
    for pointer in [
        "/after",
        "/checkout_sha",
        "/head_commit/id",
        "/commits/0/id",
    ] {
        if let Some(text) = value.pointer(pointer).and_then(|v| v.as_str())
            && !text.is_empty()
            && text.chars().all(|c| c.is_ascii_hexdigit())
        {
            return Some(text.to_owned());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &[u8] = b"0123456789abcdef0123456789abcdef";

    #[test]
    fn signature_round_trips() {
        let body = br#"{"after":"abc123"}"#;
        let signature = sign(SECRET, body, "1700000000");
        assert!(signature.starts_with("sha256="));
        assert!(verify_signature(SECRET, body, "1700000000", &signature));
    }

    #[test]
    fn a_captured_signature_cannot_be_reused_with_another_timestamp() {
        let body = b"payload";
        let signature = sign(SECRET, body, "1700000000");
        assert!(!verify_signature(SECRET, body, "1700000001", &signature));
    }

    #[test]
    fn stale_timestamps_are_rejected() {
        let now = chrono::Utc::now().timestamp().to_string();
        assert!(timestamp_is_fresh(&now));
        assert!(!timestamp_is_fresh("1000000000"));
        assert!(!timestamp_is_fresh("not-a-number"));
    }

    #[test]
    fn rejects_wrong_secret_body_and_malformed_headers() {
        let body = b"payload";
        let signature = sign(SECRET, body, "1700000000");
        assert!(!verify_signature(
            b"another-secret-but-32-bytes-long!!",
            body,
            "1700000000",
            &signature
        ));
        assert!(!verify_signature(
            SECRET,
            b"other",
            "1700000000",
            &signature
        ));
        assert!(!verify_signature(SECRET, body, "1700000000", "sha256="));
        assert!(!verify_signature(SECRET, body, "1700000000", "sha256=zzzz"));
        assert!(!verify_signature(SECRET, body, "1700000000", "sha1=abcd"));
        assert!(!verify_signature(SECRET, body, "1700000000", ""));
    }

    #[test]
    fn accepts_uppercase_hex() {
        let body = b"payload";
        let signature = sign(SECRET, body, "1700000000");
        let upper = format!("sha256={}", signature[7..].to_uppercase());
        assert!(verify_signature(SECRET, body, "1700000000", &upper));
    }

    #[test]
    fn extract_revision_from_common_push_payloads() {
        assert_eq!(
            revision_from_body(br#"{"after":"deadbeef"}"#).as_deref(),
            Some("deadbeef")
        );
        assert_eq!(
            revision_from_body(br#"{"checkout_sha":"cafe01"}"#).as_deref(),
            Some("cafe01")
        );
        assert_eq!(
            revision_from_body(br#"{"head_commit":{"id":"f00d"}}"#).as_deref(),
            Some("f00d")
        );
        assert_eq!(revision_from_body(br#"{"after":"not-a-sha!"}"#), None);
        assert_eq!(revision_from_body(b"not json"), None);
    }
}
