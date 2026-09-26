//! The pairing offer: `ainb://pair#` + base64url(JSON), no padding.
//!
//! The offer rides in the URI FRAGMENT, so it is never sent in a request line
//! or written to an access log. The QR code carries the same string.
//!
//! ```json
//! { "v": 1, "host_id": "<ULID>", "host_static_pubkey": "<b64url 32 bytes>",
//!   "endpoints": [{ "carrier": "tailnet", "url": "ws://h:47300/peer" }],
//!   "invite_id": "<ULID>", "invite_secret": "<b64url 32 bytes>",
//!   "expires_at_ms": 0, "relay": null }
//! ```
//!
//! Every timestamp is `*_ms`. An unknown `v`, a `local` host id, or a key that
//! is not exactly 32 bytes is refused.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};

use ainb_hangar_proto::hosts::{CarrierKind, HostId};

/// The URI prefix; the payload follows the `#`.
pub const URI_PREFIX: &str = "ainb://pair#";
/// The only offer version this build reads and writes.
pub const OFFER_VERSION: u32 = 1;

/// One address the host listens on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Endpoint {
    /// The carrier.
    pub carrier: CarrierKind,
    /// `ws://host:port/peer`.
    pub url: String,
}

/// A pairing offer (version 1).
///
/// `Debug` redacts [`Self::invite_secret`] and the contents of
/// [`Self::relay`]; the host key is public and prints.
/// [`Self::to_uri`] carries the secret, so never log the URI either.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingOffer {
    /// Always [`OFFER_VERSION`].
    pub v: u32,
    /// The host, always minted.
    pub host_id: HostId,
    /// The host's Noise static public key.
    #[serde(with = "b64_32")]
    pub host_static_pubkey: [u8; 32],
    /// Where to dial, in preference order.
    pub endpoints: Vec<Endpoint>,
    /// The invite, a ULID.
    pub invite_id: String,
    /// The single-use invite secret.
    #[serde(with = "b64_32")]
    pub invite_secret: [u8; 32],
    /// Unix milliseconds the invite expires.
    pub expires_at_ms: i64,
    /// The D12 relay slot; always absent in R1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relay: Option<serde_json::Value>,
}

impl std::fmt::Debug for PairingOffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self {
            v,
            host_id,
            host_static_pubkey,
            endpoints,
            invite_id,
            invite_secret: _,
            expires_at_ms,
            relay,
        } = self;
        f.debug_struct("PairingOffer")
            .field("v", v)
            .field("host_id", host_id)
            .field("host_static_pubkey", host_static_pubkey)
            .field("endpoints", endpoints)
            .field("invite_id", invite_id)
            .field("invite_secret", &ainb_hangar_proto::Redacted)
            .field("expires_at_ms", expires_at_ms)
            // The reserved D12 relay slot may one day carry relay
            // credentials, so only its presence prints.
            .field(
                "relay",
                &relay.as_ref().map(|_| ainb_hangar_proto::Redacted),
            )
            .finish()
    }
}

/// Why a string is not a pairing offer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OfferError {
    /// Not an `ainb://pair#` URI.
    Prefix,
    /// The fragment is not base64url.
    Base64,
    /// The payload is not an offer (including a bad key length or host id).
    Json(String),
    /// An offer version this build does not read.
    Version(u32),
    /// A `local` host id: a peer is always minted.
    LocalHost,
}

impl std::fmt::Display for OfferError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Prefix => write!(f, "not an {URI_PREFIX} uri"),
            Self::Base64 => f.write_str("offer fragment is not base64url"),
            Self::Json(e) => write!(f, "offer payload is invalid: {e}"),
            Self::Version(v) => write!(f, "unsupported offer version {v}"),
            Self::LocalHost => f.write_str("offer names the local host"),
        }
    }
}

impl std::error::Error for OfferError {}

impl PairingOffer {
    /// The `ainb://pair#…` URI.
    #[must_use]
    pub fn to_uri(&self) -> String {
        let json = serde_json::to_vec(self).unwrap_or_default();
        format!("{URI_PREFIX}{}", URL_SAFE_NO_PAD.encode(json))
    }

    /// Parse an `ainb://pair#…` URI.
    pub fn parse(uri: &str) -> Result<Self, OfferError> {
        let payload = uri.strip_prefix(URI_PREFIX).ok_or(OfferError::Prefix)?;
        let json = URL_SAFE_NO_PAD.decode(payload).map_err(|_| OfferError::Base64)?;
        let value: serde_json::Value =
            serde_json::from_slice(&json).map_err(|e| OfferError::Json(e.to_string()))?;
        let version = value.get("v").and_then(serde_json::Value::as_u64);
        if version != Some(u64::from(OFFER_VERSION)) {
            let seen = version.and_then(|v| u32::try_from(v).ok()).unwrap_or(0);
            return Err(OfferError::Version(seen));
        }
        let offer: Self =
            serde_json::from_value(value).map_err(|e| OfferError::Json(e.to_string()))?;
        if offer.host_id.is_local() {
            return Err(OfferError::LocalHost);
        }
        Ok(offer)
    }
}

mod b64_32 {
    use base64::Engine as _;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use serde::{Deserialize, Deserializer, Serializer, de::Error as _};

    pub fn serialize<S: Serializer>(bytes: &[u8; 32], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&URL_SAFE_NO_PAD.encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 32], D::Error> {
        let text = String::deserialize(d)?;
        let raw = URL_SAFE_NO_PAD.decode(text).map_err(D::Error::custom)?;
        <[u8; 32]>::try_from(raw.as_slice())
            .map_err(|_| D::Error::custom(format!("expected 32 bytes, got {}", raw.len())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> PairingOffer {
        PairingOffer {
            v: 1,
            host_id: HostId::parse("01K5A0000000000000000ABCDE").unwrap(),
            host_static_pubkey: [7; 32],
            endpoints: vec![Endpoint {
                carrier: CarrierKind::Tailnet,
                url: "ws://100.64.0.1:47300/peer".to_string(),
            }],
            invite_id: "01K5A0000000000000000QRSTV".to_string(),
            invite_secret: [9; 32],
            expires_at_ms: 1_700_000_000_000,
            relay: None,
        }
    }

    #[test]
    fn an_offer_round_trips_through_its_uri() {
        let offer = sample();
        let uri = offer.to_uri();
        assert!(uri.starts_with(URI_PREFIX));
        assert!(!uri.contains('='), "no padding");
        assert_eq!(PairingOffer::parse(&uri), Ok(offer));
    }

    #[test]
    fn bad_offers_are_refused() {
        assert_eq!(PairingOffer::parse("https://x"), Err(OfferError::Prefix));
        assert_eq!(
            PairingOffer::parse("ainb://pair#***"),
            Err(OfferError::Base64)
        );
        let mut v2 = serde_json::to_value(sample()).unwrap();
        v2["v"] = serde_json::json!(2);
        let uri = format!(
            "{URI_PREFIX}{}",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&v2).unwrap())
        );
        assert_eq!(PairingOffer::parse(&uri), Err(OfferError::Version(2)));
        let mut local = serde_json::to_value(sample()).unwrap();
        local["host_id"] = serde_json::json!("local");
        let uri = format!(
            "{URI_PREFIX}{}",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&local).unwrap())
        );
        assert_eq!(PairingOffer::parse(&uri), Err(OfferError::LocalHost));
        let mut short = serde_json::to_value(sample()).unwrap();
        short["invite_secret"] = serde_json::json!(URL_SAFE_NO_PAD.encode([1u8; 31]));
        let uri = format!(
            "{URI_PREFIX}{}",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&short).unwrap())
        );
        assert!(matches!(
            PairingOffer::parse(&uri),
            Err(OfferError::Json(_))
        ));
    }

    /// The invite secret never reaches a `Debug` rendering in any encoding:
    /// raw bytes, base64url (the wire and URI form) or hex.
    #[test]
    fn debug_redacts_the_invite_secret() {
        let offer = PairingOffer {
            invite_secret: [0xa7; 32],
            ..sample()
        };
        let secret_b64 = URL_SAFE_NO_PAD.encode(offer.invite_secret);
        let secret_bytes = format!("{:?}", offer.invite_secret);
        let secret_hex = offer.invite_secret.iter().fold(String::new(), |mut out, b| {
            use std::fmt::Write as _;
            let _ = write!(out, "{b:02x}");
            out
        });
        for rendered in [format!("{offer:?}"), format!("{offer:#?}")] {
            assert!(rendered.contains("<redacted>"), "{rendered}");
            assert!(!rendered.contains(&secret_b64), "{rendered}");
            assert!(!rendered.contains(&secret_bytes), "{rendered}");
            assert!(!rendered.contains(&secret_hex), "{rendered}");
            assert!(
                rendered.contains("01K5A0000000000000000ABCDE"),
                "{rendered}"
            );
        }
    }

    /// The relay slot prints only whether it is set.
    #[test]
    fn debug_redacts_the_relay_contents() {
        let offer = PairingOffer {
            relay: Some(serde_json::json!({"url": "wss://relay", "token": "s3cr3tRelay"})),
            ..sample()
        };
        let rendered = format!("{offer:?}");
        assert!(!rendered.contains("s3cr3tRelay"), "{rendered}");
        assert!(rendered.contains("relay: Some(<redacted>)"), "{rendered}");
        assert!(format!("{:?}", sample()).contains("relay: None"));
    }
}
