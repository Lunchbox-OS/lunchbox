//! Minimal HTTP/1.1 client for the e2e harness.
//!
//! The shepherdd management API is small and only exposed on localhost, so a
//! hand-rolled tokio-based client avoids pulling in `reqwest` and its TLS
//! dependency tree. Supports the request/response surface the tests need:
//! GET/POST/PUT/DELETE with optional JSON body, optional Bearer auth, and an
//! SSE stream that yields parsed JSON values from `data: ...` frames.

use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

/// Decoded HTTP response from the harness client.
#[derive(Debug, Clone)]
pub struct HttpResponse {
    pub status: u16,
    pub body: String,
}

impl HttpResponse {
    pub fn json(&self) -> Result<Value> {
        serde_json::from_str(&self.body).map_err(|e| anyhow!("invalid JSON body: {e}"))
    }
}

/// Cheap-to-clone HTTP client targeting `127.0.0.1:port`.
#[derive(Debug, Clone)]
pub struct HttpClient {
    port: u16,
    auth_token: Option<String>,
}

impl HttpClient {
    pub fn new(port: u16, auth_token: Option<String>) -> Self {
        Self { port, auth_token }
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub async fn get(&self, path: &str) -> Result<HttpResponse> {
        self.request("GET", path, None).await
    }

    pub async fn delete(&self, path: &str) -> Result<HttpResponse> {
        self.request("DELETE", path, None).await
    }

    pub async fn post_json(&self, path: &str, body: &Value) -> Result<HttpResponse> {
        self.request("POST", path, Some(body)).await
    }

    pub async fn put_json(&self, path: &str, body: &Value) -> Result<HttpResponse> {
        self.request("PUT", path, Some(body)).await
    }

    /// JSON-RPC dispatch through `POST /api/v1/rpc`. The management
    /// HTTP surface is now RPC-only, so nearly every e2e test call
    /// goes through this helper.
    pub async fn rpc(&self, method: &str, params: Value) -> Result<HttpResponse> {
        let body = serde_json::json!({ "method": method, "params": params });
        self.request("POST", "/api/v1/rpc", Some(&body)).await
    }

    /// Open an SSE stream against `path`. The returned [`SseStream`] yields
    /// successive JSON-decoded events.
    pub async fn sse(&self, path: &str) -> Result<SseStream> {
        let stream = self.connect().await?;
        let (read, mut write) = stream.into_split();
        let mut req = format!(
            "GET {path} HTTP/1.1\r\n\
             Host: 127.0.0.1:{port}\r\n\
             Accept: text/event-stream\r\n\
             Connection: keep-alive\r\n",
            port = self.port,
        );
        if let Some(t) = &self.auth_token {
            req.push_str(&format!("Authorization: Bearer {t}\r\n"));
        }
        req.push_str("\r\n");
        write
            .write_all(req.as_bytes())
            .await
            .context("write SSE request")?;
        write.flush().await.ok();

        let mut reader = BufReader::new(read);
        let (status, _headers) = read_response_head(&mut reader).await?;
        if status != 200 {
            bail!("SSE handshake returned status {status}");
        }
        Ok(SseStream {
            reader,
            _write: write,
        })
    }

    async fn connect(&self) -> Result<TcpStream> {
        let addr = format!("127.0.0.1:{}", self.port);
        TcpStream::connect(&addr)
            .await
            .with_context(|| format!("connect to {addr}"))
    }

    async fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
    ) -> Result<HttpResponse> {
        let stream = self.connect().await?;
        let (read, mut write) = stream.into_split();

        let serialized = body.map(|v| v.to_string());
        let mut req = format!(
            "{method} {path} HTTP/1.1\r\n\
             Host: 127.0.0.1:{port}\r\n\
             Connection: close\r\n\
             Accept: application/json\r\n",
            port = self.port,
        );
        if let Some(t) = &self.auth_token {
            req.push_str(&format!("Authorization: Bearer {t}\r\n"));
        }
        if let Some(b) = &serialized {
            req.push_str("Content-Type: application/json\r\n");
            req.push_str(&format!("Content-Length: {}\r\n", b.len()));
        } else {
            req.push_str("Content-Length: 0\r\n");
        }
        req.push_str("\r\n");
        if let Some(b) = &serialized {
            req.push_str(b);
        }

        write
            .write_all(req.as_bytes())
            .await
            .context("write HTTP request")?;
        write.flush().await.ok();

        let mut reader = BufReader::new(read);
        let (status, headers) = read_response_head(&mut reader).await?;

        let mut body_bytes = Vec::new();
        let chunked = headers
            .iter()
            .any(|(k, v)| k.eq_ignore_ascii_case("transfer-encoding") && v.contains("chunked"));
        let content_length = headers
            .iter()
            .find_map(|(k, v)| k.eq_ignore_ascii_case("content-length").then(|| v.clone()))
            .and_then(|v| v.trim().parse::<usize>().ok());

        if chunked {
            read_chunked_body(&mut reader, &mut body_bytes).await?;
        } else if let Some(len) = content_length {
            body_bytes.resize(len, 0);
            tokio::io::AsyncReadExt::read_exact(&mut reader, &mut body_bytes)
                .await
                .context("read response body (Content-Length)")?;
        } else {
            // Connection: close — read to EOF.
            tokio::io::AsyncReadExt::read_to_end(&mut reader, &mut body_bytes)
                .await
                .context("read response body (EOF)")?;
        }

        let body = String::from_utf8_lossy(&body_bytes).into_owned();
        Ok(HttpResponse { status, body })
    }
}

async fn read_response_head(
    reader: &mut BufReader<tokio::net::tcp::OwnedReadHalf>,
) -> Result<(u16, Vec<(String, String)>)> {
    let mut status_line = String::new();
    reader
        .read_line(&mut status_line)
        .await
        .context("read status line")?;
    if status_line.is_empty() {
        bail!("server closed connection before status line");
    }
    // e.g. "HTTP/1.1 200 OK\r\n"
    let parts: Vec<&str> = status_line.splitn(3, ' ').collect();
    let status = parts
        .get(1)
        .ok_or_else(|| anyhow!("malformed status line: {status_line:?}"))?
        .parse::<u16>()
        .with_context(|| format!("parse status code from {status_line:?}"))?;

    let mut headers = Vec::new();
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).await.context("read header")?;
        if n == 0 {
            bail!("connection closed mid-headers");
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some((name, value)) = trimmed.split_once(':') {
            headers.push((name.trim().to_owned(), value.trim().to_owned()));
        }
    }
    Ok((status, headers))
}

async fn read_chunked_body(
    reader: &mut BufReader<tokio::net::tcp::OwnedReadHalf>,
    out: &mut Vec<u8>,
) -> Result<()> {
    use tokio::io::AsyncReadExt;
    loop {
        let mut size_line = String::new();
        reader
            .read_line(&mut size_line)
            .await
            .context("read chunk size")?;
        let size_str = size_line.trim_end_matches(['\r', '\n']);
        // Drop chunk extensions (after `;`).
        let size_hex = size_str.split(';').next().unwrap_or("0").trim();
        let size = usize::from_str_radix(size_hex, 16)
            .with_context(|| format!("parse chunk size {size_str:?}"))?;
        if size == 0 {
            // Consume trailing CRLF after the zero chunk (and any trailers).
            loop {
                let mut trail = String::new();
                let n = reader.read_line(&mut trail).await?;
                if n == 0 || trail.trim().is_empty() {
                    break;
                }
            }
            return Ok(());
        }
        let start = out.len();
        out.resize(start + size, 0);
        reader.read_exact(&mut out[start..]).await?;
        // Consume trailing CRLF after each chunk.
        let mut crlf = [0u8; 2];
        reader.read_exact(&mut crlf).await?;
    }
}

/// Streaming SSE reader. Each call to [`SseStream::next_event`] returns the
/// next parsed `data:` frame as JSON.
pub struct SseStream {
    reader: BufReader<tokio::net::tcp::OwnedReadHalf>,
    // Held to keep the half-stream alive so the server doesn't see EOF.
    _write: tokio::net::tcp::OwnedWriteHalf,
}

impl SseStream {
    /// Wait for the next SSE event, or return `Err` if the stream closes or
    /// a malformed frame is observed.
    pub async fn next_event(&mut self) -> Result<Value> {
        let mut data = String::new();
        loop {
            let mut line = String::new();
            let n = self
                .reader
                .read_line(&mut line)
                .await
                .context("read SSE line")?;
            if n == 0 {
                bail!("SSE stream closed");
            }
            let line = line.trim_end_matches(['\r', '\n']);

            if line.is_empty() {
                if !data.is_empty() {
                    return serde_json::from_str(&data)
                        .with_context(|| format!("parse SSE data: {data:?}"));
                }
                continue;
            }

            if let Some(payload) = line.strip_prefix("data:") {
                let payload = payload.strip_prefix(' ').unwrap_or(payload);
                if !data.is_empty() {
                    data.push('\n');
                }
                data.push_str(payload);
            }
            // Other prefixes (event:, id:, retry:, comments) are ignored — the
            // shepherdd SSE handler does not use them.
        }
    }

    /// Wait up to `timeout` for an event whose JSON satisfies `predicate`.
    /// Other events are dropped on the floor.
    pub async fn wait_for<F>(&mut self, timeout: Duration, mut predicate: F) -> Result<Value>
    where
        F: FnMut(&Value) -> bool,
    {
        tokio::time::timeout(timeout, async {
            loop {
                let ev = self.next_event().await?;
                if predicate(&ev) {
                    return Ok::<Value, anyhow::Error>(ev);
                }
            }
        })
        .await
        .map_err(|_| anyhow!("timed out after {timeout:?} waiting for SSE event"))?
    }
}
