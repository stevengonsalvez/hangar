//! iOS Keychain custody: the keypair is a generic-password item with
//! `kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly`, ungated.
//!
//! After-first-unlock, because the app reconnects from the background grace
//! window without the user present; this-device-only, because a Noise static
//! key that migrated to a new phone through a backup would let that phone
//! impersonate this one. No biometry gate: the key is a device identity, not
//! a user secret, and a Face ID prompt on every reconnect is what the spike
//! measured as unusable.

use security_framework::access_control::{ProtectionMode, SecAccessControl};
use security_framework::passwords::{
    PasswordOptions, delete_generic_password, get_generic_password, set_generic_password_options,
};

use super::{KeyBytes, custody_error};
use crate::records::WireError;

/// The keychain service.
pub const SERVICE: &str = "com.ainb.wire";
/// The keychain account: the one device keypair.
pub const ACCOUNT: &str = "device-noise-static";

/// The stored keypair, when one exists.
pub fn load() -> Result<Option<KeyBytes>, WireError> {
    match get_generic_password(SERVICE, ACCOUNT) {
        Ok(bytes) => Ok(Some(bytes)),
        // errSecItemNotFound
        Err(e) if e.code() == -25300 => Ok(None),
        Err(e) => Err(custody_error(format!("keychain read: {e}"))),
    }
}

/// Store a freshly minted keypair.
pub fn store(bytes: &KeyBytes) -> Result<(), WireError> {
    let mut options = PasswordOptions::new_generic_password(SERVICE, ACCOUNT);
    let access = SecAccessControl::create_with_protection(
        Some(ProtectionMode::AccessibleAfterFirstUnlockThisDeviceOnly),
        0,
    )
    .map_err(|e| custody_error(format!("keychain access control: {e}")))?;
    options.set_access_control(access);
    options.set_access_synchronized(Some(false));
    // A stale item from an earlier install with other attributes would make
    // the add fail with errSecDuplicateItem; the file backend has the same
    // create-once rule, so replace rather than fail.
    let _ = delete_generic_password(SERVICE, ACCOUNT);
    set_generic_password_options(bytes, options)
        .map_err(|e| custody_error(format!("keychain write: {e}")))
}
