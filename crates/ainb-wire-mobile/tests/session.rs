//! M1-02: the wire crate against an in-process peer speaking the frozen wire.

mod common;

use std::sync::Arc;
use std::time::Duration;

use ainb_hangar_proto::agent_status::{RosterStatusResult, RosterStatusRow, status_row};
use ainb_hangar_proto::fleet::{
    AttentionState, FleetCapabilities, FleetConfidence, FleetProvenance, FleetProvider,
    FleetSession, LifecycleState, ManagementState, PaneBinding, TransportHealth,
};
use ainb_hangar_proto::hosts::{CarrierKind, HostId};
use ainb_wire_mobile::api::{ConnectParams, connect_host, mint_op_id};
use ainb_wire_mobile::custody::DeviceKey;
use ainb_wire_mobile::pairing::{self, EndpointRecord, PairingRecord};
use ainb_wire_mobile::records::{AnswerOutcome, WireError, WireEvent};
use ainb_wire_mobile::session::{ConnectConfig, Session, SessionEvent};
use common::DropKind;
use common::{FakePeer, HOST_ID, PeerOpts, Reply, hello_then, method_not_found, spawn};
use serde_json::{Value, json};

fn fixture_session() -> FleetSession {
    FleetSession {
        session_key: "claude:s-1".to_string(),
        provider: FleetProvider::Claude,
        provider_session_id: Some("s-1".to_string()),
        tmux_target: Some("dev:1.0".to_string()),
        pane_binding: PaneBinding::Bound,
        process_start_fingerprint: Some("fp-1".to_string()),
        cwd: "/w/app".to_string(),
        display_name: None,
        lifecycle: LifecycleState::Running,
        active_work_count: 0,
        attention: AttentionState::Ask,
        current_request_fingerprint: None,
        current_request: None,
        management: ManagementState::Managed,
        transport_health: TransportHealth::Healthy,
        capabilities: FleetCapabilities::default(),
        provenance: FleetProvenance::Authoritative,
        confidence: FleetConfidence::High,
        discovered_at: 1,
        last_observed_at: 9,
        lifecycle_updated_at: 5,
        session_incarnation: None,
        attention_updated_at: 7,
        model: None,
        reasoning_effort: None,
        model_updated_at: 0,
        version: 3,
        updated_revision: 1,
    }
}

fn roster() -> Value {
    let session = fixture_session();
    let status = status_row(&session, true);
    serde_json::to_value(RosterStatusResult {
        rows: vec![RosterStatusRow {
            session,
            status,
            read_revision: 11,
        }],
        read_revision: 11,
        unknown_events: vec![],
        read_at_ms: 1_700_000_000_000,
    })
    .unwrap()
}

/// Save a pairing for `peer` under `dir` and return the params to connect.
fn params_for(peer: &FakePeer, dir: &std::path::Path) -> ConnectParams {
    pairing::save(
        dir,
        PairingRecord {
            host_id: HOST_ID.into(),
            host_static_pubkey: peer.host_pubkey.to_vec(),
            endpoints: vec![EndpointRecord {
                carrier: "lan".into(),
                url: peer.url.clone(),
            }],
            device_id: "01K5A0000000000000000DEV01".into(),
            display_name: "test phone".into(),
            scope: "mobile".into(),
            admin: false,
            expires_at_ms: 1_800_000_000_000,
            paired_at_ms: 1,
            repair: None,
            notice: None,
        },
        "mdd_test",
    )
    .unwrap();
    ConnectParams {
        host_id: HOST_ID.into(),
        custody_dir: dir.to_string_lossy().into_owned(),
        log_dir: dir.to_string_lossy().into_owned(),
    }
}

#[tokio::test]
async fn hello_roster_and_answer_round_trip_with_op_id_and_fence() {
    let peer = spawn(
        hello_then("mobile", |method, _params| match method {
            "fleet/roster_status" => Reply::Result(roster()),
            "attention/answer" => Reply::Result(json!({
                "outcome": "delivered", "via": "tmux (dev)",
                "mutation": {"status": "accepted", "outcome": "created", "receipt": "delivered"}
            })),
            other => method_not_found(other),
        }),
        PeerOpts::default(),
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let host = connect_host(params_for(&peer, dir.path())).await.unwrap();

    let hello = host.hello();
    assert_eq!(hello.scope.as_deref(), Some("mobile"));
    assert_eq!(hello.host_id.as_deref(), Some(HOST_ID));
    let sent_hello = &peer.params_of("auth/hello")[0];
    assert_eq!(sent_hello["token"], "mdd_test");
    assert_eq!(
        sent_hello["device"]["device_id"],
        "01K5A0000000000000000DEV01"
    );
    assert_eq!(sent_hello["protocol"], json!({"min": 1, "max": 1}));

    let snapshot = Arc::clone(&host).roster_status().await.unwrap();
    assert_eq!(snapshot.read_revision, 11);
    let row = &snapshot.rows[0];
    assert_eq!(
        (
            row.session_key.as_str(),
            row.state.as_str(),
            row.provenance.as_str(),
            row.tier.as_str()
        ),
        ("claude:s-1", "waiting", "hook", "hook"),
        "the same tuple the TUI reads from the same row"
    );
    assert_eq!(row.lifecycle_updated_at, 5);
    assert_eq!(row.process_start_fingerprint.as_deref(), Some("fp-1"));
    assert_eq!(row.version, 3);

    // The app mints the op id before the send and keeps it for a retry.
    let minted = mint_op_id();
    assert_eq!(minted.len(), 32);
    assert!(minted.chars().all(|c| c.is_ascii_hexdigit()));
    let reply = Arc::clone(&host)
        .answer("att-1".into(), "yes".into(), 4, minted.clone())
        .await
        .unwrap();
    assert_eq!(reply.op_id, minted);
    assert_eq!(
        reply.outcome,
        AnswerOutcome::Delivered {
            via: "tmux (dev)".into()
        }
    );
    assert_eq!(reply.ack.unwrap().receipt.as_deref(), Some("delivered"));
    let sent = &peer.params_of("attention/answer")[0];
    assert_eq!(sent["attention_id"], "att-1");
    assert_eq!(sent["answer"], "yes");
    assert_eq!(sent["op_id"], reply.op_id, "the envelope is flattened");
    assert_eq!(
        sent["fence"],
        json!({"kind": "attention_version", "version": 4})
    );

    // A retry after a lost reply sends the SAME op id.
    let again = Arc::clone(&host)
        .answer("att-1".into(), "yes".into(), 4, minted.clone())
        .await
        .unwrap();
    assert_eq!(again.op_id, reply.op_id);
    let answers = peer.params_of("attention/answer");
    assert_eq!(answers.len(), 2);
    assert_eq!(answers[0]["op_id"], answers[1]["op_id"]);

    host.close();
    assert!(host.is_closed());
}

#[tokio::test]
async fn send_prompt_and_interrupt_carry_their_fences_and_surface_reasons() {
    let peer = spawn(
        hello_then("mobile", |method, params| match method {
            "fleet/message_send" => {
                if params["fence"]["lifecycle_updated_at"] == 5 {
                    Reply::Result(json!({"message_id": "m1", "deliveries": [],
                        "mutation": {"status": "accepted", "outcome": "created"}}))
                } else {
                    Reply::Error {
                        code: -32008,
                        message: "turn advanced".into(),
                        data: Some(json!({"reason": "turn_advanced"})),
                    }
                }
            }
            "fleet/action" => Reply::Result(json!({
                "receipt": {"request_id": params["request_id"], "session_key": "claude:s-1",
                    "action_kind": "interrupt", "action_fingerprint": "x", "expected_version": 3,
                    "idempotency_key": null, "status": "DELIVERED", "detail": null,
                    "session_version": 4, "created_at": 1, "updated_at": 1},
                "mutation": {"status": "accepted", "outcome": "created"}
            })),
            other => method_not_found(other),
        }),
        PeerOpts::default(),
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let host = connect_host(params_for(&peer, dir.path())).await.unwrap();

    let sent = Arc::clone(&host)
        .send_prompt("claude:s-1".into(), "hi".into(), 5, mint_op_id())
        .await
        .unwrap();
    assert_eq!(sent.message_id, "m1");
    let wire = &peer.params_of("fleet/message_send")[0];
    assert_eq!(wire["targets"], json!(["claude:s-1"]));
    assert!(wire.get("actor").is_none(), "the daemon pins the actor");
    assert_eq!(wire["request_id"], sent.op_id);
    assert_eq!(
        wire["fence"],
        json!({"kind": "lifecycle_updated_at", "lifecycle_updated_at": 5})
    );

    let stale = Arc::clone(&host)
        .send_prompt("claude:s-1".into(), "hi".into(), 4, mint_op_id())
        .await
        .unwrap_err();
    assert_eq!(
        stale,
        WireError::Rpc {
            code: -32008,
            message: "turn advanced".into(),
            reason: Some("turn_advanced".into()),
            data: Some(json!({"reason": "turn_advanced"}).to_string()),
        }
    );

    let interrupted = Arc::clone(&host)
        .interrupt("claude:s-1".into(), 3, "fp-1".into(), mint_op_id())
        .await
        .unwrap();
    assert_eq!(interrupted.receipt_status, "DELIVERED");
    let wire = &peer.params_of("fleet/action")[0];
    assert_eq!(wire["action"], json!({"action": "interrupt"}));
    assert_eq!(wire["expected_version"], 3);
    assert_eq!(
        wire["fence"],
        json!({"kind": "session_incarnation", "session_incarnation": "fp-1"})
    );
}

#[tokio::test]
async fn subscribe_replays_after_revision_and_events_arrive_in_order() {
    let peer = spawn(
        hello_then("mobile", |method, params| match method {
            "fleet/subscribe" => {
                assert_eq!(params["after_revision"], 4);
                Reply::Result(json!({
                    "snapshot": {"head_revision": 6, "sessions": [fixture_session()]},
                    "replay": [
                        {"revision": 5, "event_id": "e5", "session_key": "claude:s-1", "observed_at": 1,
                         "provenance": "authoritative", "event_type": "turn_running", "payload": {},
                         "session_version": 2, "applied": true},
                        {"revision": 6, "event_id": "e6", "session_key": "claude:s-1", "observed_at": 2,
                         "provenance": "authoritative", "event_type": "turn_completed", "payload": {},
                         "session_version": 3, "applied": true}
                    ],
                    "replay_state": {"state": "complete"}
                }))
            }
            "attention/subscribe" => Reply::Result(json!({"attention": [{
                "id": "att-9", "session_id": "s-1", "cwd": "/w", "kind": "approval", "version": 2,
                "payload": "{}", "created_at": 3, "channels": []
            }]})),
            "ping" => Reply::Result(json!({})),
            other => method_not_found(other),
        }),
        PeerOpts::default(),
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let host = connect_host(params_for(&peer, dir.path())).await.unwrap();

    let summary = Arc::clone(&host).subscribe_fleet(4).await.unwrap();
    assert!(summary.replay_complete);
    assert_eq!(summary.head_revision, 6);
    assert_eq!(
        summary.replay.iter().map(|e| e.revision).collect::<Vec<_>>(),
        vec![5, 6]
    );
    assert_eq!(summary.session_keys, vec!["claude:s-1"]);

    let open = Arc::clone(&host).subscribe_attention().await.unwrap();
    assert_eq!(open[0].id, "att-9");
    assert_eq!(open[0].version, 2);

    peer.notify
        .send((
            "fleet/event".into(),
            json!({"revision": 7, "event_id": "e7", "session_key": "claude:s-1", "observed_at": 3,
                   "provenance": "authoritative", "event_type": "turn_running", "payload": {},
                   "session_version": 4, "applied": true}),
        ))
        .unwrap();
    peer.notify
        .send((
            "hangar/event".into(),
            json!({"event": "attention_raised", "attention_id": "att-10", "session_id": "s-1",
                   "kind": "ask_user_question", "created_at": 4}),
        ))
        .unwrap();
    peer.notify
        .send((
            "hangar/event".into(),
            json!({"event": "attention_answered", "attention_id": "att-10", "by": "device:d1"}),
        ))
        .unwrap();
    peer.notify.send(("fleet/resync_required".into(), json!(null))).unwrap();

    let first = Arc::clone(&host).next_event().await;
    assert!(matches!(first, WireEvent::FleetRevision { ref event } if event.revision == 7));
    let second = Arc::clone(&host).next_event().await;
    assert!(
        matches!(second, WireEvent::AttentionRaised { ref attention_id, ref kind, .. }
        if attention_id == "att-10" && kind == "ask_user_question")
    );
    let third = Arc::clone(&host).next_event().await;
    assert_eq!(
        third,
        WireEvent::AttentionAnswered {
            attention_id: "att-10".into(),
            by: "device:d1".into()
        }
    );
    assert_eq!(
        Arc::clone(&host).next_event().await,
        WireEvent::FleetResyncRequired
    );

    host.close();
    let closed = Arc::clone(&host).next_event().await;
    assert!(
        matches!(
            closed,
            WireEvent::Closed {
                code: None,
                retryable: false,
                retry_after_ms: None,
                ..
            }
        ),
        "the app's own close is never retryable: {closed:?}"
    );
}

#[tokio::test]
async fn heartbeat_declares_dead_after_two_unanswered_pings_and_lives_on_pongs() {
    let silent = spawn(
        hello_then("mobile", |m, _| method_not_found(m)),
        PeerOpts {
            answer_pings: false,
            ..PeerOpts::default()
        },
    )
    .await;
    let key = DeviceKey::generate().unwrap();
    let mut config = silent.config(&key);
    config.heartbeat = Some(Duration::from_millis(50));
    let session = Session::connect(config).await.unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;
    let stats = session.stats();
    assert!(stats.closed, "{stats:?}");
    assert!(
        stats.close_reason.as_deref().unwrap_or("").contains("heartbeat"),
        "{stats:?}"
    );
    assert_eq!(stats.pings_sent, 2, "two probes, then dead: {stats:?}");
    assert_eq!(silent.pings.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert!(matches!(
        session.next_event().await,
        SessionEvent::Closed { code: None, .. }
    ));

    let answering = spawn(
        hello_then("mobile", |m, _| method_not_found(m)),
        PeerOpts::default(),
    )
    .await;
    let mut config = answering.config(&key);
    config.heartbeat = Some(Duration::from_millis(50));
    let session = Session::connect(config).await.unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;
    let stats = session.stats();
    assert!(!stats.closed, "{stats:?}");
    assert!(stats.pongs_received >= 3, "{stats:?}");
    assert!(stats.pings_sent >= stats.pongs_received);
}

#[tokio::test]
async fn a_host_with_another_key_or_id_is_peer_changed() {
    let peer = spawn(
        hello_then("mobile", |m, _| method_not_found(m)),
        PeerOpts::default(),
    )
    .await;
    let key = DeviceKey::generate().unwrap();

    let mut wrong_key = peer.config(&key);
    wrong_key.host_static_pubkey = [9u8; 32];
    assert_eq!(
        Session::connect(wrong_key).await.err(),
        Some(WireError::PeerChanged)
    );

    let mut wrong_host = peer.config(&key);
    wrong_host.host_id = HostId::parse("01K5A0000000000000000ABCDF").unwrap();
    assert_eq!(
        Session::connect(wrong_host).await.err(),
        Some(WireError::PeerChanged)
    );

    assert!(Session::connect(peer.config(&key)).await.is_ok());
    assert_eq!(peer.handshakes.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_close_before_the_handshake_reply_is_classified_by_its_code() {
    let key = DeviceKey::generate().unwrap();
    let closed =
        |code: u16, reason: &str, retryable: bool, retry_after_ms: Option<u64>| WireError::Closed {
            code: Some(code),
            reason: reason.into(),
            retryable,
            retry_after_ms,
        };
    let cases = [
        (
            4429,
            "retry-after=7",
            closed(4429, "retry-after=7", true, Some(7_000)),
        ),
        (
            1013,
            "retry-after=2",
            closed(1013, "retry-after=2", true, Some(2_000)),
        ),
        (4503, "draining", closed(4503, "draining", true, None)),
        (4409, "protocol", closed(4409, "protocol", false, None)),
        (4403, "revoked", closed(4403, "revoked", false, None)),
        (4999, "novel", closed(4999, "novel", false, None)),
    ];
    for (code, reason, expected) in cases {
        let peer = spawn(
            hello_then("mobile", |m, _| method_not_found(m)),
            PeerOpts {
                refuse_before_handshake: Some((code, reason.into())),
                ..PeerOpts::default()
            },
        )
        .await;
        let err = Session::connect(peer.config(&key)).await.unwrap_err();
        assert_eq!(err, expected, "close {code}");
        assert_eq!(
            err.is_retryable(),
            matches!(code, 4429 | 1013 | 4503),
            "retryable for {code}"
        );
    }
    assert!(!WireError::PeerChanged.is_retryable());
    assert!(!closed(4401, "noise", false, None).is_retryable());

    // A socket that vanishes with no close code is a network loss.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/peer", listener.local_addr().unwrap());
    tokio::spawn(async move {
        while let Ok((tcp, _)) = listener.accept().await {
            drop(tcp);
        }
    });
    let mut config = ConnectConfig::new(
        url,
        CarrierKind::Lan,
        HostId::parse(HOST_ID).unwrap(),
        [1; 32],
        key.private().to_vec(),
    );
    config.heartbeat = None;
    let err = Session::connect(config).await.unwrap_err();
    assert!(matches!(err, WireError::Connect { .. }), "{err:?}");
    assert!(err.is_retryable());
}

#[tokio::test]
async fn a_socket_ended_or_reset_after_message_1_is_a_retryable_connect() {
    let key = DeviceKey::generate().unwrap();
    for kind in [DropKind::Eof, DropKind::Reset] {
        let peer = spawn(
            hello_then("mobile", |m, _| method_not_found(m)),
            PeerOpts {
                after_message_1: Some(kind),
                ..PeerOpts::default()
            },
        )
        .await;
        let err = Session::connect(peer.config(&key)).await.unwrap_err();
        assert!(
            matches!(err, WireError::Connect { .. }),
            "{kind:?} after message 1 must be a network loss, got {err:?}"
        );
        assert!(err.is_retryable(), "{kind:?}");
    }
}

#[tokio::test]
async fn a_silent_host_times_out_the_connect() {
    let peer = spawn(
        hello_then("mobile", |m, _| method_not_found(m)),
        PeerOpts {
            hang: true,
            ..PeerOpts::default()
        },
    )
    .await;
    let key = DeviceKey::generate().unwrap();
    let mut config = peer.config(&key);
    config.connect_timeout = Duration::from_millis(300);
    let started = tokio::time::Instant::now();
    let err = Session::connect(config).await.unwrap_err();
    assert!(
        matches!(err, WireError::Connect { ref message } if message.contains("no handshake within")),
        "{err:?}"
    );
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(err.is_retryable());
}

#[tokio::test]
async fn close_codes_map_4403_to_revoked_and_4401_to_unauthenticated() {
    for code in [4403u16, 4401] {
        let expected = WireError::Closed {
            code: Some(code),
            reason: "go away".into(),
            retryable: false,
            retry_after_ms: None,
        };
        let peer = spawn(
            Arc::new(move |_m: &str, _p: Value| Reply::Close(code, "go away".into())),
            PeerOpts::default(),
        )
        .await;
        let dir = tempfile::tempdir().unwrap();
        let err = connect_host(params_for(&peer, dir.path())).await.unwrap_err();
        assert_eq!(err, expected, "close {code}");
    }
}

#[tokio::test]
async fn an_overflowing_event_queue_reports_lag_once() {
    let peer = spawn(
        hello_then("mobile", |m, _| match m {
            "ping" => Reply::Result(json!({})),
            other => method_not_found(other),
        }),
        PeerOpts::default(),
    )
    .await;
    let key = DeviceKey::generate().unwrap();
    let session = Session::connect(peer.config(&key)).await.unwrap();
    for i in 0..1500 {
        peer.notify.send(("fleet/resync_required".into(), json!(i))).unwrap();
        if i % 100 == 0 {
            // Let the peer drain its queue so the broadcast never lags.
            tokio::task::yield_now().await;
        }
    }
    // The peer interleaves its forwarding with request handling, so wait
    // for the overflow itself rather than assuming an order.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while session.stats().events_dropped == 0 {
        assert!(tokio::time::Instant::now() < deadline, "no overflow in 5 s");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    session.request("ping", json!({})).await.unwrap();
    let first = session.next_event().await;
    let SessionEvent::Lagged(dropped) = first else {
        panic!("expected Lagged, got {first:?}");
    };
    assert!(dropped >= 1, "{dropped}");
    assert!(session.stats().events_dropped >= dropped);
    let next = session.next_event().await;
    assert!(matches!(next, SessionEvent::Notification(_)), "{next:?}");
}
