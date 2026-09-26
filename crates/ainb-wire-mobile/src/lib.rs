//! The phone side of the Hangar peer wire (M1, spec D16).
//!
//! ```text
//! Expo app ──uniffi──▶ MobileHost (api) ──▶ Session (session)
//!                                              │  WebSocket ws://host:47300/peer
//!                                              │  Noise_IK_25519_ChaChaPoly_BLAKE2s
//!                                              │  16-byte frames: Rpc / StreamEnd / Ping / Pong
//!                                              ▼
//!                                       ainb-hangar-daemon peer leg (R1)
//! ```
//!
//! [`session`] is the transport: one multiplexed Noise session per host with
//! request-id correlation, notification routing, a 15 s heartbeat that skips
//! missed ticks, and jittered reconnect backoff. [`api`] is the uniffi facade
//! the app calls; [`records`] are the plain records it gets back, mapped from
//! the proto types so JavaScript never parses wire JSON.
//!
//! Dark by construction: nothing in the daemon dispatches to this crate, and
//! the peer listener it dials binds only under `AINB_HANGAR_PEER_LISTEN`.

uniffi::setup_scaffolding!();

pub mod api;
pub mod connlog;
pub mod custody;
pub mod records;
pub mod session;

pub use api::MobileHost;
pub use records::WireError;
pub use session::{ConnectConfig, Notification, Session};
