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
