//! Backend missing dialog for Cloud Sync credential resolution.
//!
//! Shown when a connection's password source references a secret backend
//! (KeePass, Bitwarden, etc.) that is not configured on this device.
//!
//! GNOME HIG: `AdwAlertDialog` with two response buttons.

use adw::prelude::*;
use libadwaita as adw;

use crate::i18n::i18n;

/// Response from the backend missing dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendMissingResponse {
    /// User chose to enter the password manually (one-time).
    EnterManually,
    /// User chose to open settings to configure the backend.
    OpenSettings,
}

/// Shows the "backend could not be read" dialog, naming the backend.
///
/// Presents an `AdwAlertDialog` offering to enter the password once or to open
/// Settings.
///
/// `backend` is the one the connection's password source points at. It was
/// already carried all the way here — `CredentialResolutionResult::BackendNotConfigured`
/// has a `required_backend` field, set from the settings when the lookup failed —
/// and then dropped on the floor, so the dialog said "no backend is set up yet"
/// to people who had set one up and whose vault was merely locked. Naming it, and
/// saying it could not be *read* rather than that it does not exist, is the
/// difference between a dead end and knowing where to look.
///
/// # Arguments
/// * `parent` — parent widget for the dialog
/// * `backend` — the backend the connection expects its password to be in
/// * `callback` — called with the user's response
pub fn show_backend_missing_dialog<F>(
    parent: &impl IsA<gtk4::Widget>,
    backend: rustconn_core::config::SecretBackendType,
    callback: F,
) where
    F: Fn(BackendMissingResponse) + 'static,
{
    let backend_name = crate::vault_ops::backend_display_name(backend);
    let heading = crate::i18n::i18n_f("Could not read the password from {}", &[&backend_name]);
    let body = crate::i18n::i18n_f(
        "This connection keeps its password in {}, which is not available right now — it may be locked, not logged in, or not set up on this computer. Open Settings, then Secrets, to check its status.",
        &[&backend_name],
    );

    let dialog = adw::AlertDialog::new(Some(&heading), Some(&body));

    dialog.add_response("manual", &i18n("Enter Password Manually"));
    dialog.add_response("settings", &i18n("Open Settings"));
    dialog.set_default_response(Some("manual"));
    dialog.set_close_response("manual");
    dialog.set_response_appearance("settings", adw::ResponseAppearance::Suggested);

    dialog.connect_response(None, move |_, response| {
        if response == "settings" {
            callback(BackendMissingResponse::OpenSettings);
        } else {
            callback(BackendMissingResponse::EnterManually);
        }
    });

    dialog.present(Some(parent));
}
