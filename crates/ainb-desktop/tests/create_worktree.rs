//! The window's worktree create, end to end against the real daemon binary:
//! the desktop's request reaches `worktree/create`, the daemon runs `ainb`,
//! and the created session comes back as the tab to attach.
//!
//! The daemon is booted by the sidecar in a private hangar home with the
//! spawn switch on and `AINB_BIN` pointing at a stand-in `ainb` that records
//! its argv and answers like `ainb --format json run`. The real CLI half is
//! proven in `crates/ainb-core/tests/run_json_worktree.rs`.
//!
//! Its own process: the env it sets is process-global and inherited by the
//! daemon it spawns. The daemon is killed by its exact pid when the test ends.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ainb_desktop::create::{CreateWorktreeArgs, request};
use ainb_desktop::sidecar::{Sidecar, SidecarConfig, SidecarState, daemon_pid};
use ainb_hangar_client::DaemonClient;
use ainb_hangar_proto::spawn::SpawnAgent;

/// A cold runner needs time to migrate a fresh store and mint the token.
const BOOT_BUDGET: Duration = Duration::from_secs(90);

fn daemon_bin() -> PathBuf {
    let bin = std::env::var_os("AINB_DESKTOP_DAEMON_BIN").map_or_else(
        || Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/ainb-hangar-daemon"),
        PathBuf::from,
    );
    assert!(
        bin.is_file(),
        "no daemon binary at {}: run `cargo build -p ainb-hangar-daemon` at the \
         workspace root or set AINB_DESKTOP_DAEMON_BIN",
        bin.display()
    );
    bin
}

/// A stand-in `ainb`: records argv (one per line) to `argv.txt` beside it,
/// then prints one created-session line.
fn fake_ainb(dir: &Path) -> PathBuf {
    let bin = dir.join("ainb");
    let argv = dir.join("argv.txt");
    std::fs::write(
        &bin,
        format!(
            "#!/bin/sh\n: > '{argv}'\nfor a in \"$@\"; do printf '%s\\n' \"$a\" >> '{argv}'; done\n\
             printf '%s\\n' '{{\"session_id\":\"22222222-2222-4222-8222-222222222222\",\"tmux_session_name\":\"tmux_app-22222222\",\"worktree_path\":\"/w/app\",\"branch\":\"feat/window\"}}'\n",
            argv = argv.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    bin
}

/// `git init` a repository at `dir`.
fn git_init(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    let ok = std::process::Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .is_ok_and(|o| o.status.success());
    assert!(ok, "git init {}", dir.display());
}

/// A create from `repo` with every optional field left out.
fn plain(repo: &Path) -> CreateWorktreeArgs {
    CreateWorktreeArgs {
        repo_path: std::fs::canonicalize(repo).unwrap().display().to_string(),
        branch: None,
        base: None,
        agent: SpawnAgent::Claude,
        model: None,
        prompt: None,
    }
}

struct Home(tempfile::TempDir);

impl Home {
    fn path(&self) -> PathBuf {
        self.0.path().join(".agents-in-a-box")
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        if let Some(pid) = daemon_pid(&self.path()) {
            let _ = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(i32::try_from(pid).expect("pid")),
                nix::sys::signal::Signal::SIGKILL,
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_window_creates_a_worktree_session_through_the_daemon() {
    let tools = tempfile::tempdir().unwrap();
    // The daemon only creates from a repository top inside a registered
    // folder: a home whose config registers `<home>/code`, holding a repo.
    let user_home = tempfile::tempdir().unwrap();
    let repo = user_home.path().join("code/app");
    std::fs::create_dir_all(&repo).unwrap();
    for args in [
        &["init", "-q", "-b", "main"][..],
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "init",
        ][..],
    ] {
        let ok = std::process::Command::new("git")
            .args(args)
            .current_dir(&repo)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .is_ok_and(|o| o.status.success());
        assert!(ok, "git {args:?}");
    }
    let config = user_home.path().join(".agents-in-a-box/config");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(
        config.join("config.toml"),
        format!(
            "[workspace_defaults]\nworkspace_scan_paths = [\"{}\"]\n",
            user_home.path().join("code").display()
        ),
    )
    .unwrap();
    let repo = std::fs::canonicalize(repo).unwrap();
    // The composer offers this repository before it has any session, and what
    // it offers is what the daemon accepts: the create below uses the listed
    // path, not one built by hand.
    let listed = ainb_desktop::projects::list(
        user_home.path(),
        &ainb_app::config::WorkspaceDefaults::default(),
    );
    assert_eq!(
        listed.iter().map(|p| p.path.as_str()).collect::<Vec<_>>(),
        [repo.display().to_string()],
        "the registered repository is listed with no session"
    );
    // Inherited by the daemon the sidecar spawns. Edition 2021: safe.
    std::env::set_var("HOME", user_home.path());
    std::env::set_var("AINB_CODEX_MANAGED", "0");
    std::env::set_var("AINB_HANGAR_SPAWN", "1");
    std::env::set_var("AINB_BIN", fake_ainb(tools.path()));

    let home = Home(tempfile::tempdir().expect("scratch home"));
    let mut config = SidecarConfig::new(home.path(), daemon_bin());
    config.hello_budget = BOOT_BUDGET;
    let sidecar = Sidecar::start(config);
    let mut state = sidecar.state();
    tokio::time::timeout(BOOT_BUDGET, async {
        loop {
            if matches!(*state.borrow_and_update(), SidecarState::Connected { .. }) {
                return;
            }
            state.changed().await.expect("supervisor running");
        }
    })
    .await
    .expect("the daemon connected");

    let token = std::fs::read_to_string(ainb_hangar_proto::auth::token_file_in(&home.path()))
        .expect("daemon token");
    let client = DaemonClient::with_parts(
        ainb_hangar_client::socket_path_in(&home.path()),
        token.trim().to_string(),
    );
    // A repository outside every registered folder is refused, and the
    // sentence says how to fix it from the window.
    let stray = user_home.path().join("stray");
    git_init(&stray);
    let refused = request(&client, plain(&stray))
        .await
        .expect_err("an unregistered repository is refused");
    assert!(
        refused.contains("Add project"),
        "the refusal names the fix: {refused}"
    );
    // The fix it names: Add project registers the repository, and the
    // daemon, which reads the registered projects per call, creates from it,
    // and from nothing nested inside it.
    let nested = stray.join("vendor/lib");
    git_init(&nested);
    ainb_desktop::projects::register(user_home.path(), &stray).expect("registered");
    request(&client, plain(&stray))
        .await
        .expect("a registered project is created from");
    request(&client, plain(&nested))
        .await
        .expect_err("a repository nested in an added project is refused");

    // A repository under an onboarding folder is already accepted: adding it
    // writes nothing, and the daemon creates from it.
    let onboarded = user_home.path().join("onboarded");
    git_init(&onboarded.join("tool"));
    std::fs::write(
        user_home.path().join(".agents-in-a-box/config/onboarding.toml"),
        format!("git_directories = [\"{}\"]\n", onboarded.display()),
    )
    .unwrap();
    let projects_file = user_home.path().join(".agents-in-a-box/config/projects.toml");
    let before = std::fs::read_to_string(&projects_file).unwrap();
    ainb_desktop::projects::register(user_home.path(), &onboarded.join("tool"))
        .expect("already accepted");
    assert_eq!(std::fs::read_to_string(&projects_file).unwrap(), before);
    request(&client, plain(&onboarded.join("tool")))
        .await
        .expect("a repository under an onboarding folder is created from");

    let created = request(
        &client,
        CreateWorktreeArgs {
            repo_path: listed[0].path.clone(),
            branch: Some("feat/window".into()),
            base: Some("main".into()),
            agent: SpawnAgent::Claude,
            model: None,
            prompt: Some("hello from the window".into()),
        },
    )
    .await
    .expect("the daemon created the session");

    assert_eq!(created.tmux_session_name, "tmux_app-22222222");
    assert_eq!(created.branch, "feat/window");

    let argv = std::fs::read_to_string(tools.path().join("argv.txt")).expect("ainb ran");
    let argv: Vec<&str> = argv.lines().collect();
    assert_eq!(
        &argv[..4],
        ["--format", "json", "run", "--worktree"],
        "{argv:?}"
    );
    let pairs: Vec<(&str, &str)> = argv.windows(2).map(|w| (w[0], w[1])).collect();
    assert!(pairs.contains(&("--tool", "claude")), "{argv:?}");
    assert!(pairs.contains(&("--base", "main")), "{argv:?}");
    assert!(
        !argv.contains(&"--dangerously-skip-permissions"),
        "the window never skips permission prompts: {argv:?}"
    );
    assert_eq!(argv.last(), Some(&"--prompt=hello from the window"));

    // Onboarding replaces workspace_scan_paths wholesale; an added project is
    // not there, so it survives.
    std::fs::write(
        user_home.path().join(".agents-in-a-box/config/config.toml"),
        "[workspace_defaults]\nworkspace_scan_paths = []\n",
    )
    .unwrap();
    request(&client, plain(&stray))
        .await
        .expect("an added project outlives a scan-path rewrite");

    drop(sidecar);
}
