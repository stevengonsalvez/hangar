//! M1-13: the terminal on the wire against a peer speaking C-R2-1 to C-R2-9.

#![allow(
    clippy::too_many_lines,
    clippy::needless_pass_by_value,
    clippy::cast_possible_truncation
)]

mod common;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use ainb_hangar_proto::terminal::ACK_EVERY_BYTES;
use ainb_wire_mobile::api::{ConnectParams, connect_host, mint_op_id};
use ainb_wire_mobile::pairing::{self, EndpointRecord, PairingRecord};
use ainb_wire_mobile::records::{WireError, WireEvent};
use ainb_wire_mobile::terminal::{
    FloorHolderRecord, ResizeOutcome, TerminalFloorOutcome, TerminalFrameRecord,
    TerminalInputOutcome,
};
use base64::Engine as _;
use common::{FakePeer, HOST_ID, Handler, PeerOpts, Reply, hello_then, method_not_found, spawn};
use serde_json::{Value, json};

/// A pane whose floor is held by a local desktop client at generation 3.
fn desktop_holder() -> Value {
    json!({"principal": "local", "label": "desktop", "stream_id": 1})
}

/// The host: attach gives stream 7; input under gen 4 (ours) types, any
/// other gen is `floor_denied`; `take` moves the floor to us at gen 5.
fn terminal_host(
    scope: &'static str,
    floor_gen: Arc<AtomicU64>,
    acks: Arc<Mutex<Vec<u64>>>,
) -> Handler {
    hello_then(scope, move |method, params| match method {
        "terminal/attach" => {
            assert_eq!(
                params["session"],
                json!({"host_id": HOST_ID, "session_key": "claude:s-1"})
            );
            assert_eq!(params["cols"], 40);
            assert_eq!(params["rows"], 20);
            assert_eq!(params["want_input"], true);
            Reply::Result(json!({
                "stream_id": 7, "epoch": 2, "snapshot_seq": 100, "cols": 40, "rows": 20,
                "floor": {"holder": desktop_holder(), "floor_gen": 3},
                "native_clients": 1
            }))
        }
        "terminal/input" => {
            assert_eq!(params["stream_id"], 7);
            assert!(params["op_id"].as_str().is_some_and(|s| s.len() == 32));
            let ours = floor_gen.load(Ordering::SeqCst);
            if params["floor_gen"] == json!(ours) {
                Reply::Result(
                    json!({"floor_gen": ours, "mutation": {"status": "accepted", "outcome": "created", "receipt": "delivered"}}),
                )
            } else {
                Reply::Error {
                    code: -32008,
                    message: "floor denied".into(),
                    data: Some(
                        json!({"reason": "floor_denied", "holder": desktop_holder(), "floor_gen": 3}),
                    ),
                }
            }
        }
        "terminal/floor" => {
            assert_eq!(params["stream_id"], 7);
            match params["action"].as_str() {
                Some("take") => {
                    floor_gen.store(5, Ordering::SeqCst);
                    Reply::Result(
                        json!({"holder": {"principal": "device:d1", "label": "phone", "stream_id": 7}, "floor_gen": 5,
                                         "mutation": {"status": "accepted", "outcome": "created"}}),
                    )
                }
                Some("acquire") => Reply::Error {
                    code: -32008,
                    message: "floor denied".into(),
                    data: Some(
                        json!({"reason": "floor_denied", "holder": desktop_holder(), "floor_gen": 3}),
                    ),
                },
                _ => method_not_found("terminal/floor"),
            }
        }
        "terminal/resize" => Reply::Result(
            json!({"outcome": "applied", "cols": params["cols"], "rows": params["rows"]}),
        ),
        "terminal/scrollback" => Reply::Result(
            json!({"data": base64::engine::general_purpose::STANDARD.encode(b"old line\r\n")}),
        ),
        "terminal/ack" => {
            assert_eq!(params["stream_id"], 7);
            acks.lock().unwrap().push(params["consumed"].as_u64().unwrap());
            Reply::Result(json!({}))
        }
        "terminal/detach" => Reply::Result(json!({})),
        other => method_not_found(other),
    })
}

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
            device_id: "d1".into(),
            display_name: "phone".into(),
            scope: "mobile+type".into(),
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

fn frame(stream_id: u64, seq: u64, frame: Value) -> (String, Value) {
    (
        "terminal/frame".into(),
        json!({"stream_id": stream_id, "seq": seq, "frame": frame}),
    )
}

#[tokio::test]
async fn attach_frames_decode_acks_flow_and_the_floor_is_a_value() {
    let floor_gen = Arc::new(AtomicU64::new(4));
    let acks = Arc::new(Mutex::new(Vec::new()));
    let peer = spawn(
        terminal_host("mobile+type", Arc::clone(&floor_gen), Arc::clone(&acks)),
        PeerOpts::default(),
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let host = connect_host(params_for(&peer, dir.path())).await.unwrap();
    assert!(!host.can_type(), "hello did not advertise terminal.input");

    let attached = Arc::clone(&host)
        .terminal_attach("claude:s-1".into(), Some(40), Some(20), true, None)
        .await
        .unwrap();
    assert_eq!(attached.stream_id, 7);
    assert_eq!((attached.cols, attached.rows), (40, 20));
    assert_eq!(attached.floor.floor_gen, 3);
    assert_eq!(
        attached.floor.holder.as_ref().map(|h| h.label.as_str()),
        Some("desktop")
    );
    assert_eq!(attached.native_clients, 1);

    // Snapshot then tail, delivered decoded, in order.
    let repaint = b"\x1b[2J\x1b[Hhello".to_vec();
    let b64 = |b: &[u8]| base64::engine::general_purpose::STANDARD.encode(b);
    for f in [
        frame(
            7,
            100,
            json!({"kind": "snapshot_start", "cols": 40, "rows": 20, "epoch": 2, "chunks": 1}),
        ),
        frame(
            7,
            100,
            json!({"kind": "snapshot_chunk", "data": b64(&repaint)}),
        ),
        frame(7, 100, json!({"kind": "snapshot_end"})),
        frame(7, 105, json!({"kind": "output", "data": b64(b"world")})),
        frame(
            7,
            105,
            json!({"kind": "data_gap", "reason": "dropped", "dropped_bytes": 12}),
        ),
        frame(
            7,
            106,
            json!({"kind": "floor", "holder": null, "floor_gen": 4}),
        ),
        frame(7, 106, json!({"kind": "presence", "native_clients": 0})),
        frame(7, 106, json!({"kind": "hologram"})),
    ] {
        peer.notify.send(f).unwrap();
    }
    let mut frames = Vec::new();
    for _ in 0..8 {
        match Arc::clone(&host).next_event().await {
            WireEvent::TerminalFrame {
                stream_id,
                seq,
                frame,
            } => frames.push((stream_id, seq, frame)),
            other => panic!("unexpected {other:?}"),
        }
    }
    assert_eq!(
        frames[0].2,
        TerminalFrameRecord::SnapshotStart {
            cols: 40,
            rows: 20,
            epoch: 2,
            chunks: 1
        }
    );
    assert_eq!(
        frames[1].2,
        TerminalFrameRecord::SnapshotChunk {
            data: repaint.clone()
        }
    );
    assert_eq!(frames[2].2, TerminalFrameRecord::SnapshotEnd);
    assert_eq!(
        frames[3],
        (
            7,
            105,
            TerminalFrameRecord::Output {
                data: b"world".to_vec()
            }
        )
    );
    assert_eq!(
        frames[4].2,
        TerminalFrameRecord::DataGap {
            reason: "dropped".into(),
            dropped_bytes: Some(12)
        }
    );
    assert_eq!(
        frames[5].2,
        TerminalFrameRecord::Floor {
            holder: None,
            floor_gen: 4
        }
    );
    assert_eq!(
        frames[6].2,
        TerminalFrameRecord::Presence { native_clients: 0 }
    );
    assert_eq!(frames[7].2, TerminalFrameRecord::Unknown);
    assert!(acks.lock().unwrap().is_empty(), "under 256 KiB, no ack yet");

    // 256 KiB of output in 48 KiB chunks: the crate acks at the threshold
    // with the cumulative count, snapshot bytes included.
    let chunk = vec![b'x'; 48 * 1024];
    let mut pushed = repaint.len() as u64 + 5;
    let mut seq = 107;
    while pushed < ACK_EVERY_BYTES {
        peer.notify
            .send(frame(
                7,
                seq,
                json!({"kind": "output", "data": b64(&chunk)}),
            ))
            .unwrap();
        pushed += chunk.len() as u64;
        seq += 1;
        assert!(matches!(
            Arc::clone(&host).next_event().await,
            WireEvent::TerminalFrame { .. }
        ));
    }
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    while acks.lock().unwrap().is_empty() {
        assert!(tokio::time::Instant::now() < deadline, "no ack within 5 s");
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(acks.lock().unwrap().as_slice(), &[pushed]);

    // Typing under the wrong generation is a value, not an error.
    let denied = Arc::clone(&host)
        .terminal_input(7, b"ls\n".to_vec(), Some(3), mint_op_id())
        .await
        .unwrap();
    assert_eq!(
        denied,
        TerminalInputOutcome::FloorDenied {
            holder: Some(FloorHolderRecord {
                principal: "local".into(),
                label: "desktop".into(),
                stream_id: 1
            }),
            floor_gen: 3
        }
    );
    let acquire = Arc::clone(&host)
        .terminal_floor(7, "acquire".into(), mint_op_id())
        .await
        .unwrap();
    assert!(
        matches!(
            acquire,
            TerminalFloorOutcome::FloorDenied { floor_gen: 3, .. }
        ),
        "{acquire:?}"
    );

    // Take the floor, then type under the new generation.
    let taken = Arc::clone(&host).terminal_floor(7, "take".into(), mint_op_id()).await.unwrap();
    let TerminalFloorOutcome::Floor { floor, op_id, .. } = taken else {
        panic!("{taken:?}")
    };
    assert_eq!(floor.floor_gen, 5);
    assert_eq!(op_id.len(), 32);
    let minted = mint_op_id();
    let typed = Arc::clone(&host)
        .terminal_input(7, b"ls\n".to_vec(), Some(5), minted.clone())
        .await
        .unwrap();
    let TerminalInputOutcome::Typed {
        op_id,
        floor_gen,
        ack,
    } = typed
    else {
        panic!("{typed:?}")
    };
    assert_eq!(floor_gen, 5);
    assert_eq!(op_id, minted);
    assert_eq!(ack.unwrap().receipt.as_deref(), Some("delivered"));
    let wire = peer.params_of("terminal/input");
    assert_eq!(
        wire.last().unwrap()["op_id"],
        op_id,
        "the envelope is flattened"
    );
    assert_eq!(wire.last().unwrap()["data"], b64(b"ls\n"));
    let retry = Arc::clone(&host)
        .terminal_input(7, b"ls\n".to_vec(), Some(5), minted.clone())
        .await
        .unwrap();
    assert!(
        matches!(retry, TerminalInputOutcome::Typed { op_id: ref again, .. } if *again == op_id)
    );

    assert_eq!(
        Arc::clone(&host).terminal_resize(7, 41, 21).await.unwrap(),
        ResizeOutcome::Applied { cols: 41, rows: 21 }
    );
    assert_eq!(
        Arc::clone(&host).terminal_scrollback(7, 100, 5).await.unwrap(),
        b"old line\r\n".to_vec()
    );
    Arc::clone(&host).terminal_detach(7).await.unwrap();
    assert_eq!(peer.params_of("terminal/detach")[0]["stream_id"], 7);

    // A frame for a detached stream is still delivered but no longer acked.
    peer.notify
        .send(frame(
            7,
            200,
            json!({"kind": "output", "data": b64(&chunk)}),
        ))
        .unwrap();
    assert!(matches!(
        Arc::clone(&host).next_event().await,
        WireEvent::TerminalFrame { .. }
    ));
    assert!(Arc::clone(&host).terminal_floor(7, "steal".into(), mint_op_id()).await.is_err());
    host.close();
}

#[tokio::test]
async fn a_refused_ack_still_delivers_the_frame_and_is_logged_separately() {
    let peer = spawn(
        hello_then("mobile+type", |method, _p| match method {
            "terminal/attach" => Reply::Result(json!({
                "stream_id": 7, "epoch": 1, "snapshot_seq": 0, "cols": 40, "rows": 20,
                "floor": {"floor_gen": 0}, "native_clients": 0
            })),
            "terminal/ack" => Reply::Error {
                code: -32000,
                message: "ack refused".into(),
                data: None,
            },
            other => method_not_found(other),
        }),
        PeerOpts::default(),
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let host = connect_host(params_for(&peer, dir.path())).await.unwrap();
    Arc::clone(&host)
        .terminal_attach("claude:s-1".into(), Some(40), Some(20), true, None)
        .await
        .unwrap();
    let b64 = |b: &[u8]| base64::engine::general_purpose::STANDARD.encode(b);
    let chunk = vec![b'y'; ACK_EVERY_BYTES as usize];
    peer.notify
        .send(frame(7, 1, json!({"kind": "output", "data": b64(&chunk)})))
        .unwrap();
    let got = Arc::clone(&host).next_event().await;
    assert!(
        matches!(got, WireEvent::TerminalFrame { stream_id: 7, ref frame, .. }
            if *frame == TerminalFrameRecord::Output { data: chunk.clone() }),
        "the frame that triggered the refused ack is still delivered: {got:?}"
    );
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    while host.terminal_acks_failed() == 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "no ack failure within 5 s"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(host.terminal_acks_failed(), 1);
    let log = host.connection_log(50);
    let failed = log.iter().find(|e| e.event == "ack_failed").expect("ack_failed logged");
    assert!(failed.detail.contains("ack refused"), "{}", failed.detail);
    assert_eq!(peer.params_of("terminal/ack").len(), 1);
}

#[tokio::test]
async fn a_dark_daemon_answers_method_not_found_and_can_type_needs_scope_and_capability() {
    let peer = spawn(
        Arc::new(|method: &str, _p: Value| match method {
            "auth/hello" => Reply::Result(json!({
                "selected": 1,
                "capabilities": ["hangar.scopes", "terminal.stream", "terminal.input"],
                "host_id": HOST_ID,
                "scope": {"base": "mobile", "admin": false}
            })),
            other => method_not_found(other),
        }),
        PeerOpts::default(),
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let host = connect_host(params_for(&peer, dir.path())).await.unwrap();
    assert!(
        !host.can_type(),
        "scope mobile may not type even when terminal.input is advertised"
    );
    let err = Arc::clone(&host)
        .terminal_attach("claude:s-1".into(), None, None, false, None)
        .await
        .unwrap_err();
    assert!(
        matches!(err, WireError::Rpc { code: -32601, .. }),
        "{err:?}"
    );
}
