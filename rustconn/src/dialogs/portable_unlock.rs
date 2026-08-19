//! On-demand unlock dialog for the portable encrypted credential file.
//!
//! Shown when a connection requires credentials but the portable backend's
//! passphrase is not yet available in memory. The user enters the passphrase,
//! which is verified by attempting a trial decryption of the first entry (or
//! by opening the file itself). On success the passphrase is handed back to
//! the caller for session-lifetime storage.
//!
//! GNOME HIG: `AdwAlertDialog` with `extra_child` widget.

use adw::prelude::*;
use gtk4::prelude::*;
use libadwaita as adw;

use crate::i18n::{i18n, i18n_f};

/// Response from the portable unlock dialog.
#[derive(Debug)]
pub enum PortableUnlockResponse {
    /// User cancelled the dialog — connection is not started.
    Cancel,
    /// User entered a valid passphrase.
    Unlocked {
        /// The verified passphrase (session-only, never persisted to disk).
        passphrase: secrecy::SecretString,
    },
}

/// Shows the "Portable Credential File Locked" dialog.
///
/// Presents an `AdwAlertDialog` with:
/// - heading: "Portable credential file is locked"
/// - body: filename of the credential file
/// - extra child: `AdwPreferencesGroup` with `AdwPasswordEntryRow`
/// - responses: Cancel / Unlock & Connect
///
/// The dialog verifies the passphrase by unwrapping the store's data key.
/// Invalid passphrases show an inline error and keep the dialog open, which is
/// why `can-close` is switched off: activating a response normally closes an
/// `AdwAlertDialog` on its own, and that took the retry away — the dialog
/// vanished on a mistyped passphrase and the connection quietly never started.
/// With `can-close` off, every exit here is an explicit `force_close`.
///
/// # Arguments
/// * `parent` — parent widget for the dialog
/// * `file_path` — path to the portable credential file (displayed for context)
/// * `callback` — called with the user's response after verification
pub fn show_portable_unlock_dialog<F>(
    parent: &impl IsA<gtk4::Widget>,
    file_path: &std::path::Path,
    callback: F,
) where
    F: Fn(PortableUnlockResponse) + 'static,
{
    let heading = i18n("Portable credential file is locked");

    let filename = file_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("credentials-portable.enc");

    let body = i18n_f(
        "Enter the passphrase to unlock '{}'.\nThe passphrase will be kept in memory for this session only.",
        &[filename],
    );

    let dialog = adw::AlertDialog::new(Some(&heading), Some(&body));

    // Build extra child: passphrase entry + status label
    let content_box = gtk4::Box::new(gtk4::Orientation::Vertical, 6);

    let prefs_group = adw::PreferencesGroup::new();

    let passphrase_row = adw::PasswordEntryRow::new();
    passphrase_row.set_title(&i18n("Passphrase"));
    prefs_group.add(&passphrase_row);

    content_box.append(&prefs_group);

    // Status label for verification errors (hidden initially)
    let status_label = gtk4::Label::builder()
        .label("")
        .wrap(true)
        .xalign(0.0)
        .visible(false)
        .css_classes(["error"])
        .build();
    content_box.append(&status_label);

    dialog.set_extra_child(Some(&content_box));

    // Responses
    dialog.add_response("cancel", &i18n("Cancel"));
    dialog.add_response("unlock", &i18n("Unlock & Connect"));
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    dialog.set_response_appearance("unlock", adw::ResponseAppearance::Suggested);

    // Keep the dialog up across a failed attempt. `AdwAlertDialog` closes itself
    // when a response is activated, so the inline error and the re-enabled
    // buttons below were unreachable without this.
    dialog.set_can_close(false);

    // Disable "Unlock" until the user types something
    dialog.set_response_enabled("unlock", false);
    let dialog_enable = dialog.clone();
    passphrase_row.connect_changed(move |row| {
        dialog_enable.set_response_enabled("unlock", !row.text().is_empty());
    });

    // Capture state for the response handler
    let file_path_owned = file_path.to_path_buf();
    let passphrase_row_ref = passphrase_row.clone();
    let status_label_ref = status_label.clone();
    let callback = std::rc::Rc::new(std::cell::RefCell::new(Some(callback)));
    let cancel_callback = callback.clone();

    dialog.connect_response(None, move |dialog, response| {
        if response == "unlock" {
            let passphrase_text = passphrase_row_ref.text().to_string();
            if passphrase_text.is_empty() {
                return;
            }

            let passphrase = secrecy::SecretString::from(passphrase_text);
            let path_verify = file_path_owned.clone();
            let passphrase_verify = passphrase.clone();

            // Show verifying state
            status_label_ref.set_text(&i18n("Verifying…"));
            status_label_ref.remove_css_class("error");
            status_label_ref.add_css_class("dim-label");
            status_label_ref.set_visible(true);
            dialog.set_response_enabled("unlock", false);
            dialog.set_response_enabled("cancel", false);

            let status_async = status_label_ref.clone();
            let dialog_async = dialog.clone();
            let callback_clone = callback.clone();

            // Run verification in background thread (argon2 key derivation is CPU-heavy)
            gtk4::glib::spawn_future_local(async move {
                let result = gtk4::gio::spawn_blocking(move || {
                    verify_passphrase(&path_verify, &passphrase_verify)
                })
                .await;

                match result {
                    Ok(Ok(())) => {
                        // Verification succeeded — close dialog and notify caller
                        dialog_async.force_close();
                        if let Some(cb) = callback_clone.borrow_mut().take() {
                            cb(PortableUnlockResponse::Unlocked { passphrase });
                        }
                    }
                    Ok(Err(e)) => {
                        // Invalid passphrase — show error and let the user retry
                        status_async.set_text(&e);
                        status_async.remove_css_class("dim-label");
                        status_async.add_css_class("error");
                        status_async.set_visible(true);
                        dialog_async.set_response_enabled("unlock", true);
                        dialog_async.set_response_enabled("cancel", true);
                    }
                    Err(_join_err) => {
                        status_async.set_text(&i18n("Verification failed"));
                        status_async.remove_css_class("dim-label");
                        status_async.add_css_class("error");
                        status_async.set_visible(true);
                        dialog_async.set_response_enabled("unlock", true);
                        dialog_async.set_response_enabled("cancel", true);
                    }
                }
            });
        } else {
            // Cancel. `can-close` is off, so this has to close the dialog itself.
            dialog.force_close();
            if let Some(cb) = callback.borrow_mut().take() {
                cb(PortableUnlockResponse::Cancel);
            }
        }
    });

    // Escape and the close button do not emit `response` while `can-close` is
    // off; they arrive here instead. Same outcome as Cancel — the user asked to
    // get out, and a dialog with no way out would be worse than the bug above.
    dialog.connect_close_attempt(move |dialog| {
        dialog.force_close();
        if let Some(cb) = cancel_callback.borrow_mut().take() {
            cb(PortableUnlockResponse::Cancel);
        }
    });

    dialog.present(Some(parent));
}

/// Verifies a passphrase against the portable credential file.
///
/// Delegates to `rustconn-core`, which unwraps the store's data key — one Argon2
/// derivation, and a definitive answer. Translating the outcome here rather than
/// showing the core error keeps the message in the user's language and keeps the
/// three cases distinct: wrong passphrase, unreadable file, and a file that does
/// not exist yet (accepted, since the first write creates it).
fn verify_passphrase(
    path: &std::path::Path,
    passphrase: &secrecy::SecretString,
) -> Result<(), String> {
    use rustconn_core::error::SecretError;

    rustconn_core::secret::verify_portable_passphrase(path, passphrase).map_err(|e| match e {
        SecretError::IncorrectPassphrase => i18n("Incorrect passphrase"),
        other => i18n_f("Cannot open the file: {}", &[&other.to_string()]),
    })
}
