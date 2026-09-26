//! A minimal HTTP/1.1 reader and writer for the daemon's loopback listeners.
//!
//! Shared by the webhook ingress ([`crate::webhook_ingress`]) and the agent hook
//! ingress. Hand-rolled on purpose: each listener serves one or two POST routes
//! on `127.0.0.1`, and a byte-level parse with hard caps is smaller to audit than
//! a server framework.
//!
//! The read is split in two so a caller can authenticate on the HEAD before it
//! reads a single body byte: [`read_head`] stops at the blank line, and
//! [`read_body`] then reads exactly `Content-Length` bytes under its own cap.
//! Neither sets a deadline; a caller that needs one wraps the call in
//! `tokio::time::timeout`.

use std::collections::HashMap;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// A parsed request line and header block.
#[derive(Debug)]
pub(crate) struct RequestHead {
    /// The request method, as sent.
    pub(crate) method: String,
    /// The request target, as sent.
    pub(crate) path: String,
    /// Header names lower-cased, values trimmed. A repeated name keeps the last.
    pub(crate) headers: HashMap<String, String>,
    /// Bytes of the head, including the terminating blank line.
    pub(crate) head_len: usize,
    /// The declared `Content-Length`, `0` when absent or unparseable.
    pub(crate) content_length: usize,
    /// Body bytes that arrived in the same reads as the head.
    leftover: Vec<u8>,
}

/// Read until the header terminator and parse the head.
///
/// `Ok(None)` means malformed: the connection closed early, the head is over
/// `max_head` bytes, or the request line has no method and target. The caller
/// answers 400.
///
/// # Errors
/// A read error from `stream`.
pub(crate) async fn read_head<S: AsyncRead + Unpin>(
    stream: &mut S,
    max_head: usize,
) -> std::io::Result<Option<RequestHead>> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0_u8; 4096];

    let head_len = loop {
        if let Some(pos) = find_subsequence(&buf, b"\r\n\r\n") {
            break pos + 4;
        }
        if buf.len() > max_head {
            return Ok(None);
        }
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            return Ok(None);
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    if head_len > max_head {
        return Ok(None);
    }

    let head = String::from_utf8_lossy(&buf[..head_len]);
    let mut lines = head.split("\r\n");
    let Some(request_line) = lines.next() else {
        return Ok(None);
    };
    let mut parts = request_line.split_whitespace();
    let (Some(method), Some(path)) = (parts.next(), parts.next()) else {
        return Ok(None);
    };

    let mut headers = HashMap::new();
    for line in lines {
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    let content_length = headers.get("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);

    Ok(Some(RequestHead {
        method: method.to_string(),
        path: path.to_string(),
        headers,
        head_len,
        content_length,
        leftover: buf[head_len..].to_vec(),
    }))
}

/// Read the body the head declared.
///
/// `Ok(None)` when `Content-Length` is over `max_body`; nothing past the head is
/// read in that case. A peer that closes early yields the bytes it sent.
///
/// # Errors
/// A read error from `stream`.
pub(crate) async fn read_body<S: AsyncRead + Unpin>(
    stream: &mut S,
    head: RequestHead,
    max_body: usize,
) -> std::io::Result<Option<Vec<u8>>> {
    if head.content_length > max_body {
        return Ok(None);
    }
    let mut body = head.leftover;
    let mut chunk = [0_u8; 4096];
    while body.len() < head.content_length {
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    body.truncate(head.content_length);
    Ok(Some(body))
}

/// Write a minimal HTTP/1.1 response and flush. The connection is closed by the
/// caller dropping the stream.
///
/// # Errors
/// A write error from `stream`.
pub(crate) async fn write_response<S: AsyncWrite + Unpin>(
    stream: &mut S,
    status: u16,
    content_type: &str,
    body: &[u8],
) -> std::io::Result<()> {
    let reason = reason_phrase(status);
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {len}\r\n\
         Connection: close\r\n\
         \r\n",
        len = body.len(),
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(body).await?;
    stream.flush().await
}

/// The reason phrase for the statuses the loopback listeners return.
pub(crate) const fn reason_phrase(status: u16) -> &'static str {
    match status {
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        413 => "Payload Too Large",
        415 => "Unsupported Media Type",
        429 => "Too Many Requests",
        503 => "Service Unavailable",
        _ => "Internal Server Error",
    }
}

/// First index of `needle` in `haystack`. The head is small, so a naive scan.
fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn head_of(raw: &[u8], max: usize) -> Option<RequestHead> {
        let mut s: &[u8] = raw;
        read_head(&mut s, max).await.unwrap()
    }

    #[tokio::test]
    async fn parses_a_post_and_its_body() {
        let raw = b"POST /hook/claude HTTP/1.1\r\nHost: 127.0.0.1:9\r\nContent-Length: 4\r\nX-A: b\r\n\r\nabcdEXTRA";
        let mut s: &[u8] = raw;
        let head = read_head(&mut s, 1024).await.unwrap().unwrap();
        assert_eq!(head.method, "POST");
        assert_eq!(head.path, "/hook/claude");
        assert_eq!(head.headers.get("x-a").map(String::as_str), Some("b"));
        assert_eq!(head.content_length, 4);
        let body = read_body(&mut s, head, 16).await.unwrap().unwrap();
        assert_eq!(body, b"abcd");
    }

    #[tokio::test]
    async fn an_oversized_head_is_malformed() {
        let mut raw = b"POST / HTTP/1.1\r\nX: ".to_vec();
        raw.extend(std::iter::repeat_n(b'a', 5000));
        raw.extend_from_slice(b"\r\n\r\n");
        assert!(head_of(&raw, 1024).await.is_none());
    }

    #[tokio::test]
    async fn an_oversized_body_is_refused_before_it_is_read() {
        let raw = b"POST / HTTP/1.1\r\nContent-Length: 100\r\n\r\n";
        let mut s: &[u8] = raw;
        let head = read_head(&mut s, 1024).await.unwrap().unwrap();
        assert!(read_body(&mut s, head, 99).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn a_bare_lf_head_never_terminates() {
        // Only CRLF CRLF ends a head; a peer that sends LF LF and closes is
        // malformed, not a request with an empty header block.
        assert!(head_of(b"POST / HTTP/1.1\n\n", 1024).await.is_none());
    }

    #[tokio::test]
    async fn an_early_close_is_malformed() {
        assert!(head_of(b"POST / HTTP/1.1\r\nHost: x\r\n", 1024).await.is_none());
        assert!(head_of(b"GARBAGE\r\n\r\n", 1024).await.is_none());
    }

    #[tokio::test]
    async fn a_slow_head_is_bounded_by_the_callers_deadline() {
        let (mut client, mut server) = tokio::io::duplex(64);
        tokio::spawn(async move {
            client.write_all(b"POST / HTTP/1.1\r\n").await.unwrap();
            // Never finishes the head; hold the pipe open.
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            drop(client);
        });
        let res = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            read_head(&mut server, 1024),
        )
        .await;
        assert!(res.is_err(), "the caller's timeout fires");
    }

    #[tokio::test]
    async fn writes_status_type_length_and_body() {
        let mut out = Vec::new();
        write_response(&mut out, 204, "text/plain", b"").await.unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.starts_with("HTTP/1.1 204 No Content\r\n"), "{text}");
        assert!(text.contains("Content-Length: 0\r\n"));
        assert!(text.ends_with("\r\n\r\n"));
    }
}
