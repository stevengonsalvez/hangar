//! The peer wire, version 1, against its committed golden bytes.
//!
//! A Contracts-job gate. `tests/fixtures/peer_v1.json` records the frame header
//! bytes, the prologue bytes for a 26-character host id, a pairing offer URI,
//! and the opcode registry. The phone crate and the daemon both build against
//! these bytes, so any change here breaks a peer that was never rebuilt: it is
//! a `PROTOCOL_VERSION` bump, never a quiet edit.
//!
//! Never rerun this job to green it. After an INTENDED change, regenerate with
//! `UPDATE_PEER_GOLDEN=1 cargo test -p ainb-hangar-noise --test wire_contract`
//! and commit the fixture diff.

use ainb_hangar_noise::frame::FrameHeader;
use ainb_hangar_noise::offer::{Endpoint, OfferError, PairingOffer};
use ainb_hangar_noise::opcode::{Opcode, RETIRED};
use ainb_hangar_noise::{PrologueError, prologue};
use ainb_hangar_proto::hosts::{CarrierKind, HostId, HostIdError};
use serde_json::{Value, json};

const FIXTURE_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/peer_v1.json");
const COMMITTED: &str = include_str!("fixtures/peer_v1.json");
const HOST: &str = "01K5A0000000000000000ABCDE";

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

fn headers() -> Vec<FrameHeader> {
    vec![
        FrameHeader {
            opcode: Opcode::Rpc,
            fin: true,
            stream_id: 0,
            seq: 1,
        },
        FrameHeader {
            opcode: Opcode::Rpc,
            fin: false,
            stream_id: 0,
            seq: 0x0000_0001_0000_0002,
        },
        FrameHeader {
            opcode: Opcode::Ping,
            fin: true,
            stream_id: 0,
            seq: 42,
        },
        FrameHeader {
            opcode: Opcode::Pong,
            fin: true,
            stream_id: 0,
            seq: 42,
        },
        FrameHeader {
            opcode: Opcode::StreamEnd,
            fin: true,
            stream_id: 7,
            seq: u64::MAX,
        },
    ]
}

fn offer() -> PairingOffer {
    PairingOffer {
        v: 1,
        host_id: HostId::parse(HOST).unwrap(),
        host_static_pubkey: std::array::from_fn(|i| u8::try_from(i).unwrap()),
        endpoints: vec![
            Endpoint {
                carrier: CarrierKind::Tailnet,
                url: "ws://100.64.0.1:47300/peer".to_string(),
            },
            Endpoint {
                carrier: CarrierKind::SshL,
                url: "ws://127.0.0.1:47300/peer".to_string(),
            },
        ],
        invite_id: "01K5A0000000000000000QRSTV".to_string(),
        invite_secret: std::array::from_fn(|i| 0xff - u8::try_from(i).unwrap()),
        expires_at_ms: 1_758_800_000_000,
        relay: None,
    }
}

fn render() -> Value {
    let host = HostId::parse(HOST).unwrap();
    let header_rows: Vec<Value> = headers()
        .iter()
        .map(|h| {
            json!({
                "opcode": h.opcode.name(),
                "fin": h.fin,
                "stream_id": h.stream_id,
                "seq": h.seq.to_string(),
                "hex": hex(&h.encode()),
            })
        })
        .collect();
    let prologue_rows: Vec<Value> = [CarrierKind::Tailnet, CarrierKind::Lan, CarrierKind::SshL]
        .iter()
        .map(|c| {
            json!({
                "transport": c.as_str(),
                "host_id": HOST,
                "hex": hex(&prologue(*c, &host).unwrap()),
            })
        })
        .collect();
    let opcodes: Vec<Value> = Opcode::ALL
        .iter()
        .map(|o| json!({"number": o.as_u8(), "name": o.name(), "in_use": o.in_use()}))
        .collect();
    json!({
        "version": 1,
        "header": header_rows,
        "prologue": prologue_rows,
        "offer": {
            "json": serde_json::to_value(offer()).unwrap(),
            "uri": offer().to_uri(),
        },
        "opcodes": opcodes,
        "retired_opcodes": RETIRED,
    })
}

#[test]
fn the_wire_matches_the_committed_golden() {
    let live = render();
    if std::env::var_os("UPDATE_PEER_GOLDEN").is_some() {
        let text = serde_json::to_string_pretty(&live).unwrap() + "\n";
        std::fs::write(FIXTURE_PATH, text).expect("write peer_v1.json");
        return;
    }
    let committed: Value = serde_json::from_str(COMMITTED).expect("fixture is JSON");
    assert_eq!(
        live, committed,
        "the peer wire changed. That is a PROTOCOL_VERSION bump; if intended, \
         regenerate with UPDATE_PEER_GOLDEN=1 and commit the diff"
    );
}

/// The committed bytes decode back to the headers they claim, so the fixture
/// is also a decoder test a phone implementation can reuse.
#[test]
fn golden_headers_decode() {
    let committed: Value = serde_json::from_str(COMMITTED).unwrap();
    let rows = committed["header"].as_array().unwrap();
    assert_eq!(rows.len(), headers().len());
    for (row, want) in rows.iter().zip(headers()) {
        let text = row["hex"].as_str().unwrap();
        let bytes: Vec<u8> = (0..text.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
            .collect();
        assert_eq!(bytes.len(), 16);
        assert_eq!(FrameHeader::decode(&bytes), Ok(want));
    }
}

/// The golden offer URI parses back to the same offer.
#[test]
fn golden_offer_parses() {
    let committed: Value = serde_json::from_str(COMMITTED).unwrap();
    let uri = committed["offer"]["uri"].as_str().unwrap();
    assert_eq!(PairingOffer::parse(uri), Ok(offer()));
}

/// A peer is always a minted host: `local` is refused in the prologue and in
/// the offer, and the 26-character id is the only accepted length.
#[test]
fn local_is_refused_on_the_peer_wire() {
    assert_eq!(
        prologue(CarrierKind::SshL, &HostId::local()),
        Err(PrologueError::Host(HostIdError::LocalNotAllowed))
    );
    let mut value = serde_json::to_value(offer()).unwrap();
    value["host_id"] = json!("local");
    let uri = format!(
        "ainb://pair#{}",
        base64_url(&serde_json::to_vec(&value).unwrap())
    );
    assert_eq!(PairingOffer::parse(&uri), Err(OfferError::LocalHost));
    assert!(
        HostId::parse("0123456789ABCDEF").is_err(),
        "spike 3's 16 chars"
    );
}

fn base64_url(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}
