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
    /// The re-pair latch: set when the session layer sees a 4401 or 4403
    /// close from this host, cleared only by a successful pair. The app
    /// shows it as `HostRow.repair` and keeps no copy.
    #[serde(default)]
    pub repair: bool,
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

fn write_index(custody_dir: &Path, records: &[PairingRecord]) -> Result<(), WireError> {
    std::fs::create_dir_all(custody_dir).map_err(index_error)?;
    let tmp = custody_dir.join(format!("{INDEX_FILE}.tmp"));
    let body = serde_json::to_vec_pretty(records).map_err(index_error)?;
    std::fs::write(&tmp, body)
        .and_then(|()| std::fs::rename(&tmp, custody_dir.join(INDEX_FILE)))
        .map_err(index_error)
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
    let mut records = list(custody_dir)?;
    records.retain(|r| r.host_id != record.host_id);
    records.push(record);
    write_index(custody_dir, &records)
}

/// Set or clear the re-pair latch on `host_id`; absent is not an error.
pub fn mark_repair(custody_dir: &Path, host_id: &str, repair: bool) -> Result<(), WireError> {
    let mut records = list(custody_dir)?;
    let mut changed = false;
    for r in records.iter_mut().filter(|r| r.host_id == host_id) {
        changed |= r.repair != repair;
        r.repair = repair;
    }
    if changed {
        write_index(custody_dir, &records)?;
    }
    Ok(())
}

/// Forget a pairing: the token and the record. Absent is not an error.
pub fn forget(custody_dir: &Path, host_id: &str) -> Result<(), WireError> {
    delete_secret(custody_dir, &token_secret(host_id))?;
    let mut records = list(custody_dir)?;
    records.retain(|r| r.host_id != host_id);
    write_index(custody_dir, &records)
}

/// The device token for `host_id`, for the crate's own hello.
pub(crate) fn token(custody_dir: &Path, host_id: &str) -> Result<Option<String>, WireError> {
    Ok(load_secret(custody_dir, &token_secret(host_id))?.and_then(|b| String::from_utf8(b).ok()))
}

fn carrier_of(name: &str) -> Result<CarrierKind, WireError> {
    serde_json::from_value(serde_json::Value::String(name.to_owned())).map_err(WireError::protocol)
}

/// Dial `endpoints` in order with the pinned key; the first that opens wins.
/// A refusal that is not a dial failure (a changed key, a bad prologue) is
/// returned at once: the next endpoint reaches the same host.
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
        let mut config = ConnectConfig::new(
            endpoint.url.clone(),
            carrier_of(&endpoint.carrier)?,
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

/// Redeem `uri` as `display_name`: dial, `device/redeem`, `auth/hello`, save.
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
    let key = DeviceKey::load_or_create(custody_dir)?;
    let session = dial(
        &endpoints,
        &offer.host_id,
        offer.host_static_pubkey,
        &key,
        log,
    )
    .await?;
    let outcome = redeem_and_hello(&session, &offer, display_name).await;
    session.close();
    let (redeemed, hello) = outcome?;
    let record = PairingRecord {
        host_id: offer.host_id.as_str().to_owned(),
        host_static_pubkey: offer.host_static_pubkey.to_vec(),
        endpoints,
        device_id: redeemed.device_id,
        display_name: display_name.to_owned(),
        scope: redeemed.scope.base().as_str().to_owned(),
        admin: redeemed.scope.admin(),
        expires_at_ms: redeemed.expires_at_ms,
        paired_at_ms: now_ms(),
        repair: false,
    };
    save(custody_dir, record.clone(), &redeemed.device_token)?;
    Ok((record, hello))
}

async fn redeem_and_hello(
    session: &Session,
    offer: &PairingOffer,
    display_name: &str,
) -> Result<(DeviceRedeemResult, HelloSummary), WireError> {
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
    let hello = crate::api::hello(
        session,
        &redeemed.device_token,
        &redeemed.device_id,
        display_name,
    )
    .await?;
    Ok((redeemed, hello))
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
            repair: false,
        }
    }

    #[test]
    fn the_repair_latch_sets_clears_and_survives_an_old_index() {
        let dir = tempfile::tempdir().unwrap();
        save(dir.path(), record("h1"), "mdd_one").unwrap();
        mark_repair(dir.path(), "h1", true).unwrap();
        assert!(find(dir.path(), "h1").unwrap().unwrap().repair);
        mark_repair(dir.path(), "nope", true).unwrap();
        mark_repair(dir.path(), "h1", false).unwrap();
        assert!(!find(dir.path(), "h1").unwrap().unwrap().repair);
        // An index written before the field reads as not latched.
        let mut old = serde_json::to_value(vec![record("h2")]).unwrap();
        old[0].as_object_mut().unwrap().remove("repair");
        std::fs::write(dir.path().join(INDEX_FILE), old.to_string()).unwrap();
        assert!(!find(dir.path(), "h2").unwrap().unwrap().repair);
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
    fn carriers_parse_by_wire_name_and_a_new_one_reads_as_unknown() {
        assert_eq!(carrier_of("ssh-l").unwrap(), CarrierKind::SshL);
        assert_eq!(carrier_of("tailnet").unwrap(), CarrierKind::Tailnet);
        // The prologue refuses `Unknown`, so a dial on it fails before any
        // bytes leave the phone.
        assert_eq!(carrier_of("carrier-pigeon").unwrap(), CarrierKind::Unknown);
    }
}
