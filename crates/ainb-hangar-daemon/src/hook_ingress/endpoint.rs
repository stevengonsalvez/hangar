//! The two files a hook script reads to find the listener.
//!
//! `hook-headers` (the token, 0600) is written BEFORE `hook-endpoint.env` (the
//! port), each by temp file and rename, so a script that reads both never pairs
//! a new port with an old token. Both are removed on clean shutdown, but only
//! while they still name this process: a newer daemon's files are never
//! deleted by an older one's exit.

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
        let headers_abs = std::fs::canonicalize(&dir)?.join(HEADERS_FILE_NAME);

        write_private(&headers, render_headers_file(token).as_bytes())?;
        let rendered = HookEndpoint {
            port,
            version: HOOK_ENDPOINT_VERSION,
            pid: std::process::id(),
            headers_path: headers_abs,
        }
        .render_env_file();
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
            std::fs::read_to_string(&parsed.headers_path).unwrap(),
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
}
