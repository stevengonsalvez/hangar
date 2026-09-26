//! Agent hook ingest over loopback HTTP (hooks-and-answers).
//!
//! Dark behind [`crate::protocol::CAP_HOOKS_HTTP`]. The daemon binds the
//! listener only when `AINB_HANGAR_HOOK_LISTEN` is set at boot; a hook script
//! finds it through two files under `<hangar_home>/hangar/`:
//!
//! - [`ENDPOINT_FILE_NAME`]: port, protocol version, daemon pid and the path of
//!   the headers file. No secret. The script PARSES it line by line with
//!   [`HookEndpoint::parse_env_file`]'s rules and never sources it, so a
//!   corrupted file cannot become shell code.
//! - [`HEADERS_FILE_NAME`]: the one line `X-Ainb-Hook-Token: <token>`, mode
//!   0600. The script hands it to curl as `-H @<file>`, so the token never
//!   appears in any process argv (on macOS `ps` shows every user's argv).
//!
//! Both are re-read on every hook call, so a pane that outlives a daemon
//! restart reaches the new port and the new token.

use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Name of the endpoint file under `<hangar_home>/hangar/`.
pub const ENDPOINT_FILE_NAME: &str = "hook-endpoint.env";
/// Name of the 0600 headers file (the token) under `<hangar_home>/hangar/`.
pub const HEADERS_FILE_NAME: &str = "hook-headers";
/// Name of the spool directory under `<hangar_home>/hangar/`.
pub const SPOOL_DIR_NAME: &str = "hook-spool";

/// The endpoint protocol version the script and the daemon agree on.
pub const HOOK_ENDPOINT_VERSION: u32 = 1;

/// Header carrying the per-start token.
pub const TOKEN_HEADER: &str = "X-Ainb-Hook-Token";
/// Header carrying `$AINB_PANE_KEY`.
pub const PANE_KEY_HEADER: &str = "X-Ainb-Pane-Key";
/// Header carrying `$TMUX_PANE` (the `%N` pane id).
pub const TMUX_PANE_HEADER: &str = "X-Ainb-Tmux-Pane";
/// Header carrying `$AINB_PARENT_SESSION`.
pub const PARENT_HEADER: &str = "X-Ainb-Parent";

/// The 15 managed Claude Code events: Orca's 13 plus `Notification` and
/// `Elicitation` (both status-only).
pub const CLAUDE_HOOK_EVENTS: [&str; 15] = [
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "PostToolUseFailure",
    "PermissionRequest",
    "Notification",
    "Elicitation",
    "Stop",
    "StopFailure",
    "SubagentStart",
    "SubagentStop",
    "TeammateIdle",
    "PostCompact",
    "SessionEnd",
];

/// The 8 managed Codex events (Orca's set). All non-blocking.
pub const CODEX_HOOK_EVENTS: [&str; 8] = [
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PermissionRequest",
    "PostToolUse",
    "SubagentStart",
    "SubagentStop",
    "Stop",
];

/// Which agent CLI fired a hook. The route is `/hook/<source>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookSource {
    /// Claude Code.
    Claude,
    /// OpenAI Codex CLI.
    Codex,
    /// GitHub Copilot CLI.
    Copilot,
    /// Antigravity CLI.
    Antigravity,
    /// A source this build does not know. Decodes, never routes.
    #[serde(other)]
    Unknown,
}

impl HookSource {
    /// The route segment and wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Copilot => "copilot",
            Self::Antigravity => "antigravity",
            Self::Unknown => "unknown",
        }
    }

    /// Parse a route segment. `None` for anything this build does not route,
    /// including the literal `unknown`.
    #[must_use]
    pub fn from_route(segment: &str) -> Option<Self> {
        match segment {
            "claude" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            "copilot" => Some(Self::Copilot),
            "antigravity" => Some(Self::Antigravity),
            _ => None,
        }
    }
}

/// Whether a hook call must wait for a human (a hold) rather than return at
/// once. Only Claude's `PermissionRequest` and its `PreToolUse` on the
/// `AskUserQuestion` tool hold; everything else is status.
#[must_use]
pub fn is_blocking(source: HookSource, event: &str, tool_name: Option<&str>) -> bool {
    matches!(source, HookSource::Claude)
        && (event == "PermissionRequest"
            || (event == "PreToolUse" && tool_name == Some("AskUserQuestion")))
}

/// A pane's identity, injected at spawn as `$AINB_PANE_KEY` and carried on
/// every hook call.
///
/// `v1:<ainb session id>`: one pane per session today, so the session id is
/// the identity. The `%N` pane id is the ADDRESS and travels separately
/// ([`TMUX_PANE_HEADER`]).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct PaneKey(String);

/// Longest pane key accepted.
pub const PANE_KEY_MAX_LEN: usize = 128;

impl PaneKey {
    /// The v1 key for an ainb session id.
    ///
    /// # Errors
    /// [`PaneKeyError`] when the id has characters outside `[A-Za-z0-9_-]` or
    /// the key would be too long.
    pub fn v1(session_id: &str) -> Result<Self, PaneKeyError> {
        Self::parse(&format!("v1:{session_id}"))
    }

    /// Parse a key from the wire or the environment.
    ///
    /// # Errors
    /// [`PaneKeyError`] unless the key is `v1:` followed by one or more
    /// `[A-Za-z0-9_-]` characters, at most [`PANE_KEY_MAX_LEN`] bytes in all.
    pub fn parse(raw: &str) -> Result<Self, PaneKeyError> {
        let Some(id) = raw.strip_prefix("v1:") else {
            return Err(PaneKeyError);
        };
        let ok = !id.is_empty()
            && raw.len() <= PANE_KEY_MAX_LEN
            && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
        if ok {
            Ok(Self(raw.to_owned()))
        } else {
            Err(PaneKeyError)
        }
    }

    /// The ainb session id the key names.
    #[must_use]
    pub fn session_id(&self) -> &str {
        &self.0[3..]
    }

    /// The key as sent.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PaneKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for PaneKey {
    type Error = PaneKeyError;
    fn try_from(raw: String) -> Result<Self, Self::Error> {
        Self::parse(&raw)
    }
}

impl From<PaneKey> for String {
    fn from(key: PaneKey) -> Self {
        key.0
    }
}

/// A pane key that is not `v1:[A-Za-z0-9_-]+`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaneKeyError;

impl fmt::Display for PaneKeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("pane key must be v1:[A-Za-z0-9_-]+")
    }
}

impl std::error::Error for PaneKeyError {}

/// Map any string to a spool file stem: `[A-Za-z0-9:_-]` kept, everything
/// else `_`, capped at 96 bytes, never empty.
#[must_use]
pub fn spool_stem(raw: &str) -> String {
    let mut out: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, ':' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .take(96)
        .collect();
    if out.is_empty() {
        out.push('_');
    }
    out
}

/// The contents of [`ENDPOINT_FILE_NAME`].
#[derive(Clone, PartialEq, Eq)]
pub struct HookEndpoint {
    /// The loopback port the listener bound.
    pub port: u16,
    /// [`HOOK_ENDPOINT_VERSION`] at write time.
    pub version: u32,
    /// The daemon pid, for diagnostics only.
    pub pid: u32,
    /// Absolute path of [`HEADERS_FILE_NAME`].
    pub headers_path: PathBuf,
}

impl fmt::Debug for HookEndpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HookEndpoint")
            .field("port", &self.port)
            .field("version", &self.version)
            .field("pid", &self.pid)
            .field("headers_path", &self.headers_path)
            .finish()
    }
}

/// Why an endpoint file was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EndpointParseError {
    /// A line was not `KEY=value`.
    Malformed,
    /// A key outside the allowlist.
    UnknownKey(String),
    /// A value outside its key's character set.
    BadValue(&'static str),
    /// A required key was missing.
    Missing(&'static str),
}

impl fmt::Display for EndpointParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed => f.write_str("endpoint line is not KEY=value"),
            Self::UnknownKey(k) => write!(f, "unknown endpoint key {k:?}"),
            Self::BadValue(k) => write!(f, "bad value for {k}"),
            Self::Missing(k) => write!(f, "missing {k}"),
        }
    }
}

impl std::error::Error for EndpointParseError {}

impl HookEndpoint {
    /// Render the file. Keys in a fixed order, one per line.
    #[must_use]
    pub fn render_env_file(&self) -> String {
        format!(
            "AINB_HOOK_PORT={}\nAINB_HOOK_VERSION={}\nAINB_HOOK_PID={}\nAINB_HOOK_HEADERS={}\n",
            self.port,
            self.version,
            self.pid,
            self.headers_path.display()
        )
    }

    /// Parse the file with the same rules the hook script applies: only the
    /// four allowlisted keys, digits for the numbers, and an absolute headers
    /// path made of `[A-Za-z0-9/._-]` whose file name is [`HEADERS_FILE_NAME`].
    ///
    /// # Errors
    /// [`EndpointParseError`] on any other shape.
    pub fn parse_env_file(text: &str) -> Result<Self, EndpointParseError> {
        let mut port = None;
        let mut version = None;
        let mut pid = None;
        let mut headers = None;
        for line in text.lines() {
            if line.is_empty() {
                continue;
            }
            let (key, value) = line.split_once('=').ok_or(EndpointParseError::Malformed)?;
            match key {
                "AINB_HOOK_PORT" => {
                    port = Some(
                        digits(value)
                            .and_then(|v| v.parse::<u16>().ok())
                            .filter(|p| *p != 0)
                            .ok_or(EndpointParseError::BadValue("AINB_HOOK_PORT"))?,
                    );
                }
                "AINB_HOOK_VERSION" => {
                    version = Some(
                        digits(value)
                            .and_then(|v| v.parse::<u32>().ok())
                            .ok_or(EndpointParseError::BadValue("AINB_HOOK_VERSION"))?,
                    );
                }
                "AINB_HOOK_PID" => {
                    pid = Some(
                        digits(value)
                            .and_then(|v| v.parse::<u32>().ok())
                            .ok_or(EndpointParseError::BadValue("AINB_HOOK_PID"))?,
                    );
                }
                "AINB_HOOK_HEADERS" => {
                    headers = Some(
                        safe_headers_path(value)
                            .ok_or(EndpointParseError::BadValue("AINB_HOOK_HEADERS"))?,
                    );
                }
                other => return Err(EndpointParseError::UnknownKey(other.to_owned())),
            }
        }
        Ok(Self {
            port: port.ok_or(EndpointParseError::Missing("AINB_HOOK_PORT"))?,
            version: version.ok_or(EndpointParseError::Missing("AINB_HOOK_VERSION"))?,
            pid: pid.ok_or(EndpointParseError::Missing("AINB_HOOK_PID"))?,
            headers_path: headers.ok_or(EndpointParseError::Missing("AINB_HOOK_HEADERS"))?,
        })
    }
}

fn digits(value: &str) -> Option<&str> {
    (!value.is_empty() && value.len() <= 10 && value.bytes().all(|b| b.is_ascii_digit()))
        .then_some(value)
}

fn safe_headers_path(value: &str) -> Option<PathBuf> {
    let path = Path::new(value);
    let charset_ok = !value.is_empty()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'.' | b'_' | b'-'));
    let named_ok = path.file_name().is_some_and(|n| n == HEADERS_FILE_NAME);
    let no_parent = !path.components().any(|c| matches!(c, std::path::Component::ParentDir));
    (charset_ok && path.is_absolute() && named_ok && no_parent).then(|| path.to_path_buf())
}

/// Render [`HEADERS_FILE_NAME`]: the token header line, nothing else.
#[must_use]
pub fn render_headers_file(token: &str) -> String {
    format!("{TOKEN_HEADER}: {token}\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoint() -> HookEndpoint {
        HookEndpoint {
            port: 41234,
            version: HOOK_ENDPOINT_VERSION,
            pid: 77,
            headers_path: PathBuf::from("/home/u/.agents-in-a-box/hangar/hook-headers"),
        }
    }

    #[test]
    fn endpoint_file_round_trips() {
        let e = endpoint();
        assert_eq!(HookEndpoint::parse_env_file(&e.render_env_file()), Ok(e));
    }

    #[test]
    fn endpoint_file_never_holds_a_token() {
        let text = endpoint().render_env_file();
        assert!(!text.contains("TOKEN"), "{text}");
    }

    #[test]
    fn shell_in_a_value_is_refused() {
        for bad in [
            "AINB_HOOK_PORT=1; touch pwned",
            "AINB_HOOK_PORT=$(touch pwned)",
            "AINB_HOOK_PORT=`id`",
            "AINB_HOOK_HEADERS=/tmp/x/hook-headers;id",
            "AINB_HOOK_HEADERS=/tmp/$(id)/hook-headers",
        ] {
            assert!(
                HookEndpoint::parse_env_file(bad).is_err(),
                "{bad} should be refused"
            );
        }
    }

    #[test]
    fn unknown_keys_are_refused() {
        let text = format!("{}PATH=/evil\n", endpoint().render_env_file());
        assert_eq!(
            HookEndpoint::parse_env_file(&text),
            Err(EndpointParseError::UnknownKey("PATH".into()))
        );
    }

    #[test]
    fn headers_path_must_be_absolute_and_named() {
        for bad in [
            "relative/hook-headers",
            "/tmp/other-file",
            "/tmp/../etc/hook-headers",
        ] {
            let text = format!(
                "AINB_HOOK_PORT=1\nAINB_HOOK_VERSION=1\nAINB_HOOK_PID=1\nAINB_HOOK_HEADERS={bad}\n"
            );
            assert!(HookEndpoint::parse_env_file(&text).is_err(), "{bad}");
        }
    }

    #[test]
    fn port_zero_and_missing_keys_are_refused() {
        assert!(HookEndpoint::parse_env_file("AINB_HOOK_PORT=0\n").is_err());
        assert_eq!(
            HookEndpoint::parse_env_file("AINB_HOOK_PORT=5\n"),
            Err(EndpointParseError::Missing("AINB_HOOK_VERSION"))
        );
    }

    #[test]
    fn headers_file_is_one_token_line() {
        assert_eq!(render_headers_file("abc"), "X-Ainb-Hook-Token: abc\n");
    }

    #[test]
    fn pane_key_v1_round_trips_and_names_the_session() {
        let key = PaneKey::v1("0b7e-4c1d_x").unwrap();
        assert_eq!(key.as_str(), "v1:0b7e-4c1d_x");
        assert_eq!(key.session_id(), "0b7e-4c1d_x");
        let json = serde_json::to_string(&key).unwrap();
        assert_eq!(json, "\"v1:0b7e-4c1d_x\"");
        assert_eq!(serde_json::from_str::<PaneKey>(&json).unwrap(), key);
    }

    #[test]
    fn pane_key_refuses_paths_and_other_versions() {
        for bad in ["", "v1:", "v2:abc", "abc", "v1:../x", "v1:a b", "v1:a/b"] {
            assert!(PaneKey::parse(bad).is_err(), "{bad:?}");
        }
        assert!(PaneKey::parse(&format!("v1:{}", "a".repeat(200))).is_err());
        assert!(serde_json::from_str::<PaneKey>("\"v1:../x\"").is_err());
    }

    #[test]
    fn spool_stem_keeps_only_safe_characters() {
        assert_eq!(spool_stem("v1:abc-1_2"), "v1:abc-1_2");
        assert_eq!(spool_stem("../../etc/passwd"), "______etc_passwd");
        assert_eq!(spool_stem("a b\nc"), "a_b_c");
        assert_eq!(spool_stem(""), "_");
        assert_eq!(spool_stem(&"x".repeat(300)).len(), 96);
    }

    #[test]
    fn claude_set_is_orcas_thirteen_plus_notification_and_elicitation() {
        let set: std::collections::HashSet<_> = CLAUDE_HOOK_EVENTS.into_iter().collect();
        assert_eq!(set.len(), 15, "no duplicates");
        assert!(set.contains("Notification"));
        assert!(set.contains("Elicitation"));
        for dropped in [
            "PreCompact",
            "PermissionDenied",
            "ElicitationResult",
            "Setup",
        ] {
            assert!(!set.contains(dropped), "{dropped}");
        }
    }

    #[test]
    fn only_claude_permission_and_ask_hold() {
        assert!(is_blocking(HookSource::Claude, "PermissionRequest", None));
        assert!(is_blocking(
            HookSource::Claude,
            "PreToolUse",
            Some("AskUserQuestion")
        ));
        assert!(!is_blocking(HookSource::Claude, "PreToolUse", Some("Bash")));
        assert!(!is_blocking(HookSource::Claude, "Notification", None));
        assert!(!is_blocking(HookSource::Claude, "Elicitation", None));
        assert!(!is_blocking(HookSource::Codex, "PermissionRequest", None));
    }

    #[test]
    fn sources_route_by_segment() {
        for s in [
            HookSource::Claude,
            HookSource::Codex,
            HookSource::Copilot,
            HookSource::Antigravity,
        ] {
            assert_eq!(HookSource::from_route(s.as_str()), Some(s));
        }
        assert_eq!(HookSource::from_route("unknown"), None);
        assert_eq!(HookSource::from_route("gemini"), None);
        assert_eq!(
            serde_json::from_str::<HookSource>("\"gemini\"").unwrap(),
            HookSource::Unknown
        );
    }
}
