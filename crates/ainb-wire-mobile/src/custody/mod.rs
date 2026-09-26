//! Device secrets custody: the Noise static keypair and every device token
//! never leave this crate.
//!
//! The app gets a fingerprint and nothing else. One backend per target holds
//! named secrets ([`store_secret`], [`load_secret`], [`delete_secret`]):
//!
//! | target  | backend                                              | degraded |
//! |---------|------------------------------------------------------|----------|
//! | iOS     | Keychain, `AfterFirstUnlockThisDeviceOnly`, ungated | no       |
//! | Android | app-private `0600` files (Keystore via JNI pending)  | yes      |
//! | other   | app-private `0600` files (tests, desktop)            | no       |
//!
//! `degraded` means the secret is at rest in the app sandbox rather than in
//! hardware-backed storage; the app shows it on the Log screen so the owner
//! knows which phones carry file secrets.

use std::fmt::Write as _;
use std::path::Path;

use ainb_hangar_noise::NOISE_PATTERN;
use zeroize::Zeroizing;

use crate::records::WireError;

#[cfg(target_os = "ios")]
mod ios;

/// The secret name of the device keypair (private key then public key).
pub const KEY_FILE: &str = "device.key";
const PRIVATE_LEN: usize = 32;
const PUBLIC_LEN: usize = 32;

/// Which backend holds the secrets on this target, for the Log screen.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct CustodyReport {
    /// `keychain` or `file`.
    pub backend: String,
    /// Whether secrets are at rest in the app sandbox rather than in
    /// hardware-backed storage.
    pub degraded: bool,
    /// Why, when degraded.
    pub detail: Option<String>,
}

impl CustodyReport {
    /// The report for this target.
    #[must_use]
    pub fn for_target() -> Self {
        if cfg!(target_os = "ios") {
            Self {
                backend: "keychain".to_owned(),
                degraded: false,
                detail: None,
            }
        } else if cfg!(target_os = "android") {
            Self {
                backend: "file".to_owned(),
                degraded: true,
                detail: Some(
                    "android keystore backend pending; secrets in app-private 0600 files"
                        .to_owned(),
                ),
            }
        } else {
            Self {
                backend: "file".to_owned(),
                degraded: false,
                detail: None,
            }
        }
    }
}

pub(crate) fn custody_error(e: impl std::fmt::Display) -> WireError {
    WireError::Custody {
        message: e.to_string(),
    }
}

/// A secret name: one path component, so it cannot escape the custody dir.
fn checked(name: &str) -> Result<&str, WireError> {
    if name.is_empty()
        || name
            .chars()
            .any(|c| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_')))
    {
        return Err(custody_error(format!("bad secret name {name:?}")));
    }
    Ok(name)
}

/// Store `bytes` under `name`, replacing any previous value.
pub fn store_secret(dir: &Path, name: &str, bytes: &[u8]) -> Result<(), WireError> {
    let name = checked(name)?;
    #[cfg(target_os = "ios")]
    {
        let _ = dir;
        ios::store(name, bytes)
    }
    #[cfg(not(target_os = "ios"))]
    file::store(dir, name, bytes)
}

/// The secret under `name`, when one exists.
pub fn load_secret(dir: &Path, name: &str) -> Result<Option<Vec<u8>>, WireError> {
    let name = checked(name)?;
    #[cfg(target_os = "ios")]
    {
        let _ = dir;
        ios::load(name)
    }
    #[cfg(not(target_os = "ios"))]
    file::load(dir, name)
}

/// Forget the secret under `name`; absent is not an error.
pub fn delete_secret(dir: &Path, name: &str) -> Result<(), WireError> {
    let name = checked(name)?;
    #[cfg(target_os = "ios")]
    {
        let _ = dir;
        ios::delete(name)
    }
    #[cfg(not(target_os = "ios"))]
    file::delete(dir, name)
}

/// The file backend: `<dir>/<name>`, owner-only, written whole through a
/// rename so a kill mid-write leaves the old value, never a torn one.
#[cfg(not(target_os = "ios"))]
mod file {
    use std::io::Write as _;
    use std::path::Path;

    use super::custody_error;
    use crate::records::WireError;

    pub fn store(dir: &Path, name: &str, bytes: &[u8]) -> Result<(), WireError> {
        std::fs::create_dir_all(dir).map_err(custody_error)?;
        let path = dir.join(name);
        let tmp = dir.join(format!("{name}.tmp"));
        let _ = std::fs::remove_file(&tmp);
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        options
            .open(&tmp)
            .and_then(|mut f| f.write_all(bytes).and_then(|()| f.sync_all()))
            .and_then(|()| std::fs::rename(&tmp, &path))
            .map_err(custody_error)
    }

    pub fn load(dir: &Path, name: &str) -> Result<Option<Vec<u8>>, WireError> {
        match std::fs::read(dir.join(name)) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(custody_error(e)),
        }
    }

    pub fn delete(dir: &Path, name: &str) -> Result<(), WireError> {
        match std::fs::remove_file(dir.join(name)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(custody_error(e)),
        }
    }
}

/// This device's Noise static keypair.
#[derive(Clone)]
pub struct DeviceKey {
    private: Zeroizing<Vec<u8>>,
    public: Vec<u8>,
}

impl std::fmt::Debug for DeviceKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeviceKey")
            .field("fingerprint", &self.fingerprint())
            .finish_non_exhaustive()
    }
}

impl DeviceKey {
    /// Mint a fresh keypair.
    pub fn generate() -> Result<Self, WireError> {
        let keypair = snow::Builder::new(NOISE_PATTERN.parse().map_err(custody_error)?)
            .generate_keypair()
            .map_err(custody_error)?;
        Ok(Self {
            private: Zeroizing::new(keypair.private),
            public: keypair.public,
        })
    }

    /// The keypair for this device, minted on first use and kept as the
    /// secret [`KEY_FILE`] in this target's backend.
    pub fn load_or_create(dir: &Path) -> Result<Self, WireError> {
        // One minter at a time: two first-use callers (the pair screen's
        // fingerprint and the pair itself) must not each mint a key and let
        // the second store replace the one the first already used.
        static MINT: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _mint = MINT.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(bytes) = load_secret(dir, KEY_FILE)? {
            match Self::from_bytes(&bytes) {
                Ok(key) => return Ok(key),
                // A stored item of the wrong shape (a partial write, an
                // earlier build) is unusable and, in the Keychain, outlives
                // the app: it is replaced, not a permanent error. The old
                // identity was never going to connect again either way.
                Err(_) => delete_secret(dir, KEY_FILE)?,
            }
        }
        let key = Self::generate()?;
        store_secret(dir, KEY_FILE, &key.bytes())?;
        Ok(key)
    }

    fn from_bytes(bytes: &[u8]) -> Result<Self, WireError> {
        if bytes.len() != PRIVATE_LEN + PUBLIC_LEN {
            return Err(custody_error(format!(
                "the stored {KEY_FILE} item holds {} bytes, expected {}",
                bytes.len(),
                PRIVATE_LEN + PUBLIC_LEN
            )));
        }
        Ok(Self {
            private: Zeroizing::new(bytes[..PRIVATE_LEN].to_vec()),
            public: bytes[PRIVATE_LEN..].to_vec(),
        })
    }

    /// Private then public, zeroed when dropped.
    fn bytes(&self) -> Zeroizing<Vec<u8>> {
        let mut bytes = Zeroizing::new(self.private.to_vec());
        bytes.extend_from_slice(&self.public);
        bytes
    }

    /// The private key, for the Noise builder. Never crosses the uniffi
    /// boundary: nothing in [`crate::api`] returns it.
    #[must_use]
    pub fn private(&self) -> &[u8] {
        &self.private
    }

    /// The public key.
    #[must_use]
    pub fn public(&self) -> &[u8] {
        &self.public
    }

    /// The SHA-256 of the public key, lowercase hex: what `device/list` shows
    /// as `static_pubkey_digest`, and all the app ever sees of the key.
    #[must_use]
    pub fn fingerprint(&self) -> String {
        use sha2::Digest as _;
        let digest = sha2::Sha256::digest(&self.public);
        digest.iter().fold(String::with_capacity(64), |mut out, b| {
            let _ = write!(out, "{b:02x}");
            out
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(not(target_os = "ios"))]
    fn a_malformed_stored_key_is_replaced_not_a_permanent_error() {
        let dir = tempfile::tempdir().unwrap();
        store_secret(dir.path(), KEY_FILE, b"not a key").unwrap();
        let key = DeviceKey::load_or_create(dir.path()).unwrap();
        let again = DeviceKey::load_or_create(dir.path()).unwrap();
        assert_eq!(key.public(), again.public());
        assert_eq!(std::fs::read(dir.path().join(KEY_FILE)).unwrap().len(), 64);
    }

    #[test]
    #[cfg(not(target_os = "ios"))]
    fn the_file_backend_persists_one_keypair_with_owner_only_mode() {
        let dir = tempfile::tempdir().unwrap();
        let first = DeviceKey::load_or_create(dir.path()).unwrap();
        let second = DeviceKey::load_or_create(dir.path()).unwrap();
        assert_eq!(first.public(), second.public());
        assert_eq!(first.fingerprint(), second.fingerprint());
        assert_eq!(first.fingerprint().len(), 64);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(dir.path().join(KEY_FILE)).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        assert!(!format!("{first:?}").contains("private"));
        let other = DeviceKey::load_or_create(&dir.path().join("other")).unwrap();
        assert_ne!(other.public(), first.public());
    }

    #[test]
    fn secrets_replace_delete_and_refuse_path_names() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load_secret(dir.path(), "token-h1").unwrap(), None);
        store_secret(dir.path(), "token-h1", b"one").unwrap();
        store_secret(dir.path(), "token-h1", b"two").unwrap();
        assert_eq!(
            load_secret(dir.path(), "token-h1").unwrap().as_deref(),
            Some(&b"two"[..])
        );
        assert!(!dir.path().join("token-h1.tmp").exists());
        delete_secret(dir.path(), "token-h1").unwrap();
        delete_secret(dir.path(), "token-h1").unwrap();
        assert_eq!(load_secret(dir.path(), "token-h1").unwrap(), None);
        assert!(store_secret(dir.path(), "../escape", b"x").is_err());
        assert!(store_secret(dir.path(), "a/b", b"x").is_err());
        assert!(load_secret(dir.path(), "").is_err());
    }

    #[test]
    fn the_host_report_is_the_file_backend_and_only_android_is_degraded() {
        let report = CustodyReport::for_target();
        assert_eq!(
            report.backend,
            if cfg!(target_os = "ios") {
                "keychain"
            } else {
                "file"
            }
        );
        assert_eq!(report.degraded, cfg!(target_os = "android"));
        assert_eq!(report.detail.is_some(), report.degraded);
    }
}
