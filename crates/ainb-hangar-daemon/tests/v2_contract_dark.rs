//! The v2-next contract freeze (PR-0) ships no behaviour.
//!
//! PR-0 declares the federation, terminal and mobile methods, capabilities and
//! switches, and wires none of them. This test pins that, so a daemon built
//! from main after the freeze answers exactly as v1.29.0 does:
//!
//! 1. every new `device/*` and `terminal/*` method answers `METHOD_NOT_FOUND`
//!    (-32601), which a client cannot tell from an older daemon;
//! 2. no new capability reaches the catalogue the hello reply advertises;
//! 3. no new method is in the mutation registry `mutation_dedupe` replays;
//! 4. the phase switches are environment variables, off unless set at boot.
//!
//! Each phase's flip PR edits the assertion it turns on, deliberately.

use std::time::Instant;

use ainb_hangar_daemon::events::EventBroker;
use ainb_hangar_daemon::rpc::{self, DaemonHealth, auth::Caller};
use ainb_hangar_proto::methods as m;
use ainb_hangar_proto::{RpcId, RpcRequest};
use ainb_hangar_store::Store;

const METHOD_NOT_FOUND: i64 = -32601;

const V2_METHODS: [&str; 12] = [
    m::DEVICE_REDEEM,
    m::DEVICE_INVITE_CREATE,
    m::DEVICE_LIST,
    m::DEVICE_REVOKE,
    m::DEVICE_RESCOPE,
    m::TERMINAL_ATTACH,
    m::TERMINAL_DETACH,
    m::TERMINAL_ACK,
    m::TERMINAL_SCROLLBACK,
    m::TERMINAL_INPUT,
    m::TERMINAL_FLOOR,
    m::TERMINAL_RESIZE,
];

fn health() -> DaemonHealth {
    DaemonHealth {
        socket_path: "/tmp/v2-contract-dark.sock".to_string(),
        pid: std::process::id(),
        started_at: Instant::now(),
        version: "0.1.0".into(),
        stats: std::sync::Arc::new(ainb_hangar_daemon::health_stats::HealthStats::default()),
    }
}

/// With both switches unset, every v2-next method is unknown to the daemon,
/// even for the operator, who may call everything that exists.
#[tokio::test]
async fn every_v2_method_is_method_not_found_by_default() {
    assert!(
        std::env::var_os(ainb_hangar_daemon::peer_listener::LISTEN_ENV).is_none()
            && std::env::var_os(ainb_hangar_daemon::term::STREAM_ENV).is_none(),
        "run this test with {} and {} unset: that is the default being proven",
        ainb_hangar_daemon::peer_listener::LISTEN_ENV,
        ainb_hangar_daemon::term::STREAM_ENV
    );
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    let broker = EventBroker::new();
    let events = broker.sink();
    for method in V2_METHODS {
        let request = RpcRequest {
            jsonrpc: ainb_hangar_proto::jsonrpc_version(),
            id: RpcId::Number(1),
            method: method.to_string(),
            params: serde_json::json!({"stream_id": 1, "device_id": "d"}),
        };
        let response = rpc::dispatch_as(
            store.pool(),
            &request,
            &health(),
            &events,
            &Caller::Operator,
        )
        .await;
        let response = serde_json::to_value(response).unwrap();
        assert_eq!(
            response["error"]["code"].as_i64(),
            Some(METHOD_NOT_FOUND),
            "{method} must be unknown until its handler PR: {response}"
        );
        assert!(response.get("result").is_none(), "{method}: {response}");
    }
}

/// The hello reply advertises `catalogue_strings()`, and none of the eight
/// dark capabilities is in it.
#[test]
fn no_v2_capability_is_advertised() {
    let advertised = ainb_hangar_proto::protocol::catalogue_strings();
    for id in ainb_hangar_proto::protocol::DARK_CAPABILITIES {
        assert!(
            !advertised.iter().any(|c| c == id),
            "{id} is advertised before its flip"
        );
    }
}

/// `mutation_dedupe` replays every registry entry as the operator; a v2-next
/// method there would be a replay of a method nothing dispatches.
#[test]
fn no_v2_method_is_in_the_mutation_registry() {
    for method in V2_METHODS {
        assert!(
            !ainb_hangar_proto::mutation::is_mutating(method),
            "{method} is registered before its handler"
        );
    }
}

/// The switches are the two names the owner decided (DV16): boot-time
/// environment variables, never `daemon_config` keys a connected surface could
/// set through `hangar/daemon_config_set`.
#[test]
fn the_phase_switches_are_env_only() {
    assert_eq!(
        ainb_hangar_daemon::peer_listener::LISTEN_ENV,
        "AINB_HANGAR_PEER_LISTEN"
    );
    assert_eq!(ainb_hangar_daemon::term::STREAM_ENV, "AINB_TERMINAL_STREAM");
    for descriptor in ainb_hangar_core::daemon_config::DAEMON_CONFIG_REGISTRY {
        let key = descriptor.key.to_ascii_lowercase();
        assert!(
            !key.contains("peer") && !key.starts_with("terminal"),
            "{} looks like a phase switch in daemon_config",
            descriptor.key
        );
    }
}

/// Every method the daemon's dispatch matches on has a scope verdict: the
/// no-default rule, enforced from the daemon's side. Parsed from source so a
/// handler added without a registry entry fails here, not in production.
///
/// The parse runs over the whole dispatch file as one token stream, so an arm
/// is found wherever it sits: first on its line, on a `| methods::X`
/// continuation line of a multi-line or-pattern, behind an `if` guard, or as
/// a string literal (`"fleet/x" =>`) that never went through `methods.rs`.
#[test]
fn every_dispatched_method_is_classified() {
    let consts = method_consts(include_str!("../../ainb-hangar-proto/src/methods.rs"));
    let arms = dispatch_arms(include_str!("../src/rpc/mod.rs"));
    assert!(
        arms.len() > 100,
        "the dispatch parse found {} arms",
        arms.len()
    );
    let mut unclassified = Vec::new();
    for arm in &arms {
        let value = match arm {
            Arm::Const(name) => {
                let Some((_, value)) = consts.iter().find(|(n, _)| n == name) else {
                    unclassified.push(format!("{name} (no const)"));
                    continue;
                };
                value.clone()
            }
            Arm::Literal(value) => value.clone(),
        };
        if ainb_hangar_proto::devices::scope_row(&value).is_none() {
            unclassified.push(format!("{arm:?} = {value:?}"));
        }
    }
    assert!(
        unclassified.is_empty(),
        "dispatched methods with no scope verdict: {unclassified:?}"
    );
}

/// The arm parser itself: multi-line or-patterns, guards, parenthesised
/// alternatives and string-literal arms are found; comments, strings that
/// merely mention an arm, and comparisons are not.
#[test]
fn the_arm_parser_sees_every_arm_shape() {
    // Two hashes are needed: the source holds `""#` inside a raw string.
    #[allow(clippy::needless_raw_string_hashes)]
    let source = r##"
        match req.method.as_str() {
            methods::SINGLE => a(),
            methods::FIRST
            | methods::MIDDLE
            | methods::LAST => b(),
            methods::GUARDED if ready => c(),
            m @ (methods::PAREN_A | methods::PAREN_B) => d(m),
            "fleet/literal" => e(),
            "fleet/lit_a"
            | "fleet/lit_b" => f(),
            // methods::IN_COMMENT => never,
            /* methods::IN_BLOCK => never */
            _ if req.method == methods::COMPARED || x => g("methods::IN_STRING =>"),
            _ => h(r#"methods::IN_RAW => "quoted""#, 'x', '"', "no/arm"),
        }
    "##;
    let found: Vec<Arm> = dispatch_arms(source).into_iter().collect();
    let want: std::collections::BTreeSet<Arm> = [
        "SINGLE", "FIRST", "MIDDLE", "LAST", "GUARDED", "PAREN_A", "PAREN_B",
    ]
    .into_iter()
    .map(|n| Arm::Const(n.to_string()))
    .chain(
        ["fleet/literal", "fleet/lit_a", "fleet/lit_b"]
            .into_iter()
            .map(|v| Arm::Literal(v.to_string())),
    )
    .collect();
    assert_eq!(found, want.into_iter().collect::<Vec<_>>());
}

/// A method named by a dispatch arm.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Arm {
    /// `methods::NAME`.
    Const(String),
    /// A string literal that looks like a method (`family/name`).
    Literal(String),
}

/// One lexical token of Rust source, as coarse as the arm parse needs.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Tok {
    Method(String),
    Literal(String),
    Arrow,
    Pipe,
    OrOr,
    If,
    CloseParen,
    Other,
}

/// Every method-naming arm pattern in `source`: a `methods::NAME` path or a
/// `family/name` string literal followed by `=>`, `|` or an `if` guard, or
/// closing a parenthesised or-pattern.
fn dispatch_arms(source: &str) -> std::collections::BTreeSet<Arm> {
    let toks = lex(source);
    let mut arms = std::collections::BTreeSet::new();
    for (i, tok) in toks.iter().enumerate() {
        let arm = match tok {
            Tok::Method(name) => Arm::Const(name.clone()),
            Tok::Literal(value) if looks_like_method(value) => Arm::Literal(value.clone()),
            _ => continue,
        };
        let next = toks.get(i + 1);
        let prev = i.checked_sub(1).and_then(|p| toks.get(p));
        let is_arm = matches!(next, Some(Tok::Arrow | Tok::Pipe | Tok::If))
            || (prev == Some(&Tok::Pipe) && next == Some(&Tok::CloseParen));
        if is_arm {
            arms.insert(arm);
        }
    }
    arms
}

fn looks_like_method(value: &str) -> bool {
    let mut parts = value.split('/');
    let ok = |p: &str| !p.is_empty() && p.bytes().all(|b| b.is_ascii_lowercase() || b == b'_');
    matches!((parts.next(), parts.next(), parts.next()), (Some(a), Some(b), None) if ok(a) && ok(b))
}

/// Tokenise Rust source: comments dropped; string, raw string and char
/// literals consumed whole (so text inside them is never a token); a
/// `methods::NAME` path becomes one token.
fn lex(source: &str) -> Vec<Tok> {
    let b = source.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    let ident = |at: usize| {
        let mut end = at;
        while end < b.len() && (b[end].is_ascii_alphanumeric() || b[end] == b'_') {
            end += 1;
        }
        end
    };
    while i < b.len() {
        let c = b[i];
        let rest = &b[i..];
        if c.is_ascii_whitespace() {
            i += 1;
        } else if rest.starts_with(b"//") {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
        } else if rest.starts_with(b"/*") {
            let mut depth = 0usize;
            while i < b.len() {
                if b[i..].starts_with(b"/*") {
                    depth += 1;
                    i += 2;
                } else if b[i..].starts_with(b"*/") {
                    depth -= 1;
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    i += 1;
                }
            }
        } else if c == b'"' {
            let start = i + 1;
            i = start;
            while i < b.len() && b[i] != b'"' {
                i += if b[i] == b'\\' { 2 } else { 1 };
            }
            out.push(Tok::Literal(source[start..i.min(b.len())].to_string()));
            i += 1;
        } else if (c == b'r' || c == b'b')
            && raw_string_open(&b[i..]).is_some()
            && (i == 0 || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_'))
        {
            let (skip, hashes) = raw_string_open(&b[i..]).unwrap_or((1, 0));
            let start = i + skip;
            let mut close = vec![b'"'];
            close.extend(std::iter::repeat_n(b'#', hashes));
            let len = b[start..]
                .windows(close.len())
                .position(|w| w == close.as_slice())
                .unwrap_or(b.len() - start);
            out.push(Tok::Literal(source[start..start + len].to_string()));
            i = start + len + close.len();
        } else if c == b'\'' {
            // A char literal ('x', '\n', '"'), else a lifetime ('a).
            if b.get(i + 1) == Some(&b'\\') {
                i += 3;
                while i < b.len() && b[i] != b'\'' {
                    i += 1;
                }
                i += 1;
            } else if let Some(len) = source[i + 1..].chars().next().map(char::len_utf8) {
                if b.get(i + 1 + len) == Some(&b'\'') {
                    i += len + 2;
                } else {
                    i += 1;
                }
            } else {
                i += 1;
            }
            out.push(Tok::Other);
        } else if c.is_ascii_alphabetic() || c == b'_' {
            let end = ident(i);
            let word = &source[i..end];
            if word == "methods" && b[end..].starts_with(b"::") {
                let name_end = ident(end + 2);
                out.push(Tok::Method(source[end + 2..name_end].to_string()));
                i = name_end;
            } else {
                out.push(if word == "if" { Tok::If } else { Tok::Other });
                i = end;
            }
        } else if rest.starts_with(b"=>") {
            out.push(Tok::Arrow);
            i += 2;
        } else if rest.starts_with(b"||") {
            out.push(Tok::OrOr);
            i += 2;
        } else if c == b'|' {
            out.push(Tok::Pipe);
            i += 1;
        } else if c == b')' {
            out.push(Tok::CloseParen);
            i += 1;
        } else {
            out.push(Tok::Other);
            i += 1;
        }
    }
    out
}

/// `r"`, `r#"`, `br##"` and so on: the bytes up to and including the quote,
/// and the number of hashes.
fn raw_string_open(b: &[u8]) -> Option<(usize, usize)> {
    let mut i = usize::from(b.first() == Some(&b'b'));
    if b.get(i) != Some(&b'r') {
        return None;
    }
    i += 1;
    let hashes = b[i..].iter().take_while(|&&c| c == b'#').count();
    i += hashes;
    (b.get(i) == Some(&b'"')).then_some((i + 1, hashes))
}

/// `pub const NAME: &str = "value"` pairs from `methods.rs` source.
fn method_consts(source: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = source;
    while let Some(at) = rest.find("pub const ") {
        rest = &rest[at + "pub const ".len()..];
        let Some(colon) = rest.find(':') else { break };
        let name = rest[..colon].trim().to_string();
        let value = rest[colon + 1..]
            .trim_start()
            .strip_prefix("&str")
            .and_then(|r| r.trim_start().strip_prefix('='))
            .and_then(|r| r.trim_start().strip_prefix('"'))
            .and_then(|r| r.find('"').map(|end| r[..end].to_string()));
        if let Some(value) = value {
            out.push((name, value));
        }
    }
    out
}
