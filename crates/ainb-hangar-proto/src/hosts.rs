//! Host identity and reachability for federation (spec D11, R1).
//!
//! Frozen by PR-0. Changing any shape here after the freeze follows the D17
//! bump rule: a new optional member is a capability string, a changed meaning
//! is a `PROTOCOL_VERSION` bump.
//!
//! ```text
//! daemon_identity.host_id (ULID) ──▶ HelloResult.host_id ──▶ HostId
//!                                                     │
//!                        PairingOffer.host_id ◀───────┤ (never "local")
//!                        peer prologue host_id ◀──────┘ (never "local")
//! ```
//!
//! A [`HostId`] is the ULID a daemon mints once into `daemon_identity`, or the
//! literal [`HostId::LOCAL`] a daemon that minted none still serves its rows
//! under. A peer is always a minted host, so the pairing offer and the Noise
//! prologue take [`HostId::parse_minted`], which refuses `local`.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Length of a canonical ULID string.
pub const ULID_LEN: usize = 26;

/// Why a string is not a [`HostId`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostIdError {
    /// Not 26 characters, and not the literal `local`.
    Length(usize),
    /// A character outside the upper-case Crockford base32 alphabet.
    Alphabet(char),
    /// The first character is above `7`, so the value overflows 128 bits.
    Overflow,
    /// `local` where a minted host is required (pairing offer, prologue).
    LocalNotAllowed,
}

impl fmt::Display for HostIdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Length(n) => write!(f, "host id must be {ULID_LEN} characters, got {n}"),
            Self::Alphabet(c) => write!(f, "host id has {c:?}, outside Crockford base32"),
            Self::Overflow => f.write_str("host id overflows a 128-bit ULID"),
            Self::LocalNotAllowed => f.write_str("a peer host id must be minted, not \"local\""),
        }
    }
}

impl std::error::Error for HostIdError {}

/// Whether `c` is in the canonical (upper-case) Crockford base32 alphabet:
/// `0-9` and `A-Z` without `I`, `L`, `O` and `U`.
#[must_use]
pub const fn is_crockford(c: char) -> bool {
    matches!(c, '0'..='9' | 'A'..='H' | 'J' | 'K' | 'M' | 'N' | 'P'..='T' | 'V'..='Z')
}

/// A host: a 26-character Crockford ULID, or the legacy `"local"`.
///
/// On the wire it is a bare JSON string (the same shape as the app's current
/// `HostId`, which becomes a re-export of this type). Decoding validates, so a
/// value that is neither a canonical ULID nor `local` never reaches a handler.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct HostId(String);

impl HostId {
    /// The id a daemon that minted none serves its rows under.
    pub const LOCAL: &'static str = "local";

    /// The legacy local host.
    #[must_use]
    pub fn local() -> Self {
        Self(Self::LOCAL.to_string())
    }

    /// Parse a host id: a canonical ULID or `local`.
    pub fn parse(value: &str) -> Result<Self, HostIdError> {
        if value == Self::LOCAL {
            return Ok(Self::local());
        }
        Self::parse_minted(value)
    }

    /// Parse a host id that must be a minted ULID. `local` is refused: a peer
    /// (pairing offer, Noise prologue) is always a host that minted its id.
    pub fn parse_minted(value: &str) -> Result<Self, HostIdError> {
        if value == Self::LOCAL {
            return Err(HostIdError::LocalNotAllowed);
        }
        let count = value.chars().count();
        if count != ULID_LEN {
            return Err(HostIdError::Length(count));
        }
        if let Some(bad) = value.chars().find(|c| !is_crockford(*c)) {
            return Err(HostIdError::Alphabet(bad));
        }
        if value.as_bytes()[0] > b'7' {
            return Err(HostIdError::Overflow);
        }
        Ok(Self(value.to_string()))
    }

    /// Whether this is the legacy `local` host.
    #[must_use]
    pub fn is_local(&self) -> bool {
        self.0 == Self::LOCAL
    }

    /// The id as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for HostId {
    type Error = HostIdError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<HostId> for String {
    fn from(value: HostId) -> Self {
        value.0
    }
}

impl fmt::Display for HostId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The network path a peer connection rides.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CarrierKind {
    /// A tailnet address (`100.64.0.0/10` or `fd7a:115c:a1e0::/48`).
    #[serde(rename = "tailnet")]
    Tailnet,
    /// A LAN address, bound only with an explicit `--bind`.
    #[serde(rename = "lan")]
    Lan,
    /// Loopback, reached through `ssh -L`.
    #[serde(rename = "ssh-l")]
    SshL,
}

impl CarrierKind {
    /// The wire token, also the prologue's `transport` value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Tailnet => "tailnet",
            Self::Lan => "lan",
            Self::SshL => "ssh-l",
        }
    }
}

impl fmt::Display for CarrierKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Whether a surface can reach a host right now.
///
/// A field of the host, never folded into a row's state: a row from an
/// unreachable host keeps its last known state and shows this beside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Reachability {
    /// Connected over `carrier`.
    Reachable {
        /// The carrier the live session rides.
        carrier: CarrierKind,
    },
    /// Not connected since `since_ms`.
    Unreachable {
        /// Unix milliseconds of the last successful contact.
        since_ms: i64,
    },
    /// Connected, but the stream is behind.
    Stale {
        /// The last sequence the surface applied.
        last_seq: i64,
        /// Unix milliseconds since the stream fell behind.
        since_ms: i64,
    },
}

/// How completely one host is represented in one census listing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum HostCoverage {
    /// Every row of the host is in the listing.
    Covered,
    /// The row cap left `omitted` rows of this host out.
    OmittedByCap {
        /// Rows left out.
        omitted: u32,
    },
    /// The host did not answer; its rows are absent.
    Unreachable {
        /// Unix milliseconds of the last successful contact.
        since_ms: i64,
    },
    /// The host's rows are from an older revision.
    Stale {
        /// The newest revision the listing holds for this host.
        head_revision: i64,
        /// Unix milliseconds since the rows fell behind.
        since_ms: i64,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINTED: &str = "01K5A0000000000000000ABCDE";

    #[test]
    fn a_minted_ulid_and_local_parse() {
        assert_eq!(HostId::parse(MINTED).unwrap().as_str(), MINTED);
        assert!(HostId::parse("local").unwrap().is_local());
        assert!(!HostId::parse(MINTED).unwrap().is_local());
    }

    #[test]
    fn a_peer_id_refuses_local() {
        assert_eq!(
            HostId::parse_minted("local"),
            Err(HostIdError::LocalNotAllowed)
        );
        assert!(HostId::parse_minted(MINTED).is_ok());
    }

    #[test]
    fn non_ulid_values_are_refused() {
        // Spike 3's 16-character id is superseded by the 26-character ULID.
        assert_eq!(
            HostId::parse("0123456789ABCDEF"),
            Err(HostIdError::Length(16))
        );
        assert_eq!(
            HostId::parse("01K5A0000000000000000ABCDI"),
            Err(HostIdError::Alphabet('I'))
        );
        assert_eq!(
            HostId::parse("01k5a0000000000000000abcde"),
            Err(HostIdError::Alphabet('k')),
            "one canonical spelling: upper case"
        );
        assert_eq!(
            HostId::parse("81K5A0000000000000000ABCDE"),
            Err(HostIdError::Overflow)
        );
        assert_eq!(HostId::parse("LOCAL"), Err(HostIdError::Length(5)));
    }

    #[test]
    fn the_wire_shape_is_a_bare_string_and_decoding_validates() {
        let id = HostId::parse(MINTED).unwrap();
        assert_eq!(
            serde_json::to_value(&id).unwrap(),
            serde_json::json!(MINTED)
        );
        let back: HostId = serde_json::from_value(serde_json::json!(MINTED)).unwrap();
        assert_eq!(back, id);
        assert!(serde_json::from_value::<HostId>(serde_json::json!("nope")).is_err());
    }

    #[test]
    fn carrier_tokens_are_frozen() {
        for (carrier, token) in [
            (CarrierKind::Tailnet, "tailnet"),
            (CarrierKind::Lan, "lan"),
            (CarrierKind::SshL, "ssh-l"),
        ] {
            assert_eq!(serde_json::to_value(carrier).unwrap(), token);
            assert_eq!(carrier.as_str(), token);
        }
    }

    #[test]
    fn reachability_and_coverage_wire_shapes_are_frozen() {
        assert_eq!(
            serde_json::to_value(Reachability::Reachable {
                carrier: CarrierKind::SshL
            })
            .unwrap(),
            serde_json::json!({"state":"reachable","carrier":"ssh-l"})
        );
        assert_eq!(
            serde_json::to_value(Reachability::Unreachable { since_ms: 5 }).unwrap(),
            serde_json::json!({"state":"unreachable","since_ms":5})
        );
        assert_eq!(
            serde_json::to_value(Reachability::Stale {
                last_seq: 3,
                since_ms: 5
            })
            .unwrap(),
            serde_json::json!({"state":"stale","last_seq":3,"since_ms":5})
        );
        assert_eq!(
            serde_json::to_value(HostCoverage::Covered).unwrap(),
            serde_json::json!({"state":"covered"})
        );
        assert_eq!(
            serde_json::to_value(HostCoverage::OmittedByCap { omitted: 2 }).unwrap(),
            serde_json::json!({"state":"omitted_by_cap","omitted":2})
        );
        assert_eq!(
            serde_json::to_value(HostCoverage::Unreachable { since_ms: 9 }).unwrap(),
            serde_json::json!({"state":"unreachable","since_ms":9})
        );
        assert_eq!(
            serde_json::to_value(HostCoverage::Stale {
                head_revision: 4,
                since_ms: 9
            })
            .unwrap(),
            serde_json::json!({"state":"stale","head_revision":4,"since_ms":9})
        );
    }
}
