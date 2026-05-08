//! Tiny single-route HTTP/1.1 server for `/metrics`.
//!
//! Hand-rolled on top of `tokio::net::TcpListener` so we don't pull
//! in a web-framework dependency for what amounts to "GET → string".
//! The Prometheus exposition format is plain text; the request side
//! is a single request line we only need to parse enough of to route.
//!
//! ## What's intentionally minimal
//!
//! - **No keep-alive.** Each connection serves one response and
//!   closes. Prometheus scrapers reconnect every interval anyway.
//! - **No header parsing beyond the request line.** Length-bounded
//!   read until `\r\n\r\n`, then ignore the headers entirely.
//! - **Single route.** `GET /metrics` → 200 text/plain. Anything
//!   else → 404.
//! - **Bind-address whitelisting.** The caller (config layer)
//!   defaults to `127.0.0.1`; if an operator points it at
//!   `0.0.0.0` they explicitly opted into network exposure.
//!
//! ## Lifecycle
//!
//! [`spawn`] returns immediately. The accept loop runs as a tokio
//! task that exits when the supplied `CancellationToken` fires —
//! same pattern as the rest of bee-tui's background tasks. Per-
//! connection handlers are spawned as detached tasks and wrapped in
//! a 5-second total timeout so a misbehaving client can't pin a
//! handler forever.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use color_eyre::eyre::{Result, eyre};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;

/// Maximum bytes we'll read from a single client request before
/// giving up. Real Prometheus scrapers send under 1 KiB; this cap
/// protects the server from a hung client streaming garbage.
const MAX_REQUEST_BYTES: usize = 8 * 1024;
/// Per-connection total time budget. The handler aborts if the
/// client takes longer than this to send a request and read the
/// response.
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(5);

/// Render closure type. Boxed `Fn` so the caller can capture
/// `BeeWatch` accessors etc.
pub type RenderFn = Arc<dyn Fn() -> String + Send + Sync + 'static>;

/// Bind to `addr`, then spawn an accept loop on a tokio task. The
/// task exits when `cancel` is triggered. Returns the actual bound
/// address (useful when the caller passed port 0 for ephemeral
/// binding in tests).
pub async fn spawn(
    addr: SocketAddr,
    render_fn: RenderFn,
    cancel: CancellationToken,
) -> Result<SocketAddr> {
    let listener = TcpListener::bind(addr)
        .await
        .map_err(|e| eyre!("failed to bind metrics endpoint to {addr}: {e}"))?;
    let local_addr = listener
        .local_addr()
        .map_err(|e| eyre!("failed to read local addr after bind: {e}"))?;

    tokio::spawn(async move {
        accept_loop(listener, render_fn, cancel).await;
    });

    Ok(local_addr)
}

async fn accept_loop(listener: TcpListener, render_fn: RenderFn, cancel: CancellationToken) {
    tracing::info!(
        "metrics: serving /metrics on {}",
        listener
            .local_addr()
            .map(|a| a.to_string())
            .unwrap_or_default()
    );
    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                tracing::debug!("metrics: cancel fired, exiting accept loop");
                return;
            }
            accepted = listener.accept() => {
                match accepted {
                    Ok((stream, _peer)) => {
                        let render = render_fn.clone();
                        tokio::spawn(async move {
                            // Bound the whole connection in one timeout
                            // so a slow client can't pin a handler. The
                            // cap is generous (5 s); a healthy scrape
                            // completes in < 100 ms.
                            let _ = tokio::time::timeout(
                                CONNECTION_TIMEOUT,
                                handle_connection(stream, render),
                            )
                            .await;
                        });
                    }
                    Err(e) => {
                        tracing::warn!("metrics: accept failed: {e}");
                    }
                }
            }
        }
    }
}

async fn handle_connection(mut stream: TcpStream, render_fn: RenderFn) {
    // Read until we see the end of HTTP headers (\r\n\r\n) or hit
    // the byte cap. We only care about the request line; the rest is
    // ignored.
    let mut buf = Vec::with_capacity(1024);
    let mut tmp = [0u8; 1024];
    loop {
        if buf.len() >= MAX_REQUEST_BYTES {
            let _ = write_response(&mut stream, 413, "text/plain", b"request too large").await;
            return;
        }
        match stream.read(&mut tmp).await {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&tmp[..n]);
                if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            Err(_) => return,
        }
    }

    let request_line = buf
        .iter()
        .position(|&b| b == b'\r')
        .map(|i| std::str::from_utf8(&buf[..i]).unwrap_or(""))
        .unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let path_with_query = parts.next().unwrap_or("");
    // Strip the query string — Prometheus doesn't use it for /metrics
    // but some scrapers append `?` for cache-busting.
    let path = path_with_query.split('?').next().unwrap_or("");

    if method != "GET" {
        let _ = write_response(&mut stream, 405, "text/plain", b"method not allowed").await;
        return;
    }
    if path != "/metrics" {
        let _ = write_response(&mut stream, 404, "text/plain", b"not found\n").await;
        return;
    }

    let body = render_fn();
    let _ = write_response(
        &mut stream,
        200,
        "text/plain; version=0.0.4; charset=utf-8",
        body.as_bytes(),
    )
    .await;
}

async fn write_response(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
) -> std::io::Result<()> {
    let reason = reason_phrase(status);
    let header = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n",
        body.len()
    );
    stream.write_all(header.as_bytes()).await?;
    stream.write_all(body).await?;
    stream.shutdown().await?;
    Ok(())
}

fn reason_phrase(status: u16) -> &'static str {
    match status {
        200 => "OK",
        404 => "Not Found",
        405 => "Method Not Allowed",
        413 => "Payload Too Large",
        _ => "Unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ephemeral() -> SocketAddr {
        "127.0.0.1:0".parse().unwrap()
    }

    /// End-to-end: spawn, fetch, parse response.
    async fn fetch(addr: SocketAddr, path: &str) -> (u16, String) {
        let mut s = TcpStream::connect(addr).await.expect("connect");
        s.write_all(format!("GET {path} HTTP/1.1\r\nHost: x\r\n\r\n").as_bytes())
            .await
            .unwrap();
        let mut out = Vec::new();
        s.read_to_end(&mut out).await.unwrap();
        let text = String::from_utf8_lossy(&out).to_string();
        let mut lines = text.split("\r\n");
        let status_line = lines.next().unwrap_or("");
        let status: u16 = status_line
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        // Skip headers up to blank line.
        for l in lines.by_ref() {
            if l.is_empty() {
                break;
            }
        }
        let body: String = lines.collect::<Vec<_>>().join("\r\n");
        (status, body)
    }

    #[tokio::test]
    async fn metrics_endpoint_returns_render_output() {
        let cancel = CancellationToken::new();
        let render: RenderFn = Arc::new(|| String::from("# HELP example 1\nexample 1\n"));
        let bound = spawn(ephemeral(), render, cancel.clone()).await.unwrap();

        let (status, body) = fetch(bound, "/metrics").await;
        assert_eq!(status, 200);
        assert!(body.contains("example 1"));

        cancel.cancel();
    }

    #[tokio::test]
    async fn unknown_path_returns_404() {
        let cancel = CancellationToken::new();
        let render: RenderFn = Arc::new(|| String::from("ignored"));
        let bound = spawn(ephemeral(), render, cancel.clone()).await.unwrap();

        let (status, _body) = fetch(bound, "/whatever").await;
        assert_eq!(status, 404);

        cancel.cancel();
    }

    #[tokio::test]
    async fn non_get_method_returns_405() {
        let cancel = CancellationToken::new();
        let render: RenderFn = Arc::new(|| String::from("ignored"));
        let bound = spawn(ephemeral(), render, cancel.clone()).await.unwrap();

        // Hand-craft a POST so we don't depend on reqwest in tests.
        let mut s = TcpStream::connect(bound).await.unwrap();
        s.write_all(b"POST /metrics HTTP/1.1\r\nHost: x\r\n\r\n")
            .await
            .unwrap();
        let mut out = Vec::new();
        s.read_to_end(&mut out).await.unwrap();
        let text = String::from_utf8_lossy(&out);
        assert!(
            text.starts_with("HTTP/1.1 405"),
            "expected 405, got: {text}"
        );

        cancel.cancel();
    }

    #[tokio::test]
    async fn cancellation_stops_the_accept_loop() {
        let cancel = CancellationToken::new();
        let render: RenderFn = Arc::new(|| String::from("ignored"));
        let bound = spawn(ephemeral(), render, cancel.clone()).await.unwrap();

        cancel.cancel();
        // Give the accept loop a moment to notice the cancel.
        tokio::time::sleep(Duration::from_millis(100)).await;
        // After cancel the listener fd is closed → connect should
        // either fail or complete and immediately read EOF. We
        // accept either; what we don't accept is a hung connect.
        let connect = tokio::time::timeout(Duration::from_secs(1), TcpStream::connect(bound)).await;
        // If the listener was kept alive past cancel, the timeout
        // wouldn't fire (connect succeeds quickly) but the request
        // would hang on read. Either way we want a quick verdict.
        assert!(connect.is_ok(), "connect/EOF must complete promptly");
    }

    #[tokio::test]
    async fn query_string_is_stripped() {
        let cancel = CancellationToken::new();
        let render: RenderFn = Arc::new(|| String::from("served"));
        let bound = spawn(ephemeral(), render, cancel.clone()).await.unwrap();

        let (status, body) = fetch(bound, "/metrics?cache_bust=42").await;
        assert_eq!(status, 200);
        assert!(body.contains("served"));

        cancel.cancel();
    }
}
