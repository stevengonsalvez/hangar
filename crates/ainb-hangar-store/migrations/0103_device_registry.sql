-- Hangar v1 schema, migration 0103: the paired-device registry (R1-03, spec D13).
--
-- Three tables, all empty until a device pairs, and nothing reads them until
-- the peer leg is switched on (`AINB_HANGAR_PEER_LISTEN`):
--
--   device_invite            one single-use pairing invite
--   device                   one paired device and its credential
--   device_registry_version  the fence `device/revoke` and `device/rescope` carry
--
-- # Secrets are stored as digests
--
-- The invite secret and the device token are stored only as their lower-case
-- hex SHA-256, the `socket_token` convention. The device's Noise static public
-- key is stored whole: the hello compares it to the session's remote static.
--
-- # Scope
--
-- A scope is a base plus an additive admin flag, and admin exists only on base
-- `desktop` (RECONCILED S1). The CHECK makes any other pair unstorable, the
-- same invariant `DeviceScope::new` enforces on the wire.

CREATE TABLE device_invite (
    -- A ULID: 26 Crockford base32 characters.
    invite_id      TEXT PRIMARY KEY NOT NULL CHECK (
        length(invite_id) = 26 AND invite_id NOT GLOB '*[^0-9A-HJKMNP-TV-Z]*'
    ),
    -- SHA-256 hex of the 32-byte invite secret.
    secret_sha256  TEXT NOT NULL CHECK (
        length(secret_sha256) = 64 AND secret_sha256 NOT GLOB '*[^0-9a-f]*'
    ),
    scope_base     TEXT NOT NULL CHECK (scope_base IN ('desktop', 'mobile', 'mobile+type')),
    scope_admin    INTEGER NOT NULL DEFAULT 0 CHECK (
        scope_admin IN (0, 1) AND (scope_admin = 0 OR scope_base = 'desktop')
    ),
    -- The name the operator suggested; the redeeming device may pick another.
    display_name   TEXT,
    -- Unix milliseconds.
    created_at     INTEGER NOT NULL,
    -- Unix milliseconds. A redeem is accepted up to 30 s past this.
    expires_at     INTEGER NOT NULL CHECK (expires_at > created_at),
    -- Wrong secrets presented so far; the fifth burns the invite.
    attempts       INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    -- Unix milliseconds the invite was burned by wrong secrets.
    burned_at      INTEGER,
    -- Unix milliseconds of the one successful redeem, and the device it made.
    consumed_at    INTEGER,
    device_id      TEXT,
    CHECK ((consumed_at IS NULL) = (device_id IS NULL)),
    CHECK (consumed_at IS NULL OR burned_at IS NULL)
);

CREATE TABLE device (
    -- A ULID minted at redeem.
    device_id      TEXT PRIMARY KEY NOT NULL CHECK (
        length(device_id) = 26 AND device_id NOT GLOB '*[^0-9A-HJKMNP-TV-Z]*'
    ),
    display_name   TEXT NOT NULL,
    scope_base     TEXT NOT NULL CHECK (scope_base IN ('desktop', 'mobile', 'mobile+type')),
    scope_admin    INTEGER NOT NULL DEFAULT 0 CHECK (
        scope_admin IN (0, 1) AND (scope_admin = 0 OR scope_base = 'desktop')
    ),
    -- SHA-256 hex of the plaintext `mdd_` token.
    token_sha256   TEXT NOT NULL UNIQUE CHECK (
        length(token_sha256) = 64 AND token_sha256 NOT GLOB '*[^0-9a-f]*'
    ),
    -- The Noise static public key of the session that redeemed: the token is
    -- bound to it, and a hello from any other key is refused.
    static_pubkey  BLOB NOT NULL CHECK (length(static_pubkey) = 32),
    -- The invite this device redeemed. Not a foreign key: invites are pruned
    -- once spent, and the device outlives its invite.
    invite_id      TEXT NOT NULL UNIQUE,
    -- Unix milliseconds.
    created_at     INTEGER NOT NULL,
    last_seen_at   INTEGER NOT NULL,
    -- Sliding: last_seen_at plus 90 days, moved by every accepted hello.
    expires_at     INTEGER NOT NULL,
    -- Unix milliseconds of the revoke. A revoked row is kept for 90 days so
    -- `device/list` can show it, then pruned.
    revoked_at     INTEGER
);

CREATE INDEX idx_device_revoked_at ON device (revoked_at) WHERE revoked_at IS NOT NULL;
CREATE INDEX idx_device_invite_expires_at ON device_invite (expires_at);

-- One row. Bumped by every registry write that changes a device (a redeem, a
-- revoke, a rescope), so a fenced write can tell it read a stale list.
CREATE TABLE device_registry_version (
    singleton  INTEGER PRIMARY KEY CHECK (singleton = 1),
    version    INTEGER NOT NULL CHECK (version >= 0)
);

INSERT INTO device_registry_version (singleton, version) VALUES (1, 0);
