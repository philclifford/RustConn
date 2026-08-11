//! Property tests for the 3-state credential storage migration
//!
//! Feature: UX-7b — `SecretSettings` exposes `*_storage()` /
//! `set_*_storage()` helpers built on top of legacy
//! `*_password_encrypted` + `*_save_to_keyring` pairs. Old configs must
//! continue to round-trip through the new API.

use proptest::prelude::*;
use rustconn_core::config::SecretSettings;
use rustconn_core::secret::CredentialStorage;
use secrecy::SecretString;

/// Reduces a legacy `(encrypted_present, save_to_keyring)` pair into a
/// canonical [`CredentialStorage`] using the same rule the production
/// helpers apply. Centralised here so the property tests document the
/// expected mapping table.
fn expected_storage(encrypted_present: bool, save_to_keyring: bool) -> CredentialStorage {
    if save_to_keyring {
        CredentialStorage::SystemKeyring
    } else if encrypted_present {
        CredentialStorage::EncryptedFile
    } else {
        CredentialStorage::None
    }
}

// Property: `from_legacy` is total and matches the canonical mapping.
proptest! {
    #[test]
    fn from_legacy_matches_canonical_mapping(
        encrypted in any::<bool>(),
        keyring in any::<bool>(),
    ) {
        let actual = CredentialStorage::from_legacy(encrypted, keyring);
        let expected = expected_storage(encrypted, keyring);
        prop_assert_eq!(actual, expected);
    }
}

// Property: setting then reading a storage choice via the helper API
// preserves the value for every backend.
proptest! {
    #[test]
    fn storage_round_trip(
        kdbx in 0u8..3,
        bw in 0u8..3,
        op in 0u8..3,
        pb in 0u8..3,
    ) {
        let to_storage = |n: u8| match n {
            1 => CredentialStorage::EncryptedFile,
            2 => CredentialStorage::SystemKeyring,
            _ => CredentialStorage::None,
        };
        let kdbx_choice = to_storage(kdbx);
        let bw_choice = to_storage(bw);
        let op_choice = to_storage(op);
        let pb_choice = to_storage(pb);

        let mut settings = SecretSettings::default();
        settings.set_kdbx_storage(kdbx_choice);
        settings.set_bitwarden_storage(bw_choice);
        settings.set_onepassword_storage(op_choice);
        settings.set_passbolt_storage(pb_choice);

        // Encrypted-file selections need a sentinel blob to make the read
        // back report `EncryptedFile`. The production GUI populates this
        // blob via `encrypt_password()` before save; tests populate it
        // directly to focus on the read/write API.
        if kdbx_choice == CredentialStorage::EncryptedFile {
            settings.kdbx_password_encrypted = Some("placeholder".to_string());
        }
        if bw_choice == CredentialStorage::EncryptedFile {
            settings.bitwarden_password_encrypted = Some("placeholder".to_string());
        }
        if op_choice == CredentialStorage::EncryptedFile {
            settings.onepassword_service_account_token_encrypted =
                Some("placeholder".to_string());
        }
        if pb_choice == CredentialStorage::EncryptedFile {
            settings.passbolt_passphrase_encrypted = Some("placeholder".to_string());
        }

        prop_assert_eq!(settings.kdbx_storage(), kdbx_choice);
        prop_assert_eq!(settings.bitwarden_storage(), bw_choice);
        prop_assert_eq!(settings.onepassword_storage(), op_choice);
        prop_assert_eq!(settings.passbolt_storage(), pb_choice);
    }
}

// Property: legacy configs that combine both `*_save_to_keyring = true`
// and an encrypted blob deterministically prefer the keyring choice. This
// guards the resolution of conflicting legacy data the user could have
// produced before the 3-state UI.
proptest! {
    #[test]
    fn legacy_conflict_prefers_keyring(
        encrypted in any::<bool>(),
    ) {
        let mut settings = SecretSettings::default();
        settings.kdbx_save_to_keyring = true;
        settings.bitwarden_save_to_keyring = true;
        settings.onepassword_save_to_keyring = true;
        settings.passbolt_save_to_keyring = true;
        if encrypted {
            settings.kdbx_password_encrypted = Some("legacy".to_string());
            settings.bitwarden_password_encrypted = Some("legacy".to_string());
            settings.onepassword_service_account_token_encrypted = Some("legacy".to_string());
            settings.passbolt_passphrase_encrypted = Some("legacy".to_string());
        }

        prop_assert_eq!(settings.kdbx_storage(), CredentialStorage::SystemKeyring);
        prop_assert_eq!(settings.bitwarden_storage(), CredentialStorage::SystemKeyring);
        prop_assert_eq!(
            settings.onepassword_storage(),
            CredentialStorage::SystemKeyring
        );
        prop_assert_eq!(settings.passbolt_storage(), CredentialStorage::SystemKeyring);
    }
}

// Property: switching to `None` clears both legacy fields, regardless of
// their previous state. This is the path that "Don't save" must take.
proptest! {
    #[test]
    fn switching_to_none_clears_legacy_fields(
        encrypted_was in any::<bool>(),
        keyring_was in any::<bool>(),
    ) {
        let mut settings = SecretSettings::default();
        settings.kdbx_password_encrypted = encrypted_was.then(|| "x".to_string());
        settings.kdbx_save_to_keyring = keyring_was;
        settings.bitwarden_password_encrypted = encrypted_was.then(|| "x".to_string());
        settings.bitwarden_save_to_keyring = keyring_was;
        settings.onepassword_service_account_token_encrypted =
            encrypted_was.then(|| "x".to_string());
        settings.onepassword_save_to_keyring = keyring_was;
        settings.passbolt_passphrase_encrypted = encrypted_was.then(|| "x".to_string());
        settings.passbolt_save_to_keyring = keyring_was;

        settings.set_kdbx_storage(CredentialStorage::None);
        settings.set_bitwarden_storage(CredentialStorage::None);
        settings.set_onepassword_storage(CredentialStorage::None);
        settings.set_passbolt_storage(CredentialStorage::None);

        prop_assert!(settings.kdbx_password_encrypted.is_none());
        prop_assert!(!settings.kdbx_save_to_keyring);
        prop_assert!(settings.bitwarden_password_encrypted.is_none());
        prop_assert!(!settings.bitwarden_save_to_keyring);
        prop_assert!(
            settings
                .onepassword_service_account_token_encrypted
                .is_none()
        );
        prop_assert!(!settings.onepassword_save_to_keyring);
        prop_assert!(settings.passbolt_passphrase_encrypted.is_none());
        prop_assert!(!settings.passbolt_save_to_keyring);
    }
}

// Property: switching to `SystemKeyring` clears the encrypted blob (so
// the next save doesn't persist a stale machine-encrypted credential)
// and sets the keyring flag.
proptest! {
    #[test]
    fn switching_to_keyring_clears_encrypted(
        encrypted_was in any::<bool>(),
    ) {
        let mut settings = SecretSettings::default();
        if encrypted_was {
            settings.kdbx_password_encrypted = Some("x".to_string());
            settings.bitwarden_password_encrypted = Some("x".to_string());
            settings.onepassword_service_account_token_encrypted = Some("x".to_string());
            settings.passbolt_passphrase_encrypted = Some("x".to_string());
        }

        settings.set_kdbx_storage(CredentialStorage::SystemKeyring);
        settings.set_bitwarden_storage(CredentialStorage::SystemKeyring);
        settings.set_onepassword_storage(CredentialStorage::SystemKeyring);
        settings.set_passbolt_storage(CredentialStorage::SystemKeyring);

        prop_assert!(settings.kdbx_password_encrypted.is_none());
        prop_assert!(settings.kdbx_save_to_keyring);
        prop_assert!(settings.bitwarden_password_encrypted.is_none());
        prop_assert!(settings.bitwarden_save_to_keyring);
        prop_assert!(
            settings
                .onepassword_service_account_token_encrypted
                .is_none()
        );
        prop_assert!(settings.onepassword_save_to_keyring);
        prop_assert!(settings.passbolt_passphrase_encrypted.is_none());
        prop_assert!(settings.passbolt_save_to_keyring);
    }
}
// ---------------------------------------------------------------------------
// `has_new_runtime_secret` — the Settings-dialog dirty check for runtime-only
// secrets (issue #272). `PartialEq` ignores the `#[serde(skip)]`
// `SecretString` fields, so a keyring-backed password typed into the dialog is
// invisible to it; this helper is what keeps the save path (and with it the
// keyring write) from being skipped.
// ---------------------------------------------------------------------------

/// Builds a settings pair whose only difference is the runtime KDBX password.
fn kdbx_pair(previous: Option<&str>, current: Option<&str>) -> (SecretSettings, SecretSettings) {
    let mut before = SecretSettings::default();
    before.kdbx_enabled = true;
    before.kdbx_save_to_keyring = true;
    let mut after = before.clone();
    before.kdbx_password = previous.map(|p| SecretString::from(p.to_owned()));
    after.kdbx_password = current.map(|p| SecretString::from(p.to_owned()));
    (before, after)
}

// Property: a password typed where there was none is always a change, for
// any non-empty text. This is the issue #272 path — the keyring lookup at
// startup found nothing, so the snapshot carries `None`.
proptest! {
    #[test]
    fn newly_typed_password_counts_as_a_change(
        password in "[ -~]{1,64}",
    ) {
        let (before, after) = kdbx_pair(None, Some(&password));

        // Nothing persisted changed …
        prop_assert_eq!(&before, &after);
        // … but the runtime secret did, so the save path must still run.
        prop_assert!(after.has_new_runtime_secret(&before));
    }
}

// Property: re-entering the same password is not a change. An untouched
// open/close round trip pre-fills the entry from the keyring, and must not
// trigger a disk write or a keyring round-trip.
proptest! {
    #[test]
    fn unchanged_password_is_not_a_change(
        password in "[ -~]{1,64}",
    ) {
        let (before, after) = kdbx_pair(Some(&password), Some(&password));

        prop_assert!(!after.has_new_runtime_secret(&before));
    }
}

// Property: a different password is always a change.
proptest! {
    #[test]
    fn replaced_password_counts_as_a_change(
        old in "[ -~]{1,32}",
        new in "[ -~]{1,32}",
    ) {
        prop_assume!(old != new);
        let (before, after) = kdbx_pair(Some(&old), Some(&new));

        prop_assert!(after.has_new_runtime_secret(&before));
    }
}

// Property: an absent (`None`) collected secret is never a change, even when
// the snapshot holds one. The dialog leaves password entries it could not
// load empty, and losing that distinction would make every dialog close look
// dirty.
proptest! {
    #[test]
    fn cleared_password_is_not_a_change(
        password in "[ -~]{1,64}",
    ) {
        let (before, after) = kdbx_pair(Some(&password), None);

        prop_assert!(!after.has_new_runtime_secret(&before));
    }
}

/// Writes one runtime-secret field, used to table-drive the coverage test.
type SecretSetter = fn(&mut SecretSettings, Option<SecretString>);

// Every runtime secret field participates, not just the KDBX one — the same
// deferred-keyring-save path serves Bitwarden, 1Password and Passbolt.
#[test]
fn every_runtime_secret_field_is_checked() {
    let base = SecretSettings::default();
    let secret = || Some(SecretString::from("s3cr3t".to_owned()));

    let cases: [(&str, SecretSetter); 6] = [
        ("kdbx_password", |s, v| s.kdbx_password = v),
        ("bitwarden_password", |s, v| s.bitwarden_password = v),
        ("bitwarden_client_id", |s, v| s.bitwarden_client_id = v),
        ("bitwarden_client_secret", |s, v| {
            s.bitwarden_client_secret = v;
        }),
        ("onepassword_service_account_token", |s, v| {
            s.onepassword_service_account_token = v;
        }),
        ("passbolt_passphrase", |s, v| s.passbolt_passphrase = v),
    ];

    for (field, set) in cases {
        let mut changed = base.clone();
        set(&mut changed, secret());
        assert!(
            changed.has_new_runtime_secret(&base),
            "{field} must be treated as a new runtime secret"
        );
    }

    assert!(
        !base.has_new_runtime_secret(&base),
        "identical settings must never look dirty"
    );
}

// ---------------------------------------------------------------------------
// `apply_storage_persistence` — the storage choice decides what lands on disk.
//
// Until 0.19.19 this lived inline in the GUI's `AppState::update_settings` and
// only KDBX honoured "keyring storage → no blob on disk"; the Bitwarden password,
// the Bitwarden API key pair, the 1Password token and the Passbolt passphrase
// were persisted regardless of what the user had selected. The rule is now one
// pure method per the pattern `switching_to_keyring_clears_encrypted` set.
// ---------------------------------------------------------------------------

/// Placeholder blob the Settings dialog collects for an encrypted-file
/// selection. It marks the *intent* to encrypt; `apply_storage_persistence`
/// replaces it with real ciphertext. Mirrors the literals in
/// `secrets_tab::collect_secret_settings`.
const COLLECT_PLACEHOLDER: &str = "encrypted_password_placeholder";

/// Builds settings where every backend holds `secret` as its runtime credential,
/// with KeePass and the Bitwarden API key path both switched on.
fn every_backend_holding(secret: &str) -> SecretSettings {
    let mut settings = SecretSettings::default();
    settings.kdbx_enabled = true;
    settings.kdbx_use_password = true;
    settings.bitwarden_use_api_key = true;
    settings.kdbx_password = Some(SecretString::from(secret.to_owned()));
    settings.bitwarden_password = Some(SecretString::from(secret.to_owned()));
    settings.bitwarden_client_id = Some(SecretString::from(secret.to_owned()));
    settings.bitwarden_client_secret = Some(SecretString::from(secret.to_owned()));
    settings.onepassword_service_account_token = Some(SecretString::from(secret.to_owned()));
    settings.passbolt_passphrase = Some(SecretString::from(secret.to_owned()));
    settings
}

/// Selects one storage choice for all four backends.
fn select_everywhere(settings: &mut SecretSettings, storage: CredentialStorage) {
    settings.set_kdbx_storage(storage);
    settings.set_bitwarden_storage(storage);
    settings.set_onepassword_storage(storage);
    settings.set_passbolt_storage(storage);
}

/// Plants the placeholder blobs an encrypted-file selection arrives with. The
/// legacy `(blob, keyring_flag)` encoding can only *represent*
/// [`CredentialStorage::EncryptedFile`] while a blob is present, which is
/// exactly why the dialog collects a sentinel.
fn plant_placeholder_blobs(settings: &mut SecretSettings) {
    settings.kdbx_password_encrypted = Some(COLLECT_PLACEHOLDER.to_string());
    settings.bitwarden_password_encrypted = Some(COLLECT_PLACEHOLDER.to_string());
    settings.bitwarden_client_id_encrypted = Some(COLLECT_PLACEHOLDER.to_string());
    settings.bitwarden_client_secret_encrypted = Some(COLLECT_PLACEHOLDER.to_string());
    settings.onepassword_service_account_token_encrypted = Some(COLLECT_PLACEHOLDER.to_string());
    settings.passbolt_passphrase_encrypted = Some(COLLECT_PLACEHOLDER.to_string());
}

/// Every persisted `*_encrypted` blob, paired with the field name for messages.
fn encrypted_blobs(settings: &SecretSettings) -> [(&'static str, Option<&String>); 6] {
    [
        (
            "kdbx_password_encrypted",
            settings.kdbx_password_encrypted.as_ref(),
        ),
        (
            "bitwarden_password_encrypted",
            settings.bitwarden_password_encrypted.as_ref(),
        ),
        (
            "bitwarden_client_id_encrypted",
            settings.bitwarden_client_id_encrypted.as_ref(),
        ),
        (
            "bitwarden_client_secret_encrypted",
            settings.bitwarden_client_secret_encrypted.as_ref(),
        ),
        (
            "onepassword_service_account_token_encrypted",
            settings
                .onepassword_service_account_token_encrypted
                .as_ref(),
        ),
        (
            "passbolt_passphrase_encrypted",
            settings.passbolt_passphrase_encrypted.as_ref(),
        ),
    ]
}

/// Every runtime `SecretString`, paired with the field name for messages.
fn runtime_secrets(settings: &SecretSettings) -> [(&'static str, Option<&SecretString>); 6] {
    [
        ("kdbx_password", settings.kdbx_password.as_ref()),
        ("bitwarden_password", settings.bitwarden_password.as_ref()),
        ("bitwarden_client_id", settings.bitwarden_client_id.as_ref()),
        (
            "bitwarden_client_secret",
            settings.bitwarden_client_secret.as_ref(),
        ),
        (
            "onepassword_service_account_token",
            settings.onepassword_service_account_token.as_ref(),
        ),
        ("passbolt_passphrase", settings.passbolt_passphrase.as_ref()),
    ]
}

// Property: with the keyring selected, no backend writes an encrypted blob —
// not even a stale one that reached the save. The keyring is the persistence
// layer, so duplicating the secret on disk would contradict the choice. The
// runtime copies must survive: they are the only thing the deferred keyring
// write has to store.
proptest! {
    #[test]
    fn keyring_storage_writes_no_encrypted_blob(
        secret in "[ -~]{1,64}",
    ) {
        let mut settings = every_backend_holding(&secret);
        select_everywhere(&mut settings, CredentialStorage::SystemKeyring);
        // A stale blob left over from an earlier "Encrypted file" era.
        plant_placeholder_blobs(&mut settings);

        settings.apply_storage_persistence();

        for (field, blob) in encrypted_blobs(&settings) {
            prop_assert!(
                blob.is_none(),
                "{field} must stay empty when the keyring is the persistence layer"
            );
        }
        for (field, runtime) in runtime_secrets(&settings) {
            prop_assert!(
                runtime.is_some(),
                "{field} is the only copy the keyring write has — it must survive"
            );
        }
    }
}

// The encrypted-file choice is the other half of the same rule: every backend
// must end up with real ciphertext, not the sentinel the dialog collected.
// 1Password and Passbolt never had an encryption step at all, so their blobs
// used to be persisted as the literal placeholder string.
//
// Plain `#[test]`: each blob costs one Argon2id derivation (16 MiB), so this is
// deliberately run once rather than 256 times.
#[test]
fn encrypted_file_storage_writes_real_ciphertext() {
    let secret = "s3cr3t-pa55phra5e";
    let mut settings = every_backend_holding(secret);
    select_everywhere(&mut settings, CredentialStorage::EncryptedFile);
    plant_placeholder_blobs(&mut settings);

    settings.apply_storage_persistence();

    for (field, blob) in encrypted_blobs(&settings) {
        let blob = blob.unwrap_or_else(|| panic!("{field} must be populated"));
        assert_ne!(
            blob, COLLECT_PLACEHOLDER,
            "{field} still holds the collect-time sentinel instead of ciphertext"
        );
        assert!(
            !blob.contains(secret),
            "{field} must not contain the plaintext secret"
        );
    }

    // Round-trip two of them to prove the blobs are decryptable, one from the
    // path that always worked (KDBX) and one from the path that did not
    // (1Password).
    let mut reloaded = SecretSettings::default();
    reloaded.kdbx_password_encrypted = settings.kdbx_password_encrypted.clone();
    reloaded.onepassword_service_account_token_encrypted =
        settings.onepassword_service_account_token_encrypted.clone();
    assert!(reloaded.decrypt_password(), "KDBX blob must decrypt");
    assert!(
        reloaded.decrypt_onepassword_token(),
        "1Password blob must decrypt"
    );

    use secrecy::ExposeSecret;
    assert_eq!(
        reloaded
            .kdbx_password
            .as_ref()
            .map(|p| p.expose_secret().to_owned()),
        Some(secret.to_owned())
    );
    assert_eq!(
        reloaded
            .onepassword_service_account_token
            .as_ref()
            .map(|t| t.expose_secret().to_owned()),
        Some(secret.to_owned())
    );
}

// Property: "Don't save" persists nothing for any backend, and drops the
// runtime copy too so a later save cannot resurrect it. (The GUI restores
// runtime-only fields from the previous settings after the disk write, so the
// running session keeps working.)
proptest! {
    #[test]
    fn dont_save_storage_persists_nothing(
        secret in "[ -~]{1,64}",
    ) {
        let mut settings = every_backend_holding(&secret);
        // API-key auth off: with it on the key pair intentionally keeps being
        // encrypted, because it is the *alternative* to the master password and
        // has no selector of its own.
        settings.bitwarden_use_api_key = false;
        settings.bitwarden_client_id = None;
        settings.bitwarden_client_secret = None;
        select_everywhere(&mut settings, CredentialStorage::None);

        settings.apply_storage_persistence();

        for (field, blob) in encrypted_blobs(&settings) {
            prop_assert!(blob.is_none(), "{field} must stay empty for \"Don't save\"");
        }
        prop_assert!(settings.kdbx_password.is_none());
        prop_assert!(settings.bitwarden_password.is_none());
        prop_assert!(settings.onepassword_service_account_token.is_none());
        prop_assert!(settings.passbolt_passphrase.is_none());
    }
}

// ---------------------------------------------------------------------------
// The migration invariant: a storage-mode change *alone* must never leave a
// secret with no persistence at all.
//
// The failing sequence: the password lives in an encrypted blob and was
// decrypted into memory at startup, so the dialog's entry is what the keyring
// could not pre-fill — blank. Switching the combo to "System keyring" collected
// nothing, `apply_storage_persistence` dropped the blob, and the deferred
// keyring write had nothing to store. The session kept working from memory, so
// the loss stayed invisible until the next restart.
// ---------------------------------------------------------------------------

/// Reports whether a backend's secret has somewhere to live: an encrypted blob
/// for the file choice, a runtime copy to hand over for the keyring choice.
/// "Don't save" is a request for no persistence, so it is vacuously satisfied.
fn is_persisted(
    storage: CredentialStorage,
    blob: Option<&String>,
    runtime: Option<&SecretString>,
) -> bool {
    match storage {
        CredentialStorage::None => true,
        CredentialStorage::EncryptedFile => blob.is_some(),
        CredentialStorage::SystemKeyring => runtime.is_some(),
    }
}

#[test]
fn switching_to_keyring_carries_the_secret_over() {
    let secret = "database-unlock-phrase";

    // In force before the dialog opened: encrypted on disk, decrypted in memory.
    let mut previous = every_backend_holding(secret);
    select_everywhere(&mut previous, CredentialStorage::EncryptedFile);
    plant_placeholder_blobs(&mut previous);

    // What the dialog collects: the new choice, and no secret at all, because
    // every entry was blank.
    let mut collected = SecretSettings::default();
    collected.kdbx_enabled = true;
    collected.kdbx_use_password = true;
    collected.bitwarden_use_api_key = true;
    select_everywhere(&mut collected, CredentialStorage::SystemKeyring);

    collected.carry_over_runtime_secrets(&previous);
    collected.apply_storage_persistence();

    for (field, blob) in encrypted_blobs(&collected) {
        assert!(
            blob.is_none(),
            "{field} must not stay on disk once the keyring is the persistence layer"
        );
    }
    for (field, runtime) in runtime_secrets(&collected) {
        assert!(
            runtime.is_some(),
            "{field} was stranded: no blob on disk and nothing for the keyring write"
        );
    }

    assert!(is_persisted(
        collected.kdbx_storage(),
        collected.kdbx_password_encrypted.as_ref(),
        collected.kdbx_password.as_ref(),
    ));
    assert!(is_persisted(
        collected.bitwarden_storage(),
        collected.bitwarden_password_encrypted.as_ref(),
        collected.bitwarden_password.as_ref(),
    ));
    assert!(is_persisted(
        collected.onepassword_storage(),
        collected
            .onepassword_service_account_token_encrypted
            .as_ref(),
        collected.onepassword_service_account_token.as_ref(),
    ));
    assert!(is_persisted(
        collected.passbolt_storage(),
        collected.passbolt_passphrase_encrypted.as_ref(),
        collected.passbolt_passphrase.as_ref(),
    ));
}

// Property: the carry-over never *invents* persistence and never overwrites
// what the dialog did collect. A blank entry means "I did not retype it", so
// the previous value is reused; a typed one always wins.
proptest! {
    #[test]
    fn carry_over_never_overwrites_a_collected_secret(
        old in "[ -~]{1,32}",
        new in "[ -~]{1,32}",
    ) {
        prop_assume!(old != new);
        use secrecy::ExposeSecret;

        let mut previous = SecretSettings::default();
        previous.kdbx_enabled = true;
        previous.kdbx_save_to_keyring = true;
        previous.kdbx_password = Some(SecretString::from(old.clone()));

        let mut collected = previous.clone();
        collected.kdbx_password = Some(SecretString::from(new.clone()));
        collected.carry_over_runtime_secrets(&previous);

        prop_assert_eq!(
            collected.kdbx_password.as_ref().map(|p| p.expose_secret()),
            Some(new.as_str())
        );

        // And with nothing collected, the previous value fills the gap.
        let mut blank = previous.clone();
        blank.kdbx_password = None;
        blank.carry_over_runtime_secrets(&previous);
        prop_assert_eq!(
            blank.kdbx_password.as_ref().map(|p| p.expose_secret()),
            Some(old.as_str())
        );
    }
}

// Property: with "Don't save" or "Encrypted file" selected there is nothing to
// carry — the keyring is the only choice whose secret has no second copy.
proptest! {
    #[test]
    fn carry_over_only_applies_to_keyring_storage(
        secret in "[ -~]{1,64}",
    ) {
        for storage in [CredentialStorage::None, CredentialStorage::EncryptedFile] {
            let mut previous = every_backend_holding(&secret);
            select_everywhere(&mut previous, storage);
            plant_placeholder_blobs(&mut previous);

            let mut collected = previous.clone();
            for setter in runtime_clearers() {
                setter(&mut collected);
            }
            collected.carry_over_runtime_secrets(&previous);

            for (field, runtime) in runtime_secrets(&collected) {
                prop_assert!(
                    runtime.is_none(),
                    "{field} must not be resurrected for {storage:?} storage"
                );
            }
        }
    }
}

/// Clears one runtime-secret field, used to blank a whole settings value.
fn runtime_clearers() -> [fn(&mut SecretSettings); 6] {
    [
        |s| s.kdbx_password = None,
        |s| s.bitwarden_password = None,
        |s| s.bitwarden_client_id = None,
        |s| s.bitwarden_client_secret = None,
        |s| s.onepassword_service_account_token = None,
        |s| s.passbolt_passphrase = None,
    ]
}

// ---------------------------------------------------------------------------
// `keyring_revocations` — leaving the keyring must remove what it still holds.
//
// Switching a backend to "Encrypted file" or "Don't save" used to leave the
// keyring entry behind forever, so a stored secret could never be revoked —
// the mirror image of the leak issue #272 fixed.
// ---------------------------------------------------------------------------

#[test]
fn leaving_the_keyring_revokes_every_backend() {
    let mut previous = SecretSettings::default();
    previous.kdbx_enabled = true;
    previous.kdbx_use_password = true;
    previous.kdbx_save_to_keyring = true;
    previous.bitwarden_save_to_keyring = true;
    previous.bitwarden_use_api_key = true;
    previous.onepassword_save_to_keyring = true;
    previous.passbolt_save_to_keyring = true;

    let mut current = previous.clone();
    select_everywhere(&mut current, CredentialStorage::None);

    let revocations = current.keyring_revocations(&previous);
    assert!(revocations.any());
    assert!(revocations.kdbx_password);
    assert!(revocations.bitwarden_password);
    assert!(revocations.bitwarden_api_credentials);
    assert!(revocations.onepassword_token);
    assert!(revocations.passbolt_passphrase);
}

#[test]
fn staying_on_the_keyring_revokes_nothing() {
    let mut settings = SecretSettings::default();
    settings.kdbx_enabled = true;
    settings.kdbx_use_password = true;
    settings.kdbx_save_to_keyring = true;
    settings.bitwarden_save_to_keyring = true;
    settings.bitwarden_use_api_key = true;
    settings.onepassword_save_to_keyring = true;
    settings.passbolt_save_to_keyring = true;

    let revocations = settings.keyring_revocations(&settings.clone());
    assert!(
        !revocations.any(),
        "an unchanged keyring selection must not delete anything"
    );

    // Clearing a password entry is not a revocation either — a blank field means
    // "I did not retype it", which is what `cleared_password_is_not_a_change`
    // pins for the dirty check.
    let mut blanked = settings.clone();
    blanked.kdbx_password = None;
    blanked.bitwarden_password = None;
    blanked.onepassword_service_account_token = None;
    blanked.passbolt_passphrase = None;
    assert!(!blanked.keyring_revocations(&settings).any());
}

#[test]
fn disabling_a_backend_revokes_its_keyring_entry() {
    let mut previous = SecretSettings::default();
    previous.kdbx_enabled = true;
    previous.kdbx_use_password = true;
    previous.kdbx_save_to_keyring = true;
    previous.bitwarden_save_to_keyring = true;
    previous.bitwarden_use_api_key = true;

    // KeePass switched off entirely, and Bitwarden's API-key path turned off
    // while the master password stays in the keyring.
    let mut current = previous.clone();
    current.kdbx_enabled = false;
    current.bitwarden_use_api_key = false;

    let revocations = current.keyring_revocations(&previous);
    assert!(revocations.kdbx_password);
    assert!(revocations.bitwarden_api_credentials);
    assert!(
        !revocations.bitwarden_password,
        "the master password is still keyring-backed"
    );
}

// ---------------------------------------------------------------------------
// M-PUBLIC-DEBUG: the type holds six `Option<SecretString>` plus ciphertext
// blobs, so its `Debug` needs a test even though `secrecy` redacts itself.
// ---------------------------------------------------------------------------

#[test]
fn debug_never_renders_a_secret() {
    // Distinctive enough that a substring match cannot pass by accident.
    let secret = "Zq7-unique-plaintext-marker-Xy2";
    let settings = every_backend_holding(secret);

    let rendered = format!("{settings:?}");

    assert!(
        rendered.contains("SecretSettings"),
        "Debug must still identify the type: {rendered}"
    );
    for (field, _) in runtime_secrets(&settings) {
        assert!(
            rendered.contains(field),
            "Debug must still name {field} so the redaction is visible"
        );
    }
    assert!(
        !rendered.contains(secret),
        "Debug leaked a runtime secret: {rendered}"
    );

    // Same guarantee once the secrets have been encrypted to disk: the blobs are
    // ciphertext, never plaintext.
    let mut encrypted = settings.clone();
    select_everywhere(&mut encrypted, CredentialStorage::EncryptedFile);
    plant_placeholder_blobs(&mut encrypted);
    encrypted.apply_storage_persistence();
    assert!(!format!("{encrypted:?}").contains(secret));
}
