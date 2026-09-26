//! A minimal, strict HTTP/1.1 reader and writer for the daemon's loopback
//! listeners.
//!
//! Shared by the webhook ingress ([`crate::webhook_ingress`]) and the agent hook
//! ingress. Hand-rolled on purpose: each listener serves one or two POST routes
//! on `127.0.0.1`, and a byte-level parse with hard caps is smaller to audit than
//! a server framework.
//!
//! The read is split in two so a caller can authenticate on the HEAD before it
//! reads a single body byte: [`read_head`] stops at the blank line, and
//! [`read_body`] then reads exactly `Content-Length` bytes under its own cap.
//! Neither sets a deadline; a caller wraps each in `tokio::time::timeout`.
//!
//! Strict by design, because these listeners sit on a trust boundary. A head is
//! refused (the caller answers 400) when:
//! - it is not ASCII, or the request line is not exactly
//!   `METHOD SP TARGET SP HTTP/1.0|HTTP/1.1`;
//! - a header line has no `:`, an empty name, or whitespace or a
//!   non-token character in its name (which also refuses obsolete line folding);
//! - a header name appears twice;
//! - it carries `Transfer-Encoding` at all (only `Content-Length` framing);
//! - `Content-Length` is not all digits, or does not fit a `usize`.

use std::collections::HashMap;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// A parsed request line and header block.
#[derive(Debug)]
pub(crate) struct RequestHead {
    /// The request method, as sent.
    pub(crate) method: String,
    /// The request target, as sent.
    pub(crate) path: String,
    /// Header names lower-cased, values trimmed. Every name is unique.
    pub(crate) headers: HashMap<String, String>,
    /// Bytes of the head, including the terminating blank line.
    pub(crate) head_len: usize,
    /// The declared `Content-Length`, `0` when absent.
    pub(crate) content_length: usize,
    /// Body bytes that arrived in the same reads as the head.
    leftover: Vec<u8>,
}

/// Read until the header terminator and parse the head, strictly.
///
/// `Ok(None)` means refused: the connection closed early, the head is over
/// `max_head` bytes, or any rule in the module docs is broken. The caller
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
    let leftover = buf[head_len..].to_vec();
    Ok(
        parse_head(&buf[..head_len]).map(|(method, path, headers, content_length)| RequestHead {
            method,
            path,
            headers,
            head_len,
            content_length,
            leftover,
        }),
    )
}

type ParsedHead = (String, String, HashMap<String, String>, usize);

/// Parse a complete head (ending in CRLF CRLF). `None` on any rule broken.
fn parse_head(raw: &[u8]) -> Option<ParsedHead> {
    if !raw.is_ascii() {
        return None;
    }
    let head = std::str::from_utf8(raw).ok()?;
    let head = head.strip_suffix("\r\n\r\n")?;
    let mut lines = head.split("\r\n");

    let request_line = lines.next()?;
    let mut parts = request_line.split(' ');
    let (method, path, version) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some()
        || method.is_empty()
        || !method.bytes().all(is_tchar)
        || path.is_empty()
        || path.bytes().any(|b| b.is_ascii_control())
        || !matches!(version, "HTTP/1.0" | "HTTP/1.1")
    {
        return None;
    }

    let mut headers = HashMap::new();
    for line in lines {
        let (name, value) = line.split_once(':')?;
        if name.is_empty() || !name.bytes().all(is_tchar) {
            return None;
        }
        if value.bytes().any(|b| b.is_ascii_control() && b != b'\t') {
            return None;
        }
        let name = name.to_ascii_lowercase();
        if headers.insert(name, value.trim_matches([' ', '\t']).to_string()).is_some() {
            return None;
        }
    }
    if headers.contains_key("transfer-encoding") {
        return None;
    }
    let content_length = match headers.get("content-length") {
        None => 0,
        Some(v) if !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()) => v.parse().ok()?,
        Some(_) => return None,
    };
    Some((
        method.to_string(),
        path.to_string(),
        headers,
        content_length,
    ))
}

/// RFC 9110 `tchar`: the characters a method or a header name may use.
const fn is_tchar(b: u8) -> bool {
    b.is_ascii_alphanumeric()
        || matches!(
            b,
            b'!' | b'#'
                | b'$'
                | b'%'
                | b'&'
                | b'\''
                | b'*'
                | b'+'
                | b'-'
                | b'.'
                | b'^'
                | b'_'
                | b'`'
                | b'|'
                | b'~'
        )
}

/// Read the body the head declared.
///
/// `Ok(None)` when `Content-Length` is over `max_body` (nothing past the head
/// is read), or when the peer closes before sending all of it. The caller
/// answers 413 or 400.
///
/// # Errors
/// A read error from `stream`.
pub(crate) async fn read_body<S: AsyncRead + Unpin>(
    stream: &mut S,
    head: &mut RequestHead,
    max_body: usize,
) -> std::io::Result<Option<Vec<u8>>> {
    if head.content_length > max_body {
        return Ok(None);
    }
    let mut body = std::mem::take(&mut head.leftover);
    let mut chunk = [0_u8; 4096];
    while body.len() < head.content_length {
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            return Ok(None);
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

    fn post_with(headers: &str) -> Vec<u8> {
        format!("POST /hook/claude HTTP/1.1\r\nHost: 127.0.0.1:9\r\n{headers}\r\n").into_bytes()
    }

    #[tokio::test]
    async fn parses_a_post_and_its_body() {
        let raw = b"POST /hook/claude HTTP/1.1\r\nHost: 127.0.0.1:9\r\nContent-Length: 4\r\nX-A: b\r\n\r\nabcdEXTRA";
        let mut s: &[u8] = raw;
        let mut head = read_head(&mut s, 1024).await.unwrap().unwrap();
        assert_eq!(head.method, "POST");
        assert_eq!(head.path, "/hook/claude");
        assert_eq!(head.headers.get("x-a").map(String::as_str), Some("b"));
        assert_eq!(head.content_length, 4);
        let body = read_body(&mut s, &mut head, 16).await.unwrap().unwrap();
        assert_eq!(body, b"abcd");
    }

    #[tokio::test]
    async fn hostile_framing_is_refused() {
        for bad in [
            "Transfer-Encoding: chunked\r\n",
            "Transfer-Encoding: chunked\r\nContent-Length: 3\r\n",
            "Content-Length: 3\r\nContent-Length: 3\r\n",
            "Content-Length: -1\r\n",
            "Content-Length: +3\r\n",
            "Content-Length: 12345678901234567890123\r\n",
            "Content-Length: \r\n",
            "Content-Length: 3 3\r\n",
            "Content-Length : 3\r\n",
            " Content-Length: 3\r\n",
            "\tX-Folded: yes\r\n",
            "X-No-Colon\r\n",
            ": empty-name\r\n",
            "X-A: 1\r\nx-a: 2\r\n",
            "X-Ctl: a\u{7}b\r\n",
        ] {
            assert!(head_of(&post_with(bad), 4096).await.is_none(), "{bad:?}");
        }
    }

    #[tokio::test]
    async fn the_request_line_is_exactly_three_tokens() {
        for bad in [
            &b"POST /x\r\n\r\n"[..],
            b"POST /x HTTP/2\r\n\r\n",
            b"POST /x HTTP/1.1 extra\r\n\r\n",
            b"POST  /x HTTP/1.1\r\n\r\n",
            b"PO ST /x HTTP/1.1\r\n\r\n",
            b"POST /x\x01y HTTP/1.1\r\n\r\n",
            b"GARBAGE\r\n\r\n",
        ] {
            assert!(
                head_of(bad, 4096).await.is_none(),
                "{:?}",
                String::from_utf8_lossy(bad)
            );
        }
        assert!(head_of(b"POST /x HTTP/1.0\r\n\r\n", 4096).await.is_some());
    }

    #[tokio::test]
    async fn a_non_ascii_head_is_refused() {
        assert!(head_of(&post_with("X-A: caf\u{e9}\r\n"), 4096).await.is_none());
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
        let mut head = read_head(&mut s, 1024).await.unwrap().unwrap();
        assert!(read_body(&mut s, &mut head, 99).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn a_short_body_is_refused() {
        let raw = b"POST / HTTP/1.1\r\nContent-Length: 10\r\n\r\nabc";
        let mut s: &[u8] = raw;
        let mut head = read_head(&mut s, 1024).await.unwrap().unwrap();
        assert!(read_body(&mut s, &mut head, 100).await.unwrap().is_none());
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

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(512))]

        /// Any bytes at all: the parse answers Some or None, never panics,
        /// and anything it accepts obeys its own rules.
        #[test]
        fn read_head_and_body_never_panic(raw in proptest::collection::vec(proptest::num::u8::ANY, 0..2048)) {
            let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
            rt.block_on(async {
                let mut s: &[u8] = &raw;
                if let Some(mut head) = read_head(&mut s, 1024).await.unwrap() {
                    assert!(head.head_len <= 1024);
                    assert!(!head.headers.contains_key("transfer-encoding"));
                    if let Some(body) = read_body(&mut s, &mut head, 1024).await.unwrap() {
                        assert_eq!(body.len(), head.content_length);
                    }
                }
            });
        }

        /// Heads built from header-ish fragments: the same guarantees.
        #[test]
        fn structured_heads_never_panic(
            lines in proptest::collection::vec("[ -~\t:]{0,40}", 0..8),
            cl in proptest::option::of("[-+0-9 ]{0,24}"),
        ) {
            let mut raw = String::from("POST /x HTTP/1.1\r\n");
            for l in &lines {
                raw.push_str(l);
                raw.push_str("\r\n");
            }
            if let Some(cl) = &cl {
                raw.push_str("Content-Length: ");
                raw.push_str(cl);
                raw.push_str("\r\n");
            }
            raw.push_str("\r\n");
            let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
            rt.block_on(async {
                let mut s: &[u8] = raw.as_bytes();
                if let Some(head) = read_head(&mut s, 4096).await.unwrap() {
                    if let Some(v) = head.headers.get("content-length") {
                        assert!(v.bytes().all(|b| b.is_ascii_digit()) && !v.is_empty());
                    }
                }
            });
        }
    }
}
