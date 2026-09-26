//! M1-07: the persisted connection log holds identifiers and digests only.

mod common;

use ainb_wire_mobile::api::{ConnectParams, backoff_delay_ms, connect_host, read_connection_log};
use ainb_wire_mobile::connlog::{LOG_FILE, digest};
use ainb_wire_mobile::custody::{DeviceKey, KEY_FILE};
use ainb_wire_mobile::pairing::{self, EndpointRecord, PairingRecord};
use common::{HOST_ID, PeerOpts, hello_then, method_not_found, spawn};

const TOKEN: &str = "mdd_SECRETTOKENVALUE0123456789";

#[tokio::test]
async fn a_real_run_logs_events_with_digests_and_never_a_secret() {
    let peer = spawn(
        hello_then("mobile", |m, _| method_not_found(m)),
        PeerOpts::default(),
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let dir_s = dir.path().to_string_lossy().into_owned();
    let record = |url: &str, key: Vec<u8>| PairingRecord {
        host_id: HOST_ID.into(),
        host_static_pubkey: key,
        endpoints: vec![EndpointRecord {
            carrier: "lan".into(),
            url: url.to_owned(),
        }],
        device_id: "01K5A0000000000000000DEV01".into(),
        display_name: "test phone".into(),
        scope: "mobile".into(),
        admin: false,
        expires_at_ms: 1_800_000_000_000,
        paired_at_ms: 1,
        repair: false,
    };
    let params = ConnectParams {
        host_id: HOST_ID.into(),
        custody_dir: dir_s.clone(),
        log_dir: dir_s.clone(),
    };

    // A failed dial, a wrong pinned key, then a good session that closes.
    pairing::save(
        dir.path(),
        record("ws://127.0.0.1:1/peer", peer.host_pubkey.to_vec()),
        TOKEN,
    )
    .unwrap();
    assert!(connect_host(params.clone()).await.is_err());
    pairing::save(dir.path(), record(&peer.url, vec![9u8; 32]), TOKEN).unwrap();
    assert!(connect_host(params.clone()).await.is_err());
    pairing::save(
        dir.path(),
        record(&peer.url, peer.host_pubkey.to_vec()),
        TOKEN,
    )
    .unwrap();
    let host = connect_host(params.clone()).await.unwrap();
    host.close();
    while !host.is_closed() {
        tokio::task::yield_now().await;
    }
    backoff_delay_ms(3, Some(dir_s.clone()));

    let events: Vec<String> = host.connection_log(100).into_iter().map(|e| e.event).collect();
    assert_eq!(
        events,
        [
            "connect",
            "connect_failed",
            "connect",
            "handshake_failed",
            "connect",
            "handshake",
            "hello",
            "close",
        ],
        "events in order; the backoff went to the file after this view"
    );

    // Survives the app: read back from disk alone.
    let from_disk = read_connection_log(dir_s.clone(), 100).unwrap();
    assert_eq!(from_disk.len(), 9);
    assert_eq!(from_disk.last().unwrap().event, "backoff");
    let handshake = from_disk.iter().find(|e| e.event == "handshake").unwrap();
    assert!(handshake.detail.contains(&digest(&peer.host_pubkey)));
    let close = from_disk.iter().find(|e| e.event == "close").unwrap();
    assert!(
        close.detail.contains("closed by client"),
        "{}",
        close.detail
    );

    // No token, secret, key byte or key encoding anywhere on disk.
    let text = std::fs::read_to_string(dir.path().join(LOG_FILE)).unwrap();
    let key = DeviceKey::load_or_create(dir.path()).unwrap();
    let key_bytes = std::fs::read(dir.path().join(KEY_FILE)).unwrap();
    let forbidden: Vec<String> = vec![
        TOKEN.to_owned(),
        "SECRET".to_owned(),
        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, key.private()),
        base64::Engine::encode(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD,
            key.private(),
        ),
        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, peer.host_pubkey),
        hex(key.private()),
        hex(&peer.host_pubkey),
    ];
    for needle in &forbidden {
        assert!(!text.contains(needle.as_str()), "log leaks {needle}");
    }
    for window in key_bytes.windows(8) {
        assert!(
            !text.as_bytes().windows(8).any(|w| w == window),
            "log leaks raw key bytes"
        );
    }
    assert!(text.contains(&key.fingerprint()) || !text.contains("device_key"));
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}
