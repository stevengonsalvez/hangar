//! The persisted connection log: what happened on the wire, for the Log
//! screen and for a bug report.
//!
//! One JSON line per event in an app-data file, kept to the last
//! [`CAPACITY`] lines, surviving an app kill. It holds connect attempts,
//! handshakes, hellos, close codes and backoff delays. It holds no token, no
//! secret, no key and no terminal byte: keys appear as their SHA-256 digest
//! and nothing else is ever written, which [`Entry`]'s closed field set
//! guarantees by construction and `tests/connlog.rs` proves on a real run.

use std::collections::VecDeque;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::records::WireError;

/// Lines kept.
pub const CAPACITY: usize = 1_000;
/// The file under the log dir.
pub const LOG_FILE: &str = "connlog.jsonl";

/// One logged event. Every member is a plain identifier, a count or a
/// digest; there is no member a secret could go in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    /// A dial started.
    Connect {
        /// The endpoint.
        url: String,
        /// The carrier.
        carrier: String,
        /// The host.
        host_id: String,
    },
    /// The dial failed before the handshake.
    ConnectFailed {
        /// The transport's words.
        detail: String,
    },
    /// The Noise handshake completed.
    Handshake {
        /// The SHA-256 of the host static key, hex.
        host_key_digest: String,
        /// WebSocket open time.
        ws_connect_ms: u64,
        /// Handshake time.
        noise_handshake_ms: u64,
    },
    /// The handshake failed.
    HandshakeFailed {
        /// `peer_changed`, or the transport's words.
        detail: String,
    },
    /// `auth/hello` was answered.
    Hello {
        /// The device id.
        device_id: String,
        /// The negotiated protocol.
        protocol: u32,
        /// The scope base, when advertised.
        scope: Option<String>,
    },
    /// `auth/hello` was refused.
    HelloFailed {
        /// The error's words.
        detail: String,
    },
    /// The session closed.
    Close {
        /// The WebSocket close code, when the peer sent one.
        code: Option<u16>,
        /// Why.
        reason: String,
        /// Pings sent over the session.
        pings_sent: u64,
        /// Pongs received.
        pongs_received: u64,
    },
    /// A reconnect was scheduled.
    Backoff {
        /// The attempt, 0-based.
        attempt: u32,
        /// The delay chosen.
        delay_ms: u64,
    },
}

/// One line of the log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// Wall clock, epoch ms.
    pub t_ms: i64,
    /// What happened.
    #[serde(flatten)]
    pub event: Event,
}

/// The log, in memory and on disk.
#[derive(Debug)]
pub struct ConnLog {
    path: PathBuf,
    lines: Mutex<VecDeque<Entry>>,
}

fn io_error(e: impl std::fmt::Display) -> WireError {
    WireError::Custody {
        message: format!("connection log: {e}"),
    }
}

/// Now, epoch ms.
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_millis()).ok())
        .unwrap_or(0)
}

impl ConnLog {
    /// Open the log under `dir`, reading back the last [`CAPACITY`] lines.
    pub fn open(dir: &Path) -> Result<Self, WireError> {
        std::fs::create_dir_all(dir).map_err(io_error)?;
        let path = dir.join(LOG_FILE);
        let mut lines = VecDeque::with_capacity(CAPACITY);
        if let Ok(text) = std::fs::read_to_string(&path) {
            for line in text.lines() {
                // A torn last line from a kill mid-write is dropped, not fatal.
                if let Ok(entry) = serde_json::from_str::<Entry>(line) {
                    if lines.len() == CAPACITY {
                        lines.pop_front();
                    }
                    lines.push_back(entry);
                }
            }
        }
        Ok(Self {
            path,
            lines: Mutex::new(lines),
        })
    }

    /// Append one event.
    pub fn log(&self, event: Event) {
        let entry = Entry {
            t_ms: now_ms(),
            event,
        };
        let Ok(line) = serde_json::to_string(&entry) else {
            return;
        };
        let mut lines = self.lines.lock().unwrap();
        let evicted = lines.len() == CAPACITY;
        if evicted {
            lines.pop_front();
        }
        lines.push_back(entry);
        if evicted {
            // ponytail: rewrite the whole file once the ring is full; events
            // are connects and closes, so this is rare and 1,000 lines small.
            let body: String = lines
                .iter()
                .filter_map(|e| serde_json::to_string(e).ok())
                .map(|l| l + "\n")
                .collect();
            let _ = std::fs::write(&self.path, body);
        } else {
            let _ = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)
                .and_then(|mut f| writeln!(f, "{line}"));
        }
    }

    /// The last `limit` entries, oldest first.
    pub fn tail(&self, limit: usize) -> Vec<Entry> {
        let lines = self.lines.lock().unwrap();
        lines.iter().rev().take(limit).rev().cloned().collect()
    }

    /// Where the file is.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// SHA-256 of `bytes`, lowercase hex.
#[must_use]
pub fn digest(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    use std::fmt::Write as _;
    sha2::Sha256::digest(bytes)
        .iter()
        .fold(String::with_capacity(64), |mut out, b| {
            let _ = write!(out, "{b:02x}");
            out
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_log_survives_reopen_and_keeps_the_last_thousand() {
        let dir = tempfile::tempdir().unwrap();
        let log = ConnLog::open(dir.path()).unwrap();
        for attempt in 0..1_205u32 {
            log.log(Event::Backoff {
                attempt,
                delay_ms: 1,
            });
        }
        assert_eq!(log.tail(usize::MAX).len(), CAPACITY);
        drop(log);
        let reopened = ConnLog::open(dir.path()).unwrap();
        let tail = reopened.tail(usize::MAX);
        assert_eq!(tail.len(), CAPACITY);
        assert_eq!(
            tail.first().map(|e| &e.event),
            Some(&Event::Backoff {
                attempt: 205,
                delay_ms: 1
            })
        );
        assert_eq!(
            tail.last().map(|e| &e.event),
            Some(&Event::Backoff {
                attempt: 1_204,
                delay_ms: 1
            })
        );
        let on_disk = std::fs::read_to_string(reopened.path()).unwrap();
        assert_eq!(on_disk.lines().count(), CAPACITY);
        assert_eq!(reopened.tail(2).len(), 2);
    }

    #[test]
    fn a_torn_last_line_is_dropped_on_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let log = ConnLog::open(dir.path()).unwrap();
        log.log(Event::Close {
            code: Some(4403),
            reason: "revoked".into(),
            pings_sent: 1,
            pongs_received: 1,
        });
        let mut f = std::fs::OpenOptions::new().append(true).open(log.path()).unwrap();
        write!(f, "{{\"t_ms\": 1, \"event\": \"clo").unwrap();
        drop(f);
        let reopened = ConnLog::open(dir.path()).unwrap();
        assert_eq!(reopened.tail(usize::MAX).len(), 1);
    }
}
