//! The Hangar peer wire, version 1 (R1, frozen by PR-0).
//!
//! Everything a phone and a daemon both need below JSON-RPC, as pure data:
//!
//! ```text
//! TCP ─▶ WebSocket GET /peer, binary messages only (text closes 1003)
//!     ─▶ one WS message = one Noise message (at most 65,535 bytes)
//!     ─▶ Noise_IK_25519_ChaChaPoly_BLAKE2s, prologue = prologue::prologue()
//!     ─▶ one decrypted Noise message = one Frame (16-byte header + payload)
//! ```
//!
//! This crate holds the frozen byte layouts: the [`frame`] header, the
//! [`opcode`] registry, the [`prologue`] encoding and the pairing [`offer`]
//! codec. The IK handshake itself arrives in R1-01; no listener or client in
//! this tree uses the crate yet, so it changes no runtime behaviour.
//!
//! Golden bytes for every layout are committed in
//! `tests/fixtures/peer_v1.json` and checked by `tests/wire_contract.rs` in the
//! Contracts job. Changing a layout is a `PROTOCOL_VERSION` bump.

pub mod frame;
pub mod offer;
pub mod opcode;
pub mod prologue;

pub use frame::{FrameHeader, HEADER_LEN, MAGIC, VERSION};
pub use offer::{Endpoint, OfferError, PairingOffer};
pub use opcode::Opcode;
pub use prologue::{PrologueError, prologue};

/// The WebSocket path of the peer leg.
pub const PEER_PATH: &str = "/peer";
/// The largest Noise message, and so the largest WebSocket message.
pub const MAX_NOISE_MESSAGE: usize = 65_535;
/// The Noise protocol name.
pub const NOISE_PATTERN: &str = "Noise_IK_25519_ChaChaPoly_BLAKE2s";
/// Reassembly cap for one logical Rpc message, the unix leg's body cap.
pub const MAX_REASSEMBLED: usize = 16 * 1024 * 1024;
/// The default peer port.
pub const DEFAULT_PORT: u16 = 47_300;
/// The client pings this often.
pub const PING_INTERVAL_SECS: u64 = 15;
/// Missed Pongs after which a client declares the session dead.
pub const MISSED_PONGS_DEAD: u32 = 2;
/// The host closes a peer that sent no frame for this long (C7), which runs
/// the shared teardown and releases any terminal floor.
pub const HOST_SILENCE_CLOSE_SECS: u64 = 35;
