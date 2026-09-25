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
