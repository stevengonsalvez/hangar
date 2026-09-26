//! Hangar wire protocol: pure JSON-RPC 2.0 envelope types.
//!
//! This crate defines the request / response / error envelopes the Hangar
//! daemon speaks over stdio and the plugin SDK consumes. It is **pure data** —
//! `serde` + `serde_json` and nothing else (no `tokio`, `sqlx`, `ratatui`, or
//! any host crate), per the plugin-SDK no-host-deps discipline. P3 builds the
//! JSON-RPC dispatcher on top of these types.
//!
//! The shapes follow [JSON-RPC 2.0](https://www.jsonrpc.org/specification):
//! every envelope carries `"jsonrpc":"2.0"`; a request adds `{id, method,
//! params}`, a response adds `{id, result|error}`, and an error object is
//! `{code, message, data?}`.

use serde::{Deserialize, Serialize};

/// One status truth for every surface: the shared derivation of an agent's
/// state, its evidence tier, its provenance and its evidence clock (D14).
pub mod agent_status;
pub mod auth;
pub mod connections;
pub mod dates;
/// Paired devices, scopes and the per-scope method table (R1, frozen by PR-0).
pub mod devices;
pub mod events;
pub mod fleet;
/// Host identity and reachability (spec D11, frozen by PR-0).
pub mod hosts;
pub mod lifecycle;
pub mod methods;
pub mod mutation;
/// Close codes of the off-box peer leg (R1, frozen by PR-0).
pub mod peer_close;
pub mod pr_status;
pub mod protocol;
pub mod reprime;
/// A session named across hosts (spec D11, frozen by PR-0).
pub mod session_ref;
pub mod sessions;
pub mod settings;
pub mod snapshots;
pub mod status_topic;
pub mod status_view;
/// Terminal streams (R2, frozen by PR-0).
pub mod terminal;
pub mod transcript;

/// Re-export the notification routing vocabulary (tcp T5) so proto-only consumers
/// (ainb-web, the hangar plugin) reach [`Channel`] / [`ChannelSet`] without a
/// direct `ainb-hangar-core` dependency — the same crate the wire `AttentionRow`
/// and `AttentionRaised` event carry their resolved channels as.
pub use ainb_hangar_core::channel::{Channel, ChannelSet};

/// Stands in for a secret in a `Debug` rendering: prints `<redacted>`.
///
/// Wire types that carry a credential (a daemon or device token, an invite
/// secret, a pairing URI) implement `Debug` by hand and print this in the
/// secret's place, so a `{:?}` in a log line, a panic message or an
/// `assert_eq!` failure never leaks the value. The hand-written impls
/// destructure their struct, so a field added later fails to compile until
/// someone decides whether it is secret.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Redacted;

impl std::fmt::Debug for Redacted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The one shared marker, so a test or a log grep for it matches every
        // redaction in the workspace.
        f.write_str(ainb_hangar_core::redact::REDACTED)
    }
}

/// The JSON-RPC protocol version string carried by every envelope.
pub const JSONRPC_VERSION: &str = "2.0";

/// Application-defined error code for "the daemon's store could not be
/// reached": the request was well-formed and would have succeeded, but `SQLite`
/// reported lock contention, so nothing was read or written.
///
/// Lives here rather than in the daemon because a CLIENT branches on it. It is
/// deliberately NOT the spec's `-32603` internal error: that code is the
/// daemon's catch-all and also carries faults a caller must surface loudly, so
/// a caller willing to degrade over a busy store needs a code that means only
/// that. Callers must match the code, never the `SQLite` message text, which
/// varies by platform and extended result code.
pub const STORE_UNAVAILABLE: i32 = -32006;

/// The default `jsonrpc` member value (`"2.0"`) for [`RpcRequest`] /
/// [`RpcResponse`].
///
/// Used as the serde `default` so a peer that omits the member still decodes
/// into a spec-compliant 2.0 envelope, and as the canonical value when
/// constructing envelopes in code.
pub fn jsonrpc_version() -> String {
    JSONRPC_VERSION.to_string()
}

/// A JSON-RPC request id.
///
/// JSON-RPC 2.0 permits a string, a number, or null. We model the first two
/// explicitly and treat the `null`/absent notification id case as out of scope
/// for v1 (the Hangar transport always correlates requests with responses).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RpcId {
    /// A numeric id.
    Number(i64),
    /// A string id.
    Text(String),
}

/// A JSON-RPC 2.0 request envelope.
///
/// `Debug` redacts credentials: the whole `params` of
/// [`SECRET_PARAMS_METHODS`], and any [`SECRET_KEYS`] member elsewhere.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RpcRequest {
    /// The JSON-RPC protocol version; always `"2.0"`. Defaults to `"2.0"`
    /// when a lenient peer omits it on decode.
    #[serde(default = "jsonrpc_version")]
    pub jsonrpc: String,
    /// Correlates the request with its [`RpcResponse`].
    pub id: RpcId,
    /// The method name (e.g. `"hangar/ping"`).
    pub method: String,
    /// Method parameters; [`serde_json::Value::Null`] when there are none.
    pub params: serde_json::Value,
}

/// A JSON-RPC 2.0 response envelope.
///
/// Exactly one of `result` / `error` is populated, matching the JSON-RPC
/// contract; both are `Option` and skipped when absent so the serialized form
/// carries only the relevant half.
///
/// `Debug` redacts any [`SECRET_KEYS`] member of the result: a response does
/// not name its method, so the redaction goes by key.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RpcResponse {
    /// The JSON-RPC protocol version; always `"2.0"`. Defaults to `"2.0"`
    /// when a lenient peer omits it on decode.
    #[serde(default = "jsonrpc_version")]
    pub jsonrpc: String,
    /// Echoes the originating [`RpcRequest::id`].
    pub id: RpcId,
    /// The successful result payload, when the call succeeded.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub result: Option<serde_json::Value>,
    /// The error envelope, when the call failed.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub error: Option<RpcError>,
}

/// A JSON-RPC 2.0 error object: `{code, message, data?}`.
///
/// `Debug` redacts any [`SECRET_KEYS`] member of `data`.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RpcError {
    /// The numeric error code (JSON-RPC reserves `-32768..=-32000`).
    pub code: i32,
    /// A short, human-readable description of the error.
    pub message: String,
    /// Optional structured detail.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub data: Option<serde_json::Value>,
}

/// Object keys whose value is a credential, at any depth of a JSON body.
///
/// Covers `params`, `result` and error `data`: the daemon or device token, the
/// plaintext device token a redeem returns, the invite secret, and the pairing
/// URI that carries it.
///
/// `agent_env` (per-agent environment values) and `mcp_config` (a raw MCP
/// config that often embeds tokens) are redacted whole: their values are
/// credentials under names no list can know.
pub(crate) const SECRET_KEYS: &[&str] = &[
    "token",
    "device_token",
    "invite_secret",
    "offer",
    "agent_env",
    "mcp_config",
];

/// Methods whose whole `params` object is a credential exchange, so `Debug`
/// prints none of it.
pub(crate) const SECRET_PARAMS_METHODS: &[&str] = &[methods::AUTH_HELLO, methods::DEVICE_REDEEM];

/// A JSON value rendered for `Debug` with every [`SECRET_KEYS`] member, at any
/// depth, replaced by [`Redacted`], and every other string run through the
/// shared credential-shape scrubber (`ainb_hangar_core::redact::scrub`), so a
/// key, bearer or URL password under a name the list does not know is still
/// redacted.
struct RedactedValue<'a>(&'a serde_json::Value);

impl std::fmt::Debug for RedactedValue<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            serde_json::Value::Object(map) => {
                let mut out = f.debug_map();
                for (key, value) in map {
                    if SECRET_KEYS.contains(&key.as_str()) {
                        out.entry(key, &Redacted);
                    } else {
                        out.entry(key, &Self(value));
                    }
                }
                out.finish()
            }
            serde_json::Value::Array(items) => {
                f.debug_list().entries(items.iter().map(Self)).finish()
            }
            serde_json::Value::String(text) => {
                std::fmt::Debug::fmt(&ainb_hangar_core::redact::scrub(text), f)
            }
            scalar => std::fmt::Debug::fmt(scalar, f),
        }
    }
}

impl std::fmt::Debug for RpcRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self {
            jsonrpc,
            id,
            method,
            params,
        } = self;
        let mut out = f.debug_struct("RpcRequest");
        out.field("jsonrpc", jsonrpc).field("id", id).field("method", method);
        if SECRET_PARAMS_METHODS.contains(&method.as_str()) {
            out.field("params", &Redacted);
        } else {
            out.field("params", &RedactedValue(params));
        }
        out.finish()
    }
}

impl std::fmt::Debug for RpcResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self {
            jsonrpc,
            id,
            result,
            error,
        } = self;
        f.debug_struct("RpcResponse")
            .field("jsonrpc", jsonrpc)
            .field("id", id)
            .field("result", &result.as_ref().map(RedactedValue))
            .field("error", error)
            .finish()
    }
}

impl std::fmt::Debug for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self {
            code,
            message,
            data,
        } = self;
        f.debug_struct("RpcError")
            .field("code", code)
            // A message can echo the rejected input (a serde type error, a
            // handler that interpolates a param), so it gets the same
            // credential-shape scrub as a string value.
            .field("message", &ainb_hangar_core::redact::scrub(message))
            .field("data", &data.as_ref().map(RedactedValue))
            .finish()
    }
}

#[cfg(test)]
mod debug_redaction_tests {
    use super::*;

    const TOKEN: &str = "mdt_s3cr3tDaemonTokenValue";

    /// The frame every client sends first: nothing of it prints but the
    /// envelope.
    #[test]
    fn hello_request_debug_hides_the_token() {
        let request = auth::hello_request(7, TOKEN);
        for rendered in [format!("{request:?}"), format!("{request:#?}")] {
            assert!(!rendered.contains(TOKEN), "{rendered}");
            assert!(!rendered.contains("mdt_"), "{rendered}");
            assert!(rendered.contains("<redacted>"), "{rendered}");
            assert!(rendered.contains(methods::AUTH_HELLO), "{rendered}");
        }
    }

    #[test]
    fn redeem_params_print_nothing() {
        let request = RpcRequest {
            jsonrpc: jsonrpc_version(),
            id: RpcId::Number(1),
            method: methods::DEVICE_REDEEM.to_string(),
            params: serde_json::json!({"invite_id": "i", "invite_secret": "s3cr3tInvite"}),
        };
        assert!(!format!("{request:?}").contains("s3cr3tInvite"));
    }

    /// A response does not name its method, so the secret keys are redacted at
    /// any depth, and everything else still prints.
    #[test]
    fn response_and_error_debug_hide_secret_keys() {
        let response = RpcResponse {
            jsonrpc: jsonrpc_version(),
            id: RpcId::Number(1),
            result: Some(serde_json::json!({
                "device_id": "dev-visible",
                "device_token": "mdd_s3cr3tDevice",
                "nested": [{"offer": "ainb://pair#s3cr3tOffer", "token": TOKEN}],
            })),
            error: Some(RpcError {
                code: -1,
                message: "m".to_string(),
                data: Some(serde_json::json!({"invite_secret": "s3cr3tInvite"})),
            }),
        };
        for rendered in [format!("{response:?}"), format!("{response:#?}")] {
            for secret in ["mdd_s3cr3tDevice", "s3cr3tOffer", TOKEN, "s3cr3tInvite"] {
                assert!(!rendered.contains(secret), "{secret} in {rendered}");
            }
            assert!(rendered.contains("dev-visible"), "{rendered}");
        }
    }

    /// A secret key nested inside an ordinary method's params, in an object
    /// or inside an array of objects, is redacted too.
    #[test]
    fn nested_secret_keys_in_params_are_redacted() {
        let request = RpcRequest {
            jsonrpc: jsonrpc_version(),
            id: RpcId::Number(3),
            method: methods::PING.to_string(),
            params: serde_json::json!({
                "workspace_id": "ws-visible",
                "auth": {"token": TOKEN, "scope": "scope-visible"},
                "devices": [{"device_token": "mdd_s3cr3tNested"}],
                "pairing": {"deeper": {"invite_secret": "s3cr3tInvite", "offer": "ainb://pair#s3cr3tOffer"}},
            }),
        };
        for rendered in [format!("{request:?}"), format!("{request:#?}")] {
            for secret in [TOKEN, "mdd_s3cr3tNested", "s3cr3tInvite", "s3cr3tOffer"] {
                assert!(!rendered.contains(secret), "{secret} in {rendered}");
            }
            assert!(rendered.contains("ws-visible"), "{rendered}");
            assert!(rendered.contains("scope-visible"), "{rendered}");
        }
    }

    /// Per-agent env values and raw MCP configs are redacted whole, and a
    /// credential-shaped string under a name no list knows is scrubbed.
    #[test]
    fn env_mcp_and_credential_shaped_strings_are_redacted() {
        const KEY: &str = "sk-ant-api03-s3cr3tValueLongEnoughForTheShape";
        let request = RpcRequest {
            jsonrpc: jsonrpc_version(),
            id: RpcId::Number(4),
            method: methods::HANGAR_AGENT_UPDATE.to_string(),
            params: serde_json::json!({
                "agent_id": "agent-visible",
                "agent_env": [["OPENAI_API_KEY", "envS3cr3tValue"]],
                "mcp_config": "{\"env\":{\"TOKEN\":\"mcpS3cr3t\"}}",
                "notes": format!("use {KEY} for now"),
            }),
        };
        for rendered in [format!("{request:?}"), format!("{request:#?}")] {
            for secret in ["envS3cr3tValue", "mcpS3cr3t", KEY] {
                assert!(!rendered.contains(secret), "{secret} in {rendered}");
            }
            assert!(rendered.contains("agent-visible"), "{rendered}");
            assert!(rendered.contains("use <redacted> for now"), "{rendered}");
        }
    }

    /// An error message that echoes a credential is scrubbed like a value.
    #[test]
    fn error_message_credentials_are_scrubbed() {
        let error = RpcError {
            code: -32602,
            message: "invalid type: string \"sk-ant-api03-s3cr3tEchoedValueLongEnough\""
                .to_string(),
            data: None,
        };
        let rendered = format!("{error:?}");
        assert!(!rendered.contains("s3cr3tEchoed"), "{rendered}");
        assert!(rendered.contains("invalid type: string"), "{rendered}");
    }

    /// Other methods keep their params visible, secret keys aside.
    #[test]
    fn ordinary_params_still_print() {
        let request = RpcRequest {
            jsonrpc: jsonrpc_version(),
            id: RpcId::Number(2),
            method: methods::PING.to_string(),
            params: serde_json::json!({"workspace_id": "ws-visible", "token": TOKEN}),
        };
        let rendered = format!("{request:?}");
        assert!(rendered.contains("ws-visible"), "{rendered}");
        assert!(!rendered.contains(TOKEN), "{rendered}");
    }
}
