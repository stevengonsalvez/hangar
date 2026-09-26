//! The persisted connection log: what happened on the wire, for the Log
//! screen and for a bug report.
//!
//! One JSON line per event in an app-data file, kept to the last
//! [`CAPACITY`] lines, surviving an app kill. It holds connect attempts,
//! handshakes, hellos, close codes and backoff delays. It holds no token, no
//! secret, no key and no terminal byte: keys appear as their SHA-256 digest
//! and nothing else is ever written, which [`Entry`]'s closed field set
//! guarantees by construction and `tests/connlog.rs` proves on a real run.

use std::collections::{HashMap, VecDeque};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
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
    /// A `terminal/ack` was refused or lost; the frames it covered were
    /// still delivered to the app.
    AckFailed {
        /// The stream.
        stream_id: u64,
        /// The bytes the ack carried.
        consumed: u64,
        /// The error's words.
        detail: String,
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
    /// Disk writes, one at a time, apart from the ring's lock.
    io: Mutex<()>,
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

/// The process-wide registry: one [`ConnLog`] per log file, so two callers
/// (a `connect_host`, a `read_connection_log`, a backoff note) share one
/// ring and cannot erase each other's lines.
fn registry() -> &'static Mutex<HashMap<PathBuf, Arc<ConnLog>>> {
    static REGISTRY: OnceLock<Mutex<HashMap<PathBuf, Arc<ConnLog>>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The whole ring as file text.
fn body_of(lines: &VecDeque<Entry>) -> String {
    lines
        .iter()
        .filter_map(|e| serde_json::to_string(e).ok())
        .map(|l| l + "\n")
        .collect()
}

/// Replace the file whole: temp, fsync, rename.
fn write_whole(path: &Path, body: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("jsonl.tmp");
    let mut f = std::fs::File::create(&tmp)?;
    f.write_all(body.as_bytes())?;
    f.sync_all()?;
    std::fs::rename(&tmp, path)
}

impl ConnLog {
    /// The log under `dir`: the one instance this process holds for that
    /// path, created on first use by reading back the last [`CAPACITY`]
    /// lines.
    pub fn open(dir: &Path) -> Result<Arc<Self>, WireError> {
        std::fs::create_dir_all(dir).map_err(io_error)?;
        let key = dir.canonicalize().map_err(io_error)?;
        let mut registry = registry().lock().unwrap();
        if let Some(log) = registry.get(&key) {
            return Ok(Arc::clone(log));
        }
        let log = Arc::new(Self::load(&key)?);
        registry.insert(key, Arc::clone(&log));
        Ok(log)
    }

    /// Read the log under `dir` from disk into a fresh instance, as a new
    /// process would. Tests only: a second instance on one path is the bug
    /// [`Self::open`] exists to prevent.
    #[doc(hidden)]
    pub fn load(dir: &Path) -> Result<Self, WireError> {
        let path = dir.join(LOG_FILE);
        let mut lines = VecDeque::with_capacity(CAPACITY);
        let mut torn = false;
        // Bytes, not a String: one torn multibyte character from a kill
        // mid-write must not hide the 999 good lines before it.
        if let Ok(bytes) = std::fs::read(&path) {
            let text = String::from_utf8_lossy(&bytes);
            torn = !text.is_empty() && !text.ends_with('\n');
            for line in text.lines() {
                // A torn line from a kill mid-write is dropped, not fatal.
                match serde_json::from_str::<Entry>(line) {
                    Ok(entry) => {
                        if lines.len() == CAPACITY {
                            lines.pop_front();
                        }
                        lines.push_back(entry);
                    }
                    Err(_) => torn = true,
                }
            }
        }
        let log = Self {
            path,
            lines: Mutex::new(lines),
            io: Mutex::new(()),
        };
        if torn {
            // Repair the file from what parsed, so the next append is not
            // glued onto the fragment and lost with it.
            let body = body_of(&log.lines.lock().unwrap());
            let _ = write_whole(&log.path, &body);
        }
        Ok(log)
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
        // The ring is updated under its own lock and released before the
        // disk write, so `tail()` and the next `log()` never wait on a slow
        // flash; the writes themselves queue on the io lock, one at a time,
        // so two loggers never interleave a line or race a rewrite.
        let body = {
            let mut lines = self.lines.lock().unwrap();
            let evicted = lines.len() == CAPACITY;
            if evicted {
                lines.pop_front();
            }
            lines.push_back(entry);
            evicted.then(|| body_of(&lines))
        };
        let _io = self.io.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        match body {
            // ponytail: rewrite the whole file once the ring is full; events
            // are connects and closes, so this is rare and 1,000 lines small.
            // Written whole through a rename, so a kill mid-rewrite keeps
            // the old file rather than a truncated one.
            Some(body) => {
                let _ = write_whole(&self.path, &body);
            }
            None => {
                let _ = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&self.path)
                    .and_then(|mut f| f.write_all(format!("{line}\n").as_bytes()));
            }
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
        let reopened = ConnLog::load(dir.path()).unwrap();
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
    fn one_instance_per_path_and_two_writers_keep_every_line() {
        let dir = tempfile::tempdir().unwrap();
        let a = ConnLog::open(dir.path()).unwrap();
        let b = ConnLog::open(dir.path()).unwrap();
        assert!(Arc::ptr_eq(&a, &b), "the same path is the same instance");
        let other = tempfile::tempdir().unwrap();
        assert!(!Arc::ptr_eq(&a, &ConnLog::open(other.path()).unwrap()));
        let writers: Vec<_> = [Arc::clone(&a), Arc::clone(&b)]
            .into_iter()
            .enumerate()
            .map(|(w, log)| {
                std::thread::spawn(move || {
                    for i in 0..300u32 {
                        log.log(Event::Backoff {
                            attempt: u32::try_from(w).unwrap() * 1_000 + i,
                            delay_ms: 1,
                        });
                    }
                })
            })
            .collect();
        for w in writers {
            w.join().unwrap();
        }
        assert_eq!(a.tail(usize::MAX).len(), 600);
        let on_disk = std::fs::read_to_string(a.path()).unwrap();
        assert_eq!(on_disk.lines().count(), 600);
        assert_eq!(
            ConnLog::load(dir.path()).unwrap().tail(usize::MAX).len(),
            600
        );
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
        let reopened = ConnLog::load(dir.path()).unwrap();
        assert_eq!(reopened.tail(usize::MAX).len(), 1);
    }
}
