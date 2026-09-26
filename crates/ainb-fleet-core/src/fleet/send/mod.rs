// ABOUTME: Send-side fleet primitives: broker HTTP, tmux send-keys, route.

pub mod broker;
pub mod pane;
pub mod route;
pub mod tmux;

pub use broker::{BrokerClient, broker_health, broker_send};
pub use pane::{
    PaneHint, is_pane_id, only_pane, pane_id_of, pane_pid, pane_pid_of, pane_session,
    resolve_send_target, session_started_of,
};
pub use route::{send, tmux_delivery_preferred};
pub use tmux::{
    SendError, SendFailure, pane_has_unsubmitted_input, tmux_press_enter, tmux_send,
    tmux_send_picker_key, tmux_session_exists,
};
