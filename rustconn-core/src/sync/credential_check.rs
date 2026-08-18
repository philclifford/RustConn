//! Credential resolution result types for pre-connect checks.
//!
//! When a user attempts to connect, the credential resolver classifies
//! the outcome into an actionable result so the UI layer can show the
//! appropriate dialog (variable setup, backend missing, etc.) instead
//! of silently returning `None`.

use crate::config::SecretBackendType;
use crate::models::Credentials;

/// Result of pre-connect credential resolution.
///
/// Each variant maps to a specific UI action:
/// - `Resolved` → proceed with connection
/// - `NotNeeded` → proceed without credentials
/// - `VariableMissing` → show variable setup `AdwAlertDialog`
/// - `BackendNotConfigured` → show backend missing `AdwAlertDialog`
/// - `VaultEntryMissing` → show vault entry save dialog
/// - `KdbxLocked` → show on-demand KDBX unlock `AdwAlertDialog`
#[derive(Debug)]
pub enum CredentialResolutionResult {
    /// Credentials resolved successfully — proceed with connection.
    Resolved(Credentials),

    /// No credentials needed for this connection.
    NotNeeded,

    /// A referenced variable has no value on this device.
    ///
    /// The UI should show an `AdwAlertDialog` with `AdwPasswordEntryRow`
    /// (value) + `AdwComboRow` (backend) so the user can configure it.
    VariableMissing {
        /// Name of the missing variable (e.g. `"web_deploy_key"`).
        variable_name: String,
        /// Human-readable description from the variable template.
        description: Option<String>,
        /// Whether the variable holds a secret value.
        is_secret: bool,
    },

    /// The connection's password source references a secret backend
    /// that is not configured on this device.
    ///
    /// The UI should show an `AdwAlertDialog` with options
    /// "Enter Password Manually" / "Open Settings".
    BackendNotConfigured {
        /// The backend type that needs to be set up.
        required_backend: SecretBackendType,
    },

    /// The vault entry for this connection was not found.
    ///
    /// The UI should prompt the user to save credentials.
    VaultEntryMissing {
        /// Display name of the connection.
        connection_name: String,
        /// The lookup key used in the vault.
        lookup_key: String,
    },

    /// The KDBX database needs to be unlocked before credentials can be
    /// resolved. This occurs when "Save password = Don't save" is configured
    /// and no session-only master password is available in memory.
    ///
    /// The UI should show an `AdwAlertDialog` with a password entry prompting
    /// for the KDBX master password. On success, the password is kept in
    /// memory for the remainder of the session (never persisted) and the
    /// credential lookup is retried automatically.
    KdbxLocked {
        /// Path to the KDBX database file (displayed to the user for context).
        kdbx_path: std::path::PathBuf,
        /// The connection ID that triggered the unlock request, so the UI
        /// can retry after successful unlock.
        connection_id: uuid::Uuid,
    },

    /// The portable encrypted credential file needs to be unlocked.
    ///
    /// This occurs when the preferred backend is `PortableEncryptedFile` and
    /// the user's passphrase is not yet available in memory for this session.
    ///
    /// The UI should show an `AdwAlertDialog` prompting for the passphrase.
    /// On success the passphrase is set on the backend and kept in
    /// `SecretSettings.portable_passphrase` for the session, then the
    /// credential lookup is retried automatically.
    PortableFileLocked {
        /// Path to the portable credential file (displayed to the user).
        file_path: std::path::PathBuf,
        /// The connection ID that triggered the unlock request.
        connection_id: uuid::Uuid,
    },
}

// The `CredentialResolutionResult` enum is consumed by the GUI layer in
// `rustconn/src/window/protocols.rs` via `resolve_credentials_for_connect()`.
// The integration converts `Option<Credentials>` from the blocking resolver
// into the appropriate enum variant and shows the corresponding dialog.
