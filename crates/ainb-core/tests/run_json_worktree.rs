//! `ainb --format json run --worktree --base <ref>` creates the worktree FROM
//! the requested ref and prints exactly one JSON object on stdout.
//!
//! This is the contract the hangar daemon's `worktree/create` relies on when
//! it drives `ainb run` as a subprocess: stdout must parse as one JSON line
//! naming the session it made, and the branch must start at `--base`, not at
//! the repository's default branch.
//!
//! Real `ainb` binary, real git, real tmux on a PRIVATE server:
//!   * `TMUX_TMPDIR` points at a per-test dir, because `ainb run` shells out to
//!     `tmux` itself and must land on the same server; `TMUX` is removed on
//!     every spawn so an ambient session cannot redirect it.
//!   * A tempdir `$HOME` (seeded onboarding) isolates the session store and
//!     the worktree base dir from the developer's real ones.
//!   * Fake `gemini` and `claude` agents on PATH stand in for real CLIs.
//!   * Cleanup always kills the session by its exact name (`=<name>`), never
//!     the server, whether or not the run succeeded.

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn tmux_available() -> bool {
    Command::new("tmux").arg("-V").output().is_ok_and(|o| o.status.success())
}

fn tmux(tmux_dir: &Path, args: &[&str]) -> Output {
    Command::new("tmux")
        .env("TMUX_TMPDIR", tmux_dir)
        .env_remove("TMUX")
        .args(args)
        .output()
        .expect("run tmux")
}

/// Git pinned to a clean config so the developer's signing or hooks cannot
/// leak into the fixture.
fn git(cwd: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args([
            "-c",
            "user.name=ainb-test",
            "-c",
            "user.email=ainb@test.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .current_dir(cwd)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .expect("run git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A fake agent CLI named `name`: prints a ready marker `ainb run` knows,
/// then reads input forever. It ignores its arguments (`--session-id`, ...).
fn write_fake_agent(bin_dir: &Path, name: &str) {
    let path = bin_dir.join(name);
    let mut f = std::fs::File::create(&path).expect("create fake agent");
    f.write_all(
        b"#!/bin/sh\n\
          echo 'fake agent ready. Ctrl+C to exit'\n\
          while IFS= read -r line; do :; done\n",
    )
    .expect("write fake agent");
    drop(f);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("chmod fake agent");
}

fn seed_onboarding(home: &Path) {
    let cfg = home.join(".agents-in-a-box/config");
    std::fs::create_dir_all(&cfg).expect("create config dir");
    std::fs::write(
        cfg.join("onboarding.toml"),
        format!(
            "completed = true\ncompleted_at = \"2026-05-11T00:00:00+00:00\"\nversion = \"{}\"\nskipped_dependencies = []\ngit_directories = []\n",
            env!("CARGO_PKG_VERSION"),
        ),
    )
    .expect("seed onboarding.toml");
}

/// One isolated world: HOME with fake agents, a repo whose `feature-base`
/// branch is one commit ahead of `main`, and a private tmux server.
struct World {
    home: tempfile::TempDir,
    repo: tempfile::TempDir,
    tmux_dir: PathBuf,
    base_head: String,
}

impl World {
    fn new() -> Self {
        let home = tempfile::tempdir().expect("home tempdir");
        let repo = tempfile::tempdir().expect("repo tempdir");
        seed_onboarding(home.path());
        write_fake_agent(home.path(), "gemini");
        write_fake_agent(home.path(), "claude");

        git(repo.path(), &["init", "-q"]);
        git(
            repo.path(),
            &["commit", "-q", "--allow-empty", "-m", "on main"],
        );
        git(repo.path(), &["checkout", "-q", "-b", "feature-base"]);
        git(
            repo.path(),
            &[
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "only on feature-base",
            ],
        );
        let base_head = git(repo.path(), &["rev-parse", "feature-base"]);
        git(repo.path(), &["checkout", "-q", "main"]);

        // Under /tmp: macOS caps unix socket paths at 104 bytes, and its
        // `$TMPDIR` already burns about half of them.
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or_default();
        let tmux_dir =
            PathBuf::from("/tmp").join(format!("ainb-run-json-{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(&tmux_dir).expect("create private tmux socket dir");
        Self {
            home,
            repo,
            tmux_dir,
            base_head,
        }
    }

    /// `ainb --format json run --worktree <extra...> --name <name>`.
    fn run(&self, tool: &str, name: &str, extra: &[&str]) -> Output {
        let path_env = format!(
            "{}:{}",
            self.home.path().display(),
            std::env::var("PATH").unwrap_or_default()
        );
        Command::new(env!("CARGO_BIN_EXE_ainb"))
            .args([
                "--format",
                "json",
                "run",
                "--tool",
                tool,
                "--repo",
                &self.repo.path().display().to_string(),
                "--worktree",
                "--name",
                name,
            ])
            .args(extra)
            .env("HOME", self.home.path())
            .env("PATH", &path_env)
            .env("TMUX_TMPDIR", &self.tmux_dir)
            .env_remove("TMUX")
            .env_remove("AINB_HOME")
            .output()
            .expect("ainb run")
    }

    fn session_alive(&self, name: &str) -> bool {
        let exact = format!("={}", ainb::tmux::sanitize_session_name(name));
        tmux(&self.tmux_dir, &["has-session", "-t", &exact]).status.success()
    }

    /// Kill the session `ainb run` would have made for `name`, by exact name.
    fn kill(&self, name: &str) {
        let exact = format!("={}", ainb::tmux::sanitize_session_name(name));
        let _ = tmux(&self.tmux_dir, &["kill-session", "-t", &exact]);
    }
}

impl Drop for World {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.tmux_dir);
    }
}

/// Exactly one non-empty stdout line, parsed; `None` for anything else.
fn one_json_line(out: &Output) -> Option<serde_json::Value> {
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
    match lines.as_slice() {
        [only] => serde_json::from_str(only).ok(),
        _ => None,
    }
}

fn describe(out: &Output) -> String {
    format!(
        "status={:?}\nstdout:\n{}\nstderr:\n{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn run_json_creates_the_worktree_from_base_and_prints_one_json_line() {
    if !tmux_available() {
        eprintln!("skipping: tmux not available");
        return;
    }
    let world = World::new();
    let name = format!("run-json-{}", std::process::id());
    let out = world.run(
        "gemini",
        &name,
        &["--create-branch", "ainb/run-json", "--base", "feature-base"],
    );
    world.kill(&name);

    assert!(out.status.success(), "ainb run failed: {}", describe(&out));
    let v = one_json_line(&out)
        .unwrap_or_else(|| panic!("stdout must be exactly one JSON line: {}", describe(&out)));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("Created worktree at:"),
        "human progress moves to stderr under --format json: {}",
        describe(&out)
    );
    assert!(
        uuid::Uuid::parse_str(v["session_id"].as_str().unwrap_or_default()).is_ok(),
        "session_id is a UUID: {v}"
    );
    assert_eq!(v["branch"], "ainb/run-json");
    assert!(v["tmux_session_name"].as_str().is_some_and(|s| !s.is_empty()));
    assert!(
        v.get("claude_session_id").is_none(),
        "absent, not null: {v}"
    );

    let worktree = PathBuf::from(v["worktree_path"].as_str().expect("worktree_path"));
    assert!(
        worktree.is_dir(),
        "worktree exists at {}",
        worktree.display()
    );
    assert_eq!(
        git(&worktree, &["rev-parse", "HEAD"]),
        world.base_head,
        "the new branch must start at --base (feature-base), not main"
    );
}

/// The Claude launch path (minted session id, trust write, MCP pool step)
/// keeps stdout to the one JSON line too, and names the Claude session id.
#[test]
fn run_json_for_claude_names_its_session_id_and_keeps_stdout_clean() {
    if !tmux_available() {
        eprintln!("skipping: tmux not available");
        return;
    }
    let world = World::new();
    let name = format!("run-json-claude-{}", std::process::id());
    let out = world.run("claude", &name, &[]);
    world.kill(&name);

    assert!(out.status.success(), "ainb run failed: {}", describe(&out));
    let v = one_json_line(&out)
        .unwrap_or_else(|| panic!("stdout must be exactly one JSON line: {}", describe(&out)));
    assert!(
        v["claude_session_id"].as_str().is_some_and(|s| !s.is_empty()),
        "a Claude launch names the id Claude runs under: {v}"
    );
}

/// `--base` on a branch that already exists would be silently ignored (git
/// checks the branch out as it is). Refused before anything is created.
#[test]
fn run_refuses_base_for_an_existing_branch_and_creates_nothing() {
    if !tmux_available() {
        eprintln!("skipping: tmux not available");
        return;
    }
    let world = World::new();
    git(world.repo.path(), &["branch", "already-there", "main"]);
    let name = format!("run-json-existing-{}", std::process::id());
    let out = world.run(
        "gemini",
        &name,
        &["--create-branch", "already-there", "--base", "feature-base"],
    );
    let alive = world.session_alive(&name);
    world.kill(&name);

    assert!(!out.status.success(), "must be refused: {}", describe(&out));
    assert!(
        out.stdout.iter().all(u8::is_ascii_whitespace),
        "nothing on stdout when refused: {}",
        describe(&out)
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("already exists"),
        "the reason is on stderr: {}",
        describe(&out)
    );
    assert!(!alive, "no tmux session was started");
    let by_session = world.home.path().join(".agents-in-a-box/worktrees/by-session");
    let made = std::fs::read_dir(&by_session).map(|entries| entries.count()).unwrap_or(0);
    assert_eq!(made, 0, "no worktree was created");
}
