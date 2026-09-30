//! The pull request badge a sidebar card draws: the PR open on its branch and
//! how its CI stands, read through the `gh` CLI, cached, and silent when `gh`
//! cannot answer.
//!
//! ```text
//!  card ──pr_badge(session)──▶ host: session's worktree + branch
//!                              cache (TTL) ──stale──▶ ≤ MAX_IN_FLIGHT gh
//!                              ◀── PrBadge, or nothing on any miss
//! ```
//!
//! One `gh pr view --json … -- <branch>` per (worktree, branch) per [`TTL`]:
//! it names the PR and carries its check rollup in one call, where Orca's
//! lookup then asks for the checks separately. `gh` resolves the repository
//! from the worktree's own remotes, so the host never guesses one.
//!
//! Every way `gh` fails (not installed, not logged in, offline, no GitHub
//! remote, no PR, a slow or garbled answer) is a [`Miss`]: logged at debug
//! and drawn as no badge. A miss is cached like an answer, so a machine with
//! no `gh` login does not respawn it on every refresh.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use tokio::sync::{OnceCell, Semaphore};
use tokio::time::Instant;

/// How long an answer, or a miss, stands before `gh` is asked again. At 20
/// cards that is 600 `gh` calls an hour, an eighth of GitHub's GraphQL budget.
pub const TTL: Duration = Duration::from_secs(120);
/// The most `gh` processes running at once, Orca's own `MAX_CONCURRENT`.
pub const MAX_IN_FLIGHT: usize = 4;
/// How long one `gh` call may take before it is killed and counted a miss.
pub const GH_TIMEOUT: Duration = Duration::from_secs(20);
/// The most stdout read from `gh`: a PR with a hundred checks is ~40 KiB.
pub const MAX_OUTPUT_BYTES: usize = 256 * 1024;
/// How much of `gh`'s stderr is kept, to name a miss.
const STDERR_HEAD: usize = 4096;
/// Past this many cached keys, the stale ones are dropped.
const MAX_ENTRIES: usize = 512;
/// The fields asked for: exactly what [`parse`] reads.
const FIELDS: &str = "state,isDraft,number,url,statusCheckRollup";

/// Where the pull request stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "typescript-bindings", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum PrState {
    Open,
    Draft,
    Merged,
    Closed,
}

/// The pull request's checks, rolled up into one dot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "typescript-bindings", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum PrChecks {
    /// Every check that concluded passed (or was skipped).
    Pass,
    /// At least one check failed, whatever the others did.
    Fail,
    /// None failed, and at least one has not finished.
    Pending,
    /// No checks, or only neutral ones: no dot.
    None,
}

/// What a card's badge draws, and where a click on it goes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "typescript-bindings", derive(specta::Type))]
pub struct PrBadge {
    pub state: PrState,
    pub number: u32,
    /// Already passed by `links::web_url`, the rule `open_url` asks again.
    pub url: String,
    pub checks: PrChecks,
}

/// Why a lookup drew no badge. Only ever logged at debug.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Miss {
    /// No `gh` on this machine.
    NoGh,
    /// Empty, too long, or not a name git would give a branch.
    Branch,
    /// The worktree is not a folder here (gone, or a remote session's).
    NoWorktree,
    /// `gh` could not be started.
    Spawn(std::io::ErrorKind),
    /// `gh` is not logged in.
    Unauthenticated,
    /// The branch has no pull request.
    NoPr,
    /// `gh` exited non-zero for another reason: offline, no GitHub remote.
    Exit(Option<i32>),
    /// `gh` took longer than [`GH_TIMEOUT`].
    Timeout,
    /// `gh` wrote more than [`MAX_OUTPUT_BYTES`].
    Oversized,
    /// `gh` wrote something that is not the badge's JSON.
    Malformed(String),
}

/// `gh`'s answer, as far as the badge reads it; every other field is ignored.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawPr {
    state: String,
    #[serde(default)]
    is_draft: bool,
    number: u32,
    url: String,
    #[serde(default)]
    status_check_rollup: Option<Vec<RawCheck>>,
}

/// One rollup entry: a check run carries `name`, `status` and `conclusion`,
/// a commit status context carries `context` and `state`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawCheck {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    conclusion: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    context: Option<String>,
    #[serde(default)]
    workflow_name: Option<String>,
    #[serde(default)]
    started_at: Option<String>,
}

impl RawCheck {
    /// Which check this is a run of: a re-run carries the same workflow and
    /// name as the run it replaces.
    fn identity(&self) -> (&str, &str) {
        (
            self.workflow_name.as_deref().unwrap_or_default(),
            self.name.as_deref().or(self.context.as_deref()).unwrap_or_default(),
        )
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Passed,
    Failed,
    Pending,
    Neutral,
}

/// One check's verdict, as Orca's `classifyCheckOutcome` gives it: a failing
/// conclusion or state first, then a passing one, then anything unfinished.
fn outcome(check: &RawCheck) -> Outcome {
    let verdict = [&check.conclusion, &check.state]
        .into_iter()
        .flatten()
        .find(|value| !value.is_empty())
        .map(|value| value.to_ascii_lowercase())
        .unwrap_or_default();
    match verdict.as_str() {
        "failure" | "error" | "startup_failure" | "timed_out" | "cancelled" | "action_required" => {
            Outcome::Failed
        }
        "success" | "skipped" => Outcome::Passed,
        "pending" | "expected" => Outcome::Pending,
        // A check run that has not completed has no conclusion yet.
        _ if check.state.is_none()
            && !check
                .status
                .as_deref()
                .is_some_and(|status| status.eq_ignore_ascii_case("completed")) =>
        {
            Outcome::Pending
        }
        _ => Outcome::Neutral,
    }
}

/// The latest run of each check: a failed run that was re-run green is
/// history, not the PR's state (`gh pr checks` drops it the same way). The
/// timestamps are RFC 3339 in UTC, so they order as text; an unnamed entry is
/// always its own check.
fn latest_runs(checks: &[RawCheck]) -> Vec<&RawCheck> {
    let mut latest: HashMap<(&str, &str), &RawCheck> = HashMap::new();
    let mut unnamed = Vec::new();
    for check in checks {
        let identity = check.identity();
        if identity.1.is_empty() {
            unnamed.push(check);
            continue;
        }
        let newer = latest.get(&identity).is_none_or(|kept| check.started_at > kept.started_at);
        if newer {
            latest.insert(identity, check);
        }
    }
    unnamed.extend(latest.into_values());
    unnamed
}

/// The rollup as one dot, over each check's latest run: any failure is a
/// fail, else anything unfinished is pending, else any pass is a pass; empty
/// or all neutral is none.
fn rollup(checks: &[RawCheck]) -> PrChecks {
    let outcomes: Vec<Outcome> = latest_runs(checks).into_iter().map(outcome).collect();
    if outcomes.contains(&Outcome::Failed) {
        PrChecks::Fail
    } else if outcomes.contains(&Outcome::Pending) {
        PrChecks::Pending
    } else if outcomes.contains(&Outcome::Passed) {
        PrChecks::Pass
    } else {
        PrChecks::None
    }
}

/// `gh pr view --json state,isDraft,number,url,statusCheckRollup` output as
/// the badge.
///
/// # Errors
/// [`Miss::Malformed`] for anything else: not that JSON, an unknown state, a
/// zero number, or a URL `open_url` would refuse.
pub fn parse(stdout: &[u8]) -> Result<PrBadge, Miss> {
    let raw: RawPr =
        serde_json::from_slice(stdout).map_err(|error| Miss::Malformed(error.to_string()))?;
    let state = match raw.state.to_ascii_uppercase().as_str() {
        "MERGED" => PrState::Merged,
        "CLOSED" => PrState::Closed,
        "OPEN" if raw.is_draft => PrState::Draft,
        "OPEN" => PrState::Open,
        other => return Err(Miss::Malformed(format!("unknown state {other:?}"))),
    };
    if raw.number == 0 {
        return Err(Miss::Malformed("PR number 0".to_string()));
    }
    let url = crate::links::web_url(&raw.url)
        .map_err(|refused| Miss::Malformed(format!("PR url: {refused}")))?;
    Ok(PrBadge {
        state,
        number: raw.number,
        url: url.into(),
        checks: rollup(raw.status_check_rollup.as_deref().unwrap_or_default()),
    })
}

/// A branch name `gh` is handed: non-empty, bounded, never read as a flag, and
/// free of the whitespace and control characters git refuses in one.
fn plausible_branch(branch: &str) -> bool {
    !branch.is_empty()
        && branch.len() <= 255
        && !branch.starts_with('-')
        && !branch.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// Why `gh` exited non-zero, from the head of its stderr.
fn exit_miss(code: Option<i32>, stderr: &[u8]) -> Miss {
    let stderr = String::from_utf8_lossy(stderr).to_ascii_lowercase();
    if stderr.contains("gh auth login") || stderr.contains("not logged in") {
        Miss::Unauthenticated
    } else if stderr.contains("no pull requests found") {
        Miss::NoPr
    } else {
        Miss::Exit(code)
    }
}

/// Up to `limit` bytes of `pipe`; the rest is never read, and dropping the
/// pipe closes it, so a writer past the bound stops rather than blocking.
async fn read_bounded(pipe: Option<impl AsyncRead + Unpin>, limit: usize) -> Vec<u8> {
    let mut bytes = Vec::new();
    if let Some(pipe) = pipe {
        // A read error leaves what was read, which then fails to parse.
        let _ = pipe.take(limit as u64).read_to_end(&mut bytes).await;
    }
    bytes
}

/// Ask `gh` at `gh` for the PR on `branch`, from inside `worktree`. The
/// program is started directly with its argv, never through a shell, and is
/// killed if it outruns [`GH_TIMEOUT`] or [`MAX_OUTPUT_BYTES`].
///
/// # Errors
/// The [`Miss`] that drew no badge.
pub async fn lookup(gh: &Path, worktree: &Path, branch: &str) -> Result<PrBadge, Miss> {
    if !plausible_branch(branch) {
        return Err(Miss::Branch);
    }
    if !worktree.is_dir() {
        return Err(Miss::NoWorktree);
    }
    let mut child = Command::new(gh)
        // `--` ends the flags: the branch is only ever a positional.
        .args(["pr", "view", "--json", FIELDS, "--", branch])
        .current_dir(worktree)
        .env("GH_PROMPT_DISABLED", "1")
        .env("GH_NO_UPDATE_NOTIFIER", "1")
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| Miss::Spawn(error.kind()))?;
    let run = async {
        let (stdout, stderr) = tokio::join!(
            read_bounded(child.stdout.take(), MAX_OUTPUT_BYTES + 1),
            read_bounded(child.stderr.take(), STDERR_HEAD),
        );
        if stdout.len() > MAX_OUTPUT_BYTES {
            return Err(Miss::Oversized);
        }
        let status = child.wait().await.map_err(|error| Miss::Spawn(error.kind()))?;
        if !status.success() {
            return Err(exit_miss(status.code(), &stderr));
        }
        parse(&stdout)
    };
    // A timed-out or oversized `gh` is killed when `child` drops.
    tokio::time::timeout(GH_TIMEOUT, run).await.unwrap_or(Err(Miss::Timeout))
}

/// `gh` on this machine: the absolute `PATH` entries first, then where a
/// package manager puts it, since an app started from a desktop launcher often
/// has a `PATH` without either (as `terminal::find_tmux` looks for tmux).
#[must_use]
pub fn find_gh() -> Option<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .filter(|dir| dir.is_absolute())
        .chain(["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"].map(PathBuf::from))
        .map(|dir| dir.join("gh"))
        .find(|candidate| candidate.is_file())
}

/// One cached lookup: empty while `gh` runs, so every caller asking for the
/// same key in that time waits on the one call; then when it answered, and
/// what.
#[derive(Default)]
struct Slot(OnceCell<(Instant, Option<PrBadge>)>);

impl Slot {
    fn stale(&self, now: Instant, ttl: Duration) -> bool {
        self.0.get().is_some_and(|(at, _)| now.duration_since(*at) >= ttl)
    }
}

/// Every card's badge, cached per (worktree, branch) for a TTL, with at most
/// [`MAX_IN_FLIGHT`] `gh` calls running and one per key.
pub struct PrBadges {
    gh: Option<PathBuf>,
    ttl: Duration,
    slots: Mutex<HashMap<(PathBuf, String), Arc<Slot>>>,
    in_flight: Semaphore,
}

impl PrBadges {
    /// Badges read through `gh` at `gh`, or never when it is `None`.
    #[must_use]
    pub fn new(gh: Option<PathBuf>) -> Self {
        Self::with_ttl(gh, TTL)
    }

    #[must_use]
    pub fn with_ttl(gh: Option<PathBuf>, ttl: Duration) -> Self {
        Self {
            gh,
            ttl,
            slots: Mutex::new(HashMap::new()),
            in_flight: Semaphore::new(MAX_IN_FLIGHT),
        }
    }

    /// The badge for `branch` in `worktree`, from the cache while it is
    /// fresh; `None` for any miss, which is logged at debug and cached too.
    pub async fn badge(&self, worktree: &Path, branch: &str) -> Option<PrBadge> {
        let key = (worktree.to_path_buf(), branch.to_string());
        self.cached(key, || async {
            let Some(gh) = self.gh.as_deref() else {
                return Err(Miss::NoGh);
            };
            lookup(gh, worktree, branch).await
        })
        .await
    }

    async fn cached<F, Fut>(&self, key: (PathBuf, String), fetch: F) -> Option<PrBadge>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<PrBadge, Miss>>,
    {
        let slot = self.slot(key.clone());
        let (_, badge) = slot
            .0
            .get_or_init(|| async {
                // Never closed, so this only ever waits for a permit.
                let _permit = self.in_flight.acquire().await.ok();
                let answer = fetch().await;
                if let Err(miss) = &answer {
                    tracing::debug!(?miss, branch = %key.1, "no PR badge");
                }
                (Instant::now(), answer.ok())
            })
            .await;
        badge.clone()
    }

    /// The key's slot, or a fresh one in place of a stale one.
    fn slot(&self, key: (PathBuf, String)) -> Arc<Slot> {
        let mut slots = self.slots.lock().unwrap_or_else(PoisonError::into_inner);
        let now = Instant::now();
        if let Some(slot) = slots.get(&key).filter(|slot| !slot.stale(now, self.ttl)) {
            return Arc::clone(slot);
        }
        if slots.len() >= MAX_ENTRIES {
            slots.retain(|_, slot| !slot.stale(now, self.ttl));
        }
        let slot = Arc::new(Slot::default());
        slots.insert(key, Arc::clone(&slot));
        slot
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use super::{
        MAX_IN_FLIGHT, MAX_OUTPUT_BYTES, Miss, PrBadge, PrBadges, PrChecks, PrState, TTL, lookup,
        parse,
    };

    const URL: &str = "https://github.com/o/r/pull/7";

    fn pr(state: &str, draft: bool, rollup: &str) -> String {
        format!(
            r#"{{"state":"{state}","isDraft":{draft},"number":7,"url":"{URL}","statusCheckRollup":{rollup}}}"#
        )
    }

    fn badge(state: PrState, checks: PrChecks) -> PrBadge {
        PrBadge {
            state,
            number: 7,
            url: URL.to_string(),
            checks,
        }
    }

    fn checks_of(rollup: &str) -> PrChecks {
        parse(pr("OPEN", false, rollup).as_bytes()).unwrap().checks
    }

    const PASS: &str = r#"{"__typename":"CheckRun","status":"COMPLETED","conclusion":"SUCCESS"}"#;
    const FAIL: &str = r#"{"__typename":"CheckRun","status":"COMPLETED","conclusion":"FAILURE"}"#;
    const RUNNING: &str = r#"{"__typename":"CheckRun","status":"IN_PROGRESS","conclusion":""}"#;
    const NEUTRAL: &str =
        r#"{"__typename":"CheckRun","status":"COMPLETED","conclusion":"NEUTRAL"}"#;

    #[test]
    fn each_state_parses() {
        assert_eq!(
            parse(pr("OPEN", false, "[]").as_bytes()),
            Ok(badge(PrState::Open, PrChecks::None))
        );
        assert_eq!(
            parse(pr("OPEN", true, "[]").as_bytes()),
            Ok(badge(PrState::Draft, PrChecks::None))
        );
        assert_eq!(
            parse(pr("MERGED", false, "[]").as_bytes()),
            Ok(badge(PrState::Merged, PrChecks::None))
        );
        assert_eq!(
            parse(pr("CLOSED", false, "[]").as_bytes()),
            Ok(badge(PrState::Closed, PrChecks::None))
        );
    }

    #[test]
    fn an_empty_or_missing_rollup_is_none() {
        assert_eq!(checks_of("[]"), PrChecks::None);
        assert_eq!(checks_of("null"), PrChecks::None);
        let bare = format!(r#"{{"state":"OPEN","number":7,"url":"{URL}"}}"#);
        assert_eq!(parse(bare.as_bytes()).unwrap().checks, PrChecks::None);
    }

    #[test]
    fn any_failure_is_fail_whatever_else_ran() {
        assert_eq!(checks_of(&format!("[{FAIL}]")), PrChecks::Fail);
        assert_eq!(
            checks_of(&format!("[{PASS},{FAIL},{PASS}]")),
            PrChecks::Fail
        );
        assert_eq!(checks_of(&format!("[{RUNNING},{FAIL}]")), PrChecks::Fail);
        for conclusion in [
            "ERROR",
            "STARTUP_FAILURE",
            "TIMED_OUT",
            "CANCELLED",
            "ACTION_REQUIRED",
        ] {
            let check = format!(r#"{{"status":"COMPLETED","conclusion":"{conclusion}"}}"#);
            assert_eq!(
                checks_of(&format!("[{PASS},{check}]")),
                PrChecks::Fail,
                "{conclusion}"
            );
        }
        // A commit status context: `state`, no conclusion.
        let status = r#"{"__typename":"StatusContext","state":"ERROR"}"#;
        assert_eq!(checks_of(&format!("[{PASS},{status}]")), PrChecks::Fail);
    }

    #[test]
    fn any_pending_with_no_failure_is_pending() {
        assert_eq!(checks_of(&format!("[{PASS},{RUNNING}]")), PrChecks::Pending);
        let queued = r#"{"status":"QUEUED"}"#;
        assert_eq!(checks_of(&format!("[{queued}]")), PrChecks::Pending);
        let status = r#"{"__typename":"StatusContext","state":"PENDING"}"#;
        assert_eq!(checks_of(&format!("[{PASS},{status}]")), PrChecks::Pending);
    }

    #[test]
    fn passes_and_skips_are_pass_and_neutral_alone_is_none() {
        assert_eq!(checks_of(&format!("[{PASS}]")), PrChecks::Pass);
        let skipped = r#"{"status":"COMPLETED","conclusion":"SKIPPED"}"#;
        assert_eq!(
            checks_of(&format!("[{PASS},{skipped},{NEUTRAL}]")),
            PrChecks::Pass
        );
        let status = r#"{"__typename":"StatusContext","state":"SUCCESS"}"#;
        assert_eq!(checks_of(&format!("[{status}]")), PrChecks::Pass);
        assert_eq!(checks_of(&format!("[{NEUTRAL}]")), PrChecks::None);
    }

    #[test]
    fn malformed_json_or_an_unopenable_url_is_a_miss() {
        for stdout in [
            "",
            "not json",
            "[]",
            r#"{"state":"OPEN"}"#,
            &pr("REOPENED", false, "[]"),
            r#"{"state":"OPEN","number":0,"url":"https://github.com/o/r/pull/0"}"#,
            r#"{"state":"OPEN","number":-1,"url":"https://github.com/o/r/pull/1"}"#,
            r#"{"state":"OPEN","number":7,"url":"javascript:alert(1)"}"#,
            r#"{"state":"OPEN","number":7,"url":"file:///etc/passwd"}"#,
        ] {
            assert!(
                matches!(parse(stdout.as_bytes()), Err(Miss::Malformed(_))),
                "{stdout:?}"
            );
        }
    }

    /// A fake `gh` in a fresh folder: `script` is its `sh` body.
    fn fake_gh(script: &str) -> (tempfile::TempDir, PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let gh = dir.path().join("gh");
        std::fs::write(&gh, format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
        (dir, gh)
    }

    fn here() -> &'static Path {
        Path::new(env!("CARGO_MANIFEST_DIR"))
    }

    #[tokio::test]
    async fn a_real_answer_is_read_through_argv() {
        // Echoes its own argv back as the URL's path, so the test sees exactly
        // what `gh` was handed.
        let (_dir, gh) = fake_gh(&format!(
            r#"printf '{{"state":"OPEN","isDraft":false,"number":7,"url":"{URL}?argv=%s","statusCheckRollup":[{FAIL}]}}' "$(echo "$@" | tr ' ' '+')""#
        ));
        let got = lookup(&gh, here(), "feature/x").await.unwrap();
        assert_eq!(got.checks, PrChecks::Fail);
        assert_eq!(
            got.url,
            format!(
                "{URL}?argv=pr+view+--json+state,isDraft,number,url,statusCheckRollup+--+feature/x"
            )
        );
    }

    #[tokio::test]
    async fn a_missing_gh_is_a_miss() {
        let missing = here().join("no-such-gh");
        assert_eq!(
            lookup(&missing, here(), "main").await,
            Err(Miss::Spawn(std::io::ErrorKind::NotFound))
        );
        assert_eq!(PrBadges::new(None).badge(here(), "main").await, None);
    }

    #[tokio::test]
    async fn a_non_zero_exit_is_a_miss_named_by_its_stderr() {
        let (_dir, gh) = fake_gh(
            "echo 'To get started with GitHub CLI, please run:  gh auth login' >&2; exit 4",
        );
        assert_eq!(
            lookup(&gh, here(), "main").await,
            Err(Miss::Unauthenticated)
        );
        let (_dir, gh) = fake_gh("echo 'no pull requests found for branch \"x\"' >&2; exit 1");
        assert_eq!(lookup(&gh, here(), "x").await, Err(Miss::NoPr));
        let (_dir, gh) = fake_gh(
            "echo 'none of the git remotes configured for this repository point to a known GitHub host' >&2; exit 1",
        );
        assert_eq!(lookup(&gh, here(), "main").await, Err(Miss::Exit(Some(1))));
        // Valid JSON on stdout does not rescue a failed exit.
        let (_dir, gh) = fake_gh(&format!("echo '{}'; exit 2", pr("OPEN", false, "[]")));
        assert_eq!(lookup(&gh, here(), "main").await, Err(Miss::Exit(Some(2))));
    }

    #[tokio::test]
    async fn malformed_output_is_a_miss() {
        let (_dir, gh) = fake_gh("echo '{\"state\": '");
        assert!(matches!(
            lookup(&gh, here(), "main").await,
            Err(Miss::Malformed(_))
        ));
    }

    #[tokio::test]
    async fn oversized_output_is_a_miss_and_gh_is_not_waited_on() {
        // `yes` never stops on its own: only the bound ends this.
        let (_dir, gh) = fake_gh("exec yes");
        let started = std::time::Instant::now();
        assert_eq!(lookup(&gh, here(), "main").await, Err(Miss::Oversized));
        assert!(started.elapsed() < Duration::from_secs(10));
        // Exactly at the bound it is read, and fails only as JSON.
        let (_dir, gh) = fake_gh(&format!("head -c {MAX_OUTPUT_BYTES} /dev/zero"));
        assert!(matches!(
            lookup(&gh, here(), "main").await,
            Err(Miss::Malformed(_))
        ));
    }

    #[tokio::test]
    async fn a_branch_that_could_be_a_flag_or_a_missing_worktree_never_runs_gh() {
        let (dir, gh) = fake_gh(r#"touch "$(dirname "$0")/ran""#);
        for branch in ["", "-R", "--repo=evil/x", "a b", "a\nb"] {
            assert_eq!(
                lookup(&gh, here(), branch).await,
                Err(Miss::Branch),
                "{branch:?}"
            );
        }
        assert_eq!(
            lookup(&gh, &here().join("no-such-dir"), "main").await,
            Err(Miss::NoWorktree)
        );
        assert!(!dir.path().join("ran").exists());
    }

    fn key(branch: &str) -> (PathBuf, String) {
        (PathBuf::from("/w"), branch.to_string())
    }

    #[tokio::test(start_paused = true)]
    async fn an_answer_and_a_miss_are_both_cached_until_the_ttl() {
        let badges = PrBadges::new(None);
        let calls = AtomicUsize::new(0);
        let fetch = || async {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(badge(PrState::Open, PrChecks::Pass))
        };
        assert!(badges.cached(key("a"), fetch).await.is_some());
        tokio::time::advance(TTL - Duration::from_secs(1)).await;
        assert!(badges.cached(key("a"), fetch).await.is_some());
        assert_eq!(calls.load(Ordering::SeqCst), 1, "fresh: from the cache");
        tokio::time::advance(Duration::from_secs(1)).await;
        assert!(badges.cached(key("a"), fetch).await.is_some());
        assert_eq!(calls.load(Ordering::SeqCst), 2, "stale: asked again");

        let misses = AtomicUsize::new(0);
        let miss = || async {
            misses.fetch_add(1, Ordering::SeqCst);
            Err(Miss::Unauthenticated)
        };
        assert_eq!(badges.cached(key("b"), miss).await, None);
        assert_eq!(badges.cached(key("b"), miss).await, None);
        assert_eq!(misses.load(Ordering::SeqCst), 1, "a miss is cached too");
        tokio::time::advance(TTL).await;
        assert_eq!(badges.cached(key("b"), miss).await, None);
        assert_eq!(misses.load(Ordering::SeqCst), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn concurrent_asks_share_one_call_and_at_most_a_few_run() {
        let badges = Arc::new(PrBadges::new(None));
        let calls = Arc::new(AtomicUsize::new(0));
        let running = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let mut asks = Vec::new();
        // Three asks for each of ten keys, all at once.
        for index in 0..30 {
            let (badges, calls, running, peak) = (
                Arc::clone(&badges),
                Arc::clone(&calls),
                Arc::clone(&running),
                Arc::clone(&peak),
            );
            asks.push(tokio::spawn(async move {
                badges
                    .cached(key(&format!("b{}", index % 10)), || async {
                        calls.fetch_add(1, Ordering::SeqCst);
                        let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                        peak.fetch_max(now, Ordering::SeqCst);
                        tokio::time::sleep(Duration::from_secs(1)).await;
                        running.fetch_sub(1, Ordering::SeqCst);
                        Ok(badge(PrState::Open, PrChecks::Pending))
                    })
                    .await
            }));
        }
        for ask in asks {
            assert!(ask.await.unwrap().is_some());
        }
        assert_eq!(calls.load(Ordering::SeqCst), 10, "one call per key");
        assert_eq!(peak.load(Ordering::SeqCst), MAX_IN_FLIGHT);
    }
}
