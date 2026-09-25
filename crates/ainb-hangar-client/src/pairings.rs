//! Paired hosts (R1-09), part 2: where a surface keeps what it learned when it
//! paired, before any socket uses it.
//!
//! ```text
//!  {home}/hangar/peers.json (0600)          secret custody, one entry per host
//!  ┌──────────────────────────────┐        ┌──────────────────────────────────┐
//!  │ host_id, host_static_pubkey, │        │ keychain  ainb-hangar::global    │
//!  │ endpoints, device_id, scope, │        │           account peer:<host_id> │
//!  │ expiry, paired / active at   │        │ or, where no keychain exists,    │
//!  │ NO secret material           │        │ {home}/hangar/peer-secrets/<id>  │
//!  └──────────────────────────────┘        │ (0600, dir 0700)                 │
//!                                          │ device static key + device token │
//!                                          └──────────────────────────────────┘
//! ```
//!
//! The file holds only what identifies a pairing; the device's Noise static
//! key and its bearer token live in the OS keychain through
//! `ainb-hangar-secrets`, one item per host so a keychain prompt is asked
//! once. On a platform whose keychain backend is not implemented (Linux
//! today), they fall back to a `0600` file beside `peers.json`. Saving writes
//! the secret first, then the row, so a row never names a pairing whose
//! secret is missing. Every file write is a temp file plus rename.
//!
//! Redeeming an offer (`pair`) and revoking this device before forgetting a
//! host (`forget`) need the peer transport and land with it.

use std::path::{Path, PathBuf};

use ainb_hangar_noise::offer::Endpoint;
use ainb_hangar_proto::devices::DeviceScope;
use ainb_hangar_proto::hosts::HostId;
use ainb_hangar_secrets::{Scope, SecretBackend, SecretBytes, SecretError};
use serde::{Deserialize, Serialize};

/// The `peers.json` layout version this build reads and writes.
pub const PEERS_FILE_VERSION: u32 = 1;

/// One paired host. Holds no secret material.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pairing {
    /// The host, always minted.
    #[serde(deserialize_with = "ainb_hangar_proto::hosts::deserialize_minted")]
    pub host_id: HostId,
    /// The host's Noise static public key, pinned at pairing.
    #[serde(with = "b64_32")]
    pub host_static_pubkey: [u8; 32],
    /// Where to dial, in preference order.
    pub endpoints: Vec<Endpoint>,
    /// The id the host gave this device.
    pub device_id: String,
    /// The name this device registered under.
    pub display_name: String,
    /// The scope the host granted.
    pub scope: DeviceScope,
    /// Unix milliseconds the device credential expires (sliding on the host).
    pub device_expires_at_ms: i64,
    /// Unix milliseconds of the pairing.
    pub paired_at_ms: i64,
    /// Unix milliseconds the user last acted on this host.
    pub last_active_ms: i64,
}

/// The secret half of a pairing.
#[derive(Clone, PartialEq, Eq)]
pub struct PairingSecrets {
    /// The device's Noise static private key for this host.
    pub device_static: SecretBytes,
    /// The bearer token `device/redeem` returned.
    pub device_token: SecretBytes,
}

impl std::fmt::Debug for PairingSecrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PairingSecrets(<redacted>)")
    }
}

impl PairingSecrets {
    /// The static key, if it is exactly 32 bytes.
    #[must_use]
    pub fn device_static_key(&self) -> Option<[u8; 32]> {
        self.device_static.as_bytes().try_into().ok()
    }

    /// The token as text.
    #[must_use]
    pub fn token(&self) -> Option<&str> {
        std::str::from_utf8(self.device_token.as_bytes()).ok()
    }
}

/// Why the pairing store failed.
#[derive(Debug, thiserror::Error)]
pub enum PairingError {
    /// A file could not be read or written.
    #[error("pairing store io at {path}: {source}")]
    Io {
        /// The file.
        path: PathBuf,
        /// The cause.
        source: std::io::Error,
    },
    /// `peers.json` or a secret entry is not what this build writes.
    #[error("pairing store {what} is malformed: {detail}")]
    Malformed {
        /// Which file or entry.
        what: String,
        /// The parse error.
        detail: String,
    },
    /// A `peers.json` from a newer build.
    #[error("peers.json version {0} is newer than this build reads")]
    Version(u32),
    /// The keychain refused.
    #[error("pairing secret custody: {0}")]
    Custody(#[from] SecretError),
    /// A row names a host whose secret is missing.
    #[error("pairing for {0} has no stored secret")]
    MissingSecret(HostId),
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct PeersFile {
    version: u32,
    #[serde(default)]
    pairings: Vec<Pairing>,
}

#[derive(Serialize, Deserialize)]
struct StoredSecret {
    device_static: String,
    device_token: String,
}

/// Where the secret half lives.
enum Custody {
    /// The OS keychain; falls back to the file custody when the platform
    /// backend is not implemented.
    Keychain(Box<dyn SecretBackend + Send + Sync>),
    /// A `0600` file per host.
    File,
}

/// `peers.json` plus the secret custody beside it.
pub struct PairingStore {
    dir: PathBuf,
    custody: Custody,
}

impl std::fmt::Debug for PairingStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PairingStore")
            .field("dir", &self.dir)
            .field(
                "custody",
                &match self.custody {
                    Custody::Keychain(_) => "keychain",
                    Custody::File => "file",
                },
            )
            .finish()
    }
}

fn platform_backend() -> Option<Box<dyn SecretBackend + Send + Sync>> {
    #[cfg(target_os = "macos")]
    {
        Some(Box::new(ainb_hangar_secrets::MacKeychainBackend::new()))
    }
    #[cfg(target_os = "linux")]
    {
        Some(Box::new(
            ainb_hangar_secrets::LinuxSecretServiceBackend::new(),
        ))
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        None
    }
}

fn secret_account(host_id: &HostId) -> String {
    format!("peer:{host_id}")
}

impl PairingStore {
    /// The store under `hangar_home` (`{home}/hangar/`), with the platform
    /// keychain.
    #[must_use]
    pub fn open_in(hangar_home: &Path) -> Self {
        Self {
            dir: hangar_home.join("hangar"),
            custody: platform_backend().map_or(Custody::File, Custody::Keychain),
        }
    }

    /// The store under `hangar_home` with an explicit keychain backend (tests
    /// use the in-memory one).
    #[must_use]
    pub fn with_backend(hangar_home: &Path, backend: Box<dyn SecretBackend + Send + Sync>) -> Self {
        Self {
            dir: hangar_home.join("hangar"),
            custody: Custody::Keychain(backend),
        }
    }

    /// The store under `hangar_home` with the file custody only.
    #[must_use]
    pub fn with_file_custody(hangar_home: &Path) -> Self {
        Self {
            dir: hangar_home.join("hangar"),
            custody: Custody::File,
        }
    }

    /// `{home}/hangar/peers.json`.
    #[must_use]
    pub fn peers_path(&self) -> PathBuf {
        self.dir.join("peers.json")
    }

    fn secret_path(&self, host_id: &HostId) -> PathBuf {
        self.dir.join("peer-secrets").join(host_id.as_str())
    }

    fn read_file(&self) -> Result<PeersFile, PairingError> {
        let path = self.peers_path();
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(PeersFile {
                    version: PEERS_FILE_VERSION,
                    pairings: Vec::new(),
                });
            }
            Err(source) => return Err(PairingError::Io { path, source }),
        };
        let file: PeersFile = serde_json::from_str(&text).map_err(|e| PairingError::Malformed {
            what: "peers.json".to_string(),
            detail: e.to_string(),
        })?;
        if file.version > PEERS_FILE_VERSION {
            return Err(PairingError::Version(file.version));
        }
        Ok(file)
    }

    fn write_file(&self, file: &PeersFile) -> Result<(), PairingError> {
        let body = serde_json::to_vec_pretty(file).map_err(|e| PairingError::Malformed {
            what: "peers.json".to_string(),
            detail: e.to_string(),
        })?;
        write_private(&self.peers_path(), &body)
    }

    /// Every pairing, in the order they were first saved.
    pub fn list(&self) -> Result<Vec<Pairing>, PairingError> {
        Ok(self.read_file()?.pairings)
    }

    /// The pairing for `host_id`.
    pub fn get(&self, host_id: &HostId) -> Result<Option<Pairing>, PairingError> {
        Ok(self.read_file()?.pairings.into_iter().find(|p| &p.host_id == host_id))
    }

    /// Save (or replace) a pairing and its secret: the secret first, so the
    /// row never names a pairing without one.
    pub fn save(&self, pairing: &Pairing, secrets: &PairingSecrets) -> Result<(), PairingError> {
        self.put_secret(&pairing.host_id, secrets)?;
        let mut file = self.read_file()?;
        file.version = PEERS_FILE_VERSION;
        if let Some(row) = file.pairings.iter_mut().find(|p| p.host_id == pairing.host_id) {
            *row = pairing.clone();
        } else {
            file.pairings.push(pairing.clone());
        }
        self.write_file(&file)
    }

    /// The secret half of the pairing for `host_id`.
    pub fn secrets(&self, host_id: &HostId) -> Result<PairingSecrets, PairingError> {
        self.get_secret(host_id)?
            .ok_or_else(|| PairingError::MissingSecret(host_id.clone()))
    }

    /// The user acted on `host_id`. Returns false when it is not paired.
    pub fn touch(&self, host_id: &HostId, now_ms: i64) -> Result<bool, PairingError> {
        let mut file = self.read_file()?;
        let Some(row) = file.pairings.iter_mut().find(|p| &p.host_id == host_id) else {
            return Ok(false);
        };
        row.last_active_ms = row.last_active_ms.max(now_ms);
        self.write_file(&file)?;
        Ok(true)
    }

    /// Drop the pairing for `host_id` from this surface: the row first, then
    /// the secret, so a crash in between leaves an orphan secret, never a row
    /// without one. Local only: telling the host to revoke this device is the
    /// transport's job, before it calls this.
    pub fn remove(&self, host_id: &HostId) -> Result<Option<Pairing>, PairingError> {
        let mut file = self.read_file()?;
        let Some(at) = file.pairings.iter().position(|p| &p.host_id == host_id) else {
            self.delete_secret(host_id)?;
            return Ok(None);
        };
        let removed = file.pairings.remove(at);
        self.write_file(&file)?;
        self.delete_secret(host_id)?;
        Ok(Some(removed))
    }

    fn encode_secret(secrets: &PairingSecrets) -> Result<Vec<u8>, PairingError> {
        use base64::Engine as _;
        let stored = StoredSecret {
            device_static: base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(secrets.device_static.as_bytes()),
            device_token: secrets
                .token()
                .ok_or_else(|| PairingError::Malformed {
                    what: "device token".to_string(),
                    detail: "not UTF-8".to_string(),
                })?
                .to_string(),
        };
        serde_json::to_vec(&stored).map_err(|e| PairingError::Malformed {
            what: "secret".to_string(),
            detail: e.to_string(),
        })
    }

    fn decode_secret(host_id: &HostId, bytes: &[u8]) -> Result<PairingSecrets, PairingError> {
        use base64::Engine as _;
        let malformed = |detail: String| PairingError::Malformed {
            what: format!("secret for {host_id}"),
            detail,
        };
        let stored: StoredSecret =
            serde_json::from_slice(bytes).map_err(|e| malformed(e.to_string()))?;
        let key = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(stored.device_static.as_bytes())
            .map_err(|e| malformed(e.to_string()))?;
        if key.len() != 32 {
            return Err(malformed(format!("static key is {} bytes", key.len())));
        }
        Ok(PairingSecrets {
            device_static: SecretBytes::from(key),
            device_token: SecretBytes::from(stored.device_token.into_bytes()),
        })
    }

    fn put_secret(&self, host_id: &HostId, secrets: &PairingSecrets) -> Result<(), PairingError> {
        let bytes = Self::encode_secret(secrets)?;
        if let Custody::Keychain(backend) = &self.custody {
            match backend.put(&Scope::Global, &secret_account(host_id), &bytes) {
                Ok(()) => return Ok(()),
                Err(SecretError::NotImplemented) => {}
                Err(e) => return Err(e.into()),
            }
        }
        write_private(&self.secret_path(host_id), &bytes)
    }

    fn get_secret(&self, host_id: &HostId) -> Result<Option<PairingSecrets>, PairingError> {
        if let Custody::Keychain(backend) = &self.custody {
            match backend.get(&Scope::Global, &secret_account(host_id)) {
                Ok(Some(bytes)) => return Self::decode_secret(host_id, bytes.as_bytes()).map(Some),
                Ok(None) => return Ok(None),
                Err(SecretError::NotImplemented) => {}
                Err(e) => return Err(e.into()),
            }
        }
        let path = self.secret_path(host_id);
        match std::fs::read(&path) {
            Ok(bytes) => Self::decode_secret(host_id, &bytes).map(Some),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(PairingError::Io { path, source }),
        }
    }

    fn delete_secret(&self, host_id: &HostId) -> Result<(), PairingError> {
        if let Custody::Keychain(backend) = &self.custody {
            match backend.delete(&Scope::Global, &secret_account(host_id)) {
                Ok(()) => return Ok(()),
                Err(SecretError::NotImplemented) => {}
                Err(e) => return Err(e.into()),
            }
        }
        let path = self.secret_path(host_id);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(PairingError::Io { path, source }),
        }
    }
}

/// Write `bytes` to `path` as a `0600` file (its directory `0700`), through a
/// temp file and a rename so a reader never sees half a file.
fn write_private(path: &Path, bytes: &[u8]) -> Result<(), PairingError> {
    use std::io::Write as _;
    let io = |path: &Path| {
        let path = path.to_path_buf();
        move |source| PairingError::Io { path, source }
    };
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir).map_err(io(dir))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).map_err(io(dir))?;
    }
    let file_name = path
        .file_name()
        .map_or_else(|| "pairing".into(), |n| n.to_string_lossy().into_owned());
    let tmp = dir.join(format!(".{file_name}.{}.tmp", std::process::id()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(&tmp).map_err(io(&tmp))?;
    file.write_all(bytes).map_err(io(&tmp))?;
    file.sync_all().map_err(io(&tmp))?;
    drop(file);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600)).map_err(io(&tmp))?;
    }
    std::fs::rename(&tmp, path).map_err(io(path))
}

mod b64_32 {
    use base64::Engine as _;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8; 32], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&URL_SAFE_NO_PAD.encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 32], D::Error> {
        let text = String::deserialize(d)?;
        let bytes = URL_SAFE_NO_PAD.decode(text.as_bytes()).map_err(serde::de::Error::custom)?;
        bytes
            .try_into()
            .map_err(|b: Vec<u8>| serde::de::Error::custom(format!("key is {} bytes", b.len())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ainb_hangar_proto::hosts::CarrierKind;

    fn id(n: u8) -> HostId {
        HostId::parse(&format!("01K5A0000000000000000ABC{n:02}")).expect("a ULID")
    }

    fn pairing(n: u8) -> Pairing {
        Pairing {
            host_id: id(n),
            host_static_pubkey: [n; 32],
            endpoints: vec![Endpoint {
                carrier: CarrierKind::Tailnet,
                url: format!("ws://100.64.0.{n}:47300/peer"),
            }],
            device_id: format!("dev-{n}"),
            display_name: "laptop".to_string(),
            scope: DeviceScope::DESKTOP,
            device_expires_at_ms: 9_000,
            paired_at_ms: 1_000,
            last_active_ms: 1_000,
        }
    }

    fn secrets(n: u8) -> PairingSecrets {
        PairingSecrets {
            device_static: SecretBytes::from(vec![n.wrapping_add(100); 32]),
            device_token: SecretBytes::from(format!("mdd_token_{n}").into_bytes()),
        }
    }

    /// Run `check` once per custody, each on its own fresh home.
    fn each_custody(check: impl Fn(&PairingStore)) {
        for keychain in [true, false] {
            let home = tempfile::tempdir().unwrap();
            let store = if keychain {
                PairingStore::with_backend(
                    home.path(),
                    Box::new(ainb_hangar_secrets::InMemoryBackend::new()),
                )
            } else {
                PairingStore::with_file_custody(home.path())
            };
            check(&store);
            if keychain {
                assert!(
                    !store.dir.join("peer-secrets").exists(),
                    "a working keychain leaves no secret file"
                );
            }
        }
    }

    #[test]
    fn a_pairing_round_trips_and_the_file_holds_no_secret() {
        each_custody(|store| {
            assert!(store.list().unwrap().is_empty(), "no file is no pairings");
            store.save(&pairing(1), &secrets(1)).unwrap();
            store.save(&pairing(2), &secrets(2)).unwrap();
            assert_eq!(store.list().unwrap(), vec![pairing(1), pairing(2)]);
            assert_eq!(store.get(&id(2)).unwrap(), Some(pairing(2)));
            let got = store.secrets(&id(1)).unwrap();
            assert_eq!(got, secrets(1));
            assert_eq!(got.token(), Some("mdd_token_1"));
            assert_eq!(got.device_static_key(), Some([101; 32]));
            assert_eq!(format!("{got:?}"), "PairingSecrets(<redacted>)");

            let text = std::fs::read_to_string(store.peers_path()).unwrap();
            assert!(!text.contains("mdd_token"), "{text}");
            assert!(text.contains("\"host_static_pubkey\""));
        });
    }

    #[cfg(unix)]
    #[test]
    fn files_are_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;
        let home = tempfile::tempdir().unwrap();
        let store = PairingStore::with_file_custody(home.path());
        store.save(&pairing(1), &secrets(1)).unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&store.peers_path()), 0o600);
        assert_eq!(mode(&store.secret_path(&id(1))), 0o600);
        assert_eq!(mode(&store.dir), 0o700);
        assert_eq!(mode(&store.dir.join("peer-secrets")), 0o700);
        let leftovers: Vec<_> = std::fs::read_dir(&store.dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "no temp file is left behind");
    }

    #[test]
    fn saving_again_replaces_and_touch_only_moves_forward() {
        let home = tempfile::tempdir().unwrap();
        let store = PairingStore::with_file_custody(home.path());
        store.save(&pairing(1), &secrets(1)).unwrap();
        let mut rescoped = pairing(1);
        rescoped.scope = DeviceScope::MOBILE;
        store.save(&rescoped, &secrets(3)).unwrap();
        assert_eq!(store.list().unwrap(), vec![rescoped]);
        assert_eq!(store.secrets(&id(1)).unwrap(), secrets(3));
        assert!(store.touch(&id(1), 5_000).unwrap());
        assert!(store.touch(&id(1), 2_000).unwrap());
        assert_eq!(store.get(&id(1)).unwrap().unwrap().last_active_ms, 5_000);
        assert!(!store.touch(&id(9), 1).unwrap());
    }

    #[test]
    fn remove_drops_the_row_and_the_secret() {
        each_custody(|store| {
            store.save(&pairing(1), &secrets(1)).unwrap();
            store.save(&pairing(2), &secrets(2)).unwrap();
            assert_eq!(store.remove(&id(1)).unwrap(), Some(pairing(1)));
            assert_eq!(store.list().unwrap(), vec![pairing(2)]);
            assert!(matches!(
                store.secrets(&id(1)),
                Err(PairingError::MissingSecret(_))
            ));
            assert_eq!(store.remove(&id(1)).unwrap(), None, "idempotent");
            assert_eq!(store.secrets(&id(2)).unwrap(), secrets(2));
        });
    }

    #[test]
    fn a_newer_or_broken_file_is_refused_not_overwritten() {
        let home = tempfile::tempdir().unwrap();
        let store = PairingStore::with_file_custody(home.path());
        std::fs::create_dir_all(&store.dir).unwrap();
        std::fs::write(store.peers_path(), r#"{"version": 2, "pairings": []}"#).unwrap();
        assert!(matches!(store.list(), Err(PairingError::Version(2))));
        assert!(store.save(&pairing(1), &secrets(1)).is_err());
        assert!(
            std::fs::read_to_string(store.peers_path()).unwrap().contains("\"version\": 2"),
            "a newer build's file is left alone"
        );
        std::fs::write(store.peers_path(), "not json").unwrap();
        assert!(matches!(store.list(), Err(PairingError::Malformed { .. })));
    }

    /// Linux has no keychain backend yet: the default store falls back to the
    /// `0600` file, and still round trips. (The macOS keychain run is a
    /// separate, on-device check.)
    #[cfg(target_os = "linux")]
    #[test]
    fn the_default_store_on_linux_falls_back_to_the_file() {
        let home = tempfile::tempdir().unwrap();
        let store = PairingStore::open_in(home.path());
        store.save(&pairing(1), &secrets(1)).unwrap();
        assert!(store.secret_path(&id(1)).exists());
        assert_eq!(store.secrets(&id(1)).unwrap(), secrets(1));
        store.remove(&id(1)).unwrap();
        assert!(!store.secret_path(&id(1)).exists());
    }

    #[test]
    fn a_local_host_id_is_refused_in_the_file() {
        let row = serde_json::to_value(pairing(1)).unwrap();
        let mut local = row;
        local["host_id"] = serde_json::json!("local");
        assert!(serde_json::from_value::<Pairing>(local).is_err());
    }
}
