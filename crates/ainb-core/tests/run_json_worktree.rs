//! `ainb --format json run --worktree --base <ref>` creates the worktree FROM the
//! requested ref and prints exactly one JSON object on stdout.
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
//!   * A fake `gemini` agent on PATH stands in for a real provider CLI.
//!   * Cleanup kills the session by its exact name only, never the server.

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

fn tmux_available() -> bool {
    Command::new("tmux").arg("-V").output().is_ok_and(|o| o.status.success())
}

/// Private tmux socket dir under /tmp: macOS caps unix socket paths at 104
/// bytes, and `$TMPDIR` there already burns about half of them.
fn private_tmux_dir() -> PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or_default();
    let dir = PathBuf::from("/tmp").join(format!("ainb-run-json-{}-{nonce}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create private tmux socket dir");
    dir
}

fn tmux(tmux_dir: &Path, args: &[&str]) -> std::process::Output {
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

fn write_fake_gemini(bin_dir: &Path) {
    let path = bin_dir.join("gemini");
    let mut f = std::fs::File::create(&path).expect("create fake gemini");
    f.write_all(
        b"#!/bin/sh\n\
          echo 'fake agent ready. Ctrl+C to exit'\n\
          while IFS= read -r line; do :; done\n",
    )
    .expect("write fake gemini");
    drop(f);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("chmod fake gemini");
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

#[test]
fn run_json_creates_the_worktree_from_base_and_prints_one_json_line() {
    if !tmux_available() {
        eprintln!("skipping: tmux not available");
        return;
    }

    let tmux_dir = private_tmux_dir();
    let home = tempfile::tempdir().expect("home tempdir");
    let repo = tempfile::tempdir().expect("repo tempdir");
    seed_onboarding(home.path());
    write_fake_gemini(home.path());

    // main has one commit; `feature-base` has a second one main does not.
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

    let name = format!("run-json-{}", std::process::id());
    let path_env = format!(
        "{}:{}",
        home.path().display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = Command::new(env!("CARGO_BIN_EXE_ainb"))
        .args([
            "--format",
            "json",
            "run",
            "--tool",
            "gemini",
            "--repo",
            &repo.path().display().to_string(),
            "--worktree",
            "--create-branch",
            "ainb/run-json",
            "--base",
            "feature-base",
            "--name",
            &name,
        ])
        .env("HOME", home.path())
        .env("PATH", &path_env)
        .env("TMUX_TMPDIR", &tmux_dir)
        .env_remove("TMUX")
        .env_remove("AINB_HOME")
        .output()
        .expect("ainb run");

    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
    let parsed: Option<serde_json::Value> = match lines.as_slice() {
        [only] => serde_json::from_str(only).ok(),
        _ => None,
    };

    // Tear down before asserting, by exact names only.
    if let Some(v) = &parsed {
        if let Some(tmux_name) = v["tmux_session_name"].as_str() {
            let _ = tmux(&tmux_dir, &["kill-session", "-t", &format!("={tmux_name}")]);
        }
    }
    let _ = std::fs::remove_dir_all(&tmux_dir);

    assert!(
        out.status.success(),
        "ainb run failed: stdout={stdout} stderr={stderr}"
    );
    let v = parsed.unwrap_or_else(|| {
        panic!("stdout must be exactly one JSON line, got:\n{stdout}\nstderr:\n{stderr}")
    });
    assert!(
        stderr.contains("Created worktree at:"),
        "human progress moves to stderr under --format json, got stderr:\n{stderr}"
    );
    assert!(
        uuid::Uuid::parse_str(v["session_id"].as_str().unwrap_or_default()).is_ok(),
        "session_id is a UUID: {v}"
    );
    assert_eq!(v["branch"], "ainb/run-json");
    assert!(v["tmux_session_name"].as_str().is_some_and(|s| !s.is_empty()));

    let worktree = PathBuf::from(v["worktree_path"].as_str().expect("worktree_path"));
    assert!(
        worktree.is_dir(),
        "worktree exists at {}",
        worktree.display()
    );
    let head = git(&worktree, &["rev-parse", "HEAD"]);
    assert_eq!(
        head, base_head,
        "the new branch must start at --base (feature-base), not main"
    );
}
