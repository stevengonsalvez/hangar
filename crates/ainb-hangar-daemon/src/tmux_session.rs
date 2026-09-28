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
//!  daemon ──tmux_new_session──▶ tmux client (allowlisted env, no secrets)
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
pub const DAEMON_SECRETS: [&str; 2] = [
    crate::claude_cred::ENV_OVERRIDE,
    crate::claude_cred::CHILD_ENV_VAR,
];

/// What a tmux client needs beyond the runner's [`crate::runner::ENV_ALLOWLIST`]:
/// the socket directory the server lives under, the ssh agent a shell
/// expects, and `$TMUX`, which names the server a daemon started inside a
/// pane talks to. It stays so the session lands on the same server the
/// daemon's other tmux calls (existence checks, kills) reach; a caller that
/// wants the default server removes it, as `shell/create` does.
const TMUX_CLIENT_ENV: [&str; 3] = ["TMUX_TMPDIR", "SSH_AUTH_SOCK", "TMUX"];

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
/// The client starts from an empty environment plus the runner's allowlist
/// and [`TMUX_CLIENT_ENV`], so a server it starts never holds a secret (and
/// holds only those variables for every later pane on it). `start_dir` is
/// escaped once here ([`tmux_literal_dir`]).
///
/// With `argv`, the pane runs `env -u <secret>… argv`, executed directly (no
/// shell; tmux 3.0 or later), so a server that already holds a secret does
/// not pass it on. A window opened later in that session is not covered:
/// marking the session would be one more command after the create, and a
/// run that exits at once would make it fail.
///
/// Without `argv`, the pane is the server's default shell: the session then
/// drops the secrets from its environment and the pane is respawned once, so
/// the shell (and every later pane in the session) starts without them. The
/// first shell may have read its rc files before it is replaced. If that
/// tail fails, tmux exits non-zero with the session already made, so the
/// caller must remove it.
#[must_use]
pub fn tmux_new_session(name: &str, start_dir: &str, extra: &[&str], argv: &[OsString]) -> Command {
    let mut tmux = Command::new("tmux");
    tmux.env_clear();
    for key in crate::runner::ENV_ALLOWLIST.iter().chain(TMUX_CLIENT_ENV.iter()) {
        if let Some(value) = std::env::var_os(key) {
            tmux.env(key, value);
        }
    }
    strip_daemon_secrets(&mut tmux);
    let start_dir = tmux_literal_dir(start_dir);
    tmux.args(["new-session", "-d", "-s", name, "-c", &start_dir]).args(extra);
    if argv.is_empty() {
        let session = format!("={name}");
        for secret in DAEMON_SECRETS {
            tmux.args([";", "set-environment", "-t", &session, "-r", secret]);
        }
        tmux.args([";", "respawn-pane", "-k", "-t", &format!("{session}:")]);
    } else {
        tmux.args(["--", "env"]);
        for secret in DAEMON_SECRETS {
            tmux.args(["-u", secret]);
        }
        tmux.args(argv);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn args(cmd: &Command) -> Vec<String> {
        cmd.as_std().get_args().map(|a| a.to_string_lossy().into_owned()).collect()
    }

    #[test]
    fn a_command_runs_under_env_without_the_secrets() {
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
    fn the_client_env_is_cleared_to_the_allowlist() {
        let cmd = tmux_new_session("s", "/w", &[], &[]);
        let std = cmd.as_std();
        // `env_clear` plus explicit sets: nothing inherited, and no secret
        // is ever set.
        for (key, value) in std.get_envs() {
            let key = key.to_string_lossy();
            assert!(
                crate::runner::ENV_ALLOWLIST.contains(&key.as_ref())
                    || TMUX_CLIENT_ENV.contains(&key.as_ref())
                    || DAEMON_SECRETS.contains(&key.as_ref()) && value.is_none(),
                "{key} passed to the tmux client"
            );
        }
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
