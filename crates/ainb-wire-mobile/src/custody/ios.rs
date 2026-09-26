//! iOS Keychain custody: every secret is a generic-password item with
//! `kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly`, ungated.
//!
//! After-first-unlock, because the app reconnects from the background grace
//! window without the user present; this-device-only, because a Noise static
//! key or a device token that migrated to a new phone through a backup would
//! let that phone impersonate this one. No biometry gate: these are device
//! identities, not user secrets, and a Face ID prompt on every reconnect is
//! what the spike measured as unusable.

use security_framework::access_control::{ProtectionMode, SecAccessControl};
use security_framework::passwords::{
    PasswordOptions, delete_generic_password, get_generic_password, set_generic_password_options,
};

use super::custody_error;
use crate::records::WireError;

/// The keychain service every secret is filed under; the secret name is the
/// account.
pub const SERVICE: &str = "com.ainb.wire";

/// `errSecItemNotFound` (SecBase.h), the one status that means "no item"
/// rather than a Keychain failure.
const NOT_FOUND: i32 = -25300;

/// The secret under `name`, when one exists.
pub fn load(name: &str) -> Result<Option<Vec<u8>>, WireError> {
    match get_generic_password(SERVICE, name) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.code() == NOT_FOUND => Ok(None),
        Err(e) => Err(custody_error(format!("keychain read: {e}"))),
    }
}

/// Store `bytes` under `name`, replacing any previous item.
pub fn store(name: &str, bytes: &[u8]) -> Result<(), WireError> {
    let mut options = PasswordOptions::new_generic_password(SERVICE, name);
    let access = SecAccessControl::create_with_protection(
        Some(ProtectionMode::AccessibleAfterFirstUnlockThisDeviceOnly),
        0,
    )
    .map_err(|e| custody_error(format!("keychain access control: {e}")))?;
    options.set_access_control(access);
    options.set_access_synchronized(Some(false));
    // The library turns errSecDuplicateItem into an update in place, and an
    // update rewrites the value but keeps the old item's attributes. Replace
    // is the contract of `store_secret`, with THIS accessibility: delete
    // first so the new item carries it.
    delete(name)?;
    set_generic_password_options(bytes, options)
        .map_err(|e| custody_error(format!("keychain write: {e}")))
}

/// Forget the secret under `name`; absent is not an error.
pub fn delete(name: &str) -> Result<(), WireError> {
    match delete_generic_password(SERVICE, name) {
        Ok(()) => Ok(()),
        Err(e) if e.code() == NOT_FOUND => Ok(()),
        Err(e) => Err(custody_error(format!("keychain delete: {e}"))),
    }
}
