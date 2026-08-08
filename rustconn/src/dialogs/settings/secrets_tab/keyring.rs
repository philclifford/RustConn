//! System keyring helpers for storing/retrieving secret backend credentials.
//!
//! All save operations have a 5-second timeout ceiling to prevent blocking
//! the GTK main thread if the Secret Service daemon is unresponsive.

use std::time::Duration;

/// Timeout for keyring save operations (protects GTK main thread).
const KEYRING_SAVE_TIMEOUT: Duration = Duration::from_secs(5);

/// Saves Bitwarden master password to system keyring via rustconn-core.
///
/// Returns `true` on success, `false` on any failure.
pub(super) fn save_bw_password_to_keyring(password: &str) -> bool {
    let secret = secrecy::SecretString::from(password.to_owned());
    match crate::async_utils::with_runtime(|rt| {
        rt.block_on(async {
            tokio::time::timeout(
                KEYRING_SAVE_TIMEOUT,
                rustconn_core::secret::store_master_password_in_keyring(&secret),
            )
            .await
            .map_err(|_| "keyring save timed out after 5s")?
            .map_err(|e| e.to_string())
        })
    }) {
        Ok(Ok(())) => {
            tracing::info!("Bitwarden master password saved to keyring");
            true
        }
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "Failed to save Bitwarden password to keyring");
            false
        }
        Err(e) => {
            tracing::warn!(error = %e, "Runtime error saving Bitwarden password to keyring");
            false
        }
    }
}

/// Loads Bitwarden master password from system keyring via rustconn-core
pub(super) fn get_bw_password_from_keyring() -> Option<secrecy::SecretString> {
    let result = crate::async_utils::with_runtime(|rt| {
        rt.block_on(rustconn_core::secret::get_master_password_from_keyring())
    });
    match result {
        Ok(Ok(Some(secret))) => {
            tracing::debug!("Bitwarden master password loaded from keyring");
            Some(secret)
        }
        Ok(Ok(None)) => {
            tracing::debug!("No Bitwarden password found in keyring");
            None
        }
        Ok(Err(e)) => {
            tracing::debug!(error = %e, "Failed to load Bitwarden password from keyring");
            None
        }
        Err(e) => {
            tracing::debug!(error = %e, "Runtime error loading Bitwarden password from keyring");
            None
        }
    }
}

/// Saves 1Password service account token to system keyring.
///
/// Returns `true` on success, `false` on any failure.
pub(super) fn save_op_token_to_keyring(token: &str) -> bool {
    let secret = secrecy::SecretString::from(token.to_owned());
    match crate::async_utils::with_runtime(|rt| {
        rt.block_on(async {
            tokio::time::timeout(
                KEYRING_SAVE_TIMEOUT,
                rustconn_core::secret::store_token_in_keyring(&secret),
            )
            .await
            .map_err(|_| "keyring save timed out after 5s")?
            .map_err(|e| e.to_string())
        })
    }) {
        Ok(Ok(())) => {
            tracing::info!("1Password token saved to keyring");
            true
        }
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "Failed to save 1Password token to keyring");
            false
        }
        Err(e) => {
            tracing::warn!(error = %e, "Runtime error saving 1Password token");
            false
        }
    }
}

/// Loads 1Password service account token from system keyring
pub(super) fn get_op_token_from_keyring() -> Option<secrecy::SecretString> {
    let result = crate::async_utils::with_runtime(|rt| {
        rt.block_on(rustconn_core::secret::get_token_from_keyring())
    });
    match result {
        Ok(Ok(Some(secret))) => {
            tracing::debug!("1Password token loaded from keyring");
            Some(secret)
        }
        Ok(Ok(None) | Err(_)) | Err(_) => None,
    }
}

/// Saves Passbolt GPG passphrase to system keyring.
///
/// Returns `true` on success, `false` on any failure.
pub(super) fn save_pb_passphrase_to_keyring(passphrase: &str) -> bool {
    let secret = secrecy::SecretString::from(passphrase.to_owned());
    match crate::async_utils::with_runtime(|rt| {
        rt.block_on(async {
            tokio::time::timeout(
                KEYRING_SAVE_TIMEOUT,
                rustconn_core::secret::store_passphrase_in_keyring(&secret),
            )
            .await
            .map_err(|_| "keyring save timed out after 5s")?
            .map_err(|e| e.to_string())
        })
    }) {
        Ok(Ok(())) => {
            tracing::info!("Passbolt passphrase saved to keyring");
            true
        }
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "Failed to save Passbolt passphrase to keyring");
            false
        }
        Err(e) => {
            tracing::warn!(error = %e, "Runtime error saving Passbolt passphrase");
            false
        }
    }
}

/// Loads Passbolt GPG passphrase from system keyring
pub(super) fn get_pb_passphrase_from_keyring() -> Option<secrecy::SecretString> {
    let result = crate::async_utils::with_runtime(|rt| {
        rt.block_on(rustconn_core::secret::get_passphrase_from_keyring())
    });
    match result {
        Ok(Ok(Some(secret))) => {
            tracing::debug!("Passbolt passphrase loaded from keyring");
            Some(secret)
        }
        Ok(Ok(None) | Err(_)) | Err(_) => None,
    }
}

/// Saves KDBX database password to system keyring.
///
/// Returns `true` on success, `false` on any failure.
pub(super) fn save_kdbx_password_to_keyring(password: &str) -> bool {
    let secret = secrecy::SecretString::from(password.to_owned());
    match crate::async_utils::with_runtime(|rt| {
        rt.block_on(async {
            tokio::time::timeout(
                KEYRING_SAVE_TIMEOUT,
                rustconn_core::secret::store_kdbx_password_in_keyring(&secret),
            )
            .await
            .map_err(|_| "keyring save timed out after 5s")?
            .map_err(|e| e.to_string())
        })
    }) {
        Ok(Ok(())) => {
            tracing::info!("KDBX password saved to keyring");
            true
        }
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "Failed to save KDBX password to keyring");
            false
        }
        Err(e) => {
            tracing::warn!(error = %e, "Runtime error saving KDBX password");
            false
        }
    }
}

/// Loads KDBX database password from system keyring
pub(super) fn get_kdbx_password_from_keyring() -> Option<secrecy::SecretString> {
    let result = crate::async_utils::with_runtime(|rt| {
        rt.block_on(rustconn_core::secret::get_kdbx_password_from_keyring())
    });
    match result {
        Ok(Ok(Some(secret))) => {
            tracing::debug!("KDBX password loaded from keyring");
            Some(secret)
        }
        Ok(Ok(None) | Err(_)) | Err(_) => None,
    }
}
