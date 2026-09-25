use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;

/// Size of the PTY every spawned app draws into.
pub const ROWS: u16 = 40;
pub const COLS: u16 = 120;

/// The `ainb` binary cargo built for this test run. Never `cargo run`: its
/// compiler output lands in the PTY ahead of the app.
pub fn ainb_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ainb")
}

/// The app running in a PTY of its own.
pub struct Pty {
    pub child: Box<dyn Child + Send + Sync>,
    pub writer: Box<dyn Write + Send>,
    /// Everything the app writes, in chunks; disconnected once the app exits.
    pub output: mpsc::Receiver<Vec<u8>>,
    _master: Box<dyn MasterPty + Send>,
}

impl Drop for Pty {
    fn drop(&mut self) {
        // A failed test must not leave the app running.
        let _ = self.child.kill();
    }
}

/// Spawn the application in visual debug mode (opens separate terminal window)
pub fn spawn_app_visual(root: &Path) -> Pty {
    #[cfg(feature = "visual-debug")]
    {
        // Open in separate terminal window (macOS), against the same isolated
        // home as the PTY copy below.
        let script = format!(
            r#"
            tell application "Terminal"
                do script "env HOME={home} AINB_HOME={home} AINB_HANGAR_HOME={hangar} {bin}"
                activate
            end tell
            "#,
            home = seed_home(root).display(),
            hangar = root.join("hangar").display(),
            bin = ainb_bin(),
        );

        // For Linux, use: xterm -e or gnome-terminal --
        // For WSL, use: cmd.exe /c start

        std::process::Command::new("osascript")
            .arg("-e")
            .arg(&script)
            .spawn()
            .expect("open Terminal");

        // Give terminal time to open
        std::thread::sleep(std::time::Duration::from_secs(2));

        println!("🖥️  Visual debug mode: Terminal window opened");
        println!("   Watch the test execute in the new window");
    }

    // Continue with normal PTY spawn
    spawn_app_silent(root)
}

/// Spawn the application silently (normal headless mode).
///
/// Everything the app reads or writes lives under `root`: its home, the
/// hangar home, and the tmux server (`TMUX_TMPDIR`), so a run neither sees
/// nor touches the machine's own sessions.
pub fn spawn_app_silent(root: &Path) -> Pty {
    let home = seed_home(root);
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: ROWS,
            cols: COLS,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("open a PTY");

    let mut cmd = CommandBuilder::new(ainb_bin());
    cmd.cwd(root);
    cmd.env("HOME", &home);
    cmd.env("AINB_HOME", &home);
    cmd.env("AINB_HANGAR_HOME", root.join("hangar"));
    cmd.env("TMUX_TMPDIR", root.join("tmux"));
    cmd.env_remove("TMUX");
    cmd.env("TERM", "xterm-256color");
    cmd.env("RUST_LOG", "error");
    cmd.env("NO_COLOR", "1");
    let child = pair.slave.spawn_command(cmd).expect("spawn ainb");
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader().expect("PTY reader");
    let writer = pair.master.take_writer().expect("PTY writer");
    let (tx, output) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        // Ends at EOF, or EIO once the app has exited.
        while let Ok(n @ 1..) = reader.read(&mut buf) {
            if tx.send(buf[..n].to_vec()).is_err() {
                break;
            }
        }
    });

    Pty {
        child,
        writer,
        output,
        _master: pair.master,
    }
}

/// A home with onboarding done and the hooks install prompt dismissed, so the
/// app opens on the home screen rather than the setup wizard.
fn seed_home(root: &Path) -> PathBuf {
    let home = root.join("home");
    let config = home.join(".agents-in-a-box").join("config");
    std::fs::create_dir_all(&config).expect("create config dir");
    std::fs::create_dir_all(root.join("hangar")).expect("create hangar home");
    std::fs::create_dir_all(root.join("tmux")).expect("create tmux dir");
    std::fs::write(
        config.join("onboarding.toml"),
        format!(
            "completed = true\ncompleted_at = \"2026-05-11T00:00:00+00:00\"\n\
             version = \"{}\"\nskipped_dependencies = []\ngit_directories = []\n",
            env!("CARGO_PKG_VERSION")
        ),
    )
    .expect("seed onboarding.toml");
    std::fs::write(
        home.join(".agents-in-a-box").join("install.json"),
        r#"{"agents":[],"hook_script":"","prompt_dismissed":true}"#,
    )
    .expect("seed install.json");
    // The TUI installs the daily release checker on start unless it finds one,
    // and installing runs `launchctl` or `systemctl --user` against the REAL
    // user's service manager: a temp HOME scopes neither. These are the files
    // `update::schedule_is_enabled` looks for, so the install is skipped and
    // nothing else about the start changes. (`AINB_DISABLE_PLUGINS` would skip
    // it too, but also the hangar daemon, which slows the New Session dialog
    // to seconds.)
    for marker in [
        "Library/LaunchAgents/com.agentsinabox.release-check.plist",
        ".config/systemd/user/com.agentsinabox.release-check.timer",
    ] {
        let path = home.join(marker);
        std::fs::create_dir_all(path.parent().expect("marker dir")).expect("create marker dir");
        std::fs::write(&path, "").expect("seed release-check marker");
    }
    home
}

// Platform-specific terminal launchers
#[cfg(target_os = "macos")]
pub fn open_terminal(command: &str) {
    let script = format!(
        r#"
        tell application "Terminal"
            do script "{}"
            activate
        end tell
        "#,
        command
    );

    let _ = std::process::Command::new("osascript").arg("-e").arg(&script).spawn();
}

#[cfg(target_os = "linux")]
pub fn open_terminal(command: &str) {
    // Try xterm first, then gnome-terminal, then konsole
    let terminals = [
        ("xterm", vec!["-e", command]),
        ("gnome-terminal", vec!["--", command]),
        ("konsole", vec!["-e", command]),
    ];

    for (terminal, args) in &terminals {
        if let Ok(_) = std::process::Command::new(terminal).args(args).spawn() {
            break;
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn open_terminal(_command: &str) {
    eprintln!("Visual debug mode not supported on this platform");
}
