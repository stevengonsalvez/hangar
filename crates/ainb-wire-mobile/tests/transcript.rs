//! The transcript page: forward by default, backward only behind the
//! daemon's `fleet.transcript.page_back` capability, and never a backward
//! page whose rows are not below the cursor.

mod common;

use std::sync::Arc;

use ainb_wire_mobile::api::{ConnectParams, connect_host};
use ainb_wire_mobile::pairing::{EndpointRecord, PairingRecord};
use ainb_wire_mobile::records::WireError;
use common::{HOST_ID, PeerOpts, Reply, spawn};
use serde_json::{Value, json};

fn chunk(order: i64) -> Value {
    json!({"ingest_order": order, "event_id": format!("e{order}"), "session_key": "acp:one",
           "event_type": "acp.message", "payload": {"text": format!("line {order}")}, "observed_at": 2})
}

/// A host advertising `caps` whose transcript list answers `rows` for a
/// backward page and the newest tail otherwise, echoing the cursor it saw.
fn host(caps: &'static [&'static str], rows: Vec<i64>) -> common::Handler {
    Arc::new(move |method: &str, params: Value| match method {
        "auth/hello" => Reply::Result(json!({
            "protocol": {"min": 1, "max": 1}, "selected": 1, "capabilities": caps,
            "host_id": HOST_ID, "scope": {"base": "mobile", "admin": false}
        })),
        "fleet/transcript_list" => {
            let chunks: Vec<Value> = rows.iter().map(|o| chunk(*o)).collect();
            if params.get("before_order").is_some() {
                Reply::Result(
                    json!({"chunks": chunks, "next_before_order": rows.first(), "truncated": true}),
                )
            } else {
                Reply::Result(
                    json!({"chunks": chunks, "next_after_order": rows.last(), "truncated": false}),
                )
            }
        }
        other => Reply::Error {
            code: -32601,
            message: format!("{other} not found"),
            data: None,
        },
    })
}

async fn connected(
    peer: &common::FakePeer,
    dir: &std::path::Path,
) -> Arc<ainb_wire_mobile::api::MobileHost> {
    let dir_s = dir.to_string_lossy().into_owned();
    let record = PairingRecord {
        host_id: HOST_ID.into(),
        host_static_pubkey: peer.host_pubkey.to_vec(),
        endpoints: vec![EndpointRecord {
            carrier: "lan".into(),
            url: peer.url.clone(),
        }],
        device_id: "d1".into(),
        display_name: "phone".into(),
        scope: "mobile".into(),
        admin: false,
        expires_at_ms: 4_000_000_000_000,
        paired_at_ms: 1,
        repair: None,
        notice: None,
    };
    ainb_wire_mobile::pairing::save(dir, record, "t").unwrap();
    connect_host(ConnectParams {
        host_id: HOST_ID.into(),
        custody_dir: dir_s.clone(),
        log_dir: dir_s,
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn a_backward_page_needs_the_capability_and_rows_below_the_cursor() {
    let old = spawn(
        host(&["hangar.scopes"], vec![100, 101]),
        PeerOpts::default(),
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let h = connected(&old, dir.path()).await;
    let forward = Arc::clone(&h).transcript_page("acp:one".into(), None, None, 100).await.unwrap();
    assert_eq!(forward.chunks.len(), 2);
    assert_eq!(forward.next_after_order, Some(101));
    let err = Arc::clone(&h)
        .transcript_page("acp:one".into(), None, Some(200), 100)
        .await
        .unwrap_err();
    assert!(
        matches!(err, WireError::Protocol { .. }),
        "no capability, no backward call: {err:?}"
    );
    assert!(
        !old.received
            .lock()
            .unwrap()
            .iter()
            .any(|(m, p)| m == "fleet/transcript_list" && p.get("before_order").is_some()),
        "the daemon never saw a before_order"
    );
    let both = Arc::clone(&h)
        .transcript_page("acp:one".into(), Some(1), Some(2), 100)
        .await
        .unwrap_err();
    assert!(matches!(both, WireError::Protocol { .. }));

    let paged = spawn(
        host(
            &["hangar.scopes", "fleet.transcript.page_back"],
            vec![100, 101],
        ),
        PeerOpts::default(),
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let h = connected(&paged, dir.path()).await;
    let back = Arc::clone(&h)
        .transcript_page("acp:one".into(), None, Some(200), 100)
        .await
        .unwrap();
    assert_eq!(
        back.chunks.iter().map(|c| c.ingest_order).collect::<Vec<_>>(),
        vec![100, 101]
    );
    assert_eq!(back.next_before_order, Some(100));
    assert!(back.truncated);
    assert!(
        paged
            .received
            .lock()
            .unwrap()
            .iter()
            .any(|(m, p)| m == "fleet/transcript_list" && p["before_order"] == json!(200))
    );
    // Rows at or past the cursor are not the page asked for.
    let err = Arc::clone(&h)
        .transcript_page("acp:one".into(), None, Some(101), 100)
        .await
        .unwrap_err();
    assert!(matches!(err, WireError::Protocol { .. }), "{err:?}");
}
