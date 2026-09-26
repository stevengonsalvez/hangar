//! Terminal streams (R2).
//!
//! The `terminal/*` methods are served only by a build with the
//! `terminal-stream` feature AND with [`STREAM_ENV`] set to `1` at boot. Off,
//! which is the default, every `terminal/*` request answers
//! `METHOD_NOT_FOUND (-32601)`, which a client cannot tell from an older
//! daemon. It is an environment variable, never a `daemon_config` key, so no
//! connected surface can switch it on.
//!
//! [`feed`] is the per-pane control-mode feed actor (WP8): one
//! `tmux -C attach-session` client per watched pane, seeded from tmux's own
//! grid, driving one [`ainb_term::emulator::PaneEmulator`].

/// The boot-time switch: `AINB_TERMINAL_STREAM=1` serves terminal streams.
pub const STREAM_ENV: &str = "AINB_TERMINAL_STREAM";

#[cfg(feature = "terminal-stream")]
pub mod feed;
