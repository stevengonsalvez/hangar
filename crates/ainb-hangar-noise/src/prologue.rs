//! The Noise prologue: seven fields, each key and value a u16 big-endian
//! length followed by its bytes, in this order.
//!
//! ```text
//! protocol=ainb-peer-ws  framing=1  payload_kinds=binary  initiator=client
//! responder=host  transport=<CarrierKind>  host_id=<26-char ULID>
//! ```
//!
//! Both sides mix the prologue into the handshake hash, so a phone that dialed
//! the wrong host, over the wrong carrier, or with another framing fails at
//! Noise message 1 rather than later. The spike encodings are kept byte for
//! byte except `host_id`, which is the 26-character ULID (spike 3 used 16).

use ainb_hangar_proto::hosts::{CarrierKind, HostId, HostIdError};

/// The prologue fields before `transport` and `host_id`, in order.
pub const FIXED_FIELDS: [(&str, &str); 5] = [
    ("protocol", "ainb-peer-ws"),
    ("framing", "1"),
    ("payload_kinds", "binary"),
    ("initiator", "client"),
    ("responder", "host"),
];

fn push(out: &mut Vec<u8>, field: &str) {
    let len = u16::try_from(field.len()).unwrap_or(u16::MAX);
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(&field.as_bytes()[..usize::from(len)]);
}

/// The prologue bytes for a session to `host_id` over `carrier`.
///
/// `host_id` must be minted: a peer is never `local`.
pub fn prologue(carrier: CarrierKind, host_id: &HostId) -> Result<Vec<u8>, HostIdError> {
    let host_id = HostId::parse_minted(host_id.as_str())?;
    let mut out = Vec::with_capacity(160);
    for (key, value) in FIXED_FIELDS {
        push(&mut out, key);
        push(&mut out, value);
    }
    push(&mut out, "transport");
    push(&mut out, carrier.as_str());
    push(&mut out, "host_id");
    push(&mut out, host_id.as_str());
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_prologue_refuses_local_and_names_every_field() {
        assert_eq!(
            prologue(CarrierKind::SshL, &HostId::local()),
            Err(HostIdError::LocalNotAllowed)
        );
        let host = HostId::parse("01K5A0000000000000000ABCDE").unwrap();
        let bytes = prologue(CarrierKind::Tailnet, &host).unwrap();
        assert_eq!(&bytes[..2], &[0, 8]);
        assert_eq!(&bytes[2..10], b"protocol");
        assert!(bytes.ends_with(b"\x00\x07host_id\x00\x1a01K5A0000000000000000ABCDE"));
    }

    #[test]
    fn a_different_carrier_or_host_changes_the_bytes() {
        let a = HostId::parse("01K5A0000000000000000ABCDE").unwrap();
        let b = HostId::parse("01K5A0000000000000000ABCDF").unwrap();
        let base = prologue(CarrierKind::Tailnet, &a).unwrap();
        assert_ne!(base, prologue(CarrierKind::SshL, &a).unwrap());
        assert_ne!(base, prologue(CarrierKind::Tailnet, &b).unwrap());
    }
}
