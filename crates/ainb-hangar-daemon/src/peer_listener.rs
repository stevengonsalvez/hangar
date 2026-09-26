//! The off-box peer listener (R1-06). Only its switch exists in this tree.
//!
//! The listener binds only when [`LISTEN_ENV`] names an address at boot, the
//! webhook-port precedent. It is an environment variable, never a
//! `daemon_config` key: a paired desktop can reach `hangar/daemon_config_set`
//! nowhere, but a boot-time switch keeps a dark feature out of every RPC path.
//! Unset, which is the default, there is no socket and no new code on any
//! request path, so the daemon behaves exactly as v1.29.0.

/// The boot-time switch: `AINB_HANGAR_PEER_LISTEN=<addr>` binds the peer leg.
pub const LISTEN_ENV: &str = "AINB_HANGAR_PEER_LISTEN";

/// Whether the peer leg is switched on for this boot: [`LISTEN_ENV`] is set
/// and not blank. Read once at boot by everything the leg needs (its host key
/// first), so an unset variable leaves every one of them untouched.
#[must_use]
pub fn switched_on() -> bool {
    names_an_address(std::env::var_os(LISTEN_ENV).as_deref())
}

fn names_an_address(value: Option<&std::ffi::OsStr>) -> bool {
    value.is_some_and(|value| !value.to_string_lossy().trim().is_empty())
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use super::names_an_address;

    /// Unset is the default and means off; so does a blank value, which a
    /// shell profile that exports the name with nothing after it produces.
    #[test]
    fn only_a_non_blank_value_switches_the_leg_on() {
        assert!(!names_an_address(None));
        assert!(!names_an_address(Some(OsStr::new(""))));
        assert!(!names_an_address(Some(OsStr::new("  "))));
        assert!(names_an_address(Some(OsStr::new("127.0.0.1:47300"))));
    }
}
