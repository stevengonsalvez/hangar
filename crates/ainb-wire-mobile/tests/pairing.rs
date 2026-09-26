//! M1-12: real pairing against an in-process peer that issues single-use
//! invites and checks the token at hello.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use ainb_hangar_noise::{Endpoint, PairingOffer};
use ainb_hangar_proto::hosts::{CarrierKind, HostId};
use ainb_wire_mobile::api::{ConnectParams, connect_host, forget_pairing, list_pairings, pair};
use ainb_wire_mobile::custody::{DeviceKey, KEY_FILE};
use ainb_wire_mobile::pairing::INDEX_FILE;
use ainb_wire_mobile::records::WireError;
use common::{FakePeer, HOST_ID, Handler, PeerOpts, Reply, spawn};
use serde_json::{Value, json};

const INVITE_ID: &str = "01K5A0000000000000000INV01";
const SECRET: [u8; 32] = [42; 32];
const TOKEN: &str = "mdd_pairedtoken0123456789";
const DEVICE_ID: &str = "01K5A0000000000000000DEV01";

/// A host that redeems `INVITE_ID` once and accepts hello only with the
/// token it issued.
fn issuing_host(redeemed: Arc<AtomicBool>, hello_close: Option<u16>) -> Handler {
    Arc::new(move |method: &str, params: Value| match method {
        "device/redeem" => {
            if params["invite_id"] != INVITE_ID
                || params["invite_secret"] != "KioqKioqKioqKioqKioqKioqKioqKioqKioqKioqKio"
            {
                return Reply::Error {
                    code: -32000,
                    message: "unknown invite".into(),
                    data: None,
                };
            }
            if redeemed.swap(true, Ordering::SeqCst) {
                return Reply::Error {
                    code: -32000,
                    message: "invite already redeemed".into(),
                    data: Some(json!({"reason": "invite_used"})),
                };
            }
            assert_eq!(params["display_name"], "my phone");
            assert_eq!(params["protocol"], json!({"min": 1, "max": 1}));
            Reply::Result(json!({
                "device_id": DEVICE_ID,
                "device_token": TOKEN,
                "scope": {"base": "mobile+type", "admin": false},
                "expires_at_ms": 1_800_000_000_000i64,
                "host_id": HOST_ID
            }))
        }
        "auth/hello" => {
            if params["token"] != TOKEN {
                return Reply::Close(4401, "bad token".into());
            }
            assert_eq!(params["device"]["device_id"], DEVICE_ID);
            if let Some(code) = hello_close {
                return Reply::Close(code, "revoked".into());
            }
            Reply::Result(json!({
                "selected": 1,
                "capabilities": ["hangar.scopes"],
                "host_id": HOST_ID,
                "scope": {"base": "mobile+type", "admin": false},
                "device_expires_at_ms": 1_800_000_000_000i64
            }))
        }
        other => common::method_not_found(other),
    })
}

fn offer_for(peer: &FakePeer, key: [u8; 32], endpoints: Vec<Endpoint>) -> String {
    PairingOffer {
        v: 1,
        host_id: HostId::parse(HOST_ID).unwrap(),
        host_static_pubkey: key,
        endpoints,
        invite_id: INVITE_ID.into(),
        invite_secret: SECRET,
        expires_at_ms: 4_000_000_000_000,
        relay: None,
    }
    .to_uri()
    .replace("ws://placeholder/peer", &peer.url)
}

fn live(peer: &FakePeer) -> Endpoint {
    Endpoint {
        carrier: CarrierKind::Lan,
        url: peer.url.clone(),
    }
}

fn dead() -> Endpoint {
    Endpoint {
        carrier: CarrierKind::Tailnet,
        url: "ws://127.0.0.1:1/peer".into(),
    }
}

#[tokio::test]
async fn pair_redeems_once_stores_the_token_in_custody_and_connects_by_host_id() {
    let redeemed = Arc::new(AtomicBool::new(false));
    let peer = spawn(
        issuing_host(Arc::clone(&redeemed), None),
        PeerOpts::default(),
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let dir_s = dir.path().to_string_lossy().into_owned();

    // Endpoints in preference order: the dead one first, then the live one.
    let uri = offer_for(&peer, peer.host_pubkey, vec![dead(), live(&peer)]);
    let record = pair(uri.clone(), "my phone".into(), dir_s.clone(), dir_s.clone())
        .await
        .unwrap();
    assert_eq!(record.host_id, HOST_ID);
    assert_eq!(record.device_id, DEVICE_ID);
    assert_eq!(record.scope, "mobile+type");
    assert_eq!(record.endpoints.len(), 2);
    assert_eq!(
        (record.repair.as_deref(), record.notice.as_deref()),
        (None, None)
    );
    assert_eq!(peer.received_methods(), ["device/redeem", "auth/hello"]);

    // The token is in custody, not in the index the app reads.
    let index = std::fs::read_to_string(dir.path().join(INDEX_FILE)).unwrap();
    assert!(!index.contains(TOKEN));
    assert_eq!(list_pairings(dir_s.clone()).unwrap(), vec![record.clone()]);
    let key = DeviceKey::load_or_create(dir.path()).unwrap();
    assert!(std::fs::read(dir.path().join(KEY_FILE)).is_ok());
    assert_eq!(key.fingerprint().len(), 64);

    // A second redeem of the same invite is refused by the host.
    let again = pair(uri, "my phone".into(), dir_s.clone(), dir_s.clone()).await.unwrap_err();
    assert!(
        matches!(again, WireError::Rpc { reason: Some(ref r), .. } if r == "invite_used"),
        "{again:?}"
    );

    // Connecting takes only the host id; the crate supplies the token and
    // the pinned key, and the device static key is the one it minted.
    let host = connect_host(ConnectParams {
        host_id: HOST_ID.into(),
        custody_dir: dir_s.clone(),
        log_dir: dir_s.clone(),
    })
    .await
    .unwrap();
    assert_eq!(host.hello().scope.as_deref(), Some("mobile+type"));
    assert_eq!(peer.handshakes.load(Ordering::SeqCst), 3);

    // Forgetting removes the token and the record.
    host.close();
    forget_pairing(dir_s.clone(), HOST_ID.into()).unwrap();
    assert!(list_pairings(dir_s.clone()).unwrap().is_empty());
    let gone = connect_host(ConnectParams {
        host_id: HOST_ID.into(),
        custody_dir: dir_s.clone(),
        log_dir: dir_s.clone(),
    })
    .await
    .unwrap_err();
    assert_eq!(
        gone,
        WireError::NotPaired {
            host_id: HOST_ID.into()
        }
    );
}

#[tokio::test]
async fn a_redeemed_invite_whose_hello_fails_leaves_the_pairing_to_retry() {
    // Redeem succeeds (the single-use invite is burned), then the host
    // drops the socket at hello. Today's bug: 0 pairings saved, token lost.
    let redeemed = Arc::new(AtomicBool::new(false));
    let peer = spawn(
        issuing_host(Arc::clone(&redeemed), Some(4503)),
        PeerOpts::default(),
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let dir_s = dir.path().to_string_lossy().into_owned();
    let err = pair(
        offer_for(&peer, peer.host_pubkey, vec![live(&peer)]),
        "my phone".into(),
        dir_s.clone(),
        dir_s.clone(),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(
            err,
            WireError::Closed {
                code: Some(4503),
                retryable: true,
                ..
            }
        ),
        "4503 at hello is the host draining: {err:?}"
    );
    assert!(redeemed.load(Ordering::SeqCst), "the invite was consumed");
    let saved = list_pairings(dir_s.clone()).unwrap();
    assert_eq!(saved.len(), 1, "the pairing is saved before hello");
    assert_eq!(saved[0].device_id, DEVICE_ID);
    assert_eq!(
        (saved[0].repair.as_deref(), saved[0].notice.as_deref()),
        (None, None),
        "a draining host is not a refusal"
    );

    // The same failure at hello with a 4403 keeps the pairing AND latches.
    let revoking = spawn(
        issuing_host(Arc::new(AtomicBool::new(false)), Some(4403)),
        PeerOpts::default(),
    )
    .await;
    let other = tempfile::tempdir().unwrap();
    let other_s = other.path().to_string_lossy().into_owned();
    let err = pair(
        offer_for(&revoking, revoking.host_pubkey, vec![live(&revoking)]),
        "my phone".into(),
        other_s.clone(),
        other_s.clone(),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(
            err,
            WireError::Closed {
                code: Some(4403),
                retryable: false,
                ..
            }
        ),
        "{err:?}"
    );
    let latched = list_pairings(other_s).unwrap();
    assert_eq!(latched.len(), 1);
    assert_eq!(
        latched[0].repair.as_deref(),
        Some("revoked"),
        "4403 on the first hello sets the re-pair latch"
    );

    // The saved token works on the next connect, against the same host once
    // it answers hello.
    let steady = spawn(
        issuing_host(Arc::new(AtomicBool::new(true)), None),
        PeerOpts::default(),
    )
    .await;
    let mut record = saved.into_iter().next().unwrap();
    record.endpoints[0].url = steady.url.clone();
    record.host_static_pubkey = steady.host_pubkey.to_vec();
    let token = TOKEN;
    ainb_wire_mobile::pairing::save(dir.path(), record, token).unwrap();
    let host = connect_host(ConnectParams {
        host_id: HOST_ID.into(),
        custody_dir: dir_s.clone(),
        log_dir: dir_s.clone(),
    })
    .await
    .unwrap();
    assert_eq!(host.hello().scope.as_deref(), Some("mobile+type"));
    assert_eq!(steady.params_of("auth/hello")[0]["token"], TOKEN);
}

#[tokio::test]
async fn peer_changed_sets_the_repair_latch_on_connect_and_on_repair() {
    let redeemed = Arc::new(AtomicBool::new(false));
    let peer = spawn(
        issuing_host(Arc::clone(&redeemed), None),
        PeerOpts::default(),
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let dir_s = dir.path().to_string_lossy().into_owned();
    pair(
        offer_for(&peer, peer.host_pubkey, vec![live(&peer)]),
        "my phone".into(),
        dir_s.clone(),
        dir_s.clone(),
    )
    .await
    .unwrap();
    assert_eq!(list_pairings(dir_s.clone()).unwrap()[0].repair, None);

    // The host's key changed under the pairing: connect_host is PeerChanged
    // and the latch is set.
    let mut record = list_pairings(dir_s.clone()).unwrap().remove(0);
    record.host_static_pubkey = vec![7u8; 32];
    ainb_wire_mobile::pairing::save(dir.path(), record, TOKEN).unwrap();
    let err = connect_host(ConnectParams {
        host_id: HOST_ID.into(),
        custody_dir: dir_s.clone(),
        log_dir: dir_s.clone(),
    })
    .await
    .unwrap_err();
    assert_eq!(err, WireError::PeerChanged);
    assert_eq!(
        list_pairings(dir_s.clone()).unwrap()[0].repair.as_deref(),
        Some("peer_changed"),
        "PeerChanged at connect latches"
    );

    // The reviewer's repro: an offer is unauthenticated input. A second
    // offer for this host with a DIFFERENT key (a stale or hostile QR code)
    // is refused before any dial and leaves the record untouched.
    ainb_wire_mobile::pairing::clear_flags(dir.path(), HOST_ID).unwrap();
    let mut good = list_pairings(dir_s.clone()).unwrap().remove(0);
    good.host_static_pubkey = peer.host_pubkey.to_vec();
    ainb_wire_mobile::pairing::save(dir.path(), good.clone(), TOKEN).unwrap();
    let handshakes_before = peer.handshakes.load(Ordering::SeqCst);
    let err = pair(
        offer_for(&peer, [9u8; 32], vec![live(&peer)]),
        "my phone".into(),
        dir_s.clone(),
        dir_s.clone(),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, WireError::Offer { .. }), "{err:?}");
    let after = list_pairings(dir_s.clone()).unwrap().remove(0);
    assert_eq!(
        after.host_static_pubkey, good.host_static_pubkey,
        "the pinned key is untouched"
    );
    assert_eq!(
        (after.repair, after.notice),
        (None, None),
        "no flag from a hostile offer"
    );
    assert_eq!(
        peer.handshakes.load(Ordering::SeqCst),
        handshakes_before,
        "no dial happened"
    );

    // A re-pair offer carrying the key we already pin, refused by a host
    // whose key has changed, is the legitimate PeerChanged and latches.
    let moved = spawn(
        issuing_host(Arc::new(AtomicBool::new(false)), None),
        PeerOpts::default(),
    )
    .await;
    let err = pair(
        offer_for(&moved, peer.host_pubkey, vec![live(&moved)]),
        "my phone".into(),
        dir_s.clone(),
        dir_s.clone(),
    )
    .await
    .unwrap_err();
    assert_eq!(err, WireError::PeerChanged);
    assert_eq!(
        list_pairings(dir_s.clone()).unwrap()[0].repair.as_deref(),
        Some("peer_changed"),
        "PeerChanged on the pinned key latches"
    );

    // A good pair clears it. The host really moved to a new key, so the
    // user forgets the host (the explicit action) and pairs it afresh.
    forget_pairing(dir_s.clone(), HOST_ID.into()).unwrap();
    let fresh = spawn(
        issuing_host(Arc::new(AtomicBool::new(false)), None),
        PeerOpts::default(),
    )
    .await;
    let record = pair(
        offer_for(&fresh, fresh.host_pubkey, vec![live(&fresh)]),
        "my phone".into(),
        dir_s.clone(),
        dir_s.clone(),
    )
    .await
    .unwrap();
    assert_eq!(
        (record.repair.as_deref(), record.notice.as_deref()),
        (None, None)
    );
    assert_eq!(list_pairings(dir_s).unwrap()[0].repair, None);
}

#[tokio::test]
async fn an_offer_with_another_key_is_peer_changed_and_an_expired_one_is_refused() {
    let redeemed = Arc::new(AtomicBool::new(false));
    let peer = spawn(
        issuing_host(Arc::clone(&redeemed), None),
        PeerOpts::default(),
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let dir_s = dir.path().to_string_lossy().into_owned();

    let wrong = offer_for(&peer, [7u8; 32], vec![live(&peer)]);
    let err = pair(wrong, "my phone".into(), dir_s.clone(), dir_s.clone()).await.unwrap_err();
    assert_eq!(err, WireError::PeerChanged);
    assert!(
        !redeemed.load(Ordering::SeqCst),
        "no redeem reached the host"
    );
    assert!(list_pairings(dir_s.clone()).unwrap().is_empty());

    let mut expired: PairingOffer =
        PairingOffer::parse(&offer_for(&peer, peer.host_pubkey, vec![live(&peer)])).unwrap();
    expired.expires_at_ms = 1;
    let err = pair(
        expired.to_uri(),
        "my phone".into(),
        dir_s.clone(),
        dir_s.clone(),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, WireError::Offer { ref message } if message.contains("expired")));

    let err = pair(
        "https://nope".into(),
        "my phone".into(),
        dir_s.clone(),
        dir_s.clone(),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, WireError::Offer { .. }));
    assert_eq!(peer.received_methods(), Vec::<String>::new());
}

#[tokio::test]
async fn a_revoked_device_gets_4403_and_the_app_can_forget_the_pairing() {
    let redeemed = Arc::new(AtomicBool::new(false));
    let peer = spawn(
        issuing_host(Arc::clone(&redeemed), None),
        PeerOpts::default(),
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let dir_s = dir.path().to_string_lossy().into_owned();
    pair(
        offer_for(&peer, peer.host_pubkey, vec![live(&peer)]),
        "my phone".into(),
        dir_s.clone(),
        dir_s.clone(),
    )
    .await
    .unwrap();

    // The same host, now revoking at hello. Re-point the pairing at it by
    // saving the record with the new endpoint, as a rescan would.
    let revoking = spawn(
        issuing_host(Arc::new(AtomicBool::new(false)), Some(4403)),
        PeerOpts::default(),
    )
    .await;
    let mut record = list_pairings(dir_s.clone()).unwrap().remove(0);
    record.endpoints[0].url = revoking.url.clone();
    record.host_static_pubkey = revoking.host_pubkey.to_vec();
    ainb_wire_mobile::pairing::save(dir.path(), record, TOKEN).unwrap();

    let err = connect_host(ConnectParams {
        host_id: HOST_ID.into(),
        custody_dir: dir_s.clone(),
        log_dir: dir_s.clone(),
    })
    .await
    .unwrap_err();
    assert!(
        matches!(
            err,
            WireError::Closed {
                code: Some(4403),
                retryable: false,
                ..
            }
        ),
        "{err:?}"
    );
    assert_eq!(
        list_pairings(dir_s.clone()).unwrap()[0].repair.as_deref(),
        Some("revoked"),
        "4403 at hello sets the re-pair latch"
    );

    // A 4403 in the middle of a live session latches too, through the
    // event pump: the app keeps no copy.
    ainb_wire_mobile::pairing::clear_flags(dir.path(), HOST_ID).unwrap();
    let mid = spawn(
        Arc::new(|method: &str, _p: Value| match method {
            "auth/hello" => Reply::Result(json!({"selected": 1, "host_id": HOST_ID})),
            _ => Reply::Close(4403, "revoked".into()),
        }),
        PeerOpts::default(),
    )
    .await;
    let mut record = list_pairings(dir_s.clone()).unwrap().remove(0);
    record.endpoints[0].url = mid.url.clone();
    record.host_static_pubkey = mid.host_pubkey.to_vec();
    ainb_wire_mobile::pairing::save(dir.path(), record, TOKEN).unwrap();
    let host = connect_host(ConnectParams {
        host_id: HOST_ID.into(),
        custody_dir: dir_s.clone(),
        log_dir: dir_s.clone(),
    })
    .await
    .unwrap();
    assert_eq!(list_pairings(dir_s.clone()).unwrap()[0].repair, None);
    assert!(Arc::clone(&host).roster_status().await.is_err());
    assert!(matches!(
        Arc::clone(&host).next_event().await,
        ainb_wire_mobile::records::WireEvent::Closed {
            code: Some(4403),
            ..
        }
    ));
    assert_eq!(
        list_pairings(dir_s.clone()).unwrap()[0].repair.as_deref(),
        Some("revoked"),
        "a mid-session 4403 sets the re-pair latch"
    );

    // A successful pair with the host clears it; the pinned key belongs to
    // the earlier host, so forget it first (the explicit user action).
    forget_pairing(dir_s.clone(), HOST_ID.into()).unwrap();
    let fresh = spawn(
        issuing_host(Arc::new(AtomicBool::new(false)), None),
        PeerOpts::default(),
    )
    .await;
    let record = pair(
        offer_for(&fresh, fresh.host_pubkey, vec![live(&fresh)]),
        "my phone".into(),
        dir_s.clone(),
        dir_s.clone(),
    )
    .await
    .unwrap();
    assert_eq!(
        (record.repair.as_deref(), record.notice.as_deref()),
        (None, None)
    );
    assert_eq!(list_pairings(dir_s.clone()).unwrap()[0].repair, None);

    forget_pairing(dir_s.clone(), HOST_ID.into()).unwrap();
    assert!(list_pairings(dir_s).unwrap().is_empty());
}
