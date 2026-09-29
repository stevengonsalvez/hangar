//! The desktop shell's Rust side: a host of the `ainb-app` state machine, the
//! same way the terminal binary is one.
//!
//! - [`host::DesktopHost`] owns the `AppState`, applies intents through
//!   `ainb_app::dispatch`, and frames what moved for the webview.
//! - [`executor::DesktopExecutor`] carries out the effects a dispatch returns,
//!   and answers the ones this shell cannot run yet with their documented
//!   failure report.
//! - [`intent::RendererIntent`] is what the webview may send.
//! - [`worktree_target`] is how the page names a worktree for the strip's
//!   "+", and how the host resolves it.
//! - [`clipboard`] holds the size rule a copy and a paste share.
//! - [`projects`] lists the repositories the composer may create into,
//!   sessions or not.
//! - [`setup`] is the desktop-native path for the onboarding writes the
//!   webview may not run (#1175), behind a confirmation the shell owns.
//! - [`daemons_panel`] keeps the daemons collector alive while the settings
//!   page draws it.
//! - [`shell::Shell`] locks the host and the executor together for the
//!   window's commands and tick.
//! - [`shell_tab`] opens a plain shell tab in a worktree through the daemon,
//!   and ends it with the tab.
//! - [`sidecar`] finds or starts the bundled hangar daemon and holds this
//!   surface's presence against it.
//! - [`theme`] keeps the host's copy of the theme a person picked, so the
//!   window opens in it before the page has painted.
//!
//! None of it needs a window: the Tauri binary (`app` feature) wires these to
//! channels and commands, and the tests drive them headless.

/// TypeScript for the shapes the webview sends and receives (#1158).
#[cfg(feature = "typescript-bindings")]
pub mod bindings;
pub mod clipboard;
pub mod create;
pub mod daemons_panel;
pub mod executor;
pub mod host;
pub mod intent;
pub mod projects;
pub mod setup;
pub mod shell;
pub mod shell_tab;
pub mod sidecar;
pub mod terminal;
pub mod theme;
pub mod updater;
pub mod worktree_target;
