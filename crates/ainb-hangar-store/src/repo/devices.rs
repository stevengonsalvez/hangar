//! The paired-device registry (migration 0103, R1-03, spec D13).
//!
//! ```text
//! operator ──▶ create_invite ──▶ device_invite (5 min, single use)
//! peer leg ──▶ redeem ──▶ invite consumed + device row + version bump
//!          ──▶ verify_hello ──▶ token digest, bound key, not revoked, not expired
//!                          └──▶ slides last_seen_at and expires_at
//! admin    ──▶ revoke / rescope (fenced on the registry version)
//! sweep    ──▶ prune: dead invites, devices revoked 90 days ago
//! ```
//!
//! Stateless and clock-free: every write takes the caller's `now_ms`, so the
//! daemon's clock decides and a test can pin it. Secrets arrive as their
//! SHA-256 hex digests (`ainb_hangar_core::token::sha256_hex`), the
//! `socket_token` convention; this layer never sees a plaintext secret.
//!
//! Nothing calls this until the peer leg is switched on
//! (`AINB_HANGAR_PEER_LISTEN`), so the tables stay empty on a default boot.

use sqlx::sqlite::SqliteRow;
use sqlx::{Row, SqliteConnection, SqlitePool};

/// A redeem is accepted up to this long after the invite's `expires_at`, so a
/// phone that scanned in the last second is not refused by clock skew.
pub const REDEEM_LEEWAY_MS: i64 = 30_000;
/// Wrong secrets an invite survives; the fifth burns it.
pub const MAX_INVITE_ATTEMPTS: i64 = 5;
/// A device token expires this long after the device was last seen. Every
/// accepted hello slides it; there is no refresh RPC.
pub const IDLE_EXPIRY_MS: i64 = 90 * 24 * 60 * 60 * 1000;
/// The longest display name the registry stores, in characters. The CHECK in
/// migration 0103 enforces it (with non-blank and no line breaks), because the
/// redeeming device, not the operator, chooses the name.
pub const DISPLAY_NAME_MAX_CHARS: usize = 64;
/// A revoked device stays listed this long, then [`DeviceRepo::prune`]
/// removes it.
pub const REVOKED_RETENTION_MS: i64 = 90 * 24 * 60 * 60 * 1000;

/// The base of a stored scope. The wire type is
/// `ainb_hangar_proto::devices::BaseScope`; the daemon maps between the two.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeBase {
    /// `desktop`.
    Desktop,
    /// `mobile`.
    Mobile,
    /// `mobile+type`.
    MobileType,
}

impl ScopeBase {
    /// The column value, the same string the wire uses.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Desktop => "desktop",
            Self::Mobile => "mobile",
            Self::MobileType => "mobile+type",
        }
    }

    fn parse(value: &str) -> Result<Self, sqlx::Error> {
        match value {
            "desktop" => Ok(Self::Desktop),
            "mobile" => Ok(Self::Mobile),
            "mobile+type" => Ok(Self::MobileType),
            other => Err(sqlx::Error::Decode(
                format!("unknown device scope base {other:?}").into(),
            )),
        }
    }
}

/// A stored scope. `admin` on a base other than `desktop` is refused by the
/// table's CHECK, so a write of one fails rather than storing it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoredScope {
    /// The base.
    pub base: ScopeBase,
    /// The additive admin flag.
    pub admin: bool,
}

/// A new pairing invite.
#[derive(Debug, Clone)]
pub struct NewInvite<'a> {
    /// A ULID.
    pub invite_id: &'a str,
    /// SHA-256 hex of the 32-byte invite secret.
    pub secret_sha256: &'a str,
    /// The scope the redeemed device gets.
    pub scope: StoredScope,
    /// The suggested device name.
    pub display_name: Option<&'a str>,
    /// Unix milliseconds.
    pub created_at: i64,
    /// Unix milliseconds; at most five minutes after `created_at` by policy.
    pub expires_at: i64,
}

/// One redeem attempt, as presented inside a Noise session.
#[derive(Debug, Clone)]
pub struct Redeem<'a> {
    /// The invite presented.
    pub invite_id: &'a str,
    /// SHA-256 hex of the presented secret.
    pub secret_sha256: &'a str,
    /// The id to give the device, a ULID minted by the caller.
    pub device_id: &'a str,
    /// The name the device asked to be listed under.
    pub display_name: &'a str,
    /// SHA-256 hex of the token the caller minted for the device.
    pub token_sha256: &'a str,
    /// The Noise remote static key of the redeeming session.
    pub static_pubkey: &'a [u8; 32],
    /// Unix milliseconds.
    pub now_ms: i64,
}

/// Why a redeem was refused. Each one closes the peer socket 4401; the
/// variants exist for the log and the tests, never for the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedeemRefusal {
    /// No such invite.
    Unknown,
    /// Past `expires_at` plus [`REDEEM_LEEWAY_MS`].
    Expired,
    /// Already redeemed once.
    Spent,
    /// Burned by wrong secrets.
    Burned,
    /// The secret did not match. `burned` is `true` when this attempt was the
    /// one that burned the invite.
    WrongSecret {
        /// Whether this attempt burned the invite.
        burned: bool,
    },
}

/// What a redeem did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RedeemOutcome {
    /// The invite is consumed and the device exists.
    Redeemed(DeviceRecord),
    /// Nothing was created.
    Refused(RedeemRefusal),
}

/// One paired device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceRecord {
    /// The device id, a ULID.
    pub device_id: String,
    /// The listed name.
    pub display_name: String,
    /// The granted scope.
    pub scope: StoredScope,
    /// The Noise static public key the token is bound to.
    pub static_pubkey: Vec<u8>,
    /// The invite it redeemed.
    pub invite_id: String,
    /// Unix milliseconds of the redeem.
    pub created_at: i64,
    /// Unix milliseconds of the last accepted hello.
    pub last_seen_at: i64,
    /// Unix milliseconds the token expires unless a hello slides it.
    pub expires_at: i64,
    /// Unix milliseconds of the revoke.
    pub revoked_at: Option<i64>,
}

/// What a device hello resolved to. The peer leg closes 4401 on
/// [`HelloCheck::UnknownToken`] and [`HelloCheck::KeyMismatch`], and 4403 on
/// [`HelloCheck::Revoked`] and [`HelloCheck::Expired`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HelloCheck {
    /// Accepted; the returned record already carries the slid expiry.
    Accepted(DeviceRecord),
    /// No device holds this token.
    UnknownToken,
    /// The token is real but was presented from a different Noise static key.
    KeyMismatch {
        /// The device the token belongs to.
        device_id: String,
    },
    /// The device was revoked.
    Revoked {
        /// The device.
        device_id: String,
    },
    /// The token expired from idleness.
    Expired {
        /// The device.
        device_id: String,
    },
}

/// The result of a fenced registry write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fenced {
    /// Written (or already so); the registry version after the write.
    Applied {
        /// The version now in force.
        version: i64,
    },
    /// The caller's fence named an older version; nothing was written.
    Conflict {
        /// The version in force.
        version: i64,
    },
    /// No live device has that id (missing, or revoked for a rescope).
    NotFound,
}

/// What [`DeviceRepo::prune`] removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Pruned {
    /// Invites past their redeem window, in any state.
    pub invites: u64,
    /// Devices revoked more than [`REVOKED_RETENTION_MS`] ago.
    pub devices: u64,
}

/// Stateless typed wrapper over the device registry.
pub struct DeviceRepo;

impl DeviceRepo {
    /// Store a new invite.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the insert fails, including a CHECK
    /// violation (a malformed id or digest, or `admin` off base `desktop`).
    pub async fn create_invite(
        pool: &SqlitePool,
        invite: &NewInvite<'_>,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO device_invite \
             (invite_id, secret_sha256, scope_base, scope_admin, display_name, created_at, expires_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(invite.invite_id)
        .bind(invite.secret_sha256)
        .bind(invite.scope.base.as_str())
        .bind(invite.scope.admin)
        .bind(invite.display_name)
        .bind(invite.created_at)
        .bind(invite.expires_at)
        .execute(pool)
        .await?;
        Ok(())
    }

    /// Redeem an invite: consume it, create the device bound to the session's
    /// static key, and bump the registry version, all in one `IMMEDIATE`
    /// transaction. A wrong secret commits only its attempt count, and the
    /// fifth burns the invite.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if a statement fails (a reused token digest
    /// or device id among them); nothing but the attempt count is committed.
    pub async fn redeem(
        pool: &SqlitePool,
        redeem: &Redeem<'_>,
    ) -> Result<RedeemOutcome, sqlx::Error> {
        let mut tx = pool.begin_with(crate::repo::fleet::IMMEDIATE_TRANSACTION).await?;
        let Some(invite) = sqlx::query(
            "SELECT secret_sha256, scope_base, scope_admin, expires_at, attempts, burned_at, consumed_at \
             FROM device_invite WHERE invite_id = ?",
        )
        .bind(redeem.invite_id)
        .fetch_optional(&mut *tx)
        .await?
        else {
            return Ok(RedeemOutcome::Refused(RedeemRefusal::Unknown));
        };
        if invite.try_get::<Option<i64>, _>("consumed_at")?.is_some() {
            return Ok(RedeemOutcome::Refused(RedeemRefusal::Spent));
        }
        if invite.try_get::<Option<i64>, _>("burned_at")?.is_some() {
            return Ok(RedeemOutcome::Refused(RedeemRefusal::Burned));
        }
        if redeem.now_ms > invite.try_get::<i64, _>("expires_at")? + REDEEM_LEEWAY_MS {
            return Ok(RedeemOutcome::Refused(RedeemRefusal::Expired));
        }
        if invite.try_get::<String, _>("secret_sha256")? != redeem.secret_sha256 {
            let attempts = invite.try_get::<i64, _>("attempts")? + 1;
            let burned = attempts >= MAX_INVITE_ATTEMPTS;
            sqlx::query("UPDATE device_invite SET attempts = ?, burned_at = ? WHERE invite_id = ?")
                .bind(attempts)
                .bind(burned.then_some(redeem.now_ms))
                .bind(redeem.invite_id)
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
            return Ok(RedeemOutcome::Refused(RedeemRefusal::WrongSecret {
                burned,
            }));
        }
        let scope = StoredScope {
            base: ScopeBase::parse(&invite.try_get::<String, _>("scope_base")?)?,
            admin: invite.try_get("scope_admin")?,
        };
        // The IMMEDIATE lock already serialises redeems; the predicate makes
        // the single use hold even if it did not.
        let consumed = sqlx::query(
            "UPDATE device_invite SET consumed_at = ?, device_id = ? \
             WHERE invite_id = ? AND consumed_at IS NULL AND burned_at IS NULL",
        )
        .bind(redeem.now_ms)
        .bind(redeem.device_id)
        .bind(redeem.invite_id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if consumed != 1 {
            return Ok(RedeemOutcome::Refused(RedeemRefusal::Spent));
        }
        let record = DeviceRecord {
            device_id: redeem.device_id.to_string(),
            display_name: redeem.display_name.to_string(),
            scope,
            static_pubkey: redeem.static_pubkey.to_vec(),
            invite_id: redeem.invite_id.to_string(),
            created_at: redeem.now_ms,
            last_seen_at: redeem.now_ms,
            expires_at: redeem.now_ms + IDLE_EXPIRY_MS,
            revoked_at: None,
        };
        sqlx::query(
            "INSERT INTO device \
             (device_id, display_name, scope_base, scope_admin, token_sha256, static_pubkey, \
              invite_id, created_at, last_seen_at, expires_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&record.device_id)
        .bind(&record.display_name)
        .bind(record.scope.base.as_str())
        .bind(record.scope.admin)
        .bind(redeem.token_sha256)
        .bind(&record.static_pubkey)
        .bind(&record.invite_id)
        .bind(record.created_at)
        .bind(record.last_seen_at)
        .bind(record.expires_at)
        .execute(&mut *tx)
        .await?;
        bump_version(&mut tx).await?;
        tx.commit().await?;
        Ok(RedeemOutcome::Redeemed(record))
    }

    /// Check a device hello: the token digest names a device, the session's
    /// Noise remote static is the key the token was bound to, the device is not
    /// revoked, and the token has not expired. An accepted hello slides
    /// `last_seen_at` and `expires_at`.
    ///
    /// The key is checked before revocation, so a stolen token presented from
    /// another key is always an authentication failure, never a hint about the
    /// device's state.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if a statement fails.
    pub async fn verify_hello(
        pool: &SqlitePool,
        token_sha256: &str,
        remote_static: &[u8; 32],
        now_ms: i64,
    ) -> Result<HelloCheck, sqlx::Error> {
        let mut tx = pool.begin_with(crate::repo::fleet::IMMEDIATE_TRANSACTION).await?;
        let Some(row) = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM device WHERE token_sha256 = ?"
        ))
        .bind(token_sha256)
        .fetch_optional(&mut *tx)
        .await?
        else {
            return Ok(HelloCheck::UnknownToken);
        };
        let mut record = record_from(&row)?;
        if record.static_pubkey != remote_static {
            return Ok(HelloCheck::KeyMismatch {
                device_id: record.device_id,
            });
        }
        if record.revoked_at.is_some() {
            return Ok(HelloCheck::Revoked {
                device_id: record.device_id,
            });
        }
        if now_ms >= record.expires_at {
            return Ok(HelloCheck::Expired {
                device_id: record.device_id,
            });
        }
        record.last_seen_at = record.last_seen_at.max(now_ms);
        record.expires_at = record.last_seen_at + IDLE_EXPIRY_MS;
        sqlx::query("UPDATE device SET last_seen_at = ?, expires_at = ? WHERE device_id = ?")
            .bind(record.last_seen_at)
            .bind(record.expires_at)
            .bind(&record.device_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(HelloCheck::Accepted(record))
    }

    /// Every device, revoked ones included, oldest first, with the registry
    /// version read in the same transaction.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if a query fails.
    pub async fn list(pool: &SqlitePool) -> Result<(Vec<DeviceRecord>, i64), sqlx::Error> {
        let mut tx = pool.begin().await?;
        let devices = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM device ORDER BY created_at, device_id"
        ))
        .fetch_all(&mut *tx)
        .await?
        .iter()
        .map(record_from)
        .collect::<Result<Vec<_>, _>>()?;
        let version = version_on(&mut tx).await?;
        tx.commit().await?;
        Ok((devices, version))
    }

    /// The registry version in force.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query fails.
    pub async fn version(pool: &SqlitePool) -> Result<i64, sqlx::Error> {
        let mut conn = pool.acquire().await?;
        version_on(&mut conn).await
    }

    /// Revoke a device, fenced on `fence` when given. Revoking a revoked device
    /// is `Applied` at the current version and bumps nothing.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if a statement fails; nothing is committed.
    pub async fn revoke(
        pool: &SqlitePool,
        device_id: &str,
        fence: Option<i64>,
        now_ms: i64,
    ) -> Result<Fenced, sqlx::Error> {
        let mut tx = pool.begin_with(crate::repo::fleet::IMMEDIATE_TRANSACTION).await?;
        let version = version_on(&mut tx).await?;
        if fence.is_some_and(|fence| fence != version) {
            return Ok(Fenced::Conflict { version });
        }
        let Some(revoked_at) = sqlx::query("SELECT revoked_at FROM device WHERE device_id = ?")
            .bind(device_id)
            .fetch_optional(&mut *tx)
            .await?
            .map(|row| row.try_get::<Option<i64>, _>("revoked_at"))
            .transpose()?
        else {
            return Ok(Fenced::NotFound);
        };
        if revoked_at.is_some() {
            return Ok(Fenced::Applied { version });
        }
        sqlx::query("UPDATE device SET revoked_at = ? WHERE device_id = ?")
            .bind(now_ms)
            .bind(device_id)
            .execute(&mut *tx)
            .await?;
        let version = bump_version(&mut tx).await?;
        tx.commit().await?;
        Ok(Fenced::Applied { version })
    }

    /// Change a live device's scope, fenced on `fence` when given. The same
    /// scope is `Applied` at the current version and bumps nothing. Whether
    /// the caller may grant `scope` is the daemon's check, not this layer's.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if a statement fails (a CHECK violation for
    /// `admin` off base `desktop` among them); nothing is committed.
    pub async fn rescope(
        pool: &SqlitePool,
        device_id: &str,
        scope: StoredScope,
        fence: Option<i64>,
    ) -> Result<Fenced, sqlx::Error> {
        let mut tx = pool.begin_with(crate::repo::fleet::IMMEDIATE_TRANSACTION).await?;
        let version = version_on(&mut tx).await?;
        if fence.is_some_and(|fence| fence != version) {
            return Ok(Fenced::Conflict { version });
        }
        let Some(row) = sqlx::query(
            "SELECT scope_base, scope_admin FROM device WHERE device_id = ? AND revoked_at IS NULL",
        )
        .bind(device_id)
        .fetch_optional(&mut *tx)
        .await?
        else {
            return Ok(Fenced::NotFound);
        };
        let current = StoredScope {
            base: ScopeBase::parse(&row.try_get::<String, _>("scope_base")?)?,
            admin: row.try_get("scope_admin")?,
        };
        if current == scope {
            return Ok(Fenced::Applied { version });
        }
        sqlx::query("UPDATE device SET scope_base = ?, scope_admin = ? WHERE device_id = ?")
            .bind(scope.base.as_str())
            .bind(scope.admin)
            .bind(device_id)
            .execute(&mut *tx)
            .await?;
        let version = bump_version(&mut tx).await?;
        tx.commit().await?;
        Ok(Fenced::Applied { version })
    }

    /// Remove invites past their redeem window (spent, burned or never used:
    /// none of them can be redeemed any more) and devices revoked more than
    /// [`REVOKED_RETENTION_MS`] ago. A pruned device leaves the version alone:
    /// nothing a fence protects changed.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if a delete fails.
    pub async fn prune(pool: &SqlitePool, now_ms: i64) -> Result<Pruned, sqlx::Error> {
        let invites = sqlx::query("DELETE FROM device_invite WHERE expires_at + ? < ?")
            .bind(REDEEM_LEEWAY_MS)
            .bind(now_ms)
            .execute(pool)
            .await?
            .rows_affected();
        let devices =
            sqlx::query("DELETE FROM device WHERE revoked_at IS NOT NULL AND revoked_at < ?")
                .bind(now_ms - REVOKED_RETENTION_MS)
                .execute(pool)
                .await?
                .rows_affected();
        Ok(Pruned { invites, devices })
    }
}

const COLUMNS: &str = "device_id, display_name, scope_base, scope_admin, static_pubkey, invite_id, \
                       created_at, last_seen_at, expires_at, revoked_at";

fn record_from(row: &SqliteRow) -> Result<DeviceRecord, sqlx::Error> {
    Ok(DeviceRecord {
        device_id: row.try_get("device_id")?,
        display_name: row.try_get("display_name")?,
        scope: StoredScope {
            base: ScopeBase::parse(&row.try_get::<String, _>("scope_base")?)?,
            admin: row.try_get("scope_admin")?,
        },
        static_pubkey: row.try_get("static_pubkey")?,
        invite_id: row.try_get("invite_id")?,
        created_at: row.try_get("created_at")?,
        last_seen_at: row.try_get("last_seen_at")?,
        expires_at: row.try_get("expires_at")?,
        revoked_at: row.try_get("revoked_at")?,
    })
}

async fn version_on(conn: &mut SqliteConnection) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT version FROM device_registry_version WHERE singleton = 1")
        .fetch_one(conn)
        .await
}

async fn bump_version(conn: &mut SqliteConnection) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "UPDATE device_registry_version SET version = version + 1 WHERE singleton = 1 \
         RETURNING version",
    )
    .fetch_one(conn)
    .await
}

#[cfg(test)]
mod tests {
    use ainb_hangar_core::token::sha256_hex;

    use super::*;

    const T0: i64 = 1_800_000_000_000;
    const FIVE_MIN_MS: i64 = 5 * 60 * 1000;
    const PHONE_KEY: [u8; 32] = [7; 32];
    const OTHER_KEY: [u8; 32] = [9; 32];
    const PHONE: StoredScope = StoredScope {
        base: ScopeBase::MobileType,
        admin: false,
    };

    /// A valid 26-character ULID-shaped id: digits are in the Crockford set.
    fn id(n: u32) -> String {
        format!("{n:0>26}")
    }

    async fn store() -> (tempfile::TempDir, crate::Store) {
        let home = tempfile::tempdir().expect("home");
        let store = crate::Store::open_in(home.path()).await.expect("store");
        (home, store)
    }

    async fn invite(pool: &SqlitePool, n: u32, scope: StoredScope) -> String {
        let invite_id = id(n);
        DeviceRepo::create_invite(
            pool,
            &NewInvite {
                invite_id: &invite_id,
                secret_sha256: &sha256_hex(&format!("secret-{n}")),
                scope,
                display_name: Some("phone"),
                created_at: T0,
                expires_at: T0 + FIVE_MIN_MS,
            },
        )
        .await
        .expect("invite");
        invite_id
    }

    async fn redeem_as(
        pool: &SqlitePool,
        n: u32,
        secret: &str,
        device: u32,
        now_ms: i64,
    ) -> RedeemOutcome {
        DeviceRepo::redeem(
            pool,
            &Redeem {
                invite_id: &id(n),
                secret_sha256: &sha256_hex(secret),
                device_id: &id(device),
                display_name: "my phone",
                token_sha256: &sha256_hex(&format!("mdd_token-{device}")),
                static_pubkey: &PHONE_KEY,
                now_ms,
            },
        )
        .await
        .expect("redeem")
    }

    async fn paired(pool: &SqlitePool, n: u32, scope: StoredScope) -> DeviceRecord {
        invite(pool, n, scope).await;
        match redeem_as(pool, n, &format!("secret-{n}"), 1000 + n, T0).await {
            RedeemOutcome::Redeemed(record) => record,
            refused => panic!("redeem refused: {refused:?}"),
        }
    }

    /// DECISIONS: the first schema PR proves that an older binary refuses a
    /// database a newer one migrated. A v1.29.0 daemon embeds the chain up to
    /// 0102; run exactly that chain against a database this build migrated,
    /// and it must stop with `VersionMissing(103)` rather than serve a schema
    /// it does not know. This is why the registry merges only after the tag.
    #[tokio::test]
    async fn an_older_migrator_refuses_a_database_migrated_past_it() {
        let (_home, store) = store().await;
        let older = tempfile::tempdir().expect("older chain");
        for entry in std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/migrations"))
            .expect("migrations dir")
        {
            let path = entry.expect("entry").path();
            let name = path.file_name().expect("name").to_string_lossy().into_owned();
            let version: u32 = name[..4].parse().expect("numbered migration");
            if version < 103 {
                std::fs::copy(&path, older.path().join(&name)).expect("copy");
            }
        }
        let migrator = sqlx::migrate::Migrator::new(older.path()).await.expect("older migrator");
        assert!(
            migrator.iter().all(|m| m.version < 103),
            "the stand-in for the older binary must not know 0103"
        );

        let refused = migrator.run(store.pool()).await.expect_err("must refuse");
        assert!(
            matches!(refused, sqlx::migrate::MigrateError::VersionMissing(103)),
            "{refused:?}"
        );
    }

    #[tokio::test]
    async fn an_invite_redeems_exactly_once() {
        let (_home, store) = store().await;
        let pool = store.pool();
        invite(pool, 1, PHONE).await;
        assert_eq!(DeviceRepo::version(pool).await.expect("version"), 0);

        let RedeemOutcome::Redeemed(device) = redeem_as(pool, 1, "secret-1", 10, T0).await else {
            panic!("first redeem must succeed");
        };
        assert_eq!(
            device.scope, PHONE,
            "the invite's scope, not the device's ask"
        );
        assert_eq!(device.display_name, "my phone");
        assert_eq!(device.static_pubkey, PHONE_KEY);
        assert_eq!(device.expires_at, T0 + IDLE_EXPIRY_MS);
        assert_eq!(DeviceRepo::version(pool).await.expect("version"), 1);

        assert_eq!(
            redeem_as(pool, 1, "secret-1", 11, T0 + 1).await,
            RedeemOutcome::Refused(RedeemRefusal::Spent)
        );
        let (devices, version) = DeviceRepo::list(pool).await.expect("list");
        assert_eq!(devices, [device]);
        assert_eq!(version, 1);
        assert_eq!(
            redeem_as(pool, 99, "secret-99", 12, T0).await,
            RedeemOutcome::Refused(RedeemRefusal::Unknown)
        );
    }

    #[tokio::test]
    async fn the_fifth_wrong_secret_burns_the_invite() {
        let (_home, store) = store().await;
        let pool = store.pool();
        invite(pool, 1, PHONE).await;

        for attempt in 1..=MAX_INVITE_ATTEMPTS {
            assert_eq!(
                redeem_as(pool, 1, "guess", 10, T0).await,
                RedeemOutcome::Refused(RedeemRefusal::WrongSecret {
                    burned: attempt == MAX_INVITE_ATTEMPTS
                }),
                "attempt {attempt}"
            );
        }
        assert_eq!(
            redeem_as(pool, 1, "secret-1", 10, T0).await,
            RedeemOutcome::Refused(RedeemRefusal::Burned),
            "the right secret is too late once the invite burned"
        );
        assert!(DeviceRepo::list(pool).await.expect("list").0.is_empty());
    }

    #[tokio::test]
    async fn an_invite_redeems_until_thirty_seconds_past_its_expiry() {
        let (_home, store) = store().await;
        let pool = store.pool();
        let deadline = T0 + FIVE_MIN_MS + REDEEM_LEEWAY_MS;
        invite(pool, 1, PHONE).await;
        invite(pool, 2, PHONE).await;

        assert!(matches!(
            redeem_as(pool, 1, "secret-1", 10, deadline).await,
            RedeemOutcome::Redeemed(_)
        ));
        assert_eq!(
            redeem_as(pool, 2, "secret-2", 11, deadline + 1).await,
            RedeemOutcome::Refused(RedeemRefusal::Expired)
        );
    }

    #[tokio::test]
    async fn a_hello_is_bound_to_the_redeeming_key_and_slides_the_expiry() {
        let (_home, store) = store().await;
        let pool = store.pool();
        let device = paired(pool, 1, PHONE).await;
        let token = sha256_hex(&format!("mdd_token-{}", 1001));

        assert_eq!(
            DeviceRepo::verify_hello(pool, &token, &OTHER_KEY, T0 + 1).await.expect("hello"),
            HelloCheck::KeyMismatch {
                device_id: device.device_id.clone()
            },
            "a stolen token from another key is refused"
        );
        assert_eq!(
            DeviceRepo::verify_hello(pool, &sha256_hex("mdd_nope"), &PHONE_KEY, T0 + 1)
                .await
                .expect("hello"),
            HelloCheck::UnknownToken
        );

        let later = T0 + 60 * 60 * 1000;
        let HelloCheck::Accepted(seen) =
            DeviceRepo::verify_hello(pool, &token, &PHONE_KEY, later).await.expect("hello")
        else {
            panic!("the bound key is accepted");
        };
        assert_eq!(seen.last_seen_at, later);
        assert_eq!(seen.expires_at, later + IDLE_EXPIRY_MS);
        assert_eq!(DeviceRepo::list(pool).await.expect("list").0, [seen]);
        assert_eq!(
            DeviceRepo::version(pool).await.expect("version"),
            1,
            "a hello changes nothing a fence protects"
        );
    }

    #[tokio::test]
    async fn revoked_and_idle_devices_are_refused_at_hello() {
        let (_home, store) = store().await;
        let pool = store.pool();
        let revoked = paired(pool, 1, PHONE).await;
        let idle = paired(pool, 2, PHONE).await;
        let revoked_token = sha256_hex("mdd_token-1001");
        let idle_token = sha256_hex("mdd_token-1002");

        DeviceRepo::revoke(pool, &revoked.device_id, None, T0 + 1)
            .await
            .expect("revoke");
        assert_eq!(
            DeviceRepo::verify_hello(pool, &revoked_token, &PHONE_KEY, T0 + 2)
                .await
                .expect("hello"),
            HelloCheck::Revoked {
                device_id: revoked.device_id
            }
        );
        assert!(
            matches!(
                DeviceRepo::verify_hello(pool, &revoked_token, &OTHER_KEY, T0 + 2)
                    .await
                    .expect("hello"),
                HelloCheck::KeyMismatch { .. }
            ),
            "the key is checked first, so another key learns nothing about the device"
        );
        assert_eq!(
            DeviceRepo::verify_hello(pool, &idle_token, &PHONE_KEY, idle.expires_at)
                .await
                .expect("hello"),
            HelloCheck::Expired {
                device_id: idle.device_id
            }
        );
    }

    #[tokio::test]
    async fn revoke_and_rescope_are_fenced_on_the_registry_version() {
        let (_home, store) = store().await;
        let pool = store.pool();
        let device = paired(pool, 1, PHONE).await;
        let (_, version) = DeviceRepo::list(pool).await.expect("list");
        assert_eq!(version, 1);

        let narrower = StoredScope {
            base: ScopeBase::Mobile,
            admin: false,
        };
        assert_eq!(
            DeviceRepo::rescope(pool, &device.device_id, narrower, Some(0))
                .await
                .expect("rescope"),
            Fenced::Conflict { version: 1 },
            "a stale fence writes nothing"
        );
        assert_eq!(
            DeviceRepo::list(pool).await.expect("list").0[0].scope,
            PHONE
        );
        assert_eq!(
            DeviceRepo::rescope(pool, &device.device_id, narrower, Some(1))
                .await
                .expect("rescope"),
            Fenced::Applied { version: 2 }
        );
        assert_eq!(
            DeviceRepo::rescope(pool, &device.device_id, narrower, None)
                .await
                .expect("rescope"),
            Fenced::Applied { version: 2 },
            "the same scope again bumps nothing"
        );

        assert_eq!(
            DeviceRepo::revoke(pool, &device.device_id, Some(1), T0 + 1)
                .await
                .expect("revoke"),
            Fenced::Conflict { version: 2 }
        );
        assert_eq!(
            DeviceRepo::revoke(pool, &device.device_id, Some(2), T0 + 1)
                .await
                .expect("revoke"),
            Fenced::Applied { version: 3 }
        );
        assert_eq!(
            DeviceRepo::revoke(pool, &device.device_id, None, T0 + 2).await.expect("revoke"),
            Fenced::Applied { version: 3 },
            "revoking a revoked device is idempotent"
        );
        let (devices, _) = DeviceRepo::list(pool).await.expect("list");
        assert_eq!(
            devices[0].revoked_at,
            Some(T0 + 1),
            "the first revoke's time stays"
        );

        assert_eq!(
            DeviceRepo::rescope(pool, &device.device_id, PHONE, None)
                .await
                .expect("rescope"),
            Fenced::NotFound,
            "a revoked device cannot be rescoped back to life"
        );
        assert_eq!(
            DeviceRepo::revoke(pool, &id(4242), None, T0).await.expect("revoke"),
            Fenced::NotFound
        );
    }

    /// The redeeming device chooses its name, so storage bounds it: an
    /// over-long, blank or multi-line name is refused, and the refused redeem
    /// leaves the invite unspent for a well-formed retry.
    #[tokio::test]
    async fn a_device_name_is_bounded_and_a_refused_redeem_spends_nothing() {
        let (_home, store) = store().await;
        let pool = store.pool();
        invite(pool, 1, PHONE).await;
        let redeem_named = |name: String| async move {
            DeviceRepo::redeem(
                pool,
                &Redeem {
                    invite_id: &id(1),
                    secret_sha256: &sha256_hex("secret-1"),
                    device_id: &id(10),
                    display_name: &name,
                    token_sha256: &sha256_hex("mdd_token-10"),
                    static_pubkey: &PHONE_KEY,
                    now_ms: T0,
                },
            )
            .await
        };

        for bad in [
            "x".repeat(DISPLAY_NAME_MAX_CHARS + 1),
            String::new(),
            "   ".to_string(),
            "two\nlines".to_string(),
            "carriage\rreturn".to_string(),
        ] {
            assert!(
                redeem_named(bad.clone()).await.is_err(),
                "{bad:?} must not store"
            );
        }
        // Characters, not bytes: 64 two-byte characters still fit.
        let longest = "\u{00e9}".repeat(DISPLAY_NAME_MAX_CHARS);
        assert!(matches!(
            redeem_named(longest).await.expect("64 characters store"),
            RedeemOutcome::Redeemed(_)
        ));

        let invite_id = id(2);
        let too_long = "y".repeat(DISPLAY_NAME_MAX_CHARS + 1);
        let refused = DeviceRepo::create_invite(
            pool,
            &NewInvite {
                invite_id: &invite_id,
                secret_sha256: &sha256_hex("secret-2"),
                scope: PHONE,
                display_name: Some(&too_long),
                created_at: T0,
                expires_at: T0 + FIVE_MIN_MS,
            },
        )
        .await;
        assert!(
            refused.is_err(),
            "the operator's suggested name is bounded too"
        );
    }

    /// RECONCILED S1: admin exists only on base desktop, in storage too.
    #[tokio::test]
    async fn admin_off_base_desktop_cannot_be_stored() {
        let (_home, store) = store().await;
        let pool = store.pool();
        let admin_phone = StoredScope {
            base: ScopeBase::Mobile,
            admin: true,
        };
        let invite_id = id(1);
        let refused = DeviceRepo::create_invite(
            pool,
            &NewInvite {
                invite_id: &invite_id,
                secret_sha256: &sha256_hex("secret-1"),
                scope: admin_phone,
                display_name: None,
                created_at: T0,
                expires_at: T0 + FIVE_MIN_MS,
            },
        )
        .await;
        assert!(refused.is_err(), "an admin phone invite must not store");

        let device = paired(pool, 2, PHONE).await;
        assert!(
            DeviceRepo::rescope(pool, &device.device_id, admin_phone, None).await.is_err(),
            "an admin phone rescope must not store"
        );
        assert_eq!(
            DeviceRepo::list(pool).await.expect("list").0[0].scope,
            PHONE
        );

        let admin_desktop = StoredScope {
            base: ScopeBase::Desktop,
            admin: true,
        };
        assert!(matches!(
            DeviceRepo::rescope(pool, &device.device_id, admin_desktop, None)
                .await
                .expect("rescope"),
            Fenced::Applied { .. }
        ));
    }

    #[tokio::test]
    async fn prune_keeps_revoked_devices_for_ninety_days_and_drops_dead_invites() {
        let (_home, store) = store().await;
        let pool = store.pool();
        let old = paired(pool, 1, PHONE).await;
        let recent = paired(pool, 2, PHONE).await;
        let live = paired(pool, 3, PHONE).await;
        invite(pool, 4, PHONE).await;
        DeviceRepo::revoke(pool, &old.device_id, None, T0).await.expect("revoke");
        DeviceRepo::revoke(pool, &recent.device_id, None, T0 + 1).await.expect("revoke");
        let version = DeviceRepo::version(pool).await.expect("version");

        let pruned = DeviceRepo::prune(pool, T0 + REVOKED_RETENTION_MS + 1).await.expect("prune");
        assert_eq!(
            pruned,
            Pruned {
                invites: 4,
                devices: 1
            }
        );
        let remaining: Vec<_> = DeviceRepo::list(pool)
            .await
            .expect("list")
            .0
            .into_iter()
            .map(|device| device.device_id)
            .collect();
        assert_eq!(remaining, [recent.device_id, live.device_id]);
        assert_eq!(DeviceRepo::version(pool).await.expect("version"), version);
    }
}
