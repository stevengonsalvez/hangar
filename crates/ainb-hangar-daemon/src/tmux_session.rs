//! The one way the daemon starts a tmux session, and the one list of
//! secrets it keeps out of every child it starts.
//!
//! A tmux server keeps the environment of the client that started it, and
//! hands that environment to every pane it makes from then on. So a daemon
//! child that starts the server with the daemon's OAuth token in its
//! environment puts the token in every later pane on that server, the
//! user's own included. And a server some other path started with the token
//! passes it to the daemon's new panes, whatever the daemon's own client
//! environment says.
//!
//! ```text
//!  daemon ──tmux_new_session──▶ tmux client (daemon env minus its secrets)
//!                                 │ starts the server? it starts clean
//!                                 ▼
//!                               new pane ──▶ `env -u <secret>… argv`
//!                                            or, a plain shell: the session
//!                                            drops the secrets, pane respawned
//! ```

use std::ffi::OsString;

use tokio::process::Command;

/// The daemon's own credential env names: the override it may be started
/// with, and the variable it hands a confined `claude` child. Neither
/// belongs in anything else the daemon starts.
///
/// `CLAUDE_CODE_OAUTH_TOKEN` is removed even when the user set it
/// themselves: the daemon cannot tell the user's own token from one it
/// resolved, so a shell or agent the daemon opens never sees it. A user who
/// wants it in a desktop shell sets it inside that shell.
pub const DAEMON_SECRETS: [&str; 2] = [
    crate::claude_cred::ENV_OVERRIDE,
    crate::claude_cred::CHILD_ENV_VAR,
];

/// Drop [`DAEMON_SECRETS`] from `cmd`'s environment.
pub fn strip_daemon_secrets(cmd: &mut Command) -> &mut Command {
    for secret in DAEMON_SECRETS {
        cmd.env_remove(secret);
    }
    cmd
}

/// A `tmux new-session -d -s <name> -c <start_dir>` the daemon runs, with
/// `extra` flags (such as `-x`/`-y`) before the pane's command.
///
/// The client inherits the daemon's environment minus [`DAEMON_SECRETS`],
/// the same policy as `ainb run`'s child, so a server it starts never holds
/// a secret and keeps everything else (locale, terminal, the user's own
/// keys) for the panes after. `$TMUX` is inherited like the rest, so the
/// session lands on the server the daemon's other tmux calls reach.
/// `start_dir` and every `argv` element are escaped once here
/// ([`tmux_literal_dir`], [`tmux_literal_arg`]).
///
/// Either way the session then marks the secrets removed from its own
/// environment, so a window opened in it later starts without them too. tmux
/// runs the whole command line before it reaps a pane, so a command that
/// exits at once does not fail that step.
///
/// With `argv`, the pane runs `env -u <secret>… argv`, executed directly (no
/// shell; tmux 3.0 or later), so a server that already holds a secret does
/// not pass it on to the first pane.
///
/// Without `argv`, the pane is the server's default shell, respawned once
/// the session has dropped the secrets, so the shell starts without them.
/// The first shell may have read its rc files before it is replaced.
///
/// If a step after the create fails, tmux exits non-zero with the session
/// already made, so the caller must remove it.
#[must_use]
pub fn tmux_new_session(name: &str, start_dir: &str, extra: &[&str], argv: &[OsString]) -> Command {
    let mut tmux = Command::new("tmux");
    strip_daemon_secrets(&mut tmux);
    let start_dir = tmux_literal_dir(start_dir);
    tmux.args(["new-session", "-d", "-s", name, "-c", &start_dir]).args(extra);
    if !argv.is_empty() {
        tmux.args(["--", "env"]);
        for secret in DAEMON_SECRETS {
            tmux.args(["-u", secret]);
        }
        tmux.args(argv.iter().map(|arg| tmux_literal_arg(arg)));
    }
    let session = format!("={name}");
    for secret in DAEMON_SECRETS {
        tmux.args([";", "set-environment", "-t", &session, "-r", secret]);
    }
    if argv.is_empty() {
        tmux.args([";", "respawn-pane", "-k", "-t", &format!("{session}:")]);
    }
    tmux
}

/// `dir` as tmux reads it back literally in `-c`. tmux expands formats
/// there, so a `#` would be read as one (`#S` renames the directory,
/// `#(cmd)` runs a command); `##` is its literal `#`. And an argument that
/// ends in `;` ends the command, so a folder named `x;` would open its
/// sibling `x`; `\;` is its literal `;`.
#[must_use]
pub fn tmux_literal_dir(dir: &str) -> String {
    let escaped = dir.replace('#', "##");
    match escaped.strip_suffix(';') {
        Some(head) => format!("{head}\\;"),
        None => escaped,
    }
}

/// `arg` as tmux passes it to the pane's command literally. tmux reads an
/// argument that ends in `;` as the end of the command, even after `--`, so
/// `x;` would reach the command as `x`; `\;` is its literal `;`.
#[must_use]
pub fn tmux_literal_arg(arg: &OsString) -> OsString {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    let bytes = arg.as_bytes();
    match bytes.strip_suffix(b";") {
        Some(head) => {
            let mut escaped = head.to_vec();
            escaped.extend_from_slice(b"\\;");
            OsString::from_vec(escaped)
        }
        None => arg.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(cmd: &Command) -> Vec<String> {
        cmd.as_std().get_args().map(|a| a.to_string_lossy().into_owned()).collect()
    }

    #[test]
    fn a_command_runs_under_env_and_its_session_drops_the_secrets() {
        let cmd = tmux_new_session(
            "tmux_hangar-1",
            "/w/a#b",
            &["-x", "200"],
            &[OsString::from("/logs/wrapper"), OsString::from("arg")],
        );
        assert_eq!(
            args(&cmd),
            [
                "new-session",
                "-d",
                "-s",
                "tmux_hangar-1",
                "-c",
                "/w/a##b",
                "-x",
                "200",
                "--",
                "env",
                "-u",
                "HANGAR_CLAUDE_OAUTH_TOKEN",
                "-u",
                "CLAUDE_CODE_OAUTH_TOKEN",
                "/logs/wrapper",
                "arg",
                ";",
                "set-environment",
                "-t",
                "=tmux_hangar-1",
                "-r",
                "HANGAR_CLAUDE_OAUTH_TOKEN",
                ";",
                "set-environment",
                "-t",
                "=tmux_hangar-1",
                "-r",
                "CLAUDE_CODE_OAUTH_TOKEN",
            ]
        );
    }

    #[test]
    fn a_shell_drops_the_secrets_from_its_session_and_respawns() {
        let cmd = tmux_new_session("ainb-sh-0a1b2c3d", "/w/app", &[], &[]);
        assert_eq!(
            args(&cmd)[6..],
            [
                ";",
                "set-environment",
                "-t",
                "=ainb-sh-0a1b2c3d",
                "-r",
                "HANGAR_CLAUDE_OAUTH_TOKEN",
                ";",
                "set-environment",
                "-t",
                "=ainb-sh-0a1b2c3d",
                "-r",
                "CLAUDE_CODE_OAUTH_TOKEN",
                ";",
                "respawn-pane",
                "-k",
                "-t",
                "=ainb-sh-0a1b2c3d:",
            ]
        );
    }

    #[test]
    fn the_client_inherits_everything_but_the_secrets() {
        let cmd = tmux_new_session("s", "/w", &[], &[]);
        let std = cmd.as_std();
        // Nothing cleared: the only env edits are the two removals.
        let edits: Vec<_> = std.get_envs().collect();
        assert_eq!(edits.len(), DAEMON_SECRETS.len(), "{edits:?}");
        for (key, value) in edits {
            assert!(
                DAEMON_SECRETS.contains(&key.to_string_lossy().as_ref()) && value.is_none(),
                "{key:?} is edited, not just removed"
            );
        }
    }

    #[test]
    fn an_argument_ending_in_a_semicolon_reaches_the_command_whole() {
        let cmd = tmux_new_session(
            "fleet-codex-t",
            "/w",
            &[],
            &[
                OsString::from("codex"),
                OsString::from("mid;dle"),
                OsString::from("thread;"),
            ],
        );
        let args = args(&cmd);
        let command = &args[args.iter().position(|a| a == "codex").unwrap()..];
        assert_eq!(command[..3], ["codex", "mid;dle", "thread\\;"]);
    }

    #[test]
    fn a_start_dir_is_escaped_for_tmux() {
        assert_eq!(tmux_literal_dir("/w/app"), "/w/app");
        assert_eq!(
            tmux_literal_dir("/w/c#{session_name}"),
            "/w/c##{session_name}"
        );
        assert_eq!(tmux_literal_dir("/w/x;"), "/w/x\\;");
        assert_eq!(tmux_literal_dir("/w/mid;dle"), "/w/mid;dle");
    }
}
