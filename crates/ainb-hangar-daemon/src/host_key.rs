//! The daemon's Noise IK static key for the off-box peer leg (R1-02).
//!
//! ```text
//! boot, peer leg on ──▶ 0600 file present? ──▶ yes: use it
//!                              │ no
//!                              ▼
//!                       keychain get ──▶ Some: use it
//!                              │ None ──▶ mint ──▶ keychain put ──▶ ok: keychain
//!                              │                        └─▶ refused: 0600 file
//!                              │ NotImplemented ──▶ mint ──▶ 0600 file
//!                              └─ locked, denied, fault ──▶ refuse (no second key)
//! then ──▶ daemon_identity.host_static_pubkey = the public half
//! ```
//!
//! One key per home, minted once and kept: every paired device pins its public
//! half, so a second key minted beside the first would strand them all. That is
//! why the file is checked FIRST: a file exists only when a previous boot could
//! not write the keychain, and from then on it is the key in force. It is also
//! why a keychain that is present but locked, or whose prompt was denied, is a
//! refusal rather than a fallback: the key may be in there, and minting a file
//! key beside it would fork the host's identity.
//!
//! Dark: [`ensure`] runs only when the peer leg's boot switch is set
//! ([`crate::peer_listener::switched_on`]), so a default boot touches no
//! keychain item, writes no file and leaves the column NULL, as v1.29.0 does.

use std::io::Write as _;
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

use ainb_hangar_secrets::{Scope, SecretBackend, SecretError};
use ainb_hangar_store::repo::daemon_identity::DaemonIdentityRepo;
use curve25519_dalek::montgomery::MontgomeryPoint;
use rand::RngCore as _;
use sqlx::SqlitePool;
use zeroize::Zeroizing;

/// The keychain account the key is stored under, in [`Scope::Global`].
pub const KEYCHAIN_ACCOUNT: &str = "peer.host_static_key";

/// The `0600` fallback file inside a resolved Hangar home:
/// `{hangar_home}/hangar/host_static.key`, holding the 32 raw secret bytes.
#[must_use]
pub fn key_file_in(hangar_home: &Path) -> PathBuf {
    hangar_home.join("hangar").join("host_static.key")
}

/// An X25519 static key pair. The secret half is zeroed on drop and never
/// printed.
pub struct HostStaticKey {
    secret: Zeroizing<[u8; 32]>,
    public: [u8; 32],
}

impl HostStaticKey {
    /// The pair for a stored secret. Clamping happens in the scalar multiply,
    /// exactly as the Noise X25519 DH does it, so any 32 bytes are a valid
    /// secret and the public half matches what the handshake derives.
    #[must_use]
    pub fn from_secret(secret: [u8; 32]) -> Self {
        let public = MontgomeryPoint::mul_base_clamped(secret).to_bytes();
        Self {
            secret: Zeroizing::new(secret),
            public,
        }
    }

    /// A fresh pair from the operating system's CSPRNG.
    #[must_use]
    pub fn generate() -> Self {
        let mut secret = Zeroizing::new([0u8; 32]);
        rand::rngs::OsRng.fill_bytes(secret.as_mut());
        Self::from_secret(*secret)
    }

    /// The public half: what `daemon_identity` records and a pairing offer
    /// carries.
    #[must_use]
    pub fn public(&self) -> &[u8; 32] {
        &self.public
    }

    /// The secret half, for the Noise responder only.
    #[must_use]
    pub fn secret(&self) -> &[u8; 32] {
        &self.secret
    }

    fn from_stored(bytes: &[u8]) -> Result<Self, HostKeyError> {
        let secret: [u8; 32] =
            bytes.try_into().map_err(|_| HostKeyError::Malformed(bytes.len()))?;
        Ok(Self::from_secret(secret))
    }
}

impl std::fmt::Debug for HostStaticKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostStaticKey")
            .field("secret", &"<redacted>")
            .field("public", &hex(&self.public))
            .finish()
    }
}

/// Where the key in force is kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Custody {
    /// The OS keychain, [`Scope::Global`] / [`KEYCHAIN_ACCOUNT`].
    Keychain,
    /// The `0600` file at this path.
    File(PathBuf),
}

/// The key in force, where it lives, and whether this call minted it.
#[derive(Debug)]
pub struct Loaded {
    /// The key pair.
    pub key: HostStaticKey,
    /// Where it is kept.
    pub custody: Custody,
    /// `true` only on the boot that created it.
    pub minted: bool,
}

/// Why the key could not be loaded or kept. Each one leaves the peer leg off
/// for this boot; none of them mints a second key.
#[derive(Debug, thiserror::Error)]
pub enum HostKeyError {
    /// The keychain exists on this platform but could not be read: locked,
    /// its prompt denied, or a backend fault. The key may be inside it.
    #[error("the keychain could not be read, so the host key may be in it: {0}")]
    Keychain(SecretError),
    /// The fallback file could not be read or written.
    #[error("host key file {}: {source}", path.display())]
    File {
        /// The file.
        path: PathBuf,
        /// The fault.
        source: std::io::Error,
    },
    /// The fallback file grants access beyond its owner.
    #[error("host key file {} has mode {mode:o}; it must be 0600", path.display())]
    Permissions {
        /// The file.
        path: PathBuf,
        /// Its permission bits.
        mode: u32,
    },
    /// A stored key is not 32 bytes.
    #[error("the stored host key is {0} bytes, not 32")]
    Malformed(usize),
    /// The public half could not be recorded in `daemon_identity`.
    #[error("recording the host public key: {0}")]
    Store(#[from] sqlx::Error),
}

/// Load the key in force for this home, minting and storing one the first
/// time. See the module diagram for the order.
///
/// # Errors
///
/// [`HostKeyError`] when the key cannot be read or kept safely.
pub fn load_or_mint(
    backend: &dyn SecretBackend,
    hangar_home: &Path,
) -> Result<Loaded, HostKeyError> {
    let file = key_file_in(hangar_home);
    if let Some(key) = read_file(&file)? {
        return Ok(Loaded {
            key,
            custody: Custody::File(file),
            minted: false,
        });
    }
    match backend.get(&Scope::Global, KEYCHAIN_ACCOUNT) {
        Ok(Some(stored)) => Ok(Loaded {
            key: HostStaticKey::from_stored(stored.as_bytes())?,
            custody: Custody::Keychain,
            minted: false,
        }),
        Ok(None) => {
            let key = HostStaticKey::generate();
            match backend.put(&Scope::Global, KEYCHAIN_ACCOUNT, key.secret()) {
                Ok(()) => Ok(Loaded {
                    key,
                    custody: Custody::Keychain,
                    minted: true,
                }),
                // Nothing was stored, so the next boot's get is `None` again
                // and finds this file first: one key either way.
                Err(error) => {
                    tracing::warn!(
                        %error,
                        "hangar host key: the keychain refused the new key; keeping it in a 0600 file"
                    );
                    mint_to_file(key, file)
                }
            }
        }
        Err(SecretError::NotImplemented) => mint_to_file(HostStaticKey::generate(), file),
        Err(error) => Err(HostKeyError::Keychain(error)),
    }
}

/// Load or mint the key, then record its public half in `daemon_identity`.
///
/// A recorded public key that differs from the key in force is overwritten
/// and logged: the row follows the key, and every device paired against the
/// old one has to pair again (a planned change goes through the rotation in
/// R1-17 instead, which never reaches this branch).
///
/// # Errors
///
/// [`HostKeyError`] from [`load_or_mint`], or when the row cannot be written.
pub async fn ensure(
    pool: &SqlitePool,
    backend: &dyn SecretBackend,
    hangar_home: &Path,
) -> Result<Loaded, HostKeyError> {
    let loaded = load_or_mint(backend, hangar_home)?;
    let previous = DaemonIdentityRepo::set_host_static_pubkey(pool, loaded.key.public()).await?;
    if previous.as_deref().is_some_and(|previous| previous != loaded.key.public()) {
        tracing::warn!(
            public = %hex(loaded.key.public()),
            "hangar host key: the recorded public key changed; paired devices must pair again"
        );
    }
    Ok(loaded)
}

/// The key in the fallback file, `None` when there is no file.
fn read_file(path: &Path) -> Result<Option<HostStaticKey>, HostKeyError> {
    let file_error = |source| HostKeyError::File {
        path: path.to_path_buf(),
        source,
    };
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(file_error(error)),
    };
    let mode = metadata.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(HostKeyError::Permissions {
            path: path.to_path_buf(),
            mode,
        });
    }
    let bytes = Zeroizing::new(std::fs::read(path).map_err(file_error)?);
    HostStaticKey::from_stored(&bytes).map(Some)
}

/// Write `key` to the fallback file: a `0600` temporary beside it, synced,
/// then renamed over the path, so a crash never leaves a short key behind.
fn mint_to_file(key: HostStaticKey, path: PathBuf) -> Result<Loaded, HostKeyError> {
    let file_error = |source| HostKeyError::File {
        path: path.clone(),
        source,
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(file_error)?;
    }
    let temporary = path.with_extension(format!("key.tmp-{}", std::process::id()));
    let written = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)
        .and_then(|mut file| {
            file.write_all(key.secret())?;
            file.sync_all()
        })
        .and_then(|()| std::fs::rename(&temporary, &path));
    if let Err(error) = written {
        let _ = std::fs::remove_file(&temporary);
        return Err(file_error(error));
    }
    Ok(Loaded {
        key,
        custody: Custody::File(path),
        minted: true,
    })
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::with_capacity(bytes.len() * 2), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ainb_hangar_secrets::{InMemoryBackend, SecretBytes};

    /// A backend whose `get` and `put` answer as scripted, standing in for a
    /// keychain that is absent, locked, or refuses a write.
    struct Scripted {
        get: fn() -> ainb_hangar_secrets::Result<Option<SecretBytes>>,
        put: fn() -> ainb_hangar_secrets::Result<()>,
    }

    impl SecretBackend for Scripted {
        fn get(&self, _: &Scope, _: &str) -> ainb_hangar_secrets::Result<Option<SecretBytes>> {
            (self.get)()
        }
        fn put(&self, _: &Scope, _: &str, _: &[u8]) -> ainb_hangar_secrets::Result<()> {
            (self.put)()
        }
        fn delete(&self, _: &Scope, _: &str) -> ainb_hangar_secrets::Result<()> {
            Ok(())
        }
    }

    const NO_KEYCHAIN: Scripted = Scripted {
        get: || Err(SecretError::NotImplemented),
        put: || Err(SecretError::NotImplemented),
    };

    fn unhex(s: &str) -> [u8; 32] {
        let mut out = [0u8; 32];
        for (i, byte) in out.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).expect("hex");
        }
        out
    }

    fn mode_of(path: &Path) -> u32 {
        std::fs::metadata(path).expect("metadata").permissions().mode() & 0o777
    }

    /// RFC 7748 section 6.1: the public half is the X25519 base-point
    /// multiple a Noise peer derives from the same secret.
    #[test]
    fn the_public_half_is_the_rfc_7748_x25519_public_key() {
        let key = HostStaticKey::from_secret(unhex(
            "77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a",
        ));
        assert_eq!(
            key.public(),
            &unhex("8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a")
        );
    }

    #[test]
    fn debug_never_prints_the_secret() {
        let secret = [0xAB; 32];
        let printed = format!("{:?}", HostStaticKey::from_secret(secret));
        assert!(printed.contains("<redacted>"), "{printed}");
        assert!(!printed.contains(&hex(&secret)), "{printed}");
    }

    #[test]
    fn a_keychain_key_is_minted_once_and_read_back() {
        let home = tempfile::tempdir().expect("home");
        let keychain = InMemoryBackend::new();

        let first = load_or_mint(&keychain, home.path()).expect("mint");
        assert!(first.minted);
        assert_eq!(first.custody, Custody::Keychain);
        let second = load_or_mint(&keychain, home.path()).expect("reread");
        assert!(!second.minted, "a second boot reads, never mints");
        assert_eq!(second.custody, Custody::Keychain);
        assert_eq!(first.key.public(), second.key.public());
        assert_eq!(first.key.secret(), second.key.secret());
        assert!(
            !key_file_in(home.path()).exists(),
            "a working keychain leaves no file behind"
        );
    }

    #[test]
    fn without_a_keychain_backend_the_key_lives_in_a_0600_file() {
        let home = tempfile::tempdir().expect("home");
        let file = key_file_in(home.path());

        let first = load_or_mint(&NO_KEYCHAIN, home.path()).expect("mint");
        assert!(first.minted);
        assert_eq!(first.custody, Custody::File(file.clone()));
        assert_eq!(mode_of(&file), 0o600);
        assert_eq!(std::fs::read(&file).expect("read"), first.key.secret());

        let second = load_or_mint(&NO_KEYCHAIN, home.path()).expect("reread");
        assert!(!second.minted);
        assert_eq!(first.key.public(), second.key.public());
        let leftovers: Vec<_> = std::fs::read_dir(file.parent().expect("parent"))
            .expect("list")
            .map(|entry| entry.expect("entry").file_name())
            .collect();
        assert_eq!(leftovers, [std::ffi::OsString::from("host_static.key")]);
    }

    /// A keychain that answers `get` but refuses the write stores nothing, so
    /// the key goes to the file; the next boot finds the file before it asks
    /// the keychain again, and the key never changes.
    #[test]
    fn a_refused_keychain_write_falls_back_to_the_file_and_stays_there() {
        let home = tempfile::tempdir().expect("home");
        let refusing = Scripted {
            get: || Ok(None),
            put: || Err(SecretError::AccessDenied),
        };

        let first = load_or_mint(&refusing, home.path()).expect("mint");
        assert_eq!(first.custody, Custody::File(key_file_in(home.path())));
        let keychain = InMemoryBackend::new();
        let second = load_or_mint(&keychain, home.path()).expect("reread");
        assert!(!second.minted);
        assert_eq!(first.key.public(), second.key.public());
        assert!(
            keychain.get(&Scope::Global, KEYCHAIN_ACCOUNT).expect("get").is_none(),
            "a home whose key is in the file never mints a keychain key"
        );
    }

    /// A locked keychain may hold the key, so minting beside it would fork the
    /// host identity: refuse, and write nothing.
    #[test]
    fn a_locked_keychain_is_refused_not_worked_around() {
        let home = tempfile::tempdir().expect("home");
        let locked = Scripted {
            get: || Err(SecretError::BackendLocked),
            put: || Err(SecretError::BackendLocked),
        };

        let refused = load_or_mint(&locked, home.path()).expect_err("locked");
        assert!(matches!(
            refused,
            HostKeyError::Keychain(SecretError::BackendLocked)
        ));
        assert!(!key_file_in(home.path()).exists());
    }

    #[test]
    fn a_file_readable_by_others_is_refused() {
        let home = tempfile::tempdir().expect("home");
        load_or_mint(&NO_KEYCHAIN, home.path()).expect("mint");
        let file = key_file_in(home.path());
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).expect("chmod");

        let refused = load_or_mint(&NO_KEYCHAIN, home.path()).expect_err("0644");
        assert!(
            matches!(refused, HostKeyError::Permissions { mode: 0o644, .. }),
            "{refused:?}"
        );
    }

    #[test]
    fn a_short_stored_key_is_refused() {
        let home = tempfile::tempdir().expect("home");
        let keychain = InMemoryBackend::new();
        keychain.put(&Scope::Global, KEYCHAIN_ACCOUNT, &[1, 2, 3]).expect("put");

        let refused = load_or_mint(&keychain, home.path()).expect_err("3 bytes");
        assert!(matches!(refused, HostKeyError::Malformed(3)), "{refused:?}");
    }

    /// The public half lands in `daemon_identity`, a second boot leaves it
    /// alone, and a different key in force replaces it.
    #[tokio::test]
    async fn ensure_records_the_public_half_on_the_identity_row() {
        let home = tempfile::tempdir().expect("home");
        let store = ainb_hangar_store::Store::open_in(home.path()).await.expect("store");
        DaemonIdentityRepo::mint_or_read(
            store.pool(),
            &ainb_hangar_core::idgen::SystemIdGen,
            &ainb_hangar_core::clock::SystemClock,
        )
        .await
        .expect("identity");
        assert_eq!(
            DaemonIdentityRepo::host_static_pubkey(store.pool()).await.expect("read"),
            None,
            "the column stays NULL until the key exists"
        );

        let keychain = InMemoryBackend::new();
        let first = ensure(store.pool(), &keychain, home.path()).await.expect("ensure");
        let recorded = DaemonIdentityRepo::host_static_pubkey(store.pool()).await.expect("read");
        assert_eq!(recorded.as_deref(), Some(&first.key.public()[..]));

        let again = ensure(store.pool(), &keychain, home.path()).await.expect("ensure");
        assert_eq!(again.key.public(), first.key.public());

        let replaced = ensure(store.pool(), &InMemoryBackend::new(), home.path())
            .await
            .expect("ensure");
        assert_ne!(replaced.key.public(), first.key.public());
        let recorded = DaemonIdentityRepo::host_static_pubkey(store.pool()).await.expect("read");
        assert_eq!(recorded.as_deref(), Some(&replaced.key.public()[..]));
    }
}
