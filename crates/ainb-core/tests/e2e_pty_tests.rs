//! End-to-End PTY-based TUI Tests
//! These tests spawn the actual application in a PTY and interact with it
//! like a real user would, verifying the complete terminal experience.
//!
//! Each run gets a home, hangar home and tmux server of its own (see
//! `helpers::visual_debug::spawn_app_silent`). The screen is read through a
//! vt100 model of the PTY, not the raw byte stream: ratatui redraws only the
//! cells that changed, so a label on screen is not always one run of bytes.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::time::{Duration, Instant};

mod helpers;
use helpers::visual_debug::{COLS, Pty, ROWS};
use helpers::vt100_helper::ScreenCapture;

/// Longest any screen may take to appear, app start included.
const WAIT: Duration = Duration::from_secs(20);

/// Title of the home screen's help panel: the app is up and drawing.
const HOME: &str = "Getting Started";

// Helper function to spawn the app with proper environment
fn spawn_app(root: &Path) -> Pty {
    #[cfg(feature = "visual-debug")]
    {
        helpers::visual_debug::spawn_app_visual(root)
    }

    #[cfg(not(feature = "visual-debug"))]
    {
        helpers::visual_debug::spawn_app_silent(root)
    }
}

/// The app running in a PTY, and what its screen shows.
struct App {
    pty: Pty,
    screen: ScreenCapture,
    // Dropped last: the app is gone before its home is deleted.
    _root: tempfile::TempDir,
}

impl App {
    fn start() -> Self {
        Self::start_in(tempfile::tempdir().expect("tempdir"))
    }

    fn start_in(root: tempfile::TempDir) -> Self {
        let mut app = Self {
            pty: spawn_app(root.path()),
            screen: ScreenCapture::new(ROWS, COLS),
            _root: root,
        };
        app.wait_until("the home screen on the alternate screen", |s| {
            s.screen().alternate_screen() && s.has_text(HOME)
        });
        // The event loop drops keys for its first 100 ms (`STARTUP_GUARD_MS`
        // in main.rs), which can outlast the first frame.
        std::thread::sleep(Duration::from_millis(300));
        app
    }

    fn press(&mut self, keys: &str) {
        self.pty.writer.write_all(keys.as_bytes()).expect("send keys");
        self.pty.writer.flush().expect("flush keys");
    }

    /// Feed everything the app has written so far into the screen model.
    fn pump(&mut self) {
        while let Ok(bytes) = self.pty.output.try_recv() {
            self.screen.process_output(&bytes);
        }
    }

    /// Wait until `done` holds for the screen, and return how long that took.
    fn wait_until(&mut self, what: &str, done: impl Fn(&ScreenCapture) -> bool) -> Duration {
        let start = Instant::now();
        loop {
            self.pump();
            if done(&self.screen) {
                return start.elapsed();
            }
            assert!(
                start.elapsed() < WAIT,
                "timed out after {WAIT:?} waiting for {what}; screen:\n{}",
                self.screen.contents()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn wait_for(&mut self, text: &str) -> Duration {
        self.wait_until(&format!("{text:?} on screen"), |s| s.has_text(text))
    }

    fn wait_gone(&mut self, text: &str) {
        self.wait_until(&format!("{text:?} to leave the screen"), |s| {
            !s.has_text(text)
        });
    }

    /// The row and column (in chars) where `text` first appears.
    fn find(&self, text: &str) -> Option<(usize, usize)> {
        self.screen.contents().lines().enumerate().find_map(|(row, line)| {
            Some((row, line.find(text).map(|b| line[..b].chars().count())?))
        })
    }

    /// Ctrl+C quits (`q` only goes back). A dialog with a text field keeps
    /// Ctrl+C for itself, so close it first.
    fn quit(mut self) {
        self.press("\x03");
        let deadline = Instant::now() + WAIT;
        // The reader disconnects once the app has exited and closed the PTY.
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.pty.output.recv_timeout(left) {
                Ok(bytes) => self.screen.process_output(&bytes),
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    panic!("app still running {WAIT:?} after Ctrl+C")
                }
            }
        }
        assert!(
            !self.screen.screen().alternate_screen(),
            "the app left the alternate screen on the way out"
        );
        let status = self.pty.child.wait().expect("reap app");
        assert!(status.success(), "app exited with {status:?}");
    }
}

/// A tmux session on the app's own tmux server, killed when dropped.
struct TmuxSession {
    tmux_dir: PathBuf,
    name: &'static str,
}

impl TmuxSession {
    fn start(root: &Path, name: &'static str) -> Self {
        let session = Self {
            tmux_dir: root.join("tmux"),
            name,
        };
        std::fs::create_dir_all(&session.tmux_dir).expect("create tmux dir");
        let made = session.tmux(&["new-session", "-d", "-s", name, "sleep", "600"]).success();
        assert!(made, "could not start tmux session {name}");
        session
    }

    fn alive(&self) -> bool {
        self.tmux(&["has-session", "-t", self.name]).success()
    }

    fn tmux(&self, args: &[&str]) -> std::process::ExitStatus {
        // `-f /dev/null`: the developer's own tmux.conf and its plugins stay
        // out of this server (tmux-continuum, for one, runs systemctl).
        Command::new("tmux")
            .args(["-f", "/dev/null"])
            .args(args)
            .env("TMUX_TMPDIR", &self.tmux_dir)
            .env_remove("TMUX")
            .status()
            .expect("run tmux")
    }
}

impl Drop for TmuxSession {
    fn drop(&mut self) {
        let _ = self.tmux(&["kill-session", "-t", self.name]);
    }
}

#[test]
#[ignore] // Run with: cargo test -p ainb --test e2e_pty_tests -- --ignored
fn test_e2e_new_session_flow() {
    let mut app = App::start();

    app.press("n");
    app.wait_for("New Session");
    app.wait_for("type to filter");

    app.press("\x1b");
    app.wait_gone("New Session");
    app.wait_for(HOME);

    app.quit();
}

#[test]
#[ignore]
fn test_e2e_keyboard_shortcuts() {
    let mut app = App::start();

    app.press("?");
    app.wait_for("Help - Press ? or Esc to close");
    app.press("\x1b");
    app.wait_gone("Help - Press ? or Esc to close");

    app.press("s");
    app.wait_for("Workspaces");
    app.press("q");
    app.wait_gone("Workspaces");
    app.wait_for(HOME);

    app.quit();
}

#[test]
#[ignore]
fn test_e2e_responsive_ui() {
    let mut app = App::start();

    app.press("n");
    let elapsed = app.wait_for("New Session");

    assert!(
        elapsed < Duration::from_millis(500),
        "Dialog took too long to appear: {:?}",
        elapsed
    );
    println!("✅ Dialog appeared in {:?} (responsive!)", elapsed);

    app.press("\x1b");
    app.wait_gone("New Session");
    app.quit();
}

#[test]
#[ignore]
fn test_e2e_quit() {
    App::start().quit();
}

#[test]
#[ignore]
fn test_e2e_visual_layout() {
    let mut app = App::start();

    app.press("n");
    app.wait_for("New Session");

    // The dialog is inset from both edges by the same margin.
    let (row, _) = app.find("New Session").expect("dialog title");
    let line = app.screen.contents().lines().nth(row).unwrap().to_string();
    let chars: Vec<char> = line.chars().collect();
    let left = chars.iter().position(|&c| c == '╭').expect("dialog top-left corner");
    let right = chars.iter().rposition(|&c| c == '╮').expect("dialog top-right corner");
    let right_margin = chars.len() - 1 - right;
    assert!(left > 1 && right_margin > 1, "dialog is inset: {line:?}");
    assert!(
        left.abs_diff(right_margin) <= 1,
        "dialog is centred (left {left}, right {right_margin}): {line:?}"
    );

    app.press("\x1b");
    app.wait_gone("New Session");
    app.quit();
}

// Visual debug test - only enabled with visual-debug feature
#[test]
#[ignore]
#[cfg(feature = "visual-debug")]
fn test_visual_delete_session() {
    println!("🖥️  VISUAL TEST: Watch the delete flow in the terminal window");

    let root = tempfile::tempdir().expect("tempdir");
    let _victim = TmuxSession::start(root.path(), "e2e-visual-victim");
    let mut app = App::start_in(root);

    app.press("s");
    std::thread::sleep(Duration::from_secs(2));
    app.press("d");
    std::thread::sleep(Duration::from_secs(2));
    app.press("\x1b");

    println!("✅ Visual test complete - did you see the dialog?");
    app.quit();
}

// VT100 screen verification tests
#[cfg(feature = "vt100-tests")]
mod vt100_tests {
    use super::*;

    #[test]
    #[ignore]
    fn test_e2e_screen_layout() {
        let app = App::start();

        let (header, _) = app.find("A I N B").expect("header");
        let (panel, _) = app.find(HOME).expect("help panel");
        let (footer, _) = app.find("? help").expect("footer");
        assert!(header < 6, "header is at the top, row {header}");
        assert!(
            header < panel && panel < footer,
            "header, panel, footer in order"
        );
        assert!(
            footer >= usize::from(ROWS) - 3,
            "footer is at the bottom, row {footer}"
        );

        app.quit();
    }

    #[test]
    #[ignore]
    fn test_e2e_delete_confirmation_dialog() {
        let root = tempfile::tempdir().expect("tempdir");
        let victim = TmuxSession::start(root.path(), "e2e-victim");
        // The app's tmux server is the one the session was started on.
        let mut app = App::start_in(root);

        app.press("s");
        app.wait_for("e2e-victim");
        app.press("d");
        app.wait_for("Kill tmux Session");
        app.wait_for("Are you sure you want to kill tmux session 'e2e-victim'?");

        let (_, col) = app.find("Kill tmux Session").expect("dialog title");
        assert!(col > 1, "dialog is drawn over the list, not at the edge");

        // Esc answers no: the dialog closes and the session lives.
        app.press("\x1b");
        app.wait_gone("Kill tmux Session");
        assert!(victim.alive(), "Esc must not kill the session");

        drop(victim);
        app.quit();
    }
}
