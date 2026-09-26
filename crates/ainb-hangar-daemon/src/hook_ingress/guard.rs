//! The request guards of the hook listener: who may speak, and how much.
//!
//! Every check here runs on the request HEAD, before a single body byte is
//! read. The order is fixed so an unauthenticated caller learns nothing about
//! routes: loopback `Host`, no `Origin`, token, then method and route, then
//! rate, content type and size.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

use ainb_hangar_proto::hooks::{HookSource, TOKEN_HEADER};

/// Largest request head accepted.
pub const MAX_HEAD: usize = 8 * 1024;
/// Largest body accepted. Claude's `PostToolUse` carries tool output, so this
/// is generous; the stored copy keeps the existing 3 KiB inline rule.
pub const MAX_BODY: usize = 1024 * 1024;
/// Most connections served at once.
pub const MAX_CONNECTIONS: usize = 64;
/// Token bucket refill, requests per second.
pub const RATE_PER_SEC: f64 = 200.0;
/// Token bucket size.
pub const RATE_BURST: f64 = 500.0;

/// Which endpoint a request names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// `POST /hook/<source>`: status, answered at once.
    Event(HookSource),
    /// `POST /hook/<source>/hold`: a blocking request that waits for a human.
    Hold(HookSource),
}

/// A refusal: the status to answer and a short plain-text reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Refusal {
    /// HTTP status.
    pub status: u16,
    /// Plain-text body. Never names the token or the expected value.
    pub reason: &'static str,
}

const fn refuse(status: u16, reason: &'static str) -> Refusal {
    Refusal { status, reason }
}

/// The per-listener secret and address a request is judged against.
pub struct Judge {
    token: String,
    host: String,
    bucket: Mutex<Bucket>,
}

impl std::fmt::Debug for Judge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Judge").field("host", &self.host).finish_non_exhaustive()
    }
}

impl Judge {
    /// A judge for a listener bound to `127.0.0.1:<port>` with `token`.
    #[must_use]
    pub fn new(token: String, port: u16) -> Self {
        Self {
            token,
            host: format!("127.0.0.1:{port}"),
            bucket: Mutex::new(Bucket::full(Instant::now())),
        }
    }

    /// Judge a request head. `Ok` names the route; `Err` is the answer.
    ///
    /// # Errors
    /// The [`Refusal`] to write back.
    pub fn admit(
        &self,
        method: &str,
        path: &str,
        headers: &HashMap<String, String>,
    ) -> Result<Route, Refusal> {
        self.admit_at(method, path, headers, Instant::now())
    }

    fn admit_at(
        &self,
        method: &str,
        path: &str,
        headers: &HashMap<String, String>,
        now: Instant,
    ) -> Result<Route, Refusal> {
        // DNS rebinding: a browser tricked into this port sends its own Host.
        if headers.get("host").map(String::as_str) != Some(self.host.as_str()) {
            return Err(refuse(403, "forbidden"));
        }
        // Any browser-originated request carries Origin; curl never does.
        if headers.contains_key("origin") {
            return Err(refuse(403, "forbidden"));
        }
        let presented = headers
            .get(&TOKEN_HEADER.to_ascii_lowercase())
            .map_or("", String::as_str);
        if !constant_time_eq(presented.as_bytes(), self.token.as_bytes()) {
            return Err(refuse(403, "forbidden"));
        }
        let route = parse_route(path).ok_or(refuse(404, "not found"))?;
        if method != "POST" {
            return Err(refuse(405, "method not allowed"));
        }
        if !self.take(now) {
            return Err(refuse(429, "too many requests"));
        }
        let json = headers
            .get("content-type")
            .and_then(|v| v.split(';').next())
            .is_some_and(|v| v.trim().eq_ignore_ascii_case("application/json"));
        if !json {
            return Err(refuse(415, "unsupported media type"));
        }
        let length: usize = headers
            .get("content-length")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        if length > MAX_BODY {
            return Err(refuse(413, "payload too large"));
        }
        Ok(route)
    }

    fn take(&self, now: Instant) -> bool {
        let mut bucket = self
            .bucket
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        bucket.take(now)
    }
}

fn parse_route(path: &str) -> Option<Route> {
    let rest = path.strip_prefix("/hook/")?;
    let (segment, hold) = match rest.split_once('/') {
        None => (rest, false),
        Some((segment, "hold")) => (segment, true),
        Some(_) => return None,
    };
    let source = HookSource::from_route(segment)?;
    Some(if hold {
        Route::Hold(source)
    } else {
        Route::Event(source)
    })
}

/// Compare without an early exit on the first differing byte. The length is
/// not secret (a UUID), so a length mismatch returns at once.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0_u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[derive(Debug)]
struct Bucket {
    tokens: f64,
    at: Instant,
}

impl Bucket {
    const fn full(at: Instant) -> Self {
        Self {
            tokens: RATE_BURST,
            at,
        }
    }

    fn take(&mut self, now: Instant) -> bool {
        let elapsed = now.saturating_duration_since(self.at).as_secs_f64();
        self.tokens = elapsed.mul_add(RATE_PER_SEC, self.tokens).min(RATE_BURST);
        self.at = now;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "3b0f6f5e-1c1a-4e4e-9b36-6a4a1d7c2f10";

    fn judge() -> Judge {
        Judge::new(TOKEN.to_string(), 4000)
    }

    fn ok_headers() -> HashMap<String, String> {
        HashMap::from([
            ("host".to_string(), "127.0.0.1:4000".to_string()),
            ("x-ainb-hook-token".to_string(), TOKEN.to_string()),
            ("content-type".to_string(), "application/json".to_string()),
            ("content-length".to_string(), "2".to_string()),
        ])
    }

    #[test]
    fn a_good_request_names_its_route() {
        let j = judge();
        assert_eq!(
            j.admit("POST", "/hook/claude", &ok_headers()),
            Ok(Route::Event(HookSource::Claude))
        );
        assert_eq!(
            j.admit("POST", "/hook/claude/hold", &ok_headers()),
            Ok(Route::Hold(HookSource::Claude))
        );
    }

    #[test]
    fn auth_is_judged_before_the_route() {
        let j = judge();
        let mut h = ok_headers();
        h.insert("x-ainb-hook-token".into(), "wrong".into());
        // An unknown route with a bad token is 403, not 404: no route probing.
        assert_eq!(j.admit("POST", "/nope", &h).unwrap_err().status, 403);
        h.remove("x-ainb-hook-token");
        assert_eq!(j.admit("POST", "/hook/claude", &h).unwrap_err().status, 403);
    }

    #[test]
    fn foreign_host_and_any_origin_are_refused() {
        let j = judge();
        let mut h = ok_headers();
        h.insert("host".into(), "evil.example:4000".into());
        assert_eq!(j.admit("POST", "/hook/claude", &h).unwrap_err().status, 403);
        let mut h = ok_headers();
        h.insert("origin".into(), "http://127.0.0.1:4000".into());
        assert_eq!(j.admit("POST", "/hook/claude", &h).unwrap_err().status, 403);
    }

    #[test]
    fn routes_methods_types_and_sizes() {
        let j = judge();
        let h = ok_headers();
        for bad in ["/hook/gemini", "/hook/", "/hook/claude/x", "/hook/claude/hold/x"] {
            assert_eq!(j.admit("POST", bad, &h).unwrap_err().status, 404, "{bad}");
        }
        assert_eq!(j.admit("GET", "/hook/claude", &h).unwrap_err().status, 405);
        let mut t = ok_headers();
        t.insert("content-type".into(), "text/plain".into());
        assert_eq!(j.admit("POST", "/hook/claude", &t).unwrap_err().status, 415);
        let mut t = ok_headers();
        t.insert("content-type".into(), "application/json; charset=utf-8".into());
        assert!(j.admit("POST", "/hook/claude", &t).is_ok());
        let mut big = ok_headers();
        big.insert("content-length".into(), (MAX_BODY + 1).to_string());
        assert_eq!(j.admit("POST", "/hook/claude", &big).unwrap_err().status, 413);
    }

    #[test]
    fn the_bucket_allows_a_burst_then_refills() {
        let j = judge();
        let h = ok_headers();
        let t0 = Instant::now();
        let burst = usize::try_from(RATE_BURST as u64).unwrap();
        for _ in 0..burst {
            assert!(j.admit_at("POST", "/hook/claude", &h, t0).is_ok());
        }
        assert_eq!(
            j.admit_at("POST", "/hook/claude", &h, t0).unwrap_err().status,
            429
        );
        let later = t0 + std::time::Duration::from_millis(50);
        assert!(j.admit_at("POST", "/hook/claude", &h, later).is_ok());
    }

    #[test]
    fn constant_time_eq_is_equality() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
        assert!(!constant_time_eq(b"", b"a"));
    }

    #[test]
    fn debug_never_prints_the_token() {
        assert!(!format!("{:?}", judge()).contains(TOKEN));
    }
}
