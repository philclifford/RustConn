//! System-keyring storage for the KDBX database password.
//!
//! The KDBX (KeePass-compatible) backend reads and writes its `.kdbx` database
//! directly (see [`super::kdbx`]); the database's own unlock password can be
//! cached in the OS keyring (GNOME Keyring / KDE Wallet / macOS Keychain) so the
//! user is not prompted on every launch. These helpers wrap that single entry.

use secrecy::{ExposeSecret, SecretString};

use crate::error::SecretResult;

/// Keyring entry key for the KDBX database password (namespaced).
const KEY_KDBX_PASSWORD: &str = "rustconn/kdbx-password";

/// Legacy keyring entry key (pre-0.19.18, without namespace).
const KEY_KDBX_PASSWORD_LEGACY: &str = "kdbx-password";

/// Stores the KDBX database password in the system keyring.
///
/// # Errors
/// Returns `SecretError` if storage fails.
pub async fn store_kdbx_password_in_keyring(password: &SecretString) -> SecretResult<()> {
    super::keyring::store(
        KEY_KDBX_PASSWORD,
        password.expose_secret(),
        "RustConn: KeePass Database Password",
    )
    .await
}

/// Retrieves the KDBX database password from the system keyring.
///
/// Tries the namespaced key first, then falls back to the legacy key for
/// backward compatibility. When found under the legacy key, transparently
/// migrates to the new namespaced key and removes the legacy entry.
///
/// # Errors
/// Returns `SecretError` if retrieval fails.
pub async fn get_kdbx_password_from_keyring() -> SecretResult<Option<SecretString>> {
    // Try new namespaced key first
    if let Some(value) = super::keyring::lookup(KEY_KDBX_PASSWORD).await? {
        return Ok(Some(SecretString::from(value)));
    }

    // Fall back to legacy key and migrate if found
    if let Some(value) = super::keyring::lookup(KEY_KDBX_PASSWORD_LEGACY).await? {
        let secret = SecretString::from(value);
        // Migrate: store under new key, delete legacy
        let _ = super::keyring::store(
            KEY_KDBX_PASSWORD,
            secret.expose_secret(),
            "RustConn: KeePass Database Password",
        )
        .await;
        let _ = super::keyring::clear(KEY_KDBX_PASSWORD_LEGACY).await;
        return Ok(Some(secret));
    }

    Ok(None)
}

/// Deletes the KDBX database password from the system keyring.
///
/// Removes both the namespaced and legacy entries to ensure full cleanup.
///
/// # Errors
/// Returns `SecretError` if deletion fails.
pub async fn delete_kdbx_password_from_keyring() -> SecretResult<()> {
    super::keyring::clear(KEY_KDBX_PASSWORD).await?;
    // Also clean up legacy key if it still exists
    let _ = super::keyring::clear(KEY_KDBX_PASSWORD_LEGACY).await;
    Ok(())
}
