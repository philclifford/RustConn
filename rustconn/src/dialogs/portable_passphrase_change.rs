//! Changing the portable credential file's passphrase.
//!
//! The portable store's passphrase was settable exactly once — at creation — and
//! never afterwards. The file format made a change cheap from the first release
//! and nothing exposed it, so the only route was to build a second file and
//! migrate into it.
//!
//! ## Why this is a dialog of its own
//!
//! The Settings page's passphrase field is how the store is *opened*. Reusing it
//! to also mean "and change it to this" would make one field carry two intents
//! distinguished by nothing the user can see, on a value with no recovery path. A
//! separate dialog can ask for the current passphrase and the new one at the same
//! time, which is what makes the change verifiable before anything is written.
//!
//! ## What the caller still has to do
//!
//! [`rustconn_core::secret::change_portable_passphrase`] re-keys the file and
//! nothing else. Every copy of the old passphrase — the session settings, the
//! backend instance, the keyring entry or machine-encrypted blob — is stale the
//! moment it returns. This dialog hands the new passphrase to its caller for
//! exactly that reason, and says so on screen when a remembered copy exists,
//! because the copy on disk is only rewritten when Settings is saved.

use adw::prelude::*;
use gtk4::prelude::*;
use gtk4::{Label, glib};
use libadwaita as adw;
use secrecy::SecretString;

use crate::i18n::{i18n, i18n_f};

/// Everything the dialog needs to perform and report a passphrase change.
pub struct PassphraseChangeContext {
    /// The store to re-key. Resolved by the caller, so an unsaved path typed into
    /// Settings is the one that gets changed — the same rule the transfer dialog
    /// follows.
    pub store_path: std::path::PathBuf,
    /// Whether a copy of the passphrase is kept on this machine (keyring or
    /// machine-encrypted file). Decides only whether the "save your settings"
    /// note is shown.
    pub passphrase_is_remembered: bool,
    /// Handed the new passphrase on the main thread once the file has been
    /// re-keyed, so the caller can install it everywhere the old one lived.
    pub on_changed: Box<dyn Fn(&SecretString, usize)>,
}

/// Opens the passphrase change dialog.
pub fn show_passphrase_change_dialog(
    parent: &impl IsA<gtk4::Widget>,
    context: PassphraseChangeContext,
) {
    let group = adw::PreferencesGroup::builder()
        .title(i18n("Change Passphrase"))
        .description(i18n(
            "Every password in the file is encrypted again under the new passphrase. There is still no way to recover it, so it is asked for twice.",
        ))
        .build();

    let current_row = adw::PasswordEntryRow::builder()
        .title(i18n("Current passphrase"))
        .build();
    let new_row = adw::PasswordEntryRow::builder()
        .title(i18n("New passphrase"))
        .build();
    let confirm_row = adw::PasswordEntryRow::builder()
        .title(i18n("Confirm new passphrase"))
        .build();
    group.add(&current_row);
    group.add(&new_row);
    group.add(&confirm_row);

    let status_label = Label::builder()
        .wrap(true)
        .xalign(0.0)
        .visible(false)
        .build();
    let status_row = adw::ActionRow::builder().activatable(false).build();
    status_row.add_prefix(&status_label);
    // Same binding as the Secrets page: hiding a prefix widget leaves the row
    // behind as a full-height empty band in the boxed list.
    status_label
        .bind_property("visible", &status_row, "visible")
        .sync_create()
        .build();
    group.add(&status_row);

    let page = adw::PreferencesPage::new();
    page.add(&group);

    let change_button = gtk4::Button::with_label(&i18n("Change"));
    change_button.add_css_class("suggested-action");
    let cancel_button = gtk4::Button::with_label(&i18n("Cancel"));

    let header = adw::HeaderBar::builder()
        .title_widget(&adw::WindowTitle::new(&i18n("Change Passphrase"), ""))
        .show_end_title_buttons(false)
        .show_start_title_buttons(false)
        .build();
    header.pack_start(&cancel_button);
    header.pack_end(&change_button);

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&page));

    let dialog = adw::Dialog::builder()
        .content_width(480)
        .content_height(400)
        .child(&toolbar)
        .build();

    // Live feedback on the new passphrase, so a mismatch or a weak choice is
    // visible while typing rather than after the file has been rewritten.
    {
        // Distinct names: `move ||` takes these clones, so reusing the outer
        // bindings' names would shadow them and leave nothing to attach the
        // handlers to.
        let watched_new = new_row.clone();
        let watched_confirm = confirm_row.clone();
        let status_label = status_label.clone();
        let update = move || {
            let new_text = watched_new.text();
            let confirm_text = watched_confirm.text();

            if !confirm_text.is_empty() && new_text != confirm_text {
                set_status(
                    &status_label,
                    &i18n("The two new passphrases do not match"),
                    Level::Error,
                );
                return;
            }
            // Assessed here and not only in Settings: this is the other place a
            // passphrase is chosen, and it would be the one place that says
            // nothing about it. The verdict is not logged — see
            // `assess_passphrase`.
            let strength = rustconn_core::secret::assess_passphrase(new_text.as_str());
            if strength.deserves_a_warning() {
                set_status(&status_label, &weakness_message(strength), Level::Warning);
            } else {
                status_label.set_visible(false);
            }
        };
        let on_new = update.clone();
        new_row.connect_changed(move |_| on_new());
        confirm_row.connect_changed(move |_| update());
    }

    {
        let dialog = dialog.clone();
        cancel_button.connect_clicked(move |_| {
            dialog.close();
        });
    }

    let context = std::rc::Rc::new(context);
    {
        let context = std::rc::Rc::clone(&context);
        let current_row = current_row.clone();
        let new_row = new_row.clone();
        let confirm_row = confirm_row.clone();
        let status_label = status_label.clone();
        let dialog = dialog.clone();
        let cancel_button = cancel_button.clone();
        change_button.connect_clicked(move |button| {
            let Some((current, new)) = validated_passphrases(
                &current_row,
                &new_row,
                &confirm_row,
                &status_label,
            ) else {
                return;
            };

            set_status(&status_label, &i18n("Changing the passphrase…"), Level::Dim);
            button.set_sensitive(false);
            // Cancel too: closing the dialog mid-change would leave the worker
            // running with nowhere to report, and the change is not cancellable
            // once the derivation starts.
            cancel_button.set_sensitive(false);

            let path = context.store_path.clone();
            let context = std::rc::Rc::clone(&context);
            let button = button.clone();
            let cancel_button = cancel_button.clone();
            let status_label = status_label.clone();
            let dialog = dialog.clone();
            let current_row = current_row.clone();
            let new_row = new_row.clone();
            let confirm_row = confirm_row.clone();
            glib::spawn_future_local(async move {
                // Two Argon2id derivations — one to open the old store, one to
                // build the new one — so roughly a second on a blocking thread.
                // Inline, this is exactly long enough to freeze the dialog that
                // is meant to be reporting progress.
                let outcome = gtk4::gio::spawn_blocking({
                    let current = current.clone();
                    let new = new.clone();
                    move || rustconn_core::secret::change_portable_passphrase(&path, &current, &new)
                })
                .await;
                button.set_sensitive(true);
                cancel_button.set_sensitive(true);

                match outcome {
                    Ok(Ok(reencrypted)) => {
                        (context.on_changed)(&new, reencrypted);
                        status_label.set_visible(false);
                        // Cleared before the report, for two reasons. GTK does
                        // not wipe an entry buffer, so three copies of two
                        // passphrases would otherwise sit there for as long as
                        // the dialog is open; and an unchanged form invites a
                        // second press, which would now fail on a current
                        // passphrase that is no longer current.
                        for row in [&current_row, &new_row, &confirm_row] {
                            row.set_text("");
                        }
                        // Presented on this dialog, which therefore stays open —
                        // closing it here would take the report down with it.
                        // The user dismisses the report, then this.
                        report_success(&dialog, reencrypted, context.passphrase_is_remembered);
                    }
                    Ok(Err(rustconn_core::error::SecretError::IncorrectPassphrase)) => set_status(
                        &status_label,
                        &i18n(
                            "That passphrase does not open the portable file. Enter the passphrase the file was created with.",
                        ),
                        Level::Error,
                    ),
                    Ok(Err(e)) => set_status(
                        &status_label,
                        &i18n_f("Could not change the passphrase: {}", &[&e.to_string()]),
                        Level::Error,
                    ),
                    Err(_panic) => set_status(
                        &status_label,
                        &i18n("Could not change the passphrase. The file was not modified."),
                        Level::Error,
                    ),
                }
            });
        });
    }

    dialog.present(Some(parent));
}

/// Severity of an inline status message.
#[derive(Clone, Copy)]
enum Level {
    /// In-progress or neutral.
    Dim,
    /// Advice the user may act on and proceed past.
    Warning,
    /// The action cannot go ahead.
    Error,
}

/// Sets the inline status row's text and appearance.
fn set_status(label: &Label, message: &str, level: Level) {
    label.set_label(message);
    for class in ["dim-label", "warning", "error"] {
        label.remove_css_class(class);
    }
    label.add_css_class(match level {
        Level::Dim => "dim-label",
        Level::Warning => "warning",
        Level::Error => "error",
    });
    label.set_visible(true);
}

/// The advice shown for a passphrase that scores badly.
///
/// The same two strings the Secrets page uses, so the catalogues carry one
/// translation of each rather than two that can drift.
fn weakness_message(strength: rustconn_core::secret::PassphraseStrength) -> String {
    if matches!(
        strength,
        rustconn_core::secret::PassphraseStrength::TooShort
    ) {
        i18n(
            "A passphrase this short can be guessed quickly. This file is meant to be copied to your other computers, so the passphrase is the only thing protecting it.",
        )
    } else {
        i18n(
            "This passphrase would not take long to guess. Several unrelated words make a much stronger one, and are easier to remember than symbols.",
        )
    }
}

/// Checks the three entries, returning the current and new passphrases.
///
/// Reports the first problem inline and returns `None`. A weak new passphrase is
/// *not* a problem: it is advice the live validator has already shown, and
/// refusing it here would be a policy this project does not have — the same field
/// shape has to keep working for a file created under an earlier one.
fn validated_passphrases(
    current_row: &adw::PasswordEntryRow,
    new_row: &adw::PasswordEntryRow,
    confirm_row: &adw::PasswordEntryRow,
    status_label: &Label,
) -> Option<(SecretString, SecretString)> {
    let current_text = current_row.text();
    let new_text = new_row.text();
    let confirm_text = confirm_row.text();

    if current_text.is_empty() {
        set_status(
            status_label,
            &i18n("Enter the passphrase the file is protected with now"),
            Level::Error,
        );
        current_row.grab_focus();
        return None;
    }
    if new_text.is_empty() {
        set_status(status_label, &i18n("Enter a new passphrase"), Level::Error);
        new_row.grab_focus();
        return None;
    }
    // Required, not optional. The new passphrase is checked against nothing —
    // the file is about to be re-keyed to whatever is typed — so a typo here
    // produces a store that opens with something nobody knows. This is the same
    // rule the Settings page applies to a store being created.
    if confirm_text != new_text {
        set_status(
            status_label,
            &i18n("The two new passphrases do not match"),
            Level::Error,
        );
        confirm_row.grab_focus();
        return None;
    }
    if new_text == current_text {
        // Not merely pointless: it would re-key the file, which is a full rewrite
        // of every entry, and report success for a change that changed nothing
        // the user can observe.
        set_status(
            status_label,
            &i18n("The new passphrase is the same as the current one"),
            Level::Error,
        );
        new_row.grab_focus();
        return None;
    }

    Some((
        SecretString::from(current_text.to_string()),
        SecretString::from(new_text.to_string()),
    ))
}

/// Confirms the change, and says what is still stale if a copy is remembered.
///
/// A dialog rather than a toast: the count is the only evidence the re-encryption
/// happened, and where a remembered copy exists the note is an instruction the
/// user has to act on — a five-second toast is the wrong place for both.
fn report_success(dialog: &adw::Dialog, reencrypted: usize, passphrase_is_remembered: bool) {
    // A count rather than a plural form, matching the Secrets page: this project
    // has no `ngettext` user and one row is not worth 16 catalogues of plural
    // rules.
    let mut body = i18n_f(
        "Passwords encrypted again under the new passphrase: {}",
        &[&reencrypted.to_string()],
    );
    if passphrase_is_remembered {
        body.push_str("\n\n");
        body.push_str(&i18n(
            "Save your settings so the copy of the passphrase kept on this computer is updated too. Until then this computer still remembers the old one.",
        ));
    }

    let alert = adw::AlertDialog::new(Some(&i18n("Passphrase Changed")), Some(&body));
    alert.add_response("close", &i18n("Close"));
    alert.set_default_response(Some("close"));
    alert.set_close_response("close");
    alert.present(Some(dialog));
}
