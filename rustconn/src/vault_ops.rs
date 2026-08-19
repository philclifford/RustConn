//! Vault credential operations
//!
//! Functions for saving, loading, renaming, deleting, and copying credentials
//! in the configured secret backend (KeePass, libsecret, Bitwarden, 1Password,
//! Passbolt, Pass). Extracted from `state.rs` to reduce module complexity.

/// Shows an actionable, blocking dialog when saving a credential fails.
///
/// A silently lost credential is critical, so this uses an `adw::AlertDialog`
/// following the GNOME HIG "explain what happened and what to do" pattern
/// (R1.1, R1.2) rather than a transient toast. The body states the underlying
/// cause — the [`SecretError`] `Display` string, which carries only operation
/// context / backend stderr and never any secret value (R1.4) — together with
/// a recovery action. A "settings" response opens Settings, where the user can
/// pick a working backend in the Secrets section (R1.5). Every string is routed
/// through `i18n()` / `i18n_f()` (R1.3, R16.1).
///
/// Uses `glib::idle_add_local_once` so the dialog is presented on the GTK main
/// thread. Falls back to a log message if no active window is found.
///
/// [`SecretError`]: rustconn_core::error::SecretError
fn show_vault_save_error(err: &rustconn_core::error::SecretError) {
    use gtk4::prelude::*;
    use libadwaita as adw;
    use libadwaita::prelude::*;

    // A locked portable store is not a broken backend, and the generic recovery
    // text below ("no system keyring is responding, pick another backend") is
    // wrong advice for it. It also used to surface the raw core error string,
    // which is English in every locale.
    if matches!(
        err,
        rustconn_core::error::SecretError::PassphraseRequired
            | rustconn_core::error::SecretError::IncorrectPassphrase
    ) {
        show_portable_locked_error(matches!(
            err,
            rustconn_core::error::SecretError::IncorrectPassphrase
        ));
        return;
    }

    // The cause is the SecretError Display string. Its variants embed only
    // operation context and backend diagnostics (e.g. secret-tool stderr),
    // never secret values, so it is safe to surface verbatim (R1.4).
    let cause = err.to_string();

    gtk4::glib::idle_add_local_once(move || {
        let Some(app) = gtk4::gio::Application::default() else {
            tracing::warn!("Could not show vault save error dialog: no default application");
            return;
        };
        let Some(gtk_app) = app.downcast_ref::<gtk4::Application>() else {
            tracing::warn!("Could not show vault save error dialog: not a GtkApplication");
            return;
        };
        let Some(window) = gtk_app.active_window() else {
            tracing::warn!("Could not show vault save error dialog: no active window");
            return;
        };

        // Recovery action: point the user at Settings -> Secrets to choose a
        // backend that works on their desktop (R1.5).
        let recovery = if rustconn_core::snap::is_snap() {
            crate::i18n::i18n(
                "The system keyring is not accessible. Run: sudo snap connect rustconn:password-manager-service — or open Settings, then Secrets, and choose another backend.",
            )
        } else {
            crate::i18n::i18n(
                "No system keyring is responding. Open Settings, then Secrets, and choose another backend such as Encrypted file or KeePassXC.",
            )
        };
        // Body = cause followed by the recovery action (R1.1).
        let body = crate::i18n::i18n_f("{}\n\n{}", &[&cause, &recovery]);

        let dialog =
            adw::AlertDialog::new(Some(&crate::i18n::i18n("Password not saved")), Some(&body));
        dialog.add_response("close", &crate::i18n::i18n("Close"));
        dialog.add_response("settings", &crate::i18n::i18n("Open Settings"));
        // The recovery action is the primary, suggested action.
        dialog.set_response_appearance("settings", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("settings"));
        dialog.set_close_response("close");

        let window_for_action = window.clone();
        dialog.connect_response(None, move |_, response| {
            if response == "settings" {
                // Activate the existing window action that opens the Settings
                // dialog; the user navigates to the Secrets section there.
                let _ = gtk4::prelude::WidgetExt::activate_action(
                    &window_for_action,
                    "win.settings",
                    None,
                );
            }
        });

        dialog.present(Some(&window));
    });
}

/// Reports that the portable credential file could not be written because it is
/// locked, or because the passphrase in force does not open it.
///
/// `wrong_passphrase` picks between the two: they need different next steps, and
/// telling someone to "enter the passphrase" when they already have one entered
/// and wrong is the kind of advice that gets a bug report.
///
/// The suggested action opens Settings ▸ Secrets rather than the unlock dialog
/// directly. The unlock dialog hands its result to `AppState::unlock_portable_store`,
/// and this function is reached from `spawn_blocking_with_callback` on paths that
/// hold no state handle — routing through Settings keeps one place responsible
/// for recording a verified passphrase instead of adding a second.
fn show_portable_locked_error(wrong_passphrase: bool) {
    use gtk4::prelude::*;
    use libadwaita as adw;
    use libadwaita::prelude::*;

    gtk4::glib::idle_add_local_once(move || {
        let Some(window) = gtk4::gio::Application::default()
            .and_then(|app| app.downcast_ref::<gtk4::Application>().cloned())
            .and_then(|app| app.active_window())
        else {
            tracing::warn!("Could not show portable store lock dialog: no active window");
            return;
        };

        let (heading, body) = if wrong_passphrase {
            (
                crate::i18n::i18n("Passphrase Does Not Open the Portable File"),
                crate::i18n::i18n(
                    "The password was not saved. The passphrase in use does not decrypt the portable credential file. Open Settings, then Secrets, and enter the passphrase the file was created with.",
                ),
            )
        } else {
            (
                crate::i18n::i18n("Portable File Is Locked"),
                crate::i18n::i18n(
                    "The password was not saved because the portable credential file has not been unlocked in this session. Open Settings, then Secrets, and enter its passphrase.",
                ),
            )
        };

        let dialog = adw::AlertDialog::new(Some(&heading), Some(&body));
        dialog.add_response("close", &crate::i18n::i18n("Close"));
        dialog.add_response("settings", &crate::i18n::i18n("Open Settings"));
        dialog.set_response_appearance("settings", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("settings"));
        dialog.set_close_response("close");

        let window_for_action = window.clone();
        dialog.connect_response(None, move |_, response| {
            if response == "settings" {
                let _ = gtk4::prelude::WidgetExt::activate_action(
                    &window_for_action,
                    "win.settings",
                    None,
                );
            }
        });

        dialog.present(Some(&window));
    });
}

/// Shows a transient toast when a credential was saved to the encrypted-file
/// fallback because the preferred backend (system keyring) was unavailable.
///
/// The credential *was* saved, so this is a non-blocking result notification
/// (GNOME HIG: transient result → `adw::Toast`, not an error dialog). It is
/// shown at `Warning` priority because the user's preferred backend is degraded
/// and worth their attention. The message is routed through `i18n()` (R3.4,
/// R16.1). The backend identifier is intentionally not surfaced in the UI — the
/// user-facing distinction is only "preferred vs encrypted-file fallback".
///
/// Uses `glib::idle_add_local_once` so the toast is presented on the GTK main
/// thread, mirroring [`show_vault_save_error`]. Falls back to a log message if
/// no active window is found.
fn show_vault_fallback_toast() {
    use gtk4::prelude::*;

    gtk4::glib::idle_add_local_once(move || {
        let Some(app) = gtk4::gio::Application::default() else {
            tracing::warn!("Could not show vault fallback toast: no default application");
            return;
        };
        let Some(gtk_app) = app.downcast_ref::<gtk4::Application>() else {
            tracing::warn!("Could not show vault fallback toast: not a GtkApplication");
            return;
        };
        let Some(window) = gtk_app.active_window() else {
            tracing::warn!("Could not show vault fallback toast: no active window");
            return;
        };

        let message = crate::i18n::i18n(
            "Saved to the encrypted file store because the system keyring was unavailable.",
        );
        crate::toast::show_toast_on_window(&window, &message, crate::toast::ToastType::Warning);
    });
}

/// Stores credentials through the [`SecretManager`] fallback chain, reporting
/// which backend accepted the write.
///
/// Unlike [`dispatch_vault_op`], which targets a single backend, this routes the
/// generic (non-KeePass) save path through [`SecretManager::store_reported`] so a
/// failing preferred backend gracefully falls back to the encrypted-file store
/// when `enable_fallback` is set (issue #201, R3.1). The returned [`StoreOutcome`]
/// lets the caller surface a toast when the fallback was used (R3.4).
///
/// # Errors
///
/// Returns the backend's own [`SecretError`] if the store times out after 10s or
/// every backend in the chain rejects the write. The typed error is preserved
/// rather than flattened to a string because the caller has to tell a locked
/// portable store apart from an unresponsive keyring — the two need different
/// advice, and a formatted string cannot be matched on.
///
/// [`SecretManager`]: rustconn_core::secret::SecretManager
/// [`SecretManager::store_reported`]: rustconn_core::secret::SecretManager::store_reported
/// [`StoreOutcome`]: rustconn_core::secret::StoreOutcome
/// [`SecretError`]: rustconn_core::error::SecretError
fn store_reported_blocking(
    secret_settings: &rustconn_core::config::SecretSettings,
    lookup_key: &str,
    creds: &rustconn_core::models::Credentials,
) -> Result<rustconn_core::secret::StoreOutcome, rustconn_core::error::SecretError> {
    use rustconn_core::error::SecretError;

    let manager = rustconn_core::secret::SecretManager::build_from_settings(secret_settings);
    let allow_fallback = secret_settings.enable_fallback;

    crate::async_utils::with_runtime(|rt| {
        rt.block_on(async {
            // Vault operations wait 10 seconds — a hung keyring must not block the
            // GTK callback indefinitely (matches dispatch_vault_op's timeout).
            tokio::time::timeout(
                std::time::Duration::from_secs(10),
                manager.store_reported(lookup_key, creds, allow_fallback),
            )
            .await
            .map_err(|_| SecretError::StoreFailed("Vault store timed out after 10s".to_string()))?
        })
    })
    .map_err(SecretError::StoreFailed)
    .and_then(|r| r)
}

/// Saves a connection password to the configured vault backend.
///
/// Dispatches to KeePass (hierarchical) or generic backend (flat key)
/// based on the current settings. Password is taken as `&SecretString`
/// so plaintext copies do not leak via call-site `String`s — see
/// `secrets-guide.md`.
#[expect(
    clippy::too_many_arguments,
    reason = "function parameters mirror upstream API or struct fields 1:1; bundling into a struct only restates the field list"
)]
pub fn save_password_to_vault(
    settings: &rustconn_core::config::AppSettings,
    groups: &[rustconn_core::models::ConnectionGroup],
    conn: Option<&rustconn_core::models::Connection>,
    conn_name: &str,
    conn_host: &str,
    protocol: rustconn_core::models::ProtocolType,
    username: &str,
    password: &secrecy::SecretString,
    conn_id: uuid::Uuid,
) {
    use secrecy::ExposeSecret;
    let protocol_str = protocol.as_str().to_lowercase();

    if settings.secrets.kdbx_enabled
        && matches!(
            settings.secrets.preferred_backend,
            rustconn_core::config::SecretBackendType::KeePassXc
                | rustconn_core::config::SecretBackendType::KdbxFile
        )
    {
        // KeePass backend — use hierarchical path
        if let Some(kdbx_path) = settings.secrets.kdbx_path.clone() {
            let key_file = settings.secrets.kdbx_key_file.clone();
            let db_password = settings.secrets.kdbx_password.clone();
            let entry_name = if let Some(c) = conn {
                let entry_path =
                    rustconn_core::secret::KeePassHierarchy::build_entry_path(c, groups);
                let base_path = entry_path.strip_prefix("RustConn/").unwrap_or(&entry_path);
                format!("{base_path} ({protocol_str})")
            } else {
                format!("{conn_name} ({protocol_str})")
            };
            let username = username.to_string();
            let url = format!("{}://{}", protocol_str, conn_host);
            // Wrap intermediate plaintext copy in Zeroizing so it is
            // wiped from memory on drop (M-PUBLIC-DEBUG / SecretString).
            let pwd = zeroize::Zeroizing::new(password.expose_secret().to_string());

            crate::utils::spawn_blocking_with_callback(
                move || {
                    let kdbx = std::path::Path::new(&kdbx_path);
                    let key = key_file.as_ref().map(std::path::Path::new);
                    rustconn_core::secret::KeePassStatus::save_password_to_kdbx(
                        kdbx,
                        db_password.as_ref(),
                        key,
                        &entry_name,
                        &username,
                        pwd.as_str(),
                        Some(&url),
                    )
                },
                move |result| {
                    if let Err(e) = result {
                        tracing::error!("Failed to save password to vault: {e}");
                        show_vault_save_error(&e);
                    } else {
                        tracing::info!("Password saved to vault for connection {conn_id}");
                    }
                },
            );
        }
    } else {
        // Generic backend — dispatch via consolidated helper.
        // Use the same key format that the resolver expects for each backend,
        // so that store and resolve are consistent.
        let backend_type = select_backend_for_load(&settings.secrets);
        // For LibSecret, include group path to prevent name collisions (issue #264).
        // When a connection exists, always use the "RustConn/" prefix (matching
        // the resolver's generate_keyring_key_with_hierarchy); when no connection
        // is available (e.g. quick-connect), fall back to the flat legacy format.
        let group_path: Option<String> = conn.map(|c| {
            c.group_id
                .map(|gid| {
                    rustconn_core::secret::KeePassHierarchy::resolve_group_path(gid, groups)
                        .join("/")
                })
                .unwrap_or_default()
        });
        let lookup_key = generate_store_key_with_group(
            conn_name,
            conn_host,
            &protocol_str,
            backend_type,
            group_path.as_deref(),
        );
        tracing::debug!(
            %lookup_key,
            ?backend_type,
            conn_name,
            conn_host,
            protocol_str,
            "save_password_to_vault: storing with key"
        );
        let username = username.to_string();
        // Re-wrap into a fresh SecretString for the spawn_blocking move closure.
        let secret = password.clone();
        let secret_settings = settings.secrets.clone();

        crate::utils::spawn_blocking_with_callback(
            move || {
                let creds = rustconn_core::models::Credentials {
                    username: Some(username),
                    password: Some(secret),
                    key_passphrase: None,
                    domain: None,
                };
                store_reported_blocking(&secret_settings, &lookup_key, &creds)
            },
            move |result: Result<
                rustconn_core::secret::StoreOutcome,
                rustconn_core::error::SecretError,
            >| match result {
                Ok(rustconn_core::secret::StoreOutcome::Primary) => {
                    tracing::info!("Password saved to vault for connection {conn_id}");
                }
                Ok(rustconn_core::secret::StoreOutcome::Fallback { backend_id }) => {
                    tracing::warn!(
                        backend = %backend_id,
                        %conn_id,
                        "Password saved to encrypted-file fallback backend"
                    );
                    show_vault_fallback_toast();
                }
                Err(e) => {
                    tracing::error!("Failed to save password to vault: {e}");
                    show_vault_save_error(&e);
                }
            },
        );
    }
}

/// Saves a group password to the configured vault backend.
///
/// Password is taken as `&SecretString` so plaintext copies do not leak
/// via call-site `String`s.
pub fn save_group_password_to_vault(
    settings: &rustconn_core::config::AppSettings,
    group_path: &str,
    lookup_key: &str,
    username: &str,
    password: &secrecy::SecretString,
) {
    use secrecy::ExposeSecret;

    if settings.secrets.kdbx_enabled
        && matches!(
            settings.secrets.preferred_backend,
            rustconn_core::config::SecretBackendType::KeePassXc
                | rustconn_core::config::SecretBackendType::KdbxFile
        )
    {
        if let Some(kdbx_path) = settings.secrets.kdbx_path.clone() {
            let key_file = settings.secrets.kdbx_key_file.clone();
            let db_password = settings.secrets.kdbx_password.clone();
            let entry_name = group_path
                .strip_prefix("RustConn/")
                .unwrap_or(group_path)
                .to_string();
            let username_val = username.to_string();
            // Wrap intermediate plaintext copy in Zeroizing.
            let password_val = zeroize::Zeroizing::new(password.expose_secret().to_string());

            crate::utils::spawn_blocking_with_callback(
                move || {
                    let kdbx = std::path::Path::new(&kdbx_path);
                    let key = key_file.as_ref().map(std::path::Path::new);
                    rustconn_core::secret::KeePassStatus::save_password_to_kdbx(
                        kdbx,
                        db_password.as_ref(),
                        key,
                        &entry_name,
                        &username_val,
                        password_val.as_str(),
                        None,
                    )
                },
                move |result| {
                    if let Err(e) = result {
                        tracing::error!("Failed to save group password to vault: {e}");
                        show_vault_save_error(&e);
                    } else {
                        tracing::info!("Group password saved to vault");
                    }
                },
            );
        }
    } else {
        let lookup_key = lookup_key.to_string();
        let username_val = username.to_string();
        let secret = password.clone();
        let secret_settings = settings.secrets.clone();

        crate::utils::spawn_blocking_with_callback(
            move || {
                let creds = rustconn_core::models::Credentials {
                    username: Some(username_val),
                    password: Some(secret),
                    key_passphrase: None,
                    domain: None,
                };
                store_reported_blocking(&secret_settings, &lookup_key, &creds)
            },
            move |result: Result<
                rustconn_core::secret::StoreOutcome,
                rustconn_core::error::SecretError,
            >| match result {
                Ok(rustconn_core::secret::StoreOutcome::Primary) => {
                    tracing::info!("Group password saved to vault");
                }
                Ok(rustconn_core::secret::StoreOutcome::Fallback { backend_id }) => {
                    tracing::warn!(
                        backend = %backend_id,
                        "Group password saved to encrypted-file fallback backend"
                    );
                    show_vault_fallback_toast();
                }
                Err(e) => {
                    tracing::error!("Failed to save group password to vault: {e}");
                    show_vault_save_error(&e);
                }
            },
        );
    }
}

/// Renames a credential in the configured vault backend when a connection
/// is renamed.
///
/// Thin wrapper over [`migrate_vault_credential_for_edit`]; `protocol_str` is
/// accepted for call-site compatibility but the protocol is derived from
/// `updated_conn` so that a caller cannot pass one that disagrees with it.
///
/// # Errors
///
/// Returns a human-readable error string if the backend rejects the migration.
pub fn rename_vault_credential(
    settings: &rustconn_core::config::AppSettings,
    groups: &[rustconn_core::models::ConnectionGroup],
    updated_conn: &rustconn_core::models::Connection,
    old_name: &str,
    _protocol_str: &str,
) -> Result<(), String> {
    let mut old_conn = updated_conn.clone();
    old_conn.name = old_name.to_string();
    migrate_vault_credential_for_edit(settings, groups, groups, &old_conn, updated_conn)
}

/// Where a connection's credential currently lives and where it must end up.
///
/// `old_keys` is ordered most-current-format first; the migration walks it and
/// uses the first key that yields a credential, so entries written by earlier
/// releases are picked up rather than orphaned.
#[derive(Debug, PartialEq, Eq)]
struct VaultKeyMigration {
    old_keys: Vec<String>,
    new_key: String,
    is_keepass: bool,
}

/// Computes the key migration for a connection edit, or `None` when the edit
/// cannot have changed the lookup key.
///
/// Kept separate from the I/O so the key derivation — the part that has been
/// wrong in several different ways — is unit-testable without a live vault.
fn plan_vault_key_migration(
    settings: &rustconn_core::config::AppSettings,
    old_groups: &[rustconn_core::models::ConnectionGroup],
    new_groups: &[rustconn_core::models::ConnectionGroup],
    old_conn: &rustconn_core::models::Connection,
    new_conn: &rustconn_core::models::Connection,
) -> Option<VaultKeyMigration> {
    use rustconn_core::config::SecretBackendType;

    let old_protocol = old_conn
        .protocol_config
        .protocol_type()
        .as_str()
        .to_lowercase();
    let new_protocol = new_conn
        .protocol_config
        .protocol_type()
        .as_str()
        .to_lowercase();

    let is_keepass = settings.secrets.kdbx_enabled
        && matches!(
            settings.secrets.preferred_backend,
            SecretBackendType::KeePassXc | SecretBackendType::KdbxFile
        );

    let (old_keys, new_key) = if is_keepass {
        let old_base =
            rustconn_core::secret::KeePassHierarchy::build_entry_path(old_conn, old_groups);
        let new_base =
            rustconn_core::secret::KeePassHierarchy::build_entry_path(new_conn, new_groups);
        (
            vec![format!("{old_base} ({old_protocol})")],
            format!("{new_base} ({new_protocol})"),
        )
    } else {
        let backend_type = select_backend_for_load(&settings.secrets);
        let old_keys = vault_keys_for_connection(old_groups, old_conn, &old_protocol, backend_type);
        let new_key = vault_keys_for_connection(new_groups, new_conn, &new_protocol, backend_type)
            .into_iter()
            .next()?;
        (old_keys, new_key)
    };

    if old_keys.first() == Some(&new_key) {
        return None;
    }

    Some(VaultKeyMigration {
        old_keys,
        new_key,
        is_keepass,
    })
}

/// Returns whether an edit moved the connection's vault lookup key.
///
/// Lets a caller that has just written a freshly typed password under the new
/// key decide whether the entry under the previous key is now stale. Returns
/// `false` when the key is unchanged, where deleting the old entry would delete
/// the credential that was just saved.
#[must_use]
pub fn vault_key_changed_by_edit(
    settings: &rustconn_core::config::AppSettings,
    old_groups: &[rustconn_core::models::ConnectionGroup],
    new_groups: &[rustconn_core::models::ConnectionGroup],
    old_conn: &rustconn_core::models::Connection,
    new_conn: &rustconn_core::models::Connection,
) -> bool {
    plan_vault_key_migration(settings, old_groups, new_groups, old_conn, new_conn).is_some()
}

/// Migrates a connection's vault credential after an edit that changed its
/// lookup key.
///
/// Covers a rename, a move to another group, a protocol change, or any
/// combination of the three in a single save — the connection edit dialog can
/// change all three at once, and until 0.19.19 that path performed no migration
/// at all, so editing the name in the configuration panel silently stranded the
/// credential under the old key (issue #263).
///
/// `old_groups` and `new_groups` are separate so a group rename, which changes
/// the path without changing the connection, can reuse this. Pass the same slice
/// twice when the hierarchy is unchanged.
///
/// The `SecretBackend` trait has no rename operation, so for every backend
/// except KeePass the move is retrieve → store under the new key → delete the
/// old. The delete only runs once the store has succeeded, so a failure leaves
/// the credential readable under the old key rather than losing it.
///
/// # Errors
///
/// Returns a human-readable error string if the backend rejects the migration.
pub fn migrate_vault_credential_for_edit(
    settings: &rustconn_core::config::AppSettings,
    old_groups: &[rustconn_core::models::ConnectionGroup],
    new_groups: &[rustconn_core::models::ConnectionGroup],
    old_conn: &rustconn_core::models::Connection,
    new_conn: &rustconn_core::models::Connection,
) -> Result<(), String> {
    let Some(plan) = plan_vault_key_migration(settings, old_groups, new_groups, old_conn, new_conn)
    else {
        return Ok(());
    };

    if plan.is_keepass {
        let Some(kdbx_path) = settings.secrets.kdbx_path.as_ref() else {
            return Ok(());
        };
        let old_key = plan.old_keys.first().map_or("", String::as_str);
        tracing::info!(%old_key, new_key = %plan.new_key, "Migrating KeePass entry after edit");
        let key_file = settings.secrets.kdbx_key_file.clone();
        return rustconn_core::secret::KeePassStatus::rename_entry_in_kdbx(
            std::path::Path::new(kdbx_path),
            settings.secrets.kdbx_password.as_ref(),
            key_file.as_ref().map(std::path::Path::new),
            old_key,
            &plan.new_key,
        )
        .map_err(|e| format!("{e}"));
    }

    tracing::info!(new_key = %plan.new_key, "Migrating vault entry after edit");
    let secret_settings = settings.secrets.clone();
    for old_key in &plan.old_keys {
        if *old_key == plan.new_key {
            continue;
        }
        if let Ok(Some(creds)) = dispatch_vault_op(&secret_settings, old_key, VaultOp::Retrieve) {
            dispatch_vault_op(&secret_settings, &plan.new_key, VaultOp::Store(&creds))?;
            let _ = dispatch_vault_op(&secret_settings, old_key, VaultOp::Delete);
            return Ok(());
        }
    }
    Ok(())
}

/// Renames a vault credential when a connection is moved to a different group.
///
/// For KeePass backends, the entry path includes the group hierarchy, so moving
/// a connection changes the lookup key. This function renames the old entry to
/// the new path so the password remains accessible.
///
/// LibSecret and the macOS Keychain also embed the group path in their key
/// (since 0.19.18, issue #264), so they are migrated the same way — via
/// retrieve/store/delete, since the backend trait has no rename. Bitwarden,
/// 1Password, Passbolt, pass and the encrypted file use `rustconn/{name}`,
/// which a group move does not change, so they need no migration.
///
/// Thin wrapper over [`migrate_vault_credential_for_edit`]; `protocol_str` is
/// accepted for call-site compatibility but the protocol is derived from the
/// connections themselves.
///
/// # Errors
///
/// Returns a human-readable error string if the backend rejects the migration.
pub fn rename_vault_credential_for_move(
    settings: &rustconn_core::config::AppSettings,
    groups: &[rustconn_core::models::ConnectionGroup],
    old_conn: &rustconn_core::models::Connection,
    new_conn: &rustconn_core::models::Connection,
    _protocol_str: &str,
) -> Result<(), String> {
    migrate_vault_credential_for_edit(settings, groups, groups, old_conn, new_conn)
}

/// Migrates all KeePass vault entries affected by a group rename or move.
///
/// When a group is renamed or moved to a different parent, the hierarchical
/// KeePass entry paths change for:
/// 1. The group's own credential (if `password_source == Vault`)
/// 2. All connections in the group (and descendant groups) with `password_source == Vault`
///
/// LibSecret and the macOS Keychain embed the group path in the connection key
/// too (since 0.19.18, issue #264), so their connection entries are migrated as
/// well. Their group credentials are keyed by group UUID, which a rename does
/// not change. Bitwarden, 1Password, Passbolt, pass and the encrypted file use
/// flat `rustconn/{name}` keys and need no migration.
pub fn migrate_vault_entries_on_group_change(
    settings: &rustconn_core::config::AppSettings,
    old_groups: &[rustconn_core::models::ConnectionGroup],
    new_groups: &[rustconn_core::models::ConnectionGroup],
    connections: &[rustconn_core::models::Connection],
    changed_group_id: uuid::Uuid,
) {
    use rustconn_core::config::SecretBackendType;

    let is_keepass = settings.secrets.kdbx_enabled
        && matches!(
            settings.secrets.preferred_backend,
            SecretBackendType::KeePassXc | SecretBackendType::KdbxFile
        );

    if is_keepass {
        let Some(kdbx_path) = settings.secrets.kdbx_path.clone() else {
            return;
        };
        migrate_keepass_entries_on_group_change(
            settings,
            old_groups,
            new_groups,
            connections,
            changed_group_id,
            kdbx_path,
        );
        return;
    }

    let backend_type = select_backend_for_load(&settings.secrets);
    if matches!(
        backend_type,
        SecretBackendType::LibSecret | SecretBackendType::MacOsKeychain
    ) {
        migrate_keyring_entries_on_group_change(
            settings,
            old_groups,
            new_groups,
            connections,
            changed_group_id,
            backend_type,
        );
    }
}

/// Migrates keyring connection entries whose key embeds a renamed group path.
///
/// The `SecretBackend` trait has no rename, so each entry is moved by
/// retrieve → store under the new key → delete the old key. Group credentials
/// are keyed by group UUID and are deliberately left alone.
fn migrate_keyring_entries_on_group_change(
    settings: &rustconn_core::config::AppSettings,
    old_groups: &[rustconn_core::models::ConnectionGroup],
    new_groups: &[rustconn_core::models::ConnectionGroup],
    connections: &[rustconn_core::models::Connection],
    changed_group_id: uuid::Uuid,
    backend_type: rustconn_core::config::SecretBackendType,
) {
    let affected_group_ids =
        rustconn_core::models::collect_descendant_group_ids(changed_group_id, new_groups);

    let mut moves: Vec<(Vec<String>, String)> = Vec::new();
    for conn in connections {
        if conn.password_source != rustconn_core::models::PasswordSource::Vault {
            continue;
        }
        let Some(group_id) = conn.group_id else {
            continue;
        };
        if !affected_group_ids.contains(&group_id) {
            continue;
        }

        let protocol_str = conn.protocol_config.protocol_type().as_str().to_lowercase();
        let old_keys = vault_keys_for_connection(old_groups, conn, &protocol_str, backend_type);
        let new_keys = vault_keys_for_connection(new_groups, conn, &protocol_str, backend_type);
        let Some(new_key) = new_keys.first().cloned() else {
            continue;
        };
        if old_keys.first() == Some(&new_key) {
            continue;
        }
        moves.push((old_keys, new_key));
    }

    if moves.is_empty() {
        return;
    }

    let secret_settings = settings.secrets.clone();
    crate::utils::spawn_blocking_with_callback(
        move || {
            for (old_keys, new_key) in &moves {
                tracing::info!(%new_key, "Migrating keyring entry after group change");
                for old_key in old_keys {
                    if old_key == new_key {
                        continue;
                    }
                    if let Ok(Some(creds)) =
                        dispatch_vault_op(&secret_settings, old_key, VaultOp::Retrieve)
                    {
                        if let Err(e) =
                            dispatch_vault_op(&secret_settings, new_key, VaultOp::Store(&creds))
                        {
                            tracing::error!(
                                %new_key,
                                error = %e,
                                "Failed to store keyring entry under new group path"
                            );
                        } else {
                            let _ = dispatch_vault_op(&secret_settings, old_key, VaultOp::Delete);
                        }
                        break;
                    }
                }
            }
            Ok::<(), String>(())
        },
        |result: Result<(), String>| {
            if let Err(e) = result {
                tracing::error!(error = %e, "Failed to migrate keyring entries after group change");
            }
        },
    );
}

/// Migrates KDBX entry paths for a renamed or re-parented group.
fn migrate_keepass_entries_on_group_change(
    settings: &rustconn_core::config::AppSettings,
    old_groups: &[rustconn_core::models::ConnectionGroup],
    new_groups: &[rustconn_core::models::ConnectionGroup],
    connections: &[rustconn_core::models::Connection],
    changed_group_id: uuid::Uuid,
    kdbx_path: std::path::PathBuf,
) {
    // Collect all group IDs in the subtree rooted at changed_group_id
    let affected_group_ids =
        rustconn_core::models::collect_descendant_group_ids(changed_group_id, new_groups);

    // Build rename pairs: (old_key, new_key)
    let mut rename_pairs: Vec<(String, String)> = Vec::new();

    // 1. Migrate group credentials
    for &gid in &affected_group_ids {
        let old_group = old_groups.iter().find(|g| g.id == gid);
        let new_group = new_groups.iter().find(|g| g.id == gid);
        if let (Some(og), Some(ng)) = (old_group, new_group)
            && ng.password_source == Some(rustconn_core::models::PasswordSource::Vault)
        {
            let old_path =
                rustconn_core::secret::KeePassHierarchy::build_group_entry_path(og, old_groups);
            let new_path =
                rustconn_core::secret::KeePassHierarchy::build_group_entry_path(ng, new_groups);
            if old_path != new_path {
                rename_pairs.push((old_path, new_path));
            }
        }
    }

    // 2. Migrate connection credentials
    for conn in connections {
        if conn.password_source != rustconn_core::models::PasswordSource::Vault {
            continue;
        }
        let Some(group_id) = conn.group_id else {
            continue;
        };
        if !affected_group_ids.contains(&group_id) {
            continue;
        }

        let old_path = rustconn_core::secret::KeePassHierarchy::build_entry_path(conn, old_groups);
        let new_path = rustconn_core::secret::KeePassHierarchy::build_entry_path(conn, new_groups);

        if old_path != new_path {
            let protocol_str = conn.protocol_config.protocol_type().as_str().to_lowercase();
            let old_key = format!("{old_path} ({protocol_str})");
            let new_key = format!("{new_path} ({protocol_str})");
            rename_pairs.push((old_key, new_key));
        }
    }

    if rename_pairs.is_empty() {
        return;
    }

    let key_file = settings.secrets.kdbx_key_file.clone();
    let db_password = settings.secrets.kdbx_password.clone();

    crate::utils::spawn_blocking_with_callback(
        move || {
            let kdbx = std::path::Path::new(&kdbx_path);
            let key = key_file.as_ref().map(std::path::Path::new);
            let mut errors = Vec::new();

            for (old_key, new_key) in &rename_pairs {
                tracing::info!(%old_key, %new_key, "Migrating KeePass entry after group change");
                if let Err(e) = rustconn_core::secret::KeePassStatus::rename_entry_in_kdbx(
                    kdbx,
                    db_password.as_ref(),
                    key,
                    old_key,
                    new_key,
                ) {
                    errors.push(format!("{old_key} → {new_key}: {e}"));
                }
            }

            if errors.is_empty() {
                Ok(())
            } else {
                Err(errors.join("; "))
            }
        },
        |result| {
            if let Err(e) = result {
                tracing::error!(error = %e, "Failed to migrate vault entries after group change");
            }
        },
    );
}

/// Saves a secret variable value to the configured vault backend.
///
/// Respects `preferred_backend` from secret settings, using the same
/// backend selection logic as connection passwords. Password is taken
/// as `&SecretString` so plaintext copies do not leak via call-site
/// `String`s.
///
/// # Errors
///
/// Returns an error string if the configured backend is unreachable, the
/// KeePass database cannot be written to, or no fallback backend is
/// available when the primary backend fails.
pub fn save_variable_to_vault(
    settings: &rustconn_core::config::SecretSettings,
    var_name: &str,
    password: &secrecy::SecretString,
) -> Result<(), String> {
    use rustconn_core::config::SecretBackendType;
    use secrecy::ExposeSecret;

    let lookup_key = rustconn_core::variable_secret_key(var_name);
    let backend_type = select_backend_for_load(settings);

    tracing::debug!(?backend_type, var_name, "Saving secret variable to vault");

    let creds = rustconn_core::models::Credentials {
        username: None,
        password: Some(password.clone()),
        key_passphrase: None,
        domain: None,
    };

    match backend_type {
        SecretBackendType::KdbxFile | SecretBackendType::KeePassXc => {
            if let Some(kdbx_path) = settings.kdbx_path.as_ref() {
                let key_file = settings.kdbx_key_file.clone();
                let kdbx = std::path::Path::new(kdbx_path);
                let key = key_file.as_ref().map(std::path::Path::new);
                // Wrap intermediate plaintext copy in Zeroizing for the FFI call.
                let pwd = zeroize::Zeroizing::new(password.expose_secret().to_string());
                let result = rustconn_core::secret::KeePassStatus::save_password_to_kdbx(
                    kdbx,
                    settings.kdbx_password.as_ref(),
                    key,
                    &lookup_key,
                    "",
                    pwd.as_str(),
                    None,
                )
                .map_err(|e| format!("{e}"));

                // If KeePass save failed and fallback is enabled, try LibSecret
                if result.is_err() && settings.enable_fallback {
                    tracing::info!(var_name, "KeePass save failed, falling back to LibSecret");
                    dispatch_vault_op(settings, &lookup_key, VaultOp::Store(&creds))?;
                    Ok(())
                } else {
                    result
                }
            } else if settings.enable_fallback {
                tracing::info!(
                    var_name,
                    "KeePass not configured, falling back to LibSecret"
                );
                dispatch_vault_op(settings, &lookup_key, VaultOp::Store(&creds))?;
                Ok(())
            } else {
                Err("KeePass enabled but no database file configured".to_string())
            }
        }
        _ => {
            dispatch_vault_op(settings, &lookup_key, VaultOp::Store(&creds))?;
            Ok(())
        }
    }
}

/// Loads a secret variable value from the configured vault backend,
/// optionally using a custom KeePass entry path or vault entry name.
///
/// When `kdbx_entry_path` is `Some(path)`, the KeePass backend looks up
/// the entry at that exact path (the function prepends `RustConn/` prefix
/// is NOT added — the path is used as-is for direct entry lookup).
/// This allows referencing existing entries in the user's KeePass database.
///
/// When `vault_entry_name` is `Some(name)`, non-KeePass backends
/// (Bitwarden, 1Password, Passbolt, Pass) search for an existing entry
/// by exact name instead of the default `rustconn/var/{name}` key.
/// This allows reusing credentials already stored in the vault.
pub fn load_variable_from_vault_with_path(
    settings: &rustconn_core::config::SecretSettings,
    var_name: &str,
    kdbx_entry_path: Option<&str>,
    vault_entry_name: Option<&str>,
) -> Result<Option<zeroize::Zeroizing<String>>, String> {
    use rustconn_core::config::SecretBackendType;
    use secrecy::ExposeSecret;

    let default_key = rustconn_core::variable_secret_key(var_name);
    // Filter out empty/whitespace-only custom paths — treat them as "no custom path".
    let effective_custom_path = kdbx_entry_path.filter(|p| !p.trim().is_empty());
    let lookup_key = effective_custom_path.unwrap_or(&default_key);
    let backend_type = select_backend_for_load(settings);

    tracing::debug!(
        ?backend_type,
        var_name,
        lookup_key,
        "Loading secret variable from vault"
    );

    match backend_type {
        SecretBackendType::KdbxFile | SecretBackendType::KeePassXc => {
            if let Some(kdbx_path) = settings.kdbx_path.as_ref() {
                let key_file = settings.kdbx_key_file.clone();
                let kdbx = std::path::Path::new(kdbx_path);
                let key = key_file.as_ref().map(std::path::Path::new);

                // Custom path → exact lookup (no RustConn/ prefix, no fallbacks)
                // Default path → standard lookup with RustConn/ prefix and fallbacks
                let kdbx_result = if effective_custom_path.is_some() {
                    rustconn_core::secret::KeePassStatus::get_password_from_kdbx_exact(
                        kdbx,
                        settings.kdbx_password.as_ref(),
                        key,
                        lookup_key,
                    )
                } else {
                    rustconn_core::secret::KeePassStatus::get_password_from_kdbx_with_key(
                        kdbx,
                        settings.kdbx_password.as_ref(),
                        key,
                        lookup_key,
                        None,
                    )
                }
                .map(|opt| opt.map(|s| zeroize::Zeroizing::new(s.expose_secret().to_string())))
                .map_err(|e| format!("{e}"));

                // If KeePass returned Ok(None) or Err and fallback is enabled,
                // try LibSecret as a fallback (the variable may have been saved
                // there via the "Variable Not Configured" dialog).
                match &kdbx_result {
                    Ok(Some(_)) => kdbx_result,
                    Ok(None) | Err(_) if settings.enable_fallback => {
                        tracing::debug!(
                            var_name,
                            "KeePass lookup returned nothing, trying LibSecret fallback"
                        );
                        let fallback = dispatch_vault_op(settings, &default_key, VaultOp::Retrieve);
                        match fallback {
                            Ok(Some(creds)) if creds.expose_password().is_some() => Ok(creds
                                .expose_password()
                                .map(|p| zeroize::Zeroizing::new(p.to_string()))),
                            _ => kdbx_result,
                        }
                    }
                    _ => kdbx_result,
                }
            } else {
                Err("KeePass enabled but no database file configured".to_string())
            }
        }
        _ => {
            // For non-KeePass backends: if vault_entry_name is set, search by
            // exact name in the vault (Bitwarden, 1Password, etc.) instead of
            // the default rustconn/var/{name} key.
            let effective_entry_name = vault_entry_name.filter(|n| !n.trim().is_empty());

            if let Some(entry_name) = effective_entry_name {
                // Direct lookup by exact vault entry name
                retrieve_by_vault_entry_name(settings, entry_name)
            } else {
                let creds = dispatch_vault_op(settings, &default_key, VaultOp::Retrieve)?;
                Ok(creds.and_then(|c| {
                    c.expose_password()
                        .map(|p| zeroize::Zeroizing::new(p.to_string()))
                }))
            }
        }
    }
}

/// Retrieves a password from a vault entry matched by exact name.
///
/// Used when a variable has a custom `vault_entry_name` — searches
/// for an existing entry in Bitwarden/1Password/Passbolt/Pass by
/// its exact name (without `RustConn:` prefix or `rustconn/var/` key).
///
/// # Errors
/// Returns an error string if vault operations fail or time out.
fn retrieve_by_vault_entry_name(
    settings: &rustconn_core::config::SecretSettings,
    entry_name: &str,
) -> Result<Option<zeroize::Zeroizing<String>>, String> {
    use rustconn_core::config::SecretBackendType;
    use rustconn_core::secret::SecretBackend;
    use secrecy::ExposeSecret;

    let backend_type = select_backend_for_load(settings);

    crate::async_utils::with_runtime(|rt| {
        rt.block_on(async {
            tokio::time::timeout(std::time::Duration::from_secs(10), async {
                match backend_type {
                    SecretBackendType::Bitwarden => {
                        let bw = rustconn_core::secret::auto_unlock(settings)
                            .await
                            .map_err(|e| format!("{e}"))?;
                        let password = bw
                            .find_password_by_exact_name(entry_name)
                            .await
                            .map_err(|e| format!("{e}"))?;
                        Ok(
                            password
                                .map(|p| zeroize::Zeroizing::new(p.expose_secret().to_string())),
                        )
                    }
                    SecretBackendType::OnePassword => {
                        // 1Password: use `op item get "{name}" --fields password`
                        let mut backend = rustconn_core::secret::OnePasswordBackend::new();
                        if let Some(ref token) = settings.onepassword_service_account_token {
                            backend.set_service_account_token(token.clone());
                        }
                        let creds = backend
                            .retrieve(entry_name)
                            .await
                            .map_err(|e| format!("{e}"))?;
                        Ok(creds.and_then(|c| {
                            c.expose_password()
                                .map(|p| zeroize::Zeroizing::new(p.to_string()))
                        }))
                    }
                    SecretBackendType::Pass => {
                        // Pass: entry_name is the pass path (e.g. "work/ad-creds")
                        let backend =
                            rustconn_core::secret::PassBackend::from_secret_settings(settings);
                        let creds = backend
                            .retrieve(entry_name)
                            .await
                            .map_err(|e| format!("{e}"))?;
                        Ok(creds.and_then(|c| {
                            c.expose_password()
                                .map(|p| zeroize::Zeroizing::new(p.to_string()))
                        }))
                    }
                    SecretBackendType::Passbolt => {
                        let mut backend = rustconn_core::secret::PassboltBackend::new();
                        if let Some(ref url) = settings.passbolt_server_url {
                            backend = backend.with_server_address(url.clone());
                        }
                        if let Some(ref passphrase) = settings.passbolt_passphrase {
                            backend = backend.with_user_password(passphrase.clone());
                        }
                        let creds = backend
                            .retrieve(entry_name)
                            .await
                            .map_err(|e| format!("{e}"))?;
                        Ok(creds.and_then(|c| {
                            c.expose_password()
                                .map(|p| zeroize::Zeroizing::new(p.to_string()))
                        }))
                    }
                    #[cfg(target_os = "macos")]
                    SecretBackendType::MacOsKeychain => {
                        let backend = rustconn_core::secret::MacOsKeychainBackend::new();
                        let creds = backend
                            .retrieve(entry_name)
                            .await
                            .map_err(|e| format!("{e}"))?;
                        Ok(creds.and_then(|c| {
                            c.expose_password()
                                .map(|p| zeroize::Zeroizing::new(p.to_string()))
                        }))
                    }
                    SecretBackendType::EncryptedFile => {
                        // Application-managed encrypted file: retrieve by the
                        // flat entry name (same key scheme as the other
                        // app-managed backends).
                        let backend = rustconn_core::secret::EncryptedFileBackend::new();
                        let creds = backend
                            .retrieve(entry_name)
                            .await
                            .map_err(|e| format!("{e}"))?;
                        Ok(creds.and_then(|c| {
                            c.expose_password()
                                .map(|p| zeroize::Zeroizing::new(p.to_string()))
                        }))
                    }
                    SecretBackendType::PortableEncryptedFile => {
                        // Passphrase-based portable file: needs the session
                        // passphrase from settings to unlock.
                        let backend = portable_backend_from_settings(settings);
                        let creds = backend
                            .retrieve(entry_name)
                            .await
                            .map_err(|e| format!("{e}"))?;
                        Ok(creds.and_then(|c| {
                            c.expose_password()
                                .map(|p| zeroize::Zeroizing::new(p.to_string()))
                        }))
                    }
                    _ => {
                        // System keyring — lookup by entry_name as attribute.
                        // macOS uses the Keychain; LibSecretBackend (oo7) is
                        // not compiled there (R10.1, R10.2).
                        #[cfg(target_os = "macos")]
                        let backend = rustconn_core::secret::MacOsKeychainBackend::new();
                        #[cfg(not(target_os = "macos"))]
                        let backend = rustconn_core::secret::LibSecretBackend::new("rustconn");
                        let creds = backend
                            .retrieve(entry_name)
                            .await
                            .map_err(|e| format!("{e}"))?;
                        Ok(creds.and_then(|c| {
                            c.expose_password()
                                .map(|p| zeroize::Zeroizing::new(p.to_string()))
                        }))
                    }
                }
            })
            .await
            .map_err(|_| "Vault retrieve by entry name timed out after 10s".to_string())?
        })
    })?
}

/// Returns global variables with secret values restored from vault.
///
/// Non-secret variables are returned as-is. Secret variables with empty
/// values have their values loaded from the configured vault backend.
/// Vault load failures are logged but do not prevent other variables
/// from being returned.
///
/// When a variable has a custom `kdbx_entry_path`, that path is used
/// for KeePass lookup instead of the default `rustconn/var/{name}`.
pub fn resolve_global_variables(
    settings: &rustconn_core::config::AppSettings,
) -> Vec<rustconn_core::Variable> {
    use zeroize::Zeroize;

    let mut vars = settings.global_variables.clone();
    for var in &mut vars {
        if var.is_secret && var.value.is_empty() {
            match load_variable_from_vault_with_path(
                &settings.secrets,
                &var.name,
                var.kdbx_entry_path.as_deref(),
                var.vault_entry_name.as_deref(),
            ) {
                Ok(Some(pwd)) => {
                    // Zeroize the previous value before overwriting — clone_from
                    // frees the old buffer via the allocator (no zeroize), so a
                    // stale secret could linger in freed heap memory.
                    var.value.zeroize();
                    var.value.clone_from(&pwd);
                }
                Ok(None) => {
                    tracing::debug!(var_name = %var.name, "No secret found in vault for variable");
                }
                Err(e) => {
                    tracing::warn!(var_name = %var.name, error = %e, "Failed to load secret variable from vault");
                }
            }
        }
    }
    vars
}

/// Builds every vault lookup key a connection's credentials may live under on a
/// non-KeePass backend, most-current format first.
///
/// The first entry is the key the current code stores under; the rest are
/// formats written by earlier releases, kept so that cleanup and fallback
/// retrieval still find them.
///
/// For LibSecret and the macOS Keychain the current key embeds the group path
/// (`RustConn/{group}/{name} ({protocol})`) so same-named connections in
/// different groups no longer collide (issue #264). Deleting by the flat
/// `{name} ({protocol})` key alone therefore matched nothing and silently left
/// the keyring item behind (issue #263) — the attribute search in
/// `LibSecretBackend::delete_value` finds zero items and reports success.
///
/// The legacy name-keyed formats are deliberately included even though two
/// same-named connections in different groups map onto the same legacy key, so
/// cleaning up one can remove an entry the other still resolves through. That is
/// acceptable because a shared legacy key means the two connections were already
/// sharing a single stored password — which is precisely the collision reported
/// in issue #264 — so there is no distinct credential to lose. Once either
/// connection has been resolved on 0.19.18 or later, its credential lives under
/// its own group-scoped key and is unaffected.
fn vault_keys_for_connection(
    groups: &[rustconn_core::models::ConnectionGroup],
    connection: &rustconn_core::models::Connection,
    protocol_str: &str,
    backend_type: rustconn_core::config::SecretBackendType,
) -> Vec<String> {
    use rustconn_core::config::SecretBackendType;

    let mut keys = Vec::new();

    if matches!(
        backend_type,
        SecretBackendType::LibSecret | SecretBackendType::MacOsKeychain
    ) {
        // Always `Some`, empty for an ungrouped connection — this mirrors
        // `save_password_to_vault`, which passes `Some("")` in that case so the
        // key still carries the `RustConn/` prefix. Passing `None` here instead
        // would yield the bare legacy key and miss the real entry.
        let group_path = Some(
            connection
                .group_id
                .map(|gid| {
                    rustconn_core::secret::KeePassHierarchy::resolve_group_path(gid, groups)
                        .join("/")
                })
                .unwrap_or_default(),
        );
        // Primary: exactly what `save_password_to_vault` writes.
        keys.push(generate_store_key_with_group(
            &connection.name,
            &connection.host,
            protocol_str,
            backend_type,
            group_path.as_deref(),
        ));
        // The resolver builds the same shape but additionally runs the name
        // through `sanitize_imported_value`, so an imported name with trailing
        // escape sequences can land under a second key. Read-time migration
        // writes that variant, so cleanup has to cover it too.
        keys.push(
            rustconn_core::secret::CredentialResolver::generate_keyring_key_with_hierarchy(
                connection, groups,
            ),
        );
        // Pre-0.19.18 flat key, before the group path was part of the key.
        keys.push(rustconn_core::secret::CredentialResolver::generate_keyring_key(connection));
        // Pre-0.19.19 macOS Keychain key: the store path wrote the flat
        // `rustconn/{name}` format while the resolver looked for the
        // hierarchical one, so entries can still exist under it.
        let identifier = if connection.name.trim().is_empty() {
            &connection.host
        } else {
            &connection.name
        };
        keys.push(format!("rustconn/{identifier}"));
    } else {
        keys.push(generate_store_key(
            &connection.name,
            &connection.host,
            protocol_str,
            backend_type,
        ));
    }

    // Oldest format: the connection UUID.
    keys.push(connection.id.to_string());

    keys.dedup();
    keys
}

/// Deletes a connection's vault credentials from the configured backend.
///
/// For KeePass backends, deletes the hierarchical entry. For flat backends,
/// every key format from [`vault_keys_for_connection`] is deleted so that
/// entries written by earlier releases do not survive the connection.
///
/// This runs on permanent deletion — either when the undo window for a deleted
/// connection closes, or when the trash is emptied — never on the soft-delete
/// itself, so that Undo restores a connection with its password intact.
pub fn delete_vault_credential(
    settings: &rustconn_core::config::AppSettings,
    groups: &[rustconn_core::models::ConnectionGroup],
    connection: &rustconn_core::models::Connection,
) -> Result<(), String> {
    use rustconn_core::config::SecretBackendType;

    let protocol_str = connection
        .protocol_config
        .protocol_type()
        .as_str()
        .to_lowercase();
    let backend_type = select_backend_for_load(&settings.secrets);

    tracing::debug!(
        ?backend_type,
        connection_name = %connection.name,
        protocol = %protocol_str,
        "Deleting vault credential for connection"
    );

    match backend_type {
        SecretBackendType::KdbxFile | SecretBackendType::KeePassXc => {
            if let Some(kdbx_path) = settings.secrets.kdbx_path.as_ref() {
                // Full hierarchical path incl. the "RustConn/" prefix and the
                // protocol suffix, e.g. "RustConn/Group/Name (rdp)" — the format
                // `delete_entry_from_kdbx` (and the CLI) expect.
                let entry_path =
                    rustconn_core::secret::KeePassHierarchy::build_entry_path(connection, groups);
                let full_entry_path = format!("{entry_path} ({protocol_str})");
                let key_file = settings.secrets.kdbx_key_file.clone();
                let kdbx = std::path::Path::new(kdbx_path);
                let key = key_file.as_ref().map(std::path::Path::new);
                // Actually remove the entry via keepassxc-cli (previously this
                // only overwrote it with an empty password as a best-effort,
                // leaving the entry behind in the database).
                rustconn_core::secret::KeePassStatus::delete_entry_from_kdbx(
                    kdbx,
                    settings.secrets.kdbx_password.as_ref(),
                    key,
                    &full_entry_path,
                )
                .map_err(|e| format!("{e}"))
            } else {
                Ok(()) // No KDBX configured, nothing to clean
            }
        }
        _ => {
            // Delete every key format this connection may have been stored
            // under. Best-effort per key: a backend that does not hold one of
            // the legacy keys must not abort cleanup of the others, but a
            // failure on the primary key is still reported.
            let keys = vault_keys_for_connection(groups, connection, &protocol_str, backend_type);
            let mut primary_result = Ok(());
            for (index, key) in keys.iter().enumerate() {
                let result = dispatch_vault_op(&settings.secrets, key, VaultOp::Delete).map(|_| ());
                if let Err(ref e) = result {
                    tracing::debug!(
                        lookup_key = %key,
                        error = %e,
                        "Vault credential delete failed for one key format"
                    );
                }
                if index == 0 {
                    primary_result = result;
                }
            }
            primary_result
        }
    }
}

/// Deletes a group's vault credentials from the configured backend.
///
/// Similar to [`delete_vault_credential`] but for group-level passwords.
pub fn delete_group_vault_credential(
    settings: &rustconn_core::config::AppSettings,
    groups: &[rustconn_core::models::ConnectionGroup],
    group: &rustconn_core::models::ConnectionGroup,
) -> Result<(), String> {
    use rustconn_core::config::SecretBackendType;

    let backend_type = select_backend_for_load(&settings.secrets);

    tracing::debug!(
        ?backend_type,
        group_name = %group.name,
        "Deleting vault credential for group"
    );

    match backend_type {
        SecretBackendType::KdbxFile | SecretBackendType::KeePassXc => {
            if let Some(kdbx_path) = settings.secrets.kdbx_path.as_ref() {
                let group_path =
                    rustconn_core::secret::KeePassHierarchy::build_group_entry_path(group, groups);
                let key_file = settings.secrets.kdbx_key_file.clone();
                let kdbx = std::path::Path::new(kdbx_path);
                let key = key_file.as_ref().map(std::path::Path::new);
                // Actually remove the entry. Overwriting it with an empty
                // username/password (the previous behaviour) left a visible
                // orphan entry in the user's database after the group was gone.
                rustconn_core::secret::KeePassStatus::delete_entry_from_kdbx(
                    kdbx,
                    settings.secrets.kdbx_password.as_ref(),
                    key,
                    &group_path,
                )
                .map_err(|e| format!("{e}"))
            } else {
                Ok(())
            }
        }
        _ => {
            let lookup_key = group.id.to_string();
            dispatch_vault_op(&settings.secrets, &lookup_key, VaultOp::Delete)?;
            Ok(())
        }
    }
}

/// Copies vault credentials from one connection to another.
///
/// Retrieves credentials under the old connection's key and stores them
/// under the new connection's key. Used during clipboard paste to duplicate
/// credentials for the copied connection.
pub fn copy_vault_credential(
    settings: &rustconn_core::config::AppSettings,
    groups: &[rustconn_core::models::ConnectionGroup],
    old_conn: &rustconn_core::models::Connection,
    new_conn: &rustconn_core::models::Connection,
) -> Result<(), String> {
    use rustconn_core::config::SecretBackendType;

    let protocol_str = old_conn
        .protocol_config
        .protocol_type()
        .as_str()
        .to_lowercase();
    let backend_type = select_backend_for_load(&settings.secrets);

    tracing::debug!(
        ?backend_type,
        old_name = %old_conn.name,
        new_name = %new_conn.name,
        "Copying vault credential for pasted connection"
    );

    match backend_type {
        SecretBackendType::KdbxFile | SecretBackendType::KeePassXc => {
            if let Some(kdbx_path) = settings.secrets.kdbx_path.as_ref() {
                let key_file = settings.secrets.kdbx_key_file.clone();
                let kdbx = std::path::Path::new(kdbx_path);
                let key = key_file.as_ref().map(std::path::Path::new);

                // Read from old entry
                let old_entry_path =
                    rustconn_core::secret::KeePassHierarchy::build_entry_path(old_conn, groups);
                let old_base = old_entry_path
                    .strip_prefix("RustConn/")
                    .unwrap_or(&old_entry_path);
                let old_entry_name = format!("{old_base} ({protocol_str})");

                let password_opt =
                    rustconn_core::secret::KeePassStatus::get_password_from_kdbx_with_key(
                        kdbx,
                        settings.secrets.kdbx_password.as_ref(),
                        key,
                        &old_entry_name,
                        None,
                    )
                    .map_err(|e| format!("{e}"))?;

                if let Some(pwd) = password_opt {
                    use secrecy::ExposeSecret;
                    // Write to new entry
                    let new_entry_path =
                        rustconn_core::secret::KeePassHierarchy::build_entry_path(new_conn, groups);
                    let new_base = new_entry_path
                        .strip_prefix("RustConn/")
                        .unwrap_or(&new_entry_path);
                    let new_entry_name = format!("{new_base} ({protocol_str})");
                    let username = new_conn.username.as_deref().unwrap_or("");
                    let url = format!("{}://{}", protocol_str, new_conn.host);
                    rustconn_core::secret::KeePassStatus::save_password_to_kdbx(
                        kdbx,
                        settings.secrets.kdbx_password.as_ref(),
                        key,
                        &new_entry_name,
                        username,
                        pwd.expose_secret(),
                        Some(&url),
                    )
                    .map_err(|e| format!("{e}"))?;
                }
                Ok(())
            } else {
                Ok(())
            }
        }
        _ => {
            // Both keys must account for the group path on LibSecret/Keychain,
            // otherwise a paste into another group reads nothing and writes the
            // copy where the resolver will never look for it (issue #264).
            let old_keys = vault_keys_for_connection(groups, old_conn, &protocol_str, backend_type);
            let new_keys = vault_keys_for_connection(groups, new_conn, &protocol_str, backend_type);

            for old_key in &old_keys {
                if let Some(creds) =
                    dispatch_vault_op(&settings.secrets, old_key, VaultOp::Retrieve)?
                {
                    let new_key = new_keys.first().map_or(old_key.as_str(), String::as_str);
                    dispatch_vault_op(&settings.secrets, new_key, VaultOp::Store(&creds))?;
                    break;
                }
            }
            Ok(())
        }
    }
}

/// Operation to perform on a vault backend.
///
/// Used by [`dispatch_vault_op`] to consolidate the repeated
/// `match backend_type { … }` dispatch blocks throughout this module.
pub enum VaultOp<'a> {
    /// Store credentials under the given key.
    Store(&'a rustconn_core::models::Credentials),
    /// Retrieve credentials for the given key.
    Retrieve,
    /// Delete credentials for the given key.
    Delete,
}

/// Dispatches a single vault operation to the configured non-KeePass backend.
///
/// This helper eliminates the repeated `match backend_type` blocks that were
/// duplicated across `save_password_to_vault`, `save_group_password_to_vault`,
/// `rename_vault_credential`, `resolve_credentials_blocking` (Inherit branch),
/// and credential cleanup on delete.
///
/// For KeePass backends, callers must handle KDBX operations directly because
/// they use a different API (`save_password_to_kdbx` / `get_password_from_kdbx`).
///
/// # Errors
///
/// Returns a human-readable error string if the backend is unavailable or the
/// operation fails.
///
/// # See also
///
/// - [`CredentialResolver::resolve_inherited_credentials`] — async equivalent
///   in `rustconn-core`
pub fn dispatch_vault_op(
    secret_settings: &rustconn_core::config::SecretSettings,
    lookup_key: &str,
    op: VaultOp<'_>,
) -> Result<Option<rustconn_core::models::Credentials>, String> {
    dispatch_vault_op_for(
        secret_settings,
        select_backend_for_load(secret_settings),
        lookup_key,
        op,
    )
}

/// Constructs one backend of the named type from `secret_settings`.
///
/// Split out of [`dispatch_vault_op_for`] so a caller that performs many
/// operations against the same backend can build it **once**. That is not a
/// micro-optimisation for the portable file: its `store` derives the data key
/// from the passphrase with Argon2id, and the derivation is cached *per backend
/// instance*, so a fresh instance per credential means a full ~0.5 s KDF pass
/// each time. Its `write_lock` is per instance too, so separate instances
/// serialise nothing against each other.
///
/// `KeePassXc` / `KdbxFile` deliberately resolve to the system keyring here,
/// because KDBX proper is not a [`SecretBackend`] — it goes through
/// `KeePassStatus` and the `keepassxc-cli` binary. Callers that mean the database
/// itself must intercept those two variants before calling this.
///
/// # Errors
///
/// Returns a human-readable error when the backend cannot be constructed, which
/// today only happens for Bitwarden, whose construction includes an unlock.
fn build_single_backend(
    secret_settings: &rustconn_core::config::SecretSettings,
    backend_type: rustconn_core::config::SecretBackendType,
    rt: &tokio::runtime::Runtime,
) -> Result<std::sync::Arc<dyn rustconn_core::secret::SecretBackend>, String> {
    use rustconn_core::config::SecretBackendType;
    use rustconn_core::secret::SecretBackend;

    let backend: std::sync::Arc<dyn SecretBackend> = match backend_type {
        SecretBackendType::Bitwarden => std::sync::Arc::new(rt.block_on(async {
            tokio::time::timeout(
                std::time::Duration::from_secs(30),
                rustconn_core::secret::auto_unlock(secret_settings),
            )
            .await
            .map_err(|_| "Bitwarden auto-unlock timed out after 30s".to_string())?
            .map_err(|e| format!("{e}"))
        })?),
        SecretBackendType::OnePassword => {
            let mut backend = rustconn_core::secret::OnePasswordBackend::new();
            if let Some(ref token) = secret_settings.onepassword_service_account_token {
                backend.set_service_account_token(token.clone());
            }
            std::sync::Arc::new(backend)
        }
        SecretBackendType::Passbolt => {
            let mut backend = rustconn_core::secret::PassboltBackend::new();
            if let Some(ref url) = secret_settings.passbolt_server_url {
                backend = backend.with_server_address(url.clone());
            }
            if let Some(ref passphrase) = secret_settings.passbolt_passphrase {
                backend = backend.with_user_password(passphrase.clone());
            }
            std::sync::Arc::new(backend)
        }
        SecretBackendType::Pass => std::sync::Arc::new(
            rustconn_core::secret::PassBackend::from_secret_settings(secret_settings),
        ),
        #[cfg(target_os = "macos")]
        SecretBackendType::MacOsKeychain => {
            std::sync::Arc::new(rustconn_core::secret::MacOsKeychainBackend::new())
        }
        #[cfg(not(target_os = "macos"))]
        SecretBackendType::MacOsKeychain => {
            std::sync::Arc::new(rustconn_core::secret::LibSecretBackend::new("rustconn"))
        }
        SecretBackendType::LibSecret
        | SecretBackendType::KeePassXc
        | SecretBackendType::KdbxFile => {
            // macOS uses the system Keychain; LibSecretBackend (oo7) is not
            // compiled there (R10.1, R10.2). Non-macOS keeps libsecret.
            #[cfg(target_os = "macos")]
            {
                std::sync::Arc::new(rustconn_core::secret::MacOsKeychainBackend::new())
            }
            #[cfg(not(target_os = "macos"))]
            {
                std::sync::Arc::new(rustconn_core::secret::LibSecretBackend::new("rustconn"))
            }
        }
        SecretBackendType::EncryptedFile => {
            // Application-managed encrypted file; addressed by the flat
            // lookup key, same as the other app-managed backends.
            std::sync::Arc::new(rustconn_core::secret::EncryptedFileBackend::new())
        }
        SecretBackendType::PortableEncryptedFile => {
            // Passphrase-based portable encrypted file.
            std::sync::Arc::new(portable_backend_from_settings(secret_settings))
        }
    };
    Ok(backend)
}

/// Dispatches a single vault operation to an explicitly named backend.
///
/// [`dispatch_vault_op`] always addresses whichever backend the settings prefer,
/// which is right for saving and deleting a credential. A credential *transfer*
/// has to address two backends in one operation, neither of which need be the
/// preferred one, so the type is a parameter here.
///
/// `secret_settings` is still needed in full: it carries the per-backend
/// configuration (Bitwarden unlock, the 1Password token, the Passbolt server and
/// passphrase, the `pass` store directory, the portable file's path and
/// passphrase) that constructing the backend requires.
///
/// `KeePassXc` / `KdbxFile` are **not** handled here. They resolve to the system
/// keyring, which is correct for `dispatch_vault_op`'s callers — KDBX proper goes
/// through `KeePassStatus` — but would be a silent substitution for a transfer
/// that names KeePass explicitly. [`TransferPort`] intercepts those two variants
/// for that reason.
///
/// # Errors
///
/// Returns a human-readable error string if the backend is unavailable or the
/// operation fails.
pub fn dispatch_vault_op_for(
    secret_settings: &rustconn_core::config::SecretSettings,
    backend_type: rustconn_core::config::SecretBackendType,
    lookup_key: &str,
    op: VaultOp<'_>,
) -> Result<Option<rustconn_core::models::Credentials>, String> {
    crate::async_utils::with_runtime(|rt| {
        let backend = build_single_backend(secret_settings, backend_type, rt)?;

        match op {
            VaultOp::Store(creds) => {
                tracing::debug!(
                    %lookup_key,
                    ?backend_type,
                    "dispatch_vault_op: storing credentials"
                );
                rt.block_on(async {
                    tokio::time::timeout(
                        std::time::Duration::from_secs(10),
                        backend.store(lookup_key, creds),
                    )
                    .await
                    .map_err(|_| "Vault store timed out after 10s".to_string())?
                    .map_err(|e| format!("{e}"))
                })?;
                tracing::debug!(%lookup_key, "dispatch_vault_op: store succeeded");
                Ok(None)
            }
            VaultOp::Retrieve => {
                tracing::debug!(
                    %lookup_key,
                    ?backend_type,
                    "dispatch_vault_op: retrieving credentials"
                );
                let result = rt.block_on(async {
                    tokio::time::timeout(
                        std::time::Duration::from_secs(10),
                        backend.retrieve(lookup_key),
                    )
                    .await
                    .map_err(|_| "Vault retrieve timed out after 10s".to_string())?
                    .map_err(|e| format!("{e}"))
                })?;
                tracing::debug!(
                    %lookup_key,
                    found = result.is_some(),
                    "dispatch_vault_op: retrieve completed"
                );
                Ok(result)
            }
            VaultOp::Delete => {
                rt.block_on(async {
                    tokio::time::timeout(
                        std::time::Duration::from_secs(10),
                        backend.delete(lookup_key),
                    )
                    .await
                    .map_err(|_| "Vault delete timed out after 10s".to_string())?
                    .map_err(|e| format!("{e}"))
                })?;
                Ok(None)
            }
        }
    })
    .and_then(|r| r)
}

// ─────────────────────────────────────────────────────────────────────────────
// Credential transfer between backends
// ─────────────────────────────────────────────────────────────────────────────

/// How long one entry's read or write may take before the transfer moves on.
///
/// Ten seconds, matching what the rest of this module allows a single vault
/// operation. It is per entry rather than per batch: forty entries are allowed
/// forty of these, because the alternative — one budget for the whole run — makes
/// the last entries fail for the first entries' slowness.
const TRANSFER_OP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// The message for an operation that ran out of time.
///
/// One function so the two arms of each operation cannot drift into reporting
/// different wordings, and so the duration is stated once.
fn timed_out(operation: &str) -> String {
    format!(
        "the {operation} timed out after {}s",
        TRANSFER_OP_TIMEOUT.as_secs()
    )
}

/// One side of a transfer, opened once for the whole batch.
///
/// The KDBX arms run their `keepassxc-cli` call on a blocking task under a
/// [`TRANSFER_OP_TIMEOUT`], which the backend arms already had and they did not.
/// This bounds *the transfer*, not the child process: `std::process::Command` has
/// no timeout, so a `keepassxc-cli` that never returns — a database on an
/// unreachable mount, a hardware key waiting for a touch — leaves its blocking
/// task parked until the process exits. That is a leaked thread against a loop
/// that would otherwise stop dead on one entry and never reach the other thirty
/// nine, which is the worse of the two.
///
/// Two consequences of that abandoned task are worth stating rather than
/// discovering. Its `Zeroizing` plaintext password — and its handle on the shared
/// database password — are released when the closure returns, so a task that never
/// returns holds both for as long as the child lives: the timeout bounds the
/// *transfer*, not the exposure. And on the
/// write side the abandoned `keepassxc-cli` is still writing while the loop moves
/// on, so two of them can touch one `.kdbx`; `save_password_to_kdbx` removes an
/// existing entry before adding it, so a timed-out write can leave its entry
/// deleted in the destination. The source is untouched either way and running the
/// transfer again repairs it, which is why this is a documented edge and not a
/// refusal to proceed.
///
/// Two reasons this is not just a [`SecretBackendType`] passed to
/// [`dispatch_vault_op_for`] per entry. The portable store caches its
/// passphrase-derived data key *per backend instance*, so a fresh instance per
/// credential pays a full Argon2id derivation each time — roughly half a second
/// — and its write mutex is per instance too, so separate instances serialise
/// nothing. And the checks that decide whether a side is usable at all belong
/// before the loop, not once per entry: a locked KeePass database should say so
/// once, not fail forty times.
enum TransferPort {
    /// A [`SecretBackend`](rustconn_core::secret::SecretBackend) instance, reused
    /// for every entry.
    Backend(std::sync::Arc<dyn rustconn_core::secret::SecretBackend>),
    /// The KeePass database, which is not a `SecretBackend`: it is reached through
    /// `KeePassStatus` and the `keepassxc-cli` binary.
    Kdbx {
        /// Database file.
        path: std::path::PathBuf,
        /// Database password, when the database uses one.
        ///
        /// Behind an `Arc` because each entry's call moves its inputs into a
        /// blocking task, and `SecretString` is `SecretBox<str>`: cloning it
        /// copies the plaintext into a fresh allocation. That would be one
        /// transient plaintext copy of the database master password per entry,
        /// and — since a timed-out task keeps running — several live at once.
        /// `Arc::clone` copies a pointer and leaves one copy for the whole batch.
        db_password: Option<std::sync::Arc<secrecy::SecretString>>,
        /// Key file, when the database uses one.
        key_file: Option<std::path::PathBuf>,
    },
}

impl TransferPort {
    /// Opens one side of a transfer, refusing up front what cannot work.
    ///
    /// # Errors
    /// Returns a human-readable reason the side cannot be used: no database
    /// configured, the KeePass integration switched off, a database whose
    /// password is not available, or a backend that could not be constructed.
    fn open(
        settings: &rustconn_core::config::AppSettings,
        backend_type: rustconn_core::config::SecretBackendType,
        rt: &tokio::runtime::Runtime,
    ) -> Result<Self, String> {
        use rustconn_core::config::SecretBackendType;

        if !matches!(
            backend_type,
            SecretBackendType::KeePassXc | SecretBackendType::KdbxFile
        ) {
            return build_single_backend(&settings.secrets, backend_type, rt).map(Self::Backend);
        }

        // Everything below is a precondition every *reader* of a KDBX entry
        // already applies. Skipping them would let the transfer write into a
        // database that nothing will look at, or fail once per entry on a
        // database it was never going to be able to open.
        let Some(path) = settings.secrets.kdbx_path.clone() else {
            return Err("no KeePass database is configured".to_string());
        };
        if !settings.secrets.kdbx_enabled {
            // `select_backend_for_load` and the resolver both require this flag
            // before they will touch a KDBX entry; without it they use the
            // keyring instead. Writing here anyway produces entries that are
            // never read.
            return Err(
                "KDBX integration is switched off, so entries in the database would not be used"
                    .to_string(),
            );
        }
        if settings.secrets.kdbx_use_password && settings.secrets.kdbx_password.is_none() {
            // `keepassxc-cli` would get an empty stdin and fail to unlock, once
            // per entry. The resolver reports this as a lockout and prompts; here
            // the honest answer is to refuse before starting.
            return Err(
                "the KeePass database password is not available — set it in Settings ▸ Secrets"
                    .to_string(),
            );
        }

        Ok(Self::Kdbx {
            path,
            // One plaintext copy for the batch; see the field's documentation.
            db_password: settings
                .secrets
                .kdbx_password
                .clone()
                .map(std::sync::Arc::new),
            key_file: settings.secrets.kdbx_key_file.clone(),
        })
    }

    /// Reads one credential, trying `item.source_keys` in order.
    ///
    /// # Errors
    /// Returns the backend's failure. A key that simply holds nothing is
    /// `Ok(None)`, not an error.
    fn retrieve(
        &self,
        item: &CredentialTransferItem,
        rt: &tokio::runtime::Runtime,
    ) -> Result<Option<rustconn_core::models::Credentials>, String> {
        let mut last_error = None;
        for key in &item.source_keys {
            let outcome = match self {
                Self::Backend(backend) => rt.block_on(async {
                    tokio::time::timeout(TRANSFER_OP_TIMEOUT, backend.retrieve(key))
                        .await
                        .map_err(|_| timed_out("read"))?
                        .map_err(|e| format!("{e}"))
                }),
                Self::Kdbx {
                    path,
                    db_password,
                    key_file,
                } => {
                    let path = path.clone();
                    let db_password = db_password.as_ref().map(std::sync::Arc::clone);
                    let key_file = key_file.clone();
                    let entry = key.clone();
                    let username = item.username.clone();
                    rt.block_on(async move {
                        tokio::time::timeout(
                            TRANSFER_OP_TIMEOUT,
                            tokio::task::spawn_blocking(move || {
                                rustconn_core::secret::KeePassStatus::get_password_from_kdbx_with_key(
                                    &path,
                                    db_password.as_deref(),
                                    key_file.as_deref(),
                                    &entry,
                                    None,
                                )
                            }),
                        )
                        .await
                        .map_err(|_| timed_out("read"))?
                        .map_err(|e| format!("the read could not be run: {e}"))?
                        .map_err(|e| format!("{e}"))
                        // The KDBX read yields a password and nothing else, which
                        // is why the item carries a username of its own.
                        .map(|password| {
                            password.map(|password| rustconn_core::models::Credentials {
                                username,
                                password: Some(password),
                                key_passphrase: None,
                                domain: None,
                            })
                        })
                    })
                }
            };
            match outcome {
                Ok(Some(creds)) => return Ok(Some(creds)),
                Ok(None) => {}
                Err(e) => last_error = Some(e),
            }
        }
        // Every key missed. A store that could not be opened at all failed on the
        // first key too, so a recorded error outweighs "not found": reporting a
        // miss would tell the user their passwords are not there when the truth is
        // that nothing could look.
        last_error.map_or(Ok(None), Err)
    }

    /// Writes one credential under `item.destination_key`.
    ///
    /// Returns which credential fields this side could not hold, so a partial
    /// write is not reported as a clean copy.
    ///
    /// # Errors
    /// Returns the backend's failure.
    fn store(
        &self,
        item: &CredentialTransferItem,
        creds: &rustconn_core::models::Credentials,
        rt: &tokio::runtime::Runtime,
    ) -> Result<Vec<&'static str>, String> {
        use secrecy::ExposeSecret;

        match self {
            Self::Backend(backend) => rt
                .block_on(async {
                    tokio::time::timeout(
                        TRANSFER_OP_TIMEOUT,
                        backend.store(&item.destination_key, creds),
                    )
                    .await
                    .map_err(|_| timed_out("write"))?
                    .map_err(|e| format!("{e}"))
                })
                .map(|()| Vec::new()),
            Self::Kdbx {
                path,
                db_password,
                key_file,
            } => {
                let Some(password) = creds.password.as_ref() else {
                    return Err("the entry has no password to write".to_string());
                };
                // Wrapped so the plaintext copy keepassxc-cli needs is wiped on drop.
                let plaintext = zeroize::Zeroizing::new(password.expose_secret().to_string());
                let path = path.clone();
                let db_password = db_password.as_ref().map(std::sync::Arc::clone);
                let key_file = key_file.clone();
                let entry = item.destination_key.clone();
                let username = creds.username.clone().unwrap_or_default();
                rt.block_on(async move {
                    tokio::time::timeout(
                        TRANSFER_OP_TIMEOUT,
                        tokio::task::spawn_blocking(move || {
                            rustconn_core::secret::KeePassStatus::save_password_to_kdbx(
                                &path,
                                db_password.as_deref(),
                                key_file.as_deref(),
                                &entry,
                                &username,
                                plaintext.as_str(),
                                None,
                            )
                        }),
                    )
                    .await
                    .map_err(|_| timed_out("write"))?
                    .map_err(|e| format!("the write could not be run: {e}"))?
                    .map_err(|e| format!("{e}"))
                })?;

                // `save_password_to_kdbx` writes a title, username, password and
                // URL. A key passphrase or a domain has nowhere to go, and the
                // caller has to know that rather than being told the entry copied
                // cleanly.
                let mut dropped = Vec::new();
                if creds.key_passphrase.is_some() {
                    dropped.push("key passphrase");
                }
                if creds.domain.is_some() {
                    dropped.push("domain");
                }
                Ok(dropped)
            }
        }
    }
}

/// One credential to move, resolved to the key each side files it under.
///
/// The two sides need separate keys because the shape is per backend, not per
/// credential: the system keyring uses `RustConn/{group}/{name} ({protocol})`,
/// KDBX uses an entry path, and the remaining backends use `rustconn/{name}`.
/// Copying a key verbatim between two backends of different shape would write a
/// credential that the resolver never looks for again.
#[derive(Debug, Clone)]
pub struct CredentialTransferItem {
    /// What to call this entry when reporting — a connection, group or variable
    /// name. Never a secret.
    pub label: String,
    /// Keys to try in the source, most-current format first, so credentials
    /// written by earlier releases are picked up rather than reported missing.
    pub source_keys: Vec<String>,
    /// Key to write in the destination.
    pub destination_key: String,
    /// Username to pair with the password when the source cannot supply one.
    ///
    /// The KDBX read path returns a password and nothing else, so without this a
    /// KeePassXC-to-anywhere transfer would drop every username.
    pub username: Option<String>,
}

/// Outcome of a credential transfer.
#[derive(Debug, Clone, Default)]
pub struct CredentialTransferReport {
    /// Credentials read from the source and written to the destination.
    pub transferred: usize,
    /// Entries the source held nothing for.
    ///
    /// Not a failure, and reported separately for that reason: a connection set
    /// to use the vault does not have to have a password saved yet, and counting
    /// those as errors would make a healthy transfer look broken.
    pub missing: usize,
    /// Entries whose password arrived but which lost a field the destination
    /// cannot hold, as `(label, field list)`.
    ///
    /// Separate from both counters above because it is neither: the entry works,
    /// but not identically. Writing a KeePass database drops a key passphrase and
    /// a domain, which libsecret and both file backends do store.
    pub incomplete: Vec<(String, String)>,
    /// Entries that could not be transferred, as `(label, error description)`.
    pub failures: Vec<(String, String)>,
    /// Whether the run stopped early because the user asked it to.
    ///
    /// Reported so the counts can be read correctly: "Copied: 7" out of forty
    /// planned entries is a complete answer to a cancelled run and an alarming
    /// one to a finished run, and nothing else in the report distinguishes them.
    pub cancelled: bool,
}

impl CredentialTransferReport {
    /// Whether every entry that had a credential arrived complete.
    ///
    /// A cancelled run is never complete, even when everything it did reach
    /// arrived cleanly: the entries it never looked at are the point.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        !self.cancelled && self.failures.is_empty() && self.incomplete.is_empty()
    }
}

/// The dialog's handles on a running transfer.
///
/// Both halves exist because a transfer over a CLI-backed vault is a process spawn
/// per entry: forty entries is minutes of a dialog that previously said "Copying…"
/// and nothing else, with no way to stop it.
pub struct TransferControl {
    /// Sent the number of entries finished, after each one.
    ///
    /// Unbounded, so the worker never blocks on a main loop that is busy, and
    /// `try_send` is used rather than `send` — a dropped progress tick costs a
    /// stale count for a moment, where a blocked worker costs the transfer.
    pub progress: async_channel::Sender<usize>,
    /// Set by the dialog to stop the run.
    ///
    /// Checked before each entry, never mid-entry: a credential is written by a
    /// single backend call, and abandoning one in flight would leave the
    /// destination in a state the report could not describe. So Cancel means "stop
    /// after the one you are on", and the report says how far it got.
    pub cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

/// What a transfer would move, and what stands in its way.
#[derive(Debug, Clone, Default)]
pub struct CredentialTransferPlan {
    /// The credentials to copy, one entry each.
    pub items: Vec<CredentialTransferItem>,
    /// Entries that would overwrite one another in the destination, grouped by
    /// the key they share. See [`find_destination_collisions`] for why this can
    /// happen and why the answer is to refuse rather than to pick a winner.
    pub collisions: Vec<Vec<String>>,
}

/// Builds the list of credentials a transfer from `source` to `destination`
/// would move.
///
/// The `SecretBackend` trait cannot enumerate: six of the eight backends offer no
/// way to list what they hold, so the set of credentials is taken from what
/// RustConn knows it stored — every connection and group set to use the vault,
/// plus every secret variable. A vault may well contain entries RustConn never
/// created; those are not touched, and cannot be, because nothing can name them.
///
/// Variables that point at a pre-existing external entry (`kdbx_entry_path` or
/// `vault_entry_name`) are skipped on purpose. Those are references to entries the
/// user maintains elsewhere, not credentials RustConn owns, and copying one into
/// another backend would duplicate someone else's secret under a name the
/// resolver would not look for anyway.
#[must_use]
pub fn plan_credential_transfer(
    settings: &rustconn_core::config::AppSettings,
    connections: &[rustconn_core::models::Connection],
    groups: &[rustconn_core::models::ConnectionGroup],
    source: rustconn_core::config::SecretBackendType,
    destination: rustconn_core::config::SecretBackendType,
) -> CredentialTransferPlan {
    use rustconn_core::models::PasswordSource;

    let mut items = Vec::new();

    for conn in connections {
        if conn.password_source != PasswordSource::Vault {
            continue;
        }
        let protocol = conn.protocol_config.protocol_type().as_str().to_lowercase();
        let source_keys = transfer_keys_for_connection(groups, conn, &protocol, source);
        let Some(destination_key) =
            transfer_keys_for_connection(groups, conn, &protocol, destination)
                .into_iter()
                .next()
        else {
            continue;
        };
        items.push(CredentialTransferItem {
            label: conn.name.clone(),
            source_keys,
            destination_key,
            username: conn.username.clone(),
        });
    }

    for group in groups {
        if group.password_source != Some(PasswordSource::Vault) {
            continue;
        }
        items.push(CredentialTransferItem {
            label: group.name.clone(),
            source_keys: vec![transfer_key_for_group(groups, group, source)],
            destination_key: transfer_key_for_group(groups, group, destination),
            username: group.username.clone(),
        });
    }

    for variable in &settings.global_variables {
        if !variable.is_secret
            || variable.kdbx_entry_path.is_some()
            || variable.vault_entry_name.is_some()
        {
            continue;
        }
        let key = rustconn_core::variables::variable_secret_key(&variable.name);
        items.push(CredentialTransferItem {
            label: variable.name.clone(),
            source_keys: vec![key.clone()],
            destination_key: key,
            username: None,
        });
    }

    let collisions = find_destination_collisions(&items);
    CredentialTransferPlan { items, collisions }
}

/// Groups of entries that would land on the same key in the destination.
///
/// The flat key shape the six non-keyring backends use is `rustconn/{name}` with
/// no group in it, while the keyring and KDBX shapes both carry the group path.
/// Two connections called `web` in `Prod` and `Staging` therefore have distinct
/// keys in a keyring or a database and *one* key in the portable file — so a
/// keyring-to-portable transfer would read two different passwords, write both to
/// the same entry, and count two successes. The user then has a connection that
/// authenticates with another connection's password, silently.
///
/// This is a property of the key shape, not of the transfer, and predates it (the
/// group was added to the keyring key for issue #264 and to nothing else). But
/// the transfer is the first thing that walks every entry at once, so it is the
/// first thing that can see the collision — and refusing is the only honest
/// response, since there is no key for the second entry to go to.
fn find_destination_collisions(items: &[CredentialTransferItem]) -> Vec<Vec<String>> {
    let mut by_key: std::collections::HashMap<&str, Vec<&str>> = std::collections::HashMap::new();
    for item in items {
        by_key
            .entry(item.destination_key.as_str())
            .or_default()
            .push(item.label.as_str());
    }

    let mut collisions: Vec<Vec<String>> = by_key
        .into_values()
        .filter(|labels| labels.len() > 1)
        .map(|labels| {
            let mut owned: Vec<String> = labels.into_iter().map(str::to_owned).collect();
            owned.sort_unstable();
            owned
        })
        .collect();
    // Deterministic order so the dialog and the log do not reshuffle per run.
    collisions.sort_unstable();
    collisions
}

/// Keys a connection's credential can live under in `backend_type`, current
/// format first.
fn transfer_keys_for_connection(
    groups: &[rustconn_core::models::ConnectionGroup],
    connection: &rustconn_core::models::Connection,
    protocol_str: &str,
    backend_type: rustconn_core::config::SecretBackendType,
) -> Vec<String> {
    use rustconn_core::config::SecretBackendType;

    if matches!(
        backend_type,
        SecretBackendType::KeePassXc | SecretBackendType::KdbxFile
    ) {
        // The same string `save_password_to_vault` writes: the hierarchical entry
        // path without the `RustConn/` prefix, which both KDBX helpers add back,
        // plus the protocol suffix.
        let entry_path =
            rustconn_core::secret::KeePassHierarchy::build_entry_path(connection, groups);
        let base = entry_path.strip_prefix("RustConn/").unwrap_or(&entry_path);
        return vec![format!("{base} ({protocol_str})")];
    }

    vault_keys_for_connection(groups, connection, protocol_str, backend_type)
}

/// Key a group's credential lives under in `backend_type`.
fn transfer_key_for_group(
    groups: &[rustconn_core::models::ConnectionGroup],
    group: &rustconn_core::models::ConnectionGroup,
    backend_type: rustconn_core::config::SecretBackendType,
) -> String {
    use rustconn_core::config::SecretBackendType;

    if matches!(
        backend_type,
        SecretBackendType::KeePassXc | SecretBackendType::KdbxFile
    ) {
        let path = rustconn_core::secret::KeePassHierarchy::build_group_entry_path(group, groups);
        return path.strip_prefix("RustConn/").unwrap_or(&path).to_string();
    }

    // Every other backend keys group credentials by the group's UUID, which is
    // why a group rename does not have to migrate them.
    group.id.to_string()
}

/// Copies each planned credential from `source` into `destination`.
///
/// Blocking: every entry costs at least one backend round trip, and a KDBX or
/// Bitwarden side costs a process spawn per entry. Call this from
/// `spawn_blocking`, never on the GTK main thread.
///
/// The source is never modified. Deleting from it is not offered at all: for a
/// shared vault such as Bitwarden or a Passbolt server the entries may not be
/// RustConn's to remove, and for the machine-bound file the originals are this
/// machine's fallback — the same reason the portable-file wizard keeps them.
pub fn run_credential_transfer(
    settings: &rustconn_core::config::AppSettings,
    source: rustconn_core::config::SecretBackendType,
    destination: rustconn_core::config::SecretBackendType,
    items: &[CredentialTransferItem],
    control: &TransferControl,
) -> Result<CredentialTransferReport, String> {
    crate::async_utils::with_runtime(|rt| {
        // Both sides once, before the loop: see `TransferPort`.
        let source_port = TransferPort::open(settings, source, rt)
            .map_err(|e| format!("cannot read from the source: {e}"))?;
        let destination_port = TransferPort::open(settings, destination, rt)
            .map_err(|e| format!("cannot write to the destination: {e}"))?;

        let mut report = CredentialTransferReport::default();
        for (finished, item) in items.iter().enumerate() {
            // Between entries, not within one: see `TransferControl::cancel`.
            if control.cancel.load(std::sync::atomic::Ordering::Relaxed) {
                report.cancelled = true;
                tracing::info!(
                    finished,
                    planned = items.len(),
                    "credential transfer cancelled"
                );
                break;
            }
            match source_port.retrieve(item, rt) {
                Ok(Some(creds)) => match destination_port.store(item, &creds, rt) {
                    Ok(dropped) if dropped.is_empty() => report.transferred += 1,
                    Ok(dropped) => {
                        report.transferred += 1;
                        report
                            .incomplete
                            .push((item.label.clone(), dropped.join(", ")));
                    }
                    Err(e) => {
                        // The reason goes to the log here and nowhere else. It is
                        // backend output, and a backend that takes a password on
                        // its command line can quote it back; the dialog shows the
                        // names, which is what the user acts on. Logging it at all
                        // is a judgement that a diagnosable failure is worth more
                        // than the residual risk, now that the one backend known
                        // to echo its argv scrubs it first.
                        tracing::warn!(
                            entry = %item.label,
                            error = %e,
                            "credential transfer could not write an entry"
                        );
                        report.failures.push((item.label.clone(), e));
                    }
                },
                Ok(None) => report.missing += 1,
                Err(e) => {
                    tracing::warn!(
                        entry = %item.label,
                        error = %e,
                        "credential transfer could not read an entry"
                    );
                    report.failures.push((item.label.clone(), e));
                }
            }
            // After the entry, so the count is entries *done* rather than started.
            // A full queue is ignored: the dialog showing a count one behind for a
            // moment is better than a worker waiting on the main loop.
            let _ = control.progress.try_send(finished + 1);
        }

        tracing::info!(
            ?source,
            ?destination,
            transferred = report.transferred,
            missing = report.missing,
            incomplete = report.incomplete.len(),
            failed = report.failures.len(),
            cancelled = report.cancelled,
            "credential transfer finished"
        );
        Ok(report)
    })
    .and_then(|r| r)
}

/// Selects the appropriate storage backend for variable secrets.
///
/// Mirrors `CredentialResolver::select_storage_backend` logic.
/// Also used by connection password load/save and variable vault operations.
/// Builds a portable-file backend from settings, unlocked when possible.
///
/// The passphrase comes from `SecretSettings::portable_passphrase`, the
/// session-only field that the unlock dialog, the startup restore and the
/// Settings page all write to. When it is absent the backend stays locked and
/// its operations report [`rustconn_core::error::SecretError::PassphraseRequired`],
/// which the connection path turns into an unlock prompt.
///
/// Exists so the path resolution and the unlock live in one place: four call
/// sites each had their own copy, and every one of them had forgotten to set the
/// passphrase at some point in this feature's history.
#[must_use]
pub fn portable_backend_from_settings(
    settings: &rustconn_core::config::SecretSettings,
) -> rustconn_core::secret::PortableEncryptedFileBackend {
    let path =
        rustconn_core::secret::resolve_portable_store_path(settings.portable_file_path.as_deref());
    let backend = rustconn_core::secret::PortableEncryptedFileBackend::with_path(path);
    if let Some(ref passphrase) = settings.portable_passphrase {
        backend.set_passphrase(passphrase.clone());
    }
    backend
}

pub fn select_backend_for_load(
    secrets: &rustconn_core::config::SecretSettings,
) -> rustconn_core::config::SecretBackendType {
    use rustconn_core::config::SecretBackendType;

    match secrets.preferred_backend {
        SecretBackendType::Bitwarden => SecretBackendType::Bitwarden,
        SecretBackendType::OnePassword => SecretBackendType::OnePassword,
        SecretBackendType::Passbolt => SecretBackendType::Passbolt,
        SecretBackendType::Pass => SecretBackendType::Pass,
        SecretBackendType::MacOsKeychain => SecretBackendType::MacOsKeychain,
        SecretBackendType::KeePassXc | SecretBackendType::KdbxFile => {
            if secrets.kdbx_enabled && secrets.kdbx_path.is_some() {
                SecretBackendType::KdbxFile
            } else if secrets.enable_fallback {
                SecretBackendType::LibSecret
            } else {
                secrets.preferred_backend
            }
        }
        SecretBackendType::LibSecret => SecretBackendType::LibSecret,
        // EncryptedFile is a flat-key backend; identity mapping mirrors the
        // other app-managed backends above. (Allowed flat-key wiring for 2.4.)
        SecretBackendType::EncryptedFile => SecretBackendType::EncryptedFile,
        // PortableEncryptedFile is also a flat-key backend (passphrase-based).
        SecretBackendType::PortableEncryptedFile => SecretBackendType::PortableEncryptedFile,
    }
}

/// Generates the correct store key for a connection based on the backend type.
///
/// LibSecret uses hierarchical `"RustConn/{group_path}/{name} ({protocol})"` format
/// when `group_path` is provided (matching
/// [`CredentialResolver::generate_keyring_key_with_hierarchy`]), or falls back to
/// `"{name} ({protocol})"` for backward compatibility when no group path is given.
/// All other backends use `"rustconn/{name}"` (matching
/// [`CredentialResolver::generate_lookup_key`]).
///
/// When `conn_name` is empty, falls back to `conn_host` for non-LibSecret
/// backends, matching the resolver's `generate_lookup_key` behavior.
///
/// This ensures that the key used at store time matches the primary key the
/// resolver tries at resolve time, eliminating the need for fallback lookups.
pub fn generate_store_key(
    conn_name: &str,
    conn_host: &str,
    protocol_str: &str,
    backend_type: rustconn_core::config::SecretBackendType,
) -> String {
    generate_store_key_with_group(conn_name, conn_host, protocol_str, backend_type, None)
}

/// Generates a store key that includes the group path for keyring backends.
///
/// `group_path` is the `/`-separated group hierarchy (e.g. `"Production/Web"`).
/// When provided and the backend is LibSecret or the macOS Keychain, the key is
/// `"RustConn/{group_path}/{name} ({protocol})"`.
///
/// The macOS Keychain is included because `CredentialResolver` resolves it
/// through the same hierarchical keyring path as LibSecret
/// (`resolve_from_keyring_hierarchical`). Storing it under the flat
/// `rustconn/{name}` key, as this function did until 0.19.19, meant the resolver
/// never looked where the credential had been written.
pub fn generate_store_key_with_group(
    conn_name: &str,
    conn_host: &str,
    protocol_str: &str,
    backend_type: rustconn_core::config::SecretBackendType,
    group_path: Option<&str>,
) -> String {
    use rustconn_core::config::SecretBackendType;

    if matches!(
        backend_type,
        SecretBackendType::LibSecret | SecretBackendType::MacOsKeychain
    ) {
        let name = conn_name.trim().replace('/', "-");
        let suffix = format!("{name} ({protocol_str})");
        match group_path {
            Some(path) if !path.is_empty() => format!("RustConn/{path}/{suffix}"),
            Some(_) => format!("RustConn/{suffix}"),
            None => suffix,
        }
    } else {
        // All other backends: "rustconn/{identifier}" — matches generate_lookup_key
        // Falls back to host when name is empty, same as CredentialResolver
        let identifier = if conn_name.trim().is_empty() {
            conn_host
        } else {
            conn_name
        };
        format!("rustconn/{identifier}")
    }
}

#[cfg(test)]
mod tests {
    use rustconn_core::config::{SecretBackendType, SecretSettings};
    use rustconn_core::models::{Connection, ConnectionGroup};

    use super::*;

    fn default_secret_settings(backend: SecretBackendType) -> SecretSettings {
        SecretSettings {
            preferred_backend: backend,
            kdbx_enabled: false,
            kdbx_path: None,
            kdbx_key_file: None,
            kdbx_password: None,
            enable_fallback: false,
            ..Default::default()
        }
    }

    // ── select_backend_for_load ──────────────────────────────────────

    #[test]
    fn select_backend_bitwarden() {
        let s = default_secret_settings(SecretBackendType::Bitwarden);
        assert_eq!(select_backend_for_load(&s), SecretBackendType::Bitwarden);
    }

    #[test]
    fn select_backend_onepassword() {
        let s = default_secret_settings(SecretBackendType::OnePassword);
        assert_eq!(select_backend_for_load(&s), SecretBackendType::OnePassword);
    }

    #[test]
    fn select_backend_passbolt() {
        let s = default_secret_settings(SecretBackendType::Passbolt);
        assert_eq!(select_backend_for_load(&s), SecretBackendType::Passbolt);
    }

    #[test]
    fn select_backend_pass() {
        let s = default_secret_settings(SecretBackendType::Pass);
        assert_eq!(select_backend_for_load(&s), SecretBackendType::Pass);
    }

    #[test]
    fn select_backend_libsecret() {
        let s = default_secret_settings(SecretBackendType::LibSecret);
        assert_eq!(select_backend_for_load(&s), SecretBackendType::LibSecret);
    }

    #[test]
    fn select_backend_keepass_with_kdbx_enabled() {
        let s = SecretSettings {
            preferred_backend: SecretBackendType::KeePassXc,
            kdbx_enabled: true,
            kdbx_path: Some(std::path::PathBuf::from("/tmp/test.kdbx")),
            ..Default::default()
        };
        assert_eq!(select_backend_for_load(&s), SecretBackendType::KdbxFile);
    }

    #[test]
    fn select_backend_keepass_without_kdbx_falls_back() {
        let s = SecretSettings {
            preferred_backend: SecretBackendType::KeePassXc,
            kdbx_enabled: false,
            kdbx_path: None,
            enable_fallback: true,
            ..Default::default()
        };
        assert_eq!(select_backend_for_load(&s), SecretBackendType::LibSecret);
    }

    #[test]
    fn select_backend_keepass_no_fallback() {
        let s = SecretSettings {
            preferred_backend: SecretBackendType::KeePassXc,
            kdbx_enabled: false,
            kdbx_path: None,
            enable_fallback: false,
            ..Default::default()
        };
        assert_eq!(select_backend_for_load(&s), SecretBackendType::KeePassXc);
    }

    // ── generate_store_key ───────────────────────────────────────────

    #[test]
    fn store_key_libsecret_format() {
        let key = generate_store_key("My Server", "10.0.0.1", "ssh", SecretBackendType::LibSecret);
        assert_eq!(key, "My Server (ssh)");
    }

    #[test]
    fn store_key_libsecret_strips_slashes() {
        let key = generate_store_key(
            "Prod/Web-01",
            "10.0.0.1",
            "ssh",
            SecretBackendType::LibSecret,
        );
        assert_eq!(key, "Prod-Web-01 (ssh)");
    }

    #[test]
    fn store_key_bitwarden_format() {
        let key = generate_store_key("My Server", "10.0.0.1", "ssh", SecretBackendType::Bitwarden);
        assert_eq!(key, "rustconn/My Server");
    }

    #[test]
    fn store_key_empty_name_falls_back_to_host() {
        let key = generate_store_key("", "10.0.0.1", "rdp", SecretBackendType::Bitwarden);
        assert_eq!(key, "rustconn/10.0.0.1");
    }

    #[test]
    fn store_key_whitespace_name_falls_back_to_host() {
        let key = generate_store_key("   ", "10.0.0.1", "rdp", SecretBackendType::OnePassword);
        assert_eq!(key, "rustconn/10.0.0.1");
    }

    #[test]
    fn store_key_pass_format() {
        let key = generate_store_key("DB Server", "db.local", "ssh", SecretBackendType::Pass);
        assert_eq!(key, "rustconn/DB Server");
    }

    #[test]
    fn store_key_macos_keychain_is_hierarchical_like_libsecret() {
        // The resolver reads the Keychain through the hierarchical keyring path,
        // so the store key must match it rather than the flat "rustconn/{name}".
        let key = generate_store_key_with_group(
            "admin",
            "10.0.0.1",
            "ssh",
            SecretBackendType::MacOsKeychain,
            Some("oracle"),
        );
        assert_eq!(key, "RustConn/oracle/admin (ssh)");
    }

    // ── vault_keys_for_connection ────────────────────────────────────

    fn ssh_connection(name: &str, group_id: Option<uuid::Uuid>) -> Connection {
        let mut conn = Connection::new(
            name.to_string(),
            "10.0.0.1".to_string(),
            22,
            rustconn_core::models::ProtocolConfig::Ssh(rustconn_core::models::SshConfig::default()),
        );
        conn.group_id = group_id;
        conn
    }

    #[test]
    fn keyring_keys_lead_with_the_group_scoped_key() {
        // Deleting by the flat key alone left the keyring item behind (#263),
        // because the item is stored under the group-scoped key (#264).
        let group = ConnectionGroup::new("oracle".to_string());
        let conn = ssh_connection("admin", Some(group.id));
        let keys = vault_keys_for_connection(
            std::slice::from_ref(&group),
            &conn,
            "ssh",
            SecretBackendType::LibSecret,
        );

        assert_eq!(
            keys.first().map(String::as_str),
            Some("RustConn/oracle/admin (ssh)")
        );
        assert!(
            keys.iter().any(|k| k == "admin (ssh)"),
            "legacy flat key must stay in the cleanup set: {keys:?}"
        );
        assert!(
            keys.iter().any(|k| *k == conn.id.to_string()),
            "UUID key must stay in the cleanup set: {keys:?}"
        );
    }

    #[test]
    fn keyring_keys_differ_between_groups_for_the_same_name() {
        // The collision from issue #264: two "admin" connections in different
        // groups must never share a primary key.
        let oracle = ConnectionGroup::new("oracle".to_string());
        let pve = ConnectionGroup::new("pve".to_string());
        let groups = vec![oracle.clone(), pve.clone()];

        let in_oracle = ssh_connection("admin", Some(oracle.id));
        let in_pve = ssh_connection("admin", Some(pve.id));

        let oracle_key =
            vault_keys_for_connection(&groups, &in_oracle, "ssh", SecretBackendType::LibSecret);
        let pve_key =
            vault_keys_for_connection(&groups, &in_pve, "ssh", SecretBackendType::LibSecret);

        assert_eq!(
            oracle_key.first().map(String::as_str),
            Some("RustConn/oracle/admin (ssh)")
        );
        assert_eq!(
            pve_key.first().map(String::as_str),
            Some("RustConn/pve/admin (ssh)")
        );
        assert_ne!(oracle_key.first(), pve_key.first());
    }

    #[test]
    fn flat_backend_keys_use_the_rustconn_prefix() {
        let conn = ssh_connection("admin", None);
        let keys = vault_keys_for_connection(&[], &conn, "ssh", SecretBackendType::Bitwarden);
        assert_eq!(keys.first().map(String::as_str), Some("rustconn/admin"));
    }

    // ── plan_vault_key_migration ─────────────────────────────────────

    fn app_settings(backend: SecretBackendType) -> rustconn_core::config::AppSettings {
        rustconn_core::config::AppSettings {
            secrets: default_secret_settings(backend),
            ..Default::default()
        }
    }

    fn keepass_app_settings() -> rustconn_core::config::AppSettings {
        rustconn_core::config::AppSettings {
            secrets: SecretSettings {
                preferred_backend: SecretBackendType::KeePassXc,
                kdbx_enabled: true,
                kdbx_path: Some("/tmp/does-not-need-to-exist.kdbx".into()),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn plan_migration_is_none_when_the_edit_changed_nothing_in_the_key() {
        let settings = app_settings(SecretBackendType::LibSecret);
        let conn = ssh_connection("admin", None);
        // A description or tag edit leaves the key alone.
        let mut edited = conn.clone();
        edited.description = Some("touched".to_string());

        assert!(plan_vault_key_migration(&settings, &[], &[], &conn, &edited).is_none());
    }

    #[test]
    fn plan_migration_detects_a_rename() {
        let settings = app_settings(SecretBackendType::LibSecret);
        let group = ConnectionGroup::new("oracle".to_string());
        let old = ssh_connection("admin", Some(group.id));
        let mut new = old.clone();
        new.name = "dba".to_string();

        let groups = std::slice::from_ref(&group);
        let plan = plan_vault_key_migration(&settings, groups, groups, &old, &new).unwrap();

        assert_eq!(plan.new_key, "RustConn/oracle/dba (ssh)");
        assert_eq!(
            plan.old_keys.first().map(String::as_str),
            Some("RustConn/oracle/admin (ssh)")
        );
        assert!(!plan.is_keepass);
    }

    #[test]
    fn plan_migration_detects_a_group_change() {
        let settings = app_settings(SecretBackendType::LibSecret);
        let oracle = ConnectionGroup::new("oracle".to_string());
        let pve = ConnectionGroup::new("pve".to_string());
        let groups = vec![oracle.clone(), pve.clone()];

        let old = ssh_connection("admin", Some(oracle.id));
        let mut new = old.clone();
        new.group_id = Some(pve.id);

        let plan = plan_vault_key_migration(&settings, &groups, &groups, &old, &new).unwrap();

        assert_eq!(plan.new_key, "RustConn/pve/admin (ssh)");
        assert_eq!(
            plan.old_keys.first().map(String::as_str),
            Some("RustConn/oracle/admin (ssh)")
        );
    }

    #[test]
    fn plan_migration_detects_a_protocol_change() {
        // The protocol is part of the key suffix, and neither pre-0.19.19 helper
        // could express a protocol change — both used one protocol string for the
        // old and the new key.
        let settings = app_settings(SecretBackendType::LibSecret);
        let old = ssh_connection("admin", None);
        let mut new = old.clone();
        new.protocol_config =
            rustconn_core::models::ProtocolConfig::Rdp(rustconn_core::models::RdpConfig::default());

        let plan = plan_vault_key_migration(&settings, &[], &[], &old, &new).unwrap();

        assert_eq!(plan.new_key, "RustConn/admin (rdp)");
        assert_eq!(
            plan.old_keys.first().map(String::as_str),
            Some("RustConn/admin (ssh)")
        );
    }

    #[test]
    fn plan_migration_handles_name_group_and_protocol_at_once() {
        // The connection edit dialog rebuilds the whole connection, so a single
        // save can change all three.
        let settings = app_settings(SecretBackendType::LibSecret);
        let oracle = ConnectionGroup::new("oracle".to_string());
        let pve = ConnectionGroup::new("pve".to_string());
        let groups = vec![oracle.clone(), pve.clone()];

        let old = ssh_connection("admin", Some(oracle.id));
        let mut new = old.clone();
        new.name = "dba".to_string();
        new.group_id = Some(pve.id);
        new.protocol_config =
            rustconn_core::models::ProtocolConfig::Rdp(rustconn_core::models::RdpConfig::default());

        let plan = plan_vault_key_migration(&settings, &groups, &groups, &old, &new).unwrap();

        assert_eq!(plan.new_key, "RustConn/pve/dba (rdp)");
        assert_eq!(
            plan.old_keys.first().map(String::as_str),
            Some("RustConn/oracle/admin (ssh)")
        );
    }

    #[test]
    fn plan_migration_keepass_uses_hierarchical_entry_paths() {
        let settings = keepass_app_settings();
        let group = ConnectionGroup::new("oracle".to_string());
        let old = ssh_connection("admin", Some(group.id));
        let mut new = old.clone();
        new.name = "dba".to_string();

        let groups = std::slice::from_ref(&group);
        let plan = plan_vault_key_migration(&settings, groups, groups, &old, &new).unwrap();

        assert!(plan.is_keepass);
        assert_eq!(plan.new_key, "RustConn/oracle/dba (ssh)");
        assert_eq!(
            plan.old_keys.first().map(String::as_str),
            Some("RustConn/oracle/admin (ssh)")
        );
    }

    #[test]
    fn plan_migration_flat_backend_ignores_a_group_change() {
        // Bitwarden and friends key on the name alone, so moving between groups
        // cannot move the entry.
        let settings = app_settings(SecretBackendType::Bitwarden);
        let oracle = ConnectionGroup::new("oracle".to_string());
        let pve = ConnectionGroup::new("pve".to_string());
        let groups = vec![oracle.clone(), pve.clone()];

        let old = ssh_connection("admin", Some(oracle.id));
        let mut new = old.clone();
        new.group_id = Some(pve.id);

        assert!(plan_vault_key_migration(&settings, &groups, &groups, &old, &new).is_none());
    }

    #[test]
    fn plan_migration_flat_backend_detects_a_rename() {
        let settings = app_settings(SecretBackendType::Bitwarden);
        let old = ssh_connection("admin", None);
        let mut new = old.clone();
        new.name = "dba".to_string();

        let plan = plan_vault_key_migration(&settings, &[], &[], &old, &new).unwrap();

        assert_eq!(plan.new_key, "rustconn/dba");
        assert_eq!(
            plan.old_keys.first().map(String::as_str),
            Some("rustconn/admin")
        );
    }

    // ── credential transfer ──────────────────────────────────────────
    //
    // The transfer's correctness is entirely a claim about *agreement*: the key it
    // writes has to be the key the save path would have written and the resolver
    // will later look for. Nothing in the type system enforces that, so these
    // tests pin it per backend shape.

    /// A vault-backed connection, since the plan only includes those.
    fn vault_connection(name: &str, group_id: Option<uuid::Uuid>) -> Connection {
        let mut conn = ssh_connection(name, group_id);
        conn.password_source = rustconn_core::models::PasswordSource::Vault;
        conn
    }

    #[test]
    fn transfer_destination_key_matches_what_the_save_path_writes() {
        let group = ConnectionGroup::new("Prod".to_string());
        let conn = vault_connection("web", Some(group.id));
        let groups = std::slice::from_ref(&group);

        for backend in [
            SecretBackendType::LibSecret,
            SecretBackendType::MacOsKeychain,
            SecretBackendType::Bitwarden,
            SecretBackendType::OnePassword,
            SecretBackendType::Passbolt,
            SecretBackendType::Pass,
            SecretBackendType::EncryptedFile,
            SecretBackendType::PortableEncryptedFile,
        ] {
            let transfer = transfer_keys_for_connection(groups, &conn, "ssh", backend);
            let expected = vault_keys_for_connection(groups, &conn, "ssh", backend)[0].clone();
            assert_eq!(
                transfer.first(),
                Some(&expected),
                "{backend:?}: the transfer would write a key nothing reads"
            );
        }
    }

    /// KDBX is the one shape that does not come from `vault_keys_for_connection`,
    /// because the database is addressed by entry path rather than by lookup key.
    #[test]
    fn transfer_kdbx_key_matches_the_kdbx_save_path() {
        let group = ConnectionGroup::new("Prod".to_string());
        let conn = vault_connection("web", Some(group.id));
        let groups = std::slice::from_ref(&group);

        let keys = transfer_keys_for_connection(groups, &conn, "ssh", SecretBackendType::KdbxFile);

        // Exactly what `save_password_to_vault`'s KDBX arm builds: the entry path
        // with the `RustConn/` prefix stripped, plus the protocol suffix.
        assert_eq!(keys, vec!["Prod/web (ssh)".to_string()]);
    }

    #[test]
    fn transfer_group_key_is_the_uuid_off_kdbx_and_a_path_on_it() {
        let group = ConnectionGroup::new("Prod".to_string());
        let groups = std::slice::from_ref(&group);

        assert_eq!(
            transfer_key_for_group(groups, &group, SecretBackendType::LibSecret),
            group.id.to_string(),
            "every non-KDBX backend keys group credentials by UUID"
        );
        assert_eq!(
            transfer_key_for_group(groups, &group, SecretBackendType::KdbxFile),
            "Groups/Prod",
            "KDBX keys them by entry path, prefix stripped"
        );
    }

    /// The defect this guard exists for: the flat key shape carries no group, so
    /// two same-named connections in different groups map onto one destination
    /// entry. Writing both would leave one connection authenticating with the
    /// other's password, and the report would call it two successes.
    #[test]
    fn plan_refuses_when_two_entries_would_share_a_destination_key() {
        let prod = ConnectionGroup::new("Prod".to_string());
        let staging = ConnectionGroup::new("Staging".to_string());
        let groups = vec![prod.clone(), staging.clone()];
        let connections = vec![
            vault_connection("web", Some(prod.id)),
            vault_connection("web", Some(staging.id)),
        ];
        let settings = rustconn_core::config::AppSettings::default();

        let plan = plan_credential_transfer(
            &settings,
            &connections,
            &groups,
            SecretBackendType::LibSecret,
            SecretBackendType::PortableEncryptedFile,
        );

        assert_eq!(plan.items.len(), 2);
        assert_eq!(
            plan.collisions,
            vec![vec!["web".to_string(), "web".to_string()]],
            "the flat destination key must be reported as shared, not silently reused"
        );

        // The same pair is fine when the destination keeps groups apart.
        let keyring_plan = plan_credential_transfer(
            &settings,
            &connections,
            &groups,
            SecretBackendType::PortableEncryptedFile,
            SecretBackendType::LibSecret,
        );
        assert!(
            keyring_plan.collisions.is_empty(),
            "the keyring key carries the group path, so there is no collision"
        );
    }

    #[test]
    fn plan_skips_variables_that_point_at_an_external_entry() {
        let settings = rustconn_core::config::AppSettings {
            global_variables: vec![
                rustconn_core::variables::Variable {
                    name: "ours".to_string(),
                    value: String::new(),
                    is_secret: true,
                    description: None,
                    kdbx_entry_path: None,
                    vault_entry_name: None,
                },
                rustconn_core::variables::Variable {
                    name: "theirs".to_string(),
                    value: String::new(),
                    is_secret: true,
                    description: None,
                    kdbx_entry_path: Some("Internet/MyRouter".to_string()),
                    vault_entry_name: None,
                },
                rustconn_core::variables::Variable {
                    name: "plain".to_string(),
                    value: "not a secret".to_string(),
                    is_secret: false,
                    description: None,
                    kdbx_entry_path: None,
                    vault_entry_name: None,
                },
            ],
            ..rustconn_core::config::AppSettings::default()
        };

        let plan = plan_credential_transfer(
            &settings,
            &[],
            &[],
            SecretBackendType::LibSecret,
            SecretBackendType::PortableEncryptedFile,
        );

        let labels: Vec<&str> = plan.items.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(
            labels,
            vec!["ours"],
            "only secret variables RustConn itself stores are ours to copy"
        );
    }
}
