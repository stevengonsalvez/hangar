// ABOUTME: Persistent storage for durable session labels
// Stores custom labels by stable tmux session name.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const FILE: &str = "session-labels.json";
const LEGACY_FILE: &str = "ssh_display_names.json";

/// Normalize a durable session label before it reaches disk or the UI.
///
/// Blank input clears a label. Control characters are rejected because labels
/// render inside terminal rows, and 64 characters keeps every layout usable.
pub fn normalize_session_label(raw: &str) -> Result<Option<String>, String> {
    let label = raw.trim();
    if label.is_empty() {
        return Ok(None);
    }
    if label.chars().any(char::is_control) {
        return Err("Session label cannot contain control characters".to_string());
    }
    if label.chars().count() > 64 {
        return Err("Session label must be 64 characters or fewer".to_string());
    }
    Ok(Some(label.to_string()))
}

/// Store for durable session labels.
/// Maps tmux session name to a human-provided label, independent from Git.
///
/// The file has several writers: the terminal's label popup, the desktop
/// window's rename and `ainb label`, each in its own process. So there is no
/// whole-store save: [`Self::set_label`] is the one write, and it changes one
/// label on the file as it stands, under the lock [`Self::load`] reads under.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript-bindings", derive(specta::Type))]
pub struct SessionLabelStore {
    /// Map of tmux_session_name -> display_name
    #[serde(flatten)]
    names: HashMap<String, String>,
}

impl SessionLabelStore {
    /// The directory the store lives in. The legacy file sits beside it, so a
    /// relocated home never reads the real home's labels.
    fn storage_dir() -> Option<PathBuf> {
        std::env::var_os("AINB_HOME")
            .map(PathBuf::from)
            .or_else(dirs::home_dir)
            .map(|base| base.join(".agents-in-a-box"))
    }

    /// Load from disk, under the lock writers hold. No file is an empty store,
    /// and so is a file that does not parse: a reader shows what it can, and
    /// only a write, which would replace the file, refuses it.
    pub fn load() -> Self {
        let Some(dir) = Self::storage_dir() else {
            return Self::default();
        };
        Self::load_in(&dir).unwrap_or_else(|error| {
            // The kind only: a parse error can quote a label out of the file.
            tracing::warn!(kind = ?error.kind(), "session labels not loaded");
            Self::default()
        })
    }

    fn load_in(dir: &Path) -> io::Result<Self> {
        Self::load_in_locked_by(dir, crate::config::lock::lock_for)
    }

    /// [`Self::load_in`] with the lock taken by `lock`, so a test can refuse
    /// it the way a filesystem this process cannot write does.
    fn load_in_locked_by<L>(
        dir: &Path,
        lock: impl FnOnce(&Path) -> io::Result<L>,
    ) -> io::Result<Self> {
        // Nothing has ever been saved, and a read creates no directory.
        if !dir.is_dir() {
            return Ok(Self::default());
        }
        // A home this process may read but not write, by permission or on a
        // read-only filesystem, still shows its labels: a write replaces the
        // file whole by rename, so a read without the lock never sees half of
        // one.
        let _lock = match lock(&dir.join(FILE)) {
            Ok(lock) => Some(lock),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::PermissionDenied | io::ErrorKind::ReadOnlyFilesystem
                ) =>
            {
                None
            }
            Err(error) => return Err(error),
        };
        Self::read(dir)
    }

    /// Set the label for `tmux_session_name` in the store on disk (`None`
    /// clears it) and return the store as written.
    ///
    /// Reads the file, changes the one label and writes the file back, all
    /// under its lock, so a label another writer set since this process
    /// loaded the store is kept.
    ///
    /// # Errors
    /// The file exists but does not parse, and is left byte for byte as it
    /// was (`InvalidData`); or it cannot be read or written.
    pub fn set_label(tmux_session_name: &str, label: Option<String>) -> io::Result<Self> {
        let dir = Self::storage_dir().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "no home directory to keep session labels in",
            )
        })?;
        fs::create_dir_all(&dir)?;
        let path = dir.join(FILE);
        let _lock = crate::config::lock::lock_for(&path)?;
        let mut store = Self::read(&dir).map_err(|error| {
            io::Error::new(error.kind(), format!("{error}; the label was not saved"))
        })?;
        store.set(tmux_session_name.to_string(), label);
        crate::config::write_atomic(&path, &serde_json::to_string_pretty(&store)?)?;
        Ok(store)
    }

    /// The store in `dir`: the current file, else the legacy one. The caller
    /// holds the lock.
    fn read(dir: &Path) -> io::Result<Self> {
        match fs::read_to_string(dir.join(FILE)) {
            Ok(content) => serde_json::from_str(&content).map_err(|error| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{FILE} does not parse ({error}), so it was left as it is"),
                )
            }),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Self::read_legacy(dir)),
            Err(error) => Err(error),
        }
    }

    /// The legacy file's labels, where the store starts until the current
    /// file exists. No write touches the legacy file, so one that cannot be
    /// read is only logged and the store starts empty.
    fn read_legacy(dir: &Path) -> Self {
        let Ok(content) = fs::read_to_string(dir.join(LEGACY_FILE)) else {
            return Self::default();
        };
        serde_json::from_str(&content).unwrap_or_else(|error: serde_json::Error| {
            tracing::warn!(category = ?error.classify(), "{LEGACY_FILE} not loaded");
            Self::default()
        })
    }

    /// Get display name for a tmux session
    pub fn get(&self, tmux_session_name: &str) -> Option<&String> {
        self.names.get(tmux_session_name)
    }

    /// Set display name in this copy only (None removes it); the file is
    /// written by [`Self::set_label`].
    pub fn set(&mut self, tmux_session_name: String, display_name: Option<String>) {
        match display_name {
            Some(name) => {
                self.names.insert(tmux_session_name, name);
            }
            None => {
                self.names.remove(&tmux_session_name);
            }
        }
    }
}

/// Compatibility alias for code that still refers to SSH display names.
pub type SshDisplayNameStore = SessionLabelStore;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_labels_trim_clear_and_reject_unsafe_values() {
        assert_eq!(
            normalize_session_label("  RPC flake  ").expect("valid label"),
            Some("RPC flake".to_string())
        );
        assert_eq!(normalize_session_label("   ").expect("blank clears"), None);
        assert!(normalize_session_label("two\nlines").is_err());
        assert!(normalize_session_label(&"x".repeat(65)).is_err());
    }

    #[test]
    fn test_store_get_set() {
        let mut store = SessionLabelStore::default();

        // Initially empty
        assert!(store.get("ssh-test-22").is_none());

        // Set a name
        store.set("ssh-test-22".to_string(), Some("My Server".to_string()));
        assert_eq!(store.get("ssh-test-22"), Some(&"My Server".to_string()));

        // Clear the name
        store.set("ssh-test-22".to_string(), None);
        assert!(store.get("ssh-test-22").is_none());
    }

    /// A home this process cannot write: the lock file cannot be created, so
    /// the lock is refused with `kind`, and the read goes on without it.
    fn load_with_lock_refused(kind: io::ErrorKind) -> io::Result<SessionLabelStore> {
        let dir = tempfile::tempdir().expect("temporary label dir");
        fs::write(dir.path().join(FILE), r#"{"tmux-a": "Fix login"}"#).expect("label file");
        SessionLabelStore::load_in_locked_by(dir.path(), |_| Err::<(), _>(io::Error::from(kind)))
    }

    #[test]
    fn a_home_on_a_read_only_filesystem_still_shows_its_labels() {
        for kind in [
            io::ErrorKind::PermissionDenied,
            io::ErrorKind::ReadOnlyFilesystem,
        ] {
            let store = load_with_lock_refused(kind)
                .unwrap_or_else(|error| panic!("{kind:?} lock refusal failed the read: {error}"));
            assert_eq!(
                store.get("tmux-a").map(String::as_str),
                Some("Fix login"),
                "{kind:?}"
            );
        }
    }

    #[test]
    fn a_lock_refused_for_another_reason_fails_the_read() {
        let error = load_with_lock_refused(io::ErrorKind::Other).expect_err("the read fails");
        assert_eq!(error.kind(), io::ErrorKind::Other);
    }

    #[test]
    fn test_store_serialization() {
        let mut store = SessionLabelStore::default();
        store.set("ssh-prod-22".to_string(), Some("Production".to_string()));
        store.set("ssh-staging-22".to_string(), Some("Staging".to_string()));

        let json = serde_json::to_string(&store).unwrap();
        let loaded: SessionLabelStore = serde_json::from_str(&json).unwrap();

        assert_eq!(loaded.get("ssh-prod-22"), Some(&"Production".to_string()));
        assert_eq!(loaded.get("ssh-staging-22"), Some(&"Staging".to_string()));
    }
}
