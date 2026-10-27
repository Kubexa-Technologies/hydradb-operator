//! Minimal in-cluster metrics scraper.
//!
//! The operator needs two numbers the operand only exposes over HTTP: how many
//! index generations the indexer has published, and whether it is failing.
//! Pulling a full HTTP client stack in for one plain-HTTP `GET` inside the
//! cluster is not worth the dependency tree, so this module implements exactly
//! what is needed — a bounded HTTP/1.1 request on a raw socket, with a hard
//! timeout and a size cap — and nothing else.
//!
//! It never sends a body, never follows redirects, never speaks TLS (the
//! metrics listener is plain HTTP by design), and never logs response content:
//! only the metric values it parses.

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::error::Error;

/// Hard cap on the response size we are willing to read.
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
/// Default request deadline.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(2);

/// Fetch `path` from `host:port` over plain HTTP/1.1.
///
/// # Errors
/// Returns [`Error::Timeout`] when the deadline elapses and [`Error::Server`]
/// for transport failures; a non-200 status is [`Error::Server`] too, because a
/// probing caller can only act on "I got the document".
pub async fn fetch(host_port: &str, path: &str) -> Result<String, Error> {
    fetch_with_timeout(host_port, path, DEFAULT_TIMEOUT).await
}

/// [`fetch`] with an explicit deadline.
///
/// # Errors
/// As [`fetch`].
pub async fn fetch_with_timeout(
    host_port: &str,
    path: &str,
    timeout: Duration,
) -> Result<String, Error> {
    let fut = async {
        let mut stream = TcpStream::connect(host_port)
            .await
            .map_err(|error| Error::Server(std::io::Error::other(error.to_string())))?;
        let request = format!(
            "GET {path} HTTP/1.1\r\nHost: {host_port}\r\nAccept: text/plain\r\n\
             Connection: close\r\nUser-Agent: hydradb-operator\r\n\r\n"
        );
        stream
            .write_all(request.as_bytes())
            .await
            .map_err(Error::Server)?;
        let mut buffer = Vec::with_capacity(8192);
        let mut chunk = [0u8; 8192];
        loop {
            let read = stream.read(&mut chunk).await.map_err(Error::Server)?;
            if read == 0 {
                break;
            }
            buffer.extend_from_slice(&chunk[..read]);
            if buffer.len() > MAX_RESPONSE_BYTES {
                break;
            }
        }
        Ok::<Vec<u8>, Error>(buffer)
    };

    let raw = match tokio::time::timeout(timeout, fut).await {
        Ok(result) => result?,
        Err(_) => {
            return Err(Error::Timeout(format!(
                "GET {host_port}{path} exceeded {timeout:?}"
            )));
        }
    };

    let text = String::from_utf8_lossy(&raw);
    let Some((headers, body)) = text.split_once("\r\n\r\n") else {
        return Err(Error::Server(std::io::Error::other(
            "response has no header/body separator",
        )));
    };
    if !headers.starts_with("HTTP/1.1 200") && !headers.starts_with("HTTP/1.0 200") {
        return Err(Error::Server(std::io::Error::other(
            "metrics endpoint answered non-200",
        )));
    }
    Ok(body.to_owned())
}

/// Read the first sample of `name` from Prometheus text exposition.
///
/// Labels are ignored: the operator only needs the aggregate value. `# HELP`
/// and `# TYPE` comment lines are skipped, and a value that does not parse
/// yields `None` rather than a silent zero.
#[must_use]
pub fn metric_value(text: &str, name: &str) -> Option<f64> {
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        let (series, value) = line.rsplit_once(' ')?;
        let series_name = series.split('{').next().unwrap_or(series);
        if series_name == name {
            return value.trim().parse::<f64>().ok();
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_and_labelled_samples() {
        let text = "# HELP graph_indexer_generations_published Published generations\n\
                    # TYPE graph_indexer_generations_published counter\n\
                    graph_indexer_generations_published 42\n\
                    graph_indexer_generation_failures{scope=\"a\"} 3\n";
        assert_eq!(
            metric_value(text, "graph_indexer_generations_published"),
            Some(42.0)
        );
        assert_eq!(
            metric_value(text, "graph_indexer_generation_failures"),
            Some(3.0)
        );
        assert_eq!(metric_value(text, "absent_metric"), None);
    }

    #[test]
    fn a_malformed_value_is_none_not_zero() {
        let text = "graph_indexer_generations_published not-a-number\n";
        assert_eq!(
            metric_value(text, "graph_indexer_generations_published"),
            None
        );
    }
}
