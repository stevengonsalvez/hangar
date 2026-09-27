// ABOUTME: Disk install/uninstall of the ATC lifecycle hooks into Claude's
// `~/.claude/settings.json`, using the pure merge in `hooks.rs`.
//
// This is the read-preserve-modify-write side that touches the filesystem:
// read the user's settings.json (preserving every existing hook — user,
// reflect, notifyd), merge in the ATC managed block (`hooks::merge_into`), and
// write it back atomically. Uninstall strips exactly the ATC block back out.
// Idempotent: re-running install yields byte-identical settings.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use fs2::FileExt;
use serde_json::Value;

use super::atomic::write_atomic;
use super::hooks;

/// Path to Claude Code's user settings under an explicit `$HOME`.
#[must_use]
pub fn claude_settings_path(home: &Path) -> PathBuf {
    home.join(".claude").join("settings.json")
}

/// Path to the advisory lock guarding the settings.json read-merge-write window.
#[must_use]
pub fn settings_lock_path(home: &Path) -> PathBuf {
    home.join(".claude").join("settings.json.lock")
}

/// Acquire an exclusive advisory lock guarding the settings.json
/// read-merge-write window.
///
/// HONEST SCOPE (H2): this lock only serialises concurrent **ATC installs**
/// (`install_claude_hooks` / `uninstall_claude_hooks` racing each other). It is
/// NOT a cross-tool lock — `settings.json.lock` is referenced only here; reflect
/// and notifyd write `settings.json` WITHOUT taking it. So cross-tool safety
/// (an ATC install not clobbering a concurrent reflect/notifyd rewrite) relies
/// on those installs not overlapping in time, not on this lock. (A shared
/// cross-tool lock or a marketplace-path rearchitecture is a deferred follow-up;
/// do not assume this lock provides it.) Mirrors the inbox lock mechanism.
fn lock_settings(home: &Path) -> Result<std::fs::File> {
    let path = settings_lock_path(home);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating settings dir {}", parent.display()))?;
    }
    let f = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .context("opening settings.json lock file")?;
    f.lock_exclusive().context("acquiring settings.json lock")?;
    Ok(f)
}

/// Install the ATC lifecycle hooks into `<home>/.claude/settings.json`, pointing
/// every managed event at `hook_script`. Preserves all existing hooks. Returns
/// the settings path written. Idempotent. The read-merge-write is performed
/// under an advisory lock that serialises concurrent ATC installs (NOT a
/// cross-tool lock — see [`lock_settings`]).
pub fn install_claude_hooks(home: &Path, hook_script: &Path) -> Result<PathBuf> {
    let _guard = lock_settings(home)?;
    let path = claude_settings_path(home);
    let existing = read_settings(&path)?;
    let merged = hooks::merge_into(existing, &hook_script.to_string_lossy());
    let bytes = serde_json::to_vec_pretty(&merged).context("serializing settings.json")?;
    write_settings(&path, &bytes)?;
    Ok(path)
}

/// Path of the one-time backup written before a hook transport change.
#[must_use]
pub fn settings_backup_path(home: &Path) -> PathBuf {
    home.join(".claude").join("settings.json.ainb.bak")
}

/// Install the managed hooks for `transport` into
/// `<home>/.claude/settings.json`, pointing every managed event at
/// `hook_script`. Every non-managed hook is kept.
///
/// When the settings already hold the OTHER transport's entries (or none, on
/// the way to `Http`), the file as it was is first copied to
/// [`settings_backup_path`]. Same lock, same atomic write, same refusal of a
/// malformed file as [`install_claude_hooks`].
///
/// `hangar_home` is pinned into every HTTP command as `AINB_HANGAR_HOME`, so
/// the hook reaches the daemon whose endpoint setup found; it is unused for
/// `Legacy`.
pub fn install_claude_hooks_for(
    home: &Path,
    hook_script: &Path,
    transport: hooks::HookTransport,
    hangar_home: &Path,
) -> Result<PathBuf> {
    let _guard = lock_settings(home)?;
    let path = claude_settings_path(home);
    let existing = read_settings(&path)?;
    let before = hooks::installed_transport(&existing);
    let changing = match transport {
        hooks::HookTransport::Http => before != Some(hooks::HookTransport::Http),
        hooks::HookTransport::Legacy => before == Some(hooks::HookTransport::Http),
    };
    if changing && path.exists() {
        std::fs::copy(&path, settings_backup_path(home))
            .with_context(|| format!("backing up {}", path.display()))?;
    }
    let script = hook_script.to_string_lossy();
    let merged = match transport {
        hooks::HookTransport::Http => {
            hooks::merge_http_into(existing, &script, &hangar_home.to_string_lossy())
        }
        hooks::HookTransport::Legacy => hooks::merge_legacy_into(existing, &script),
    };
    let bytes = serde_json::to_vec_pretty(&merged).context("serializing settings.json")?;
    write_settings(&path, &bytes)?;
    Ok(path)
}

/// Record which transport the managed hooks use, in
/// `<ainb_home>/hooks/transport`. `notify.sh` reads it: under `http` it stands
/// down for Claude so a plugin-registered copy never races the daemon's hold.
///
/// `Legacy` REMOVES the marker rather than writing `legacy`: no marker is the
/// v1.29.0 state, so an older binary's install, setup or teardown never has a
/// stale `http` to forget.
pub fn record_transport(ainb_home: &Path, transport: hooks::HookTransport) -> Result<PathBuf> {
    let path = transport_marker_path(ainb_home);
    match transport {
        hooks::HookTransport::Http => {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("creating {}", parent.display()))?;
            }
            write_atomic(&path, b"http\n")?;
        }
        hooks::HookTransport::Legacy => match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("removing {}", path.display())),
        },
    }
    Ok(path)
}

/// Where [`record_transport`] keeps the marker under an ainb home.
#[must_use]
pub fn transport_marker_path(ainb_home: &Path) -> PathBuf {
    ainb_home.join("hooks").join("transport")
}

/// Write `settings.json` without breaking what the user set up: a symlinked
/// settings file stays a symlink (the TARGET is replaced, by a temp file
/// beside it), and the file keeps its mode.
fn write_settings(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};

    let target = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let mode = std::fs::metadata(&target).map_or(0o644, |m| m.permissions().mode() & 0o7777);
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let tmp = target.with_file_name(format!(
        ".{}.ainb.{}.tmp",
        target
            .file_name()
            .map_or_else(|| "settings.json".into(), |n| n.to_string_lossy()),
        std::process::id()
    ));
    let _ = std::fs::remove_file(&tmp);
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(&tmp)
        .with_context(|| format!("creating {}", tmp.display()))?;
    // `mode` at create is filtered by the umask; set it exactly.
    f.set_permissions(std::fs::Permissions::from_mode(mode))?;
    f.write_all(bytes)?;
    f.sync_all()?;
    drop(f);
    std::fs::rename(&tmp, &target).with_context(|| format!("replacing {}", target.display()))
}

/// Strip the ATC lifecycle hooks from `<home>/.claude/settings.json`, leaving
/// every other hook intact. No-op when the file is absent. Idempotent. The
/// read-merge-write is performed under the same advisory lock as install.
pub fn uninstall_claude_hooks(home: &Path) -> Result<()> {
    let _guard = lock_settings(home)?;
    let path = claude_settings_path(home);
    if !path.exists() {
        return Ok(());
    }
    let existing = read_settings(&path)?;
    let stripped = hooks::strip_from(existing);
    let bytes = serde_json::to_vec_pretty(&stripped).context("serializing settings.json")?;
    write_settings(&path, &bytes)
}

/// Read + parse settings.json, tolerating absence (→ empty object) and an empty
/// file. A genuinely malformed file is an error (we refuse to clobber it).
fn read_settings(path: &Path) -> Result<Value> {
    if !path.exists() {
        return Ok(Value::Object(serde_json::Map::new()));
    }
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    if text.trim().is_empty() {
        return Ok(Value::Object(serde_json::Map::new()));
    }
    serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    fn write_reflect_and_notifyd(home: &Path) {
        let path = claude_settings_path(home);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let settings = json!({
            "hooks": {
                "Stop": [
                    { "matcher": "", "hooks": [
                        { "type": "command", "command": "uv run ${CLAUDE_PLUGIN_ROOT}/hooks/stop_reflect.py" }
                    ]},
                    { "matcher": "", "hooks": [
                        { "type": "command", "command": "AINB_AGENT=claude /x/notify.sh" }
                    ]}
                ],
                "PreCompact": [
                    { "matcher": "", "hooks": [
                        { "type": "command", "command": "uv run ${CLAUDE_PLUGIN_ROOT}/hooks/precompact_reflect.py --auto" }
                    ]}
                ]
            },
            "otherUserSetting": 42
        });
        std::fs::write(&path, serde_json::to_string_pretty(&settings).unwrap()).unwrap();
    }

    fn read(home: &Path) -> Value {
        serde_json::from_str(&std::fs::read_to_string(claude_settings_path(home)).unwrap()).unwrap()
    }

    #[test]
    fn install_preserves_reflect_notifyd_and_user_settings() {
        let home = TempDir::new().unwrap();
        write_reflect_and_notifyd(home.path());
        install_claude_hooks(home.path(), Path::new("/x/notify.sh")).unwrap();
        let v = read(home.path());

        // Unrelated user setting survives.
        assert_eq!(v["otherUserSetting"], 42);
        // reflect + notifyd Stop hooks survive.
        let stop: Vec<String> = v["hooks"]["Stop"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|e| e["hooks"].as_array().cloned().unwrap_or_default())
            .filter_map(|h| h["command"].as_str().map(str::to_string))
            .collect();
        assert!(stop.iter().any(|c| c.contains("stop_reflect.py")));
        assert!(stop.iter().any(|c| c.contains("notify.sh")));
        // PreCompact (reflect-only, unmanaged) survives.
        assert!(v["hooks"]["PreCompact"].is_array());
        // ATC events added.
        assert!(v["hooks"]["SessionEnd"].is_array());
    }

    #[test]
    fn install_is_idempotent_on_disk() {
        let home = TempDir::new().unwrap();
        write_reflect_and_notifyd(home.path());
        install_claude_hooks(home.path(), Path::new("/x/notify.sh")).unwrap();
        let first = std::fs::read_to_string(claude_settings_path(home.path())).unwrap();
        install_claude_hooks(home.path(), Path::new("/x/notify.sh")).unwrap();
        let second = std::fs::read_to_string(claude_settings_path(home.path())).unwrap();
        assert_eq!(first, second, "re-install drifted the file");
    }

    #[test]
    fn install_then_uninstall_restores_non_atc_hooks() {
        let home = TempDir::new().unwrap();
        write_reflect_and_notifyd(home.path());
        install_claude_hooks(home.path(), Path::new("/x/notify.sh")).unwrap();
        uninstall_claude_hooks(home.path()).unwrap();
        let v = read(home.path());
        // No ATC entries remain.
        for (event, _matcher) in hooks::ATC_EVENTS {
            let atc = v["hooks"][event]
                .as_array()
                .map(|a| a.iter().filter(|e| hooks::is_atc_managed(e)).count())
                .unwrap_or(0);
            assert_eq!(atc, 0, "ATC entry survived uninstall on {event}");
        }
        // reflect + notifyd remain.
        let stop: Vec<String> = v["hooks"]["Stop"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|e| e["hooks"].as_array().cloned().unwrap_or_default())
            .filter_map(|h| h["command"].as_str().map(str::to_string))
            .collect();
        assert!(stop.iter().any(|c| c.contains("stop_reflect.py")));
        assert!(stop.iter().any(|c| c.contains("notify.sh")));
    }

    #[test]
    fn install_on_fresh_machine_creates_settings() {
        let home = TempDir::new().unwrap();
        let p = install_claude_hooks(home.path(), Path::new("/x/notify.sh")).unwrap();
        assert!(p.exists());
        let v = read(home.path());
        assert!(v["hooks"]["Stop"].is_array());
    }

    #[test]
    fn uninstall_is_noop_when_settings_absent() {
        let home = TempDir::new().unwrap();
        uninstall_claude_hooks(home.path()).unwrap(); // must not error
    }

    /// Regression: the read-merge-write window is guarded by an advisory lock so
    /// two concurrent ATC installs cannot silently drop each other's merge. (It
    /// is NOT a cross-tool lock — reflect/notifyd don't take it; see
    /// `lock_settings`.) We assert the lock file is created and used by install —
    /// the marker that the locked path was actually taken.
    #[test]
    fn install_takes_the_settings_lock() {
        let home = TempDir::new().unwrap();
        let lock = settings_lock_path(home.path());
        assert!(!lock.exists(), "lock file should not exist before install");
        install_claude_hooks(home.path(), Path::new("/x/notify.sh")).unwrap();
        assert!(
            lock.exists(),
            "install must create + use the settings.json lock"
        );
    }

    /// The settings lock serialises concurrent ATC installs: two threads racing
    /// `install_claude_hooks` against the same settings.json both complete and
    /// the result is a single coherent file with the ATC hooks present (no torn
    /// write, no lost merge). With the advisory lock the read-merge-write is
    /// mutually exclusive. (Cross-tool serialisation with reflect/notifyd is NOT
    /// provided — see `lock_settings`.)
    #[test]
    fn concurrent_installs_serialise_under_the_lock() {
        use std::sync::Arc;
        let home = Arc::new(TempDir::new().unwrap());
        write_reflect_and_notifyd(home.path());

        let mut handles = Vec::new();
        for _ in 0..8 {
            let h = home.clone();
            handles.push(std::thread::spawn(move || {
                install_claude_hooks(h.path(), Path::new("/x/notify.sh")).unwrap();
            }));
        }
        for j in handles {
            j.join().unwrap();
        }

        // The file is coherent: parses, preserves reflect + notifyd, and carries
        // the ATC hooks.
        let v = read(home.path());
        assert_eq!(v["otherUserSetting"], 42);
        assert!(v["hooks"]["SessionEnd"].is_array(), "ATC hooks present");
        let stop: Vec<String> = v["hooks"]["Stop"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|e| e["hooks"].as_array().cloned().unwrap_or_default())
            .filter_map(|h| h["command"].as_str().map(str::to_string))
            .collect();
        assert!(stop.iter().any(|c| c.contains("stop_reflect.py")));
        assert!(stop.iter().any(|c| c.contains("notify.sh")));
    }

    #[test]
    fn http_then_legacy_round_trips_to_the_bytes_of_a_fresh_install() {
        let fresh_home = TempDir::new().unwrap();
        write_reflect_and_notifyd(fresh_home.path());
        install_claude_hooks(fresh_home.path(), Path::new("/x/notify.sh")).unwrap();
        let fresh = std::fs::read(claude_settings_path(fresh_home.path())).unwrap();

        let home = TempDir::new().unwrap();
        write_reflect_and_notifyd(home.path());
        install_claude_hooks(home.path(), Path::new("/x/notify.sh")).unwrap();
        install_claude_hooks_for(
            home.path(),
            Path::new("/x/ainb-hook.sh"),
            hooks::HookTransport::Http,
            Path::new("/x/hangar"),
        )
        .unwrap();
        let http = read(home.path());
        assert_eq!(
            hooks::installed_transport(&http),
            Some(hooks::HookTransport::Http)
        );
        assert_eq!(http["otherUserSetting"], 42);
        install_claude_hooks_for(
            home.path(),
            Path::new("/x/notify.sh"),
            hooks::HookTransport::Legacy,
            Path::new("/x/hangar"),
        )
        .unwrap();
        assert_eq!(
            std::fs::read(claude_settings_path(home.path())).unwrap(),
            fresh
        );
    }

    #[test]
    fn a_backup_is_taken_once_per_transport_change() {
        let home = TempDir::new().unwrap();
        write_reflect_and_notifyd(home.path());
        install_claude_hooks(home.path(), Path::new("/x/notify.sh")).unwrap();
        let before = std::fs::read(claude_settings_path(home.path())).unwrap();
        let backup = settings_backup_path(home.path());
        assert!(!backup.exists(), "a legacy install never backs up");

        install_claude_hooks_for(
            home.path(),
            Path::new("/x/ainb-hook.sh"),
            hooks::HookTransport::Http,
            Path::new("/x/hangar"),
        )
        .unwrap();
        assert_eq!(
            std::fs::read(&backup).unwrap(),
            before,
            "the pre-change file"
        );
        std::fs::remove_file(&backup).unwrap();
        install_claude_hooks_for(
            home.path(),
            Path::new("/x/ainb-hook.sh"),
            hooks::HookTransport::Http,
            Path::new("/x/hangar"),
        )
        .unwrap();
        assert!(
            !backup.exists(),
            "a re-install of the same transport is not a change"
        );
    }

    #[test]
    fn http_install_refuses_a_malformed_settings_file() {
        let home = TempDir::new().unwrap();
        let path = claude_settings_path(home.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{ not json").unwrap();
        assert!(
            install_claude_hooks_for(
                home.path(),
                Path::new("/x/ainb-hook.sh"),
                hooks::HookTransport::Http,
                Path::new("/x/hangar")
            )
            .is_err()
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ not json");
    }

    #[test]
    fn the_transport_marker_is_one_word() {
        let ainb = TempDir::new().unwrap();
        let path = record_transport(ainb.path(), hooks::HookTransport::Http).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "http\n");
        record_transport(ainb.path(), hooks::HookTransport::Legacy).unwrap();
        assert!(!path.exists(), "legacy is the absence of the marker");
        record_transport(ainb.path(), hooks::HookTransport::Legacy).unwrap();
    }

    #[test]
    fn a_symlinked_settings_file_stays_a_symlink_and_keeps_its_mode() {
        use std::os::unix::fs::PermissionsExt as _;
        let home = TempDir::new().unwrap();
        let dotfiles = TempDir::new().unwrap();
        let real = dotfiles.path().join("claude-settings.json");
        std::fs::write(&real, r#"{"theme":"dark"}"#).unwrap();
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o640)).unwrap();
        let link = claude_settings_path(home.path());
        std::fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();

        install_claude_hooks_for(
            home.path(),
            Path::new("/x/ainb-hook.sh"),
            hooks::HookTransport::Http,
            Path::new("/x/hangar"),
        )
        .unwrap();
        assert!(std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
        let written: Value =
            serde_json::from_str(&std::fs::read_to_string(&real).unwrap()).unwrap();
        assert_eq!(written["theme"], "dark");
        assert_eq!(
            hooks::installed_transport(&written),
            Some(hooks::HookTransport::Http)
        );
        assert_eq!(
            std::fs::metadata(&real).unwrap().permissions().mode() & 0o777,
            0o640
        );
        uninstall_claude_hooks(home.path()).unwrap();
        assert!(std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
    }

    #[test]
    fn a_0644_settings_file_stays_0644() {
        use std::os::unix::fs::PermissionsExt as _;
        let home = TempDir::new().unwrap();
        write_reflect_and_notifyd(home.path());
        let path = claude_settings_path(home.path());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        install_claude_hooks(home.path(), Path::new("/x/notify.sh")).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o644
        );
    }
}
