//! The two files a hook script reads to find the listener.
//!
//! `hook-headers` (the token, 0600) is written BEFORE `hook-endpoint.env` (the
//! port), each by temp file and rename, so a script that reads both never pairs
//! a new port with an old token. Both are removed on clean shutdown, but only
//! while they still name this process: a newer daemon's files are never
//! deleted by an older one's exit. A crash, a SIGKILL or a second signal
//! skips that; [`remove_stale`] runs at every boot, switch or no switch, once
//! the daemon owns the home, so leftovers never outlive the next start.

use std::io::Write as _;
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

use ainb_hangar_proto::hooks::{
    ENDPOINT_FILE_NAME, HEADERS_FILE_NAME, HOOK_ENDPOINT_VERSION, HookEndpoint, render_headers_file,
};

/// The published files; removing them is the `Drop`.
#[derive(Debug)]
pub struct EndpointFiles {
    endpoint: PathBuf,
    headers: PathBuf,
    rendered: String,
}

impl EndpointFiles {
    /// Publish `port` and `token` under `<hangar_home>/hangar/`.
    ///
    /// # Errors
    /// Any create, write, chmod or rename failure. Nothing half-written stays
    /// visible under the final names.
    pub fn publish(hangar_home: &Path, port: u16, token: &str) -> std::io::Result<Self> {
        let dir = hangar_home.join("hangar");
        std::fs::create_dir_all(&dir)?;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
        let headers = dir.join(HEADERS_FILE_NAME);
        let endpoint = dir.join(ENDPOINT_FILE_NAME);
        let rendered = HookEndpoint {
            port,
            version: HOOK_ENDPOINT_VERSION,
            pid: std::process::id(),
        }
        .render_env_file();
        write_private(&headers, render_headers_file(token).as_bytes())?;
        write_private(&endpoint, rendered.as_bytes())?;
        Ok(Self {
            endpoint,
            headers,
            rendered,
        })
    }

    /// Path of the endpoint file.
    #[must_use]
    pub fn endpoint_path(&self) -> &Path {
        &self.endpoint
    }

    /// Path of the headers file.
    #[must_use]
    pub fn headers_path(&self) -> &Path {
        &self.headers
    }
}

impl Drop for EndpointFiles {
    /// Compare-and-delete: only while the endpoint file is still ours.
    fn drop(&mut self) {
        let ours = std::fs::read_to_string(&self.endpoint).is_ok_and(|now| now == self.rendered);
        if ours {
            let _ = std::fs::remove_file(&self.endpoint);
            let _ = std::fs::remove_file(&self.headers);
        }
    }
}

/// Remove the endpoint and headers files a previous daemon left behind.
///
/// Called at boot once this process owns the home (and before the listener,
/// if any, publishes its own), and best-effort on a forced exit. With the
/// switch off a leftover would otherwise stay forever; with it on it would
/// name a dead pid until the new files replace it.
pub fn remove_stale(hangar_home: &Path) {
    let dir = hangar_home.join("hangar");
    for name in [ENDPOINT_FILE_NAME, HEADERS_FILE_NAME] {
        match std::fs::remove_file(dir.join(name)) {
            Ok(()) => tracing::info!(file = name, "removed a stale hook endpoint file"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => tracing::warn!(file = name, error = %e, "could not remove a stale hook file"),
        }
    }
}

/// Write `bytes` to a 0600 temp file beside `path`, then rename over it.
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    drop(f);
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode(p: &Path) -> u32 {
        std::fs::metadata(p).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn publishes_private_files_and_no_token_in_the_endpoint() {
        let home = tempfile::tempdir().unwrap();
        let files = EndpointFiles::publish(home.path(), 4321, "tok-123").unwrap();
        assert_eq!(mode(files.endpoint_path()), 0o600);
        assert_eq!(mode(files.headers_path()), 0o600);
        assert_eq!(mode(&home.path().join("hangar")), 0o700);
        let endpoint = std::fs::read_to_string(files.endpoint_path()).unwrap();
        assert!(!endpoint.contains("tok-123"));
        let parsed = HookEndpoint::parse_env_file(&endpoint).unwrap();
        assert_eq!(parsed.port, 4321);
        assert_eq!(
            std::fs::read_to_string(files.headers_path()).unwrap(),
            "X-Ainb-Hook-Token: tok-123\n"
        );
    }

    #[test]
    fn drop_removes_its_own_files_only() {
        let home = tempfile::tempdir().unwrap();
        let first = EndpointFiles::publish(home.path(), 1111, "a").unwrap();
        let endpoint = first.endpoint_path().to_path_buf();
        let second = EndpointFiles::publish(home.path(), 2222, "b").unwrap();
        drop(first);
        assert!(endpoint.exists(), "a newer daemon's file survives");
        drop(second);
        assert!(!endpoint.exists());
        assert!(!home.path().join("hangar").join(HEADERS_FILE_NAME).exists());
    }

    #[test]
    fn republish_replaces_a_stale_crash_leftover() {
        let home = tempfile::tempdir().unwrap();
        let stale = EndpointFiles::publish(home.path(), 1111, "old").unwrap();
        std::mem::forget(stale); // a crash: no Drop
        let fresh = EndpointFiles::publish(home.path(), 2222, "new").unwrap();
        let text = std::fs::read_to_string(fresh.endpoint_path()).unwrap();
        assert!(text.contains("AINB_HOOK_PORT=2222"));
        assert!(std::fs::read_to_string(fresh.headers_path()).unwrap().contains("new"));
    }

    #[test]
    fn a_home_with_a_space_publishes_normally() {
        // The script reads the fixed headers path, so any home it can name
        // works; nothing is refused for its characters.
        let base = tempfile::tempdir().unwrap();
        let home = base.path().join("with space");
        std::fs::create_dir_all(&home).unwrap();
        let files = EndpointFiles::publish(&home, 1234, "tok").unwrap();
        assert!(files.endpoint_path().exists());
        assert!(files.headers_path().exists());
    }

    #[test]
    fn remove_stale_clears_leftovers_and_tolerates_none() {
        let home = tempfile::tempdir().unwrap();
        let leftover = EndpointFiles::publish(home.path(), 1111, "old").unwrap();
        let (endpoint, headers) = (
            leftover.endpoint_path().to_path_buf(),
            leftover.headers_path().to_path_buf(),
        );
        std::mem::forget(leftover); // a crash: no Drop
        remove_stale(home.path());
        assert!(!endpoint.exists());
        assert!(!headers.exists());
        remove_stale(home.path()); // nothing left: no panic
    }
}
