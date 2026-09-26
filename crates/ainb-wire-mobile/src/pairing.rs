//! Pairing (M1-12): an `ainb://pair#…` offer becomes a device token bound to
//! this phone's Noise static key.
//!
//! ```text
//! offer URI ──parse──▶ dial endpoints in order (Noise IK, pinned host key)
//!           ──▶ device/redeem {invite_id, invite_secret, display_name, protocol}
//!           ──▶ auth/hello {token, device}  ──▶ record saved, token in custody
//! ```
//!
//! The token is a secret and lives in the custody backend under
//! `token-<host_id>`; the record beside it (host id, host key, endpoints,
//! device id, scope, expiry) is not, and lives in `pairings.json` under the
//! custody dir. The app never sees the token: `connect_host` takes a host id.

use std::path::Path;
use std::sync::Arc;

use ainb_hangar_noise::PairingOffer;
use ainb_hangar_proto::devices::{DeviceRedeemParams, DeviceRedeemResult};
use ainb_hangar_proto::hosts::{CarrierKind, HostId};
use ainb_hangar_proto::methods;
use ainb_hangar_proto::protocol::ProtocolRange;
use base64::Engine as _;
use serde::{Deserialize, Serialize};

use crate::connlog::{ConnLog, now_ms};
use crate::custody::{DeviceKey, delete_secret, load_secret, store_secret};
use crate::records::{HelloSummary, WireError};
use crate::session::{ConnectConfig, Session};

/// The non-secret pairing index under the custody dir.
pub const INDEX_FILE: &str = "pairings.json";

/// One endpoint of a host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, uniffi::Record)]
pub struct EndpointRecord {
    /// `tailnet`, `lan` or `ssh-l`.
    pub carrier: String,
    /// `ws://host:port/peer`.
    pub url: String,
}

/// One paired host, as the app lists it. Carries no secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, uniffi::Record)]
pub struct PairingRecord {
    /// The host, a ULID.
    pub host_id: String,
    /// The host's Noise static public key, pinned at pairing.
    pub host_static_pubkey: Vec<u8>,
    /// Where to dial, in preference order.
    pub endpoints: Vec<EndpointRecord>,
    /// The device id the host minted.
    pub device_id: String,
    /// The name this device was paired under.
    pub display_name: String,
    /// The scope base (`mobile`, `mobile+type`, `desktop`).
    pub scope: String,
    /// Whether the scope carries admin.
    pub admin: bool,
    /// When the token expires unless a hello slides it, epoch ms.
    pub expires_at_ms: i64,
    /// When the pairing happened, epoch ms.
    pub paired_at_ms: i64,
    /// The re-pair latch: why the host refused this device's identity, when
    /// it did. `peer_changed` (a wrong pinned host key, or 4401 before Noise
    /// message 2), `unauthenticated` (4401) or `revoked` (4403). Set under
    /// the index lock when the session layer sees the refusal, cleared only
    /// by a successful pair. The app shows it as `HostRow.repair` and keeps
    /// no copy.
    #[serde(
        default,
        deserialize_with = "flag",
        skip_serializing_if = "Option::is_none"
    )]
    pub repair: Option<String>,
    /// A notice the app shows without a re-pair: `incompatible` (4409, one
    /// side needs an update) or `unknown_code` (a close code this build does
    /// not know, in the daemon's 4xxx range; a standard WebSocket close is a
    /// network loss and sets nothing). Set like `repair`; cleared by a
    /// successful hello as well as by a successful pair.
    #[serde(
        default,
        deserialize_with = "flag",
        skip_serializing_if = "Option::is_none"
    )]
    pub notice: Option<String>,
}

/// A flag on the index: a string, absent, or the `true` / `false` an index
/// written by an earlier build carries (read as `revoked` / clear).
fn flag<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Raw {
        Text(String),
        Old(bool),
        None(()),
    }
    Ok(match Option::<Raw>::deserialize(d)? {
        Some(Raw::Text(s)) => Some(s),
        Some(Raw::Old(true)) => Some(REPAIR_REVOKED.to_owned()),
        _ => None,
    })
}

/// `repair` value: a wrong pinned host key, or 4401 before Noise message 2.
pub const REPAIR_PEER_CHANGED: &str = "peer_changed";
/// `repair` value: 4401.
pub const REPAIR_UNAUTHENTICATED: &str = "unauthenticated";
/// `repair` value: 4403.
pub const REPAIR_REVOKED: &str = "revoked";
/// `notice` value: 4409.
pub const NOTICE_INCOMPATIBLE: &str = "incompatible";
/// `notice` value: a close code this build does not know.
pub const NOTICE_UNKNOWN_CODE: &str = "unknown_code";

/// What a refusal sets on the record: `(repair, notice)`. A retryable close
/// (no code, 4429, 1013, 4503) sets nothing.
#[must_use]
pub fn refusal_flags(err: &WireError) -> (Option<&'static str>, Option<&'static str>) {
    use ainb_hangar_proto::peer_close as pc;
    match err {
        WireError::PeerChanged => (Some(REPAIR_PEER_CHANGED), None),
        WireError::Closed {
            code: Some(code),
            retryable: false,
            ..
        } => match *code {
            pc::UNAUTHENTICATED => (Some(REPAIR_UNAUTHENTICATED), None),
            pc::REVOKED => (Some(REPAIR_REVOKED), None),
            pc::PROTOCOL_INCOMPATIBLE => (None, Some(NOTICE_INCOMPATIBLE)),
            // Only the daemon's own range is a code worth a notice; the
            // session layer already treats standard codes as retryable.
            code if code >= 4000 => (None, Some(NOTICE_UNKNOWN_CODE)),
            _ => (None, None),
        },
        _ => (None, None),
    }
}

fn token_secret(host_id: &str) -> String {
    format!("token-{host_id}")
}

fn index_error(e: impl std::fmt::Display) -> WireError {
    WireError::Custody {
        message: format!("pairing index: {e}"),
    }
}

/// Every pairing, oldest first.
pub fn list(custody_dir: &Path) -> Result<Vec<PairingRecord>, WireError> {
    match std::fs::read(custody_dir.join(INDEX_FILE)) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(index_error),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(index_error(e)),
    }
}

/// The lock file beside the index; every writer holds it exclusively.
pub const LOCK_FILE: &str = "pairings.lock";

/// The one writer path: an exclusive file lock, read the index, apply
/// `change`, write to a temp file and rename it over the index. Two
/// callers in one process or two processes cannot erase each other's
/// records, and a kill mid-write leaves the old index, never a torn one.
fn with_index<T>(
    custody_dir: &Path,
    change: impl FnOnce(&mut Vec<PairingRecord>) -> T,
) -> Result<T, WireError> {
    std::fs::create_dir_all(custody_dir).map_err(index_error)?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(custody_dir.join(LOCK_FILE))
        .map_err(index_error)?;
    lock.lock().map_err(index_error)?;
    let mut records = list(custody_dir)?;
    let out = change(&mut records);
    let tmp = custody_dir.join(format!("{INDEX_FILE}.tmp"));
    let body = serde_json::to_vec_pretty(&records).map_err(index_error)?;
    std::fs::File::create(&tmp)
        .and_then(|mut f| {
            std::io::Write::write_all(&mut f, &body)?;
            // Durable before the rename: a power loss after the rename
            // must not leave an empty index behind the new name.
            f.sync_all()
        })
        .and_then(|()| std::fs::rename(&tmp, custody_dir.join(INDEX_FILE)))
        .map_err(index_error)?;
    let _ = lock.unlock();
    Ok(out)
}

/// The pairing for `host_id`, when one exists.
pub fn find(custody_dir: &Path, host_id: &str) -> Result<Option<PairingRecord>, WireError> {
    Ok(list(custody_dir)?.into_iter().find(|r| r.host_id == host_id))
}

/// Save a pairing: the token into custody, the record into the index,
/// replacing any earlier pairing with the same host.
pub fn save(custody_dir: &Path, record: PairingRecord, token: &str) -> Result<(), WireError> {
    store_secret(
        custody_dir,
        &token_secret(&record.host_id),
        token.as_bytes(),
    )?;
    with_index(custody_dir, |records| {
        records.retain(|r| r.host_id != record.host_id);
        records.push(record);
    })
}

/// Record a refusal from `host_id` on its pairing, under the index lock:
/// `repair` for an identity refusal, `notice` for an incompatible or unknown
/// close. A retryable error sets nothing. An absent host is not an error.
pub fn mark_refusal(custody_dir: &Path, host_id: &str, err: &WireError) -> Result<(), WireError> {
    let (repair, notice) = refusal_flags(err);
    if repair.is_none() && notice.is_none() {
        return Ok(());
    }
    with_index(custody_dir, |records| {
        for r in records.iter_mut().filter(|r| r.host_id == host_id) {
            if let Some(repair) = repair {
                r.repair = Some(repair.to_owned());
            }
            if let Some(notice) = notice {
                r.notice = Some(notice.to_owned());
            }
        }
    })
}

/// Put a refusal on the record and never let the bookkeeping replace the
/// refusal itself: an index that cannot be written is logged by the caller's
/// connection log through the close line, and the host's word stands.
pub(crate) fn latch(custody_dir: &Path, host_id: &str, err: &WireError) {
    let _ = mark_refusal(custody_dir, host_id, err);
}

/// Clear `notice` on `host_id`, under the index lock: a successful hello
/// proves the host and this build agree again. `repair` is untouched; only
/// a successful pair clears that. No write when nothing is set.
pub fn clear_notice(custody_dir: &Path, host_id: &str) -> Result<(), WireError> {
    if find(custody_dir, host_id)?.is_none_or(|r| r.notice.is_none()) {
        return Ok(());
    }
    with_index(custody_dir, |records| {
        for r in records.iter_mut().filter(|r| r.host_id == host_id) {
            r.notice = None;
        }
    })
}

/// Record the token expiry a hello slid, so the host list shows the live
/// value. No write when it did not move.
pub fn note_expiry(
    custody_dir: &Path,
    host_id: &str,
    expires_at_ms: Option<i64>,
) -> Result<(), WireError> {
    let Some(expires_at_ms) = expires_at_ms else {
        return Ok(());
    };
    if find(custody_dir, host_id)?.is_none_or(|r| r.expires_at_ms == expires_at_ms) {
        return Ok(());
    }
    with_index(custody_dir, |records| {
        for r in records.iter_mut().filter(|r| r.host_id == host_id) {
            r.expires_at_ms = expires_at_ms;
        }
    })
}

/// The five latch strings the app mirrors, in one place: `repair` values
/// first, then `notice` values.
pub const LATCH_VALUES: [&str; 5] = [
    REPAIR_REVOKED,
    REPAIR_UNAUTHENTICATED,
    REPAIR_PEER_CHANGED,
    NOTICE_INCOMPATIBLE,
    NOTICE_UNKNOWN_CODE,
];

/// Clear both flags on `host_id`, under the index lock (tests and the
/// successful-pair path use `save`, which replaces the record whole).
pub fn clear_flags(custody_dir: &Path, host_id: &str) -> Result<(), WireError> {
    with_index(custody_dir, |records| {
        for r in records.iter_mut().filter(|r| r.host_id == host_id) {
            r.repair = None;
            r.notice = None;
        }
    })
}

/// Forget a pairing: the token and the record. Absent is not an error.
pub fn forget(custody_dir: &Path, host_id: &str) -> Result<(), WireError> {
    delete_secret(custody_dir, &token_secret(host_id))?;
    with_index(custody_dir, |records| {
        records.retain(|r| r.host_id != host_id);
    })
}

/// The device token for `host_id`, for the crate's own hello.
pub(crate) fn token(custody_dir: &Path, host_id: &str) -> Result<Option<String>, WireError> {
    Ok(load_secret(custody_dir, &token_secret(host_id))?.and_then(|b| String::from_utf8(b).ok()))
}

fn carrier_of(name: &str) -> Result<CarrierKind, WireError> {
    serde_json::from_value(serde_json::Value::String(name.to_owned())).map_err(WireError::protocol)
}

/// Dial `endpoints` in order with the pinned key; the first that opens wins.
/// An endpoint whose carrier this build does not know is skipped (a newer
/// daemon may list one the phone cannot name in the prologue). A refusal
/// that is not a dial failure (a changed key, a coded close) is returned at
/// once: the next endpoint reaches the same host.
pub(crate) async fn dial(
    endpoints: &[EndpointRecord],
    host_id: &HostId,
    host_static_pubkey: [u8; 32],
    key: &DeviceKey,
    log: Option<Arc<ConnLog>>,
) -> Result<Arc<Session>, WireError> {
    let mut last = WireError::Connect {
        message: "the pairing has no endpoints".to_owned(),
    };
    for endpoint in endpoints {
        let carrier = carrier_of(&endpoint.carrier)?;
        if carrier == CarrierKind::Unknown {
            last = WireError::Connect {
                message: format!(
                    "endpoint carrier {:?} is not one this build dials",
                    endpoint.carrier
                ),
            };
            continue;
        }
        let mut config = ConnectConfig::new(
            endpoint.url.clone(),
            carrier,
            host_id.clone(),
            host_static_pubkey,
            key.private().to_vec(),
        );
        config.log = log.clone();
        match Session::connect(config).await {
            Ok(session) => return Ok(session),
            Err(e @ WireError::Connect { .. }) => last = e,
            Err(e) => return Err(e),
        }
    }
    Err(last)
}

/// Redeem `uri` as `display_name`: dial, `device/redeem`, save, `auth/hello`.
/// The pairing is saved the moment redeem succeeds (the invite is burned),
/// so a hello that fails still leaves a pairing to connect later.
pub(crate) async fn pair(
    uri: &str,
    display_name: &str,
    custody_dir: &Path,
    log: Option<Arc<ConnLog>>,
) -> Result<(PairingRecord, HelloSummary), WireError> {
    let offer = PairingOffer::parse(uri).map_err(|e| WireError::Offer {
        message: e.to_string(),
    })?;
    if offer.expires_at_ms <= now_ms() {
        return Err(WireError::Offer {
            message: "the invite has expired".to_owned(),
        });
    }
    let endpoints: Vec<EndpointRecord> = offer
        .endpoints
        .iter()
        .map(|e| EndpointRecord {
            carrier: e.carrier.as_str().to_owned(),
            url: e.url.clone(),
        })
        .collect();
    // An offer is unauthenticated input (a QR code, a link, a paste). When
    // a pairing for this host already exists, only an offer carrying the key
    // already pinned may touch it: a stale or hostile offer with another key
    // is refused here, before any dial, and the record stays as it was.
    let pinned = find(custody_dir, offer.host_id.as_str())?;
    if let Some(existing) = &pinned {
        if existing.host_static_pubkey != offer.host_static_pubkey {
            return Err(WireError::Offer {
                message: "the offer's host key differs from the key pinned for this host; \
                          forget the host to pair it afresh"
                    .to_owned(),
            });
        }
    }
    let key = DeviceKey::load_or_create(custody_dir)?;
    let session = match dial(
        &endpoints,
        &offer.host_id,
        offer.host_static_pubkey,
        &key,
        log,
    )
    .await
    {
        Ok(session) => session,
        // No latch from an offer's dial: the offer's endpoints are as
        // unauthenticated as the offer, so a PeerChanged here may be a
        // hostile endpoint that never was the host. `connect_host`, which
        // dials only the stored endpoints, is where a refusal latches.
        Err(e) => return Err(e),
    };
    let redeemed = match redeem(&session, &offer, display_name).await {
        Ok(redeemed) => redeemed,
        Err(e) => {
            session.close();
            return Err(e);
        }
    };
    // The invite is single-use and burned now: persist the token and the
    // record BEFORE hello, so a hello that fails (a dropped socket, a 4401
    // from a clock skew) leaves a pairing to retry, never a lost token.
    let record = PairingRecord {
        host_id: offer.host_id.as_str().to_owned(),
        host_static_pubkey: offer.host_static_pubkey.to_vec(),
        endpoints,
        device_id: redeemed.device_id.clone(),
        display_name: display_name.to_owned(),
        scope: redeemed.scope.base().as_str().to_owned(),
        admin: redeemed.scope.admin(),
        expires_at_ms: redeemed.expires_at_ms,
        paired_at_ms: now_ms(),
        repair: None,
        notice: None,
    };
    if let Err(e) = save(custody_dir, record.clone(), &redeemed.device_token) {
        session.close();
        return Err(e);
    }
    let hello = match crate::api::hello(
        &session,
        &redeemed.device_token,
        &redeemed.device_id,
        display_name,
    )
    .await
    {
        Ok(hello) => Ok(hello),
        Err(e) => Err(crate::api::hello_refusal(&session, e).await),
    };
    session.close();
    match hello {
        Ok(hello) => Ok((record, hello)),
        Err(e) => {
            // The pairing stays saved to retry; a 4401 or 4403 on the very
            // first hello is still the host refusing this device (the peer
            // authenticated with the pinned key), so the re-pair latch is set
            // exactly as it would be on a later connect.
            latch(custody_dir, &record.host_id, &e);
            Err(e)
        }
    }
}

async fn redeem(
    session: &Session,
    offer: &PairingOffer,
    display_name: &str,
) -> Result<DeviceRedeemResult, WireError> {
    let params = DeviceRedeemParams {
        invite_id: offer.invite_id.clone(),
        invite_secret: base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(offer.invite_secret),
        display_name: display_name.to_owned(),
        protocol: ProtocolRange::supported(),
    };
    let redeemed: DeviceRedeemResult = session.call(methods::DEVICE_REDEEM, &params).await?;
    if redeemed.host_id != offer.host_id {
        return Err(WireError::Protocol {
            message: format!(
                "redeem answered for host {} but the offer named {}",
                redeemed.host_id.as_str(),
                offer.host_id.as_str()
            ),
        });
    }
    Ok(redeemed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(host_id: &str) -> PairingRecord {
        PairingRecord {
            host_id: host_id.to_owned(),
            host_static_pubkey: vec![1; 32],
            endpoints: vec![EndpointRecord {
                carrier: "lan".into(),
                url: "ws://127.0.0.1:1/peer".into(),
            }],
            device_id: "d1".into(),
            display_name: "phone".into(),
            scope: "mobile".into(),
            admin: false,
            expires_at_ms: 1,
            paired_at_ms: 2,
            repair: None,
            notice: None,
        }
    }

    fn closed(code: Option<u16>, retryable: bool) -> WireError {
        WireError::Closed {
            code,
            reason: String::new(),
            retryable,
            retry_after_ms: None,
        }
    }

    #[test]
    fn refusals_set_repair_or_notice_and_retryable_closes_set_nothing() {
        assert_eq!(
            refusal_flags(&WireError::PeerChanged),
            (Some("peer_changed"), None)
        );
        assert_eq!(
            refusal_flags(&closed(Some(4401), false)),
            (Some("unauthenticated"), None)
        );
        assert_eq!(
            refusal_flags(&closed(Some(4403), false)),
            (Some("revoked"), None)
        );
        assert_eq!(
            refusal_flags(&closed(Some(4409), false)),
            (None, Some("incompatible"))
        );
        assert_eq!(
            refusal_flags(&closed(Some(4999), false)),
            (None, Some("unknown_code"))
        );
        assert_eq!(refusal_flags(&closed(None, true)), (None, None));
        assert_eq!(refusal_flags(&closed(Some(4429), true)), (None, None));
        assert_eq!(refusal_flags(&closed(Some(4503), true)), (None, None));

        let dir = tempfile::tempdir().unwrap();
        save(dir.path(), record("h1"), "mdd_one").unwrap();
        mark_refusal(dir.path(), "h1", &closed(Some(4403), false)).unwrap();
        mark_refusal(dir.path(), "h1", &closed(Some(4409), false)).unwrap();
        let r = find(dir.path(), "h1").unwrap().unwrap();
        assert_eq!(
            (r.repair.as_deref(), r.notice.as_deref()),
            (Some("revoked"), Some("incompatible"))
        );
        mark_refusal(dir.path(), "h1", &closed(None, true)).unwrap();
        assert_eq!(
            find(dir.path(), "h1").unwrap().unwrap().repair.as_deref(),
            Some("revoked")
        );
        mark_refusal(dir.path(), "nope", &WireError::PeerChanged).unwrap();
        clear_flags(dir.path(), "h1").unwrap();
        let r = find(dir.path(), "h1").unwrap().unwrap();
        assert_eq!((r.repair, r.notice), (None, None));
        // A successful pair replaces the record whole, flags cleared.
        mark_refusal(dir.path(), "h1", &WireError::PeerChanged).unwrap();
        save(dir.path(), record("h1"), "mdd_two").unwrap();
        assert_eq!(find(dir.path(), "h1").unwrap().unwrap().repair, None);
        // An index written before the fields, or with the old bool, reads.
        let mut old = serde_json::to_value(vec![record("h2"), record("h3")]).unwrap();
        old[0].as_object_mut().unwrap().remove("repair");
        old[0].as_object_mut().unwrap().remove("notice");
        old[1]["repair"] = serde_json::json!(true);
        std::fs::write(dir.path().join(INDEX_FILE), old.to_string()).unwrap();
        assert_eq!(find(dir.path(), "h2").unwrap().unwrap().repair, None);
        assert_eq!(
            find(dir.path(), "h3").unwrap().unwrap().repair.as_deref(),
            Some("revoked")
        );
    }

    #[test]
    fn save_list_replace_and_forget_keep_the_token_out_of_the_index() {
        let dir = tempfile::tempdir().unwrap();
        assert!(list(dir.path()).unwrap().is_empty());
        save(dir.path(), record("h1"), "mdd_one").unwrap();
        save(dir.path(), record("h2"), "mdd_two").unwrap();
        let mut again = record("h1");
        again.device_id = "d1b".into();
        save(dir.path(), again, "mdd_one_b").unwrap();
        let listed = list(dir.path()).unwrap();
        assert_eq!(
            listed.iter().map(|r| r.host_id.as_str()).collect::<Vec<_>>(),
            ["h2", "h1"]
        );
        assert_eq!(find(dir.path(), "h1").unwrap().unwrap().device_id, "d1b");
        assert_eq!(
            token(dir.path(), "h1").unwrap().as_deref(),
            Some("mdd_one_b")
        );
        let index = std::fs::read_to_string(dir.path().join(INDEX_FILE)).unwrap();
        assert!(!index.contains("mdd_"), "the index holds no token");
        forget(dir.path(), "h1").unwrap();
        forget(dir.path(), "h1").unwrap();
        assert!(find(dir.path(), "h1").unwrap().is_none());
        assert_eq!(token(dir.path(), "h1").unwrap(), None);
        assert_eq!(token(dir.path(), "h2").unwrap().as_deref(), Some("mdd_two"));
    }

    #[test]
    fn two_concurrent_writers_lose_no_record() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        let writers: Vec<_> = (0..2)
            .map(|w| {
                let path = path.clone();
                std::thread::spawn(move || {
                    for i in 0..50 {
                        save(&path, record(&format!("w{w}-{i}")), "mdd_x").unwrap();
                    }
                })
            })
            .collect();
        for w in writers {
            w.join().unwrap();
        }
        let listed = list(dir.path()).unwrap();
        assert_eq!(listed.len(), 100, "every save survived the other writer");
        let text = std::fs::read_to_string(dir.path().join(INDEX_FILE)).unwrap();
        assert!(serde_json::from_str::<Vec<PairingRecord>>(&text).is_ok());
        assert!(!dir.path().join(format!("{INDEX_FILE}.tmp")).exists());
    }

    #[test]
    fn carriers_parse_by_wire_name_and_a_new_one_reads_as_unknown() {
        assert_eq!(carrier_of("ssh-l").unwrap(), CarrierKind::SshL);
        assert_eq!(carrier_of("tailnet").unwrap(), CarrierKind::Tailnet);
        // The prologue refuses `Unknown`, so a dial on it fails before any
        // bytes leave the phone.
        assert_eq!(carrier_of("carrier-pigeon").unwrap(), CarrierKind::Unknown);
    }
}
