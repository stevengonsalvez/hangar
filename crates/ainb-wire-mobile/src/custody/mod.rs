//! Device key custody: the Noise static keypair never leaves this crate.
//!
//! The app gets a fingerprint and nothing else. One entry point,
//! [`DeviceKey::load_or_create`], picks the backend for the target:
//!
//! | target  | backend                                              | degraded |
//! |---------|------------------------------------------------------|----------|
//! | iOS     | Keychain, `AfterFirstUnlockThisDeviceOnly`, ungated | no       |
//! | Android | app-private `0600` file (Keystore via JNI pending)   | yes      |
//! | other   | app-private `0600` file (tests, desktop)             | no       |
//!
//! `degraded` means the key is at rest in the app sandbox rather than in
//! hardware-backed storage; the app shows it on the Log screen so the owner
//! knows which phones carry a file key.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use ainb_hangar_noise::NOISE_PATTERN;

use crate::records::WireError;

#[cfg(target_os = "ios")]
mod ios;

/// The bytes a backend stores: private key then public key.
pub type KeyBytes = Vec<u8>;

/// Which backend holds the key on this target, for the Log screen.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct CustodyReport {
    /// `keychain` or `file`.
    pub backend: String,
    /// Whether the key is at rest in the app sandbox rather than in
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
                    "android keystore backend pending; key in an app-private 0600 file".to_owned(),
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

/// The file the file backend keeps the keypair in, under the custody dir.
pub const KEY_FILE: &str = "device.key";
const PRIVATE_LEN: usize = 32;
const PUBLIC_LEN: usize = 32;

/// This device's Noise static keypair.
#[derive(Clone)]
pub struct DeviceKey {
    private: Vec<u8>,
    public: Vec<u8>,
}

impl std::fmt::Debug for DeviceKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeviceKey")
            .field("fingerprint", &self.fingerprint())
            .finish_non_exhaustive()
    }
}

pub(crate) fn custody_error(e: impl std::fmt::Display) -> WireError {
    WireError::Custody {
        message: e.to_string(),
    }
}

impl DeviceKey {
    /// Mint a fresh keypair.
    pub fn generate() -> Result<Self, WireError> {
        let keypair = snow::Builder::new(NOISE_PATTERN.parse().map_err(custody_error)?)
            .generate_keypair()
            .map_err(custody_error)?;
        Ok(Self {
            private: keypair.private,
            public: keypair.public,
        })
    }

    /// The keypair for this device, minted on first use: the Keychain on iOS,
    /// the `0600` file under `dir` elsewhere.
    pub fn load_or_create(dir: &Path) -> Result<Self, WireError> {
        #[cfg(target_os = "ios")]
        {
            let _ = dir;
            if let Some(bytes) = ios::load()? {
                return Self::from_file_bytes(&bytes);
            }
            let key = Self::generate()?;
            ios::store(&key.bytes())?;
            return Ok(key);
        }
        #[cfg(not(target_os = "ios"))]
        Self::load_or_create_file(dir)
    }

    fn bytes(&self) -> KeyBytes {
        let mut bytes = self.private.clone();
        bytes.extend_from_slice(&self.public);
        bytes
    }

    /// The file backend: `<dir>/device.key`, created `0600`.
    pub fn load_or_create_file(dir: &Path) -> Result<Self, WireError> {
        let path = dir.join(KEY_FILE);
        match std::fs::read(&path) {
            Ok(bytes) => Self::from_file_bytes(&bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let key = Self::generate()?;
                key.write(&path)?;
                Ok(key)
            }
            Err(e) => Err(custody_error(e)),
        }
    }

    fn from_file_bytes(bytes: &[u8]) -> Result<Self, WireError> {
        if bytes.len() != PRIVATE_LEN + PUBLIC_LEN {
            return Err(custody_error(format!(
                "{KEY_FILE} holds {} bytes, expected {}",
                bytes.len(),
                PRIVATE_LEN + PUBLIC_LEN
            )));
        }
        Ok(Self {
            private: bytes[..PRIVATE_LEN].to_vec(),
            public: bytes[PRIVATE_LEN..].to_vec(),
        })
    }

    fn write(&self, path: &PathBuf) -> Result<(), WireError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(custody_error)?;
        }
        let bytes = self.bytes();
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        options
            .open(path)
            .and_then(|mut f| std::io::Write::write_all(&mut f, &bytes))
            .map_err(custody_error)
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
