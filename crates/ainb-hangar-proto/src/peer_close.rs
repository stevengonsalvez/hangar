//! WebSocket close codes for the off-box peer leg (R1).
//!
//! Frozen by PR-0. A code's number and its client reaction never change; a new
//! condition gets a new code.
//!
//! ```text
//! 1013 over capacity   ──▶ back off retry-after
//! 4401 unauthenticated ──▶ "host identity changed or wrong route, re-pair"
//! 4403 revoked/expired ──▶ latch "re-pair", never retry
//! 4409 incompatible    ──▶ "update one side"
//! 4429 rate limited    ──▶ back off retry-after
//! 4503 draining        ──▶ jittered reconnect (also a device rescope)
//! ```

use serde::{Deserialize, Serialize};

/// Over capacity; the close reason carries `retry-after=<s>`.
pub const OVER_CAPACITY: u16 = 1013;
/// Unauthenticated.
///
/// A Noise failure, a bad token, a bad invite, no first frame in time, or a
/// hello whose `DeviceInfo.device_id` differs from the device the token row
/// names: identity comes from the credential, never the request.
pub const UNAUTHENTICATED: u16 = 4401;
/// The device was revoked or its token expired.
pub const REVOKED: u16 = 4403;
/// The protocol ranges do not overlap; sent after the `-32007` reply.
pub const PROTOCOL_INCOMPATIBLE: u16 = 4409;
/// Rate limited; the close reason carries `retry-after=<s>`.
pub const RATE_LIMITED: u16 = 4429;
/// The host is draining, or the device was rescoped and must hello again under
/// its new scope. Never 4403 for a rescope: that would latch "re-pair".
pub const DRAINING: u16 = 4503;

/// The close reason prefix for [`OVER_CAPACITY`] and [`RATE_LIMITED`].
pub const RETRY_AFTER_PREFIX: &str = "retry-after=";

/// A peer close, by meaning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PeerClose {
    /// [`OVER_CAPACITY`].
    OverCapacity,
    /// [`UNAUTHENTICATED`].
    Unauthenticated,
    /// [`REVOKED`].
    Revoked,
    /// [`PROTOCOL_INCOMPATIBLE`].
    ProtocolIncompatible,
    /// [`RATE_LIMITED`].
    RateLimited,
    /// [`DRAINING`].
    Draining,
}

impl PeerClose {
    /// Every close, in code order.
    pub const ALL: [Self; 6] = [
        Self::OverCapacity,
        Self::Unauthenticated,
        Self::Revoked,
        Self::ProtocolIncompatible,
        Self::RateLimited,
        Self::Draining,
    ];

    /// The WebSocket close code.
    #[must_use]
    pub const fn code(self) -> u16 {
        match self {
            Self::OverCapacity => OVER_CAPACITY,
            Self::Unauthenticated => UNAUTHENTICATED,
            Self::Revoked => REVOKED,
            Self::ProtocolIncompatible => PROTOCOL_INCOMPATIBLE,
            Self::RateLimited => RATE_LIMITED,
            Self::Draining => DRAINING,
        }
    }

    /// The close for `code`, or `None` for a code this build does not know.
    #[must_use]
    pub const fn from_code(code: u16) -> Option<Self> {
        match code {
            OVER_CAPACITY => Some(Self::OverCapacity),
            UNAUTHENTICATED => Some(Self::Unauthenticated),
            REVOKED => Some(Self::Revoked),
            PROTOCOL_INCOMPATIBLE => Some(Self::ProtocolIncompatible),
            RATE_LIMITED => Some(Self::RateLimited),
            DRAINING => Some(Self::Draining),
            _ => None,
        }
    }

    /// Whether a client may reconnect on its own after this close.
    ///
    /// Only [`Self::Revoked`] latches: the device must pair again.
    #[must_use]
    pub const fn may_retry(self) -> bool {
        !matches!(self, Self::Revoked)
    }
}

/// Parse the `<s>` of a `retry-after=<s>` close reason.
#[must_use]
pub fn retry_after_secs(reason: &str) -> Option<u32> {
    reason.strip_prefix(RETRY_AFTER_PREFIX)?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_codes_are_frozen() {
        let codes: Vec<u16> = PeerClose::ALL.iter().map(|c| c.code()).collect();
        assert_eq!(codes, vec![1013, 4401, 4403, 4409, 4429, 4503]);
        for close in PeerClose::ALL {
            assert_eq!(PeerClose::from_code(close.code()), Some(close));
        }
        assert_eq!(PeerClose::from_code(1000), None);
    }

    #[test]
    fn only_a_revoke_latches() {
        for close in PeerClose::ALL {
            assert_eq!(close.may_retry(), close != PeerClose::Revoked, "{close:?}");
        }
    }

    #[test]
    fn retry_after_parses() {
        assert_eq!(retry_after_secs("retry-after=30"), Some(30));
        assert_eq!(retry_after_secs("retry-after=x"), None);
        assert_eq!(retry_after_secs("busy"), None);
    }
}
