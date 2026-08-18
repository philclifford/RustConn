//! Portable encrypted-file secret backend (passphrase-based, cloud-syncable).
//!
//! A variant of [`super::encrypted_file::EncryptedFileBackend`] whose encryption
//! key comes from a **user-supplied passphrase** instead of a machine-specific
//! key, so the file can live in any cloud-synced directory and be opened on any
//! machine — including across Linux and macOS (issue #293).
//!
//! ## Key hierarchy
//!
//! The store uses a key-encryption-key / data-encryption-key split rather than
//! deriving a key per entry:
//!
//! ```text
//! passphrase ──Argon2id(kdf_salt, kdf_params)──▶ KEK
//!                                                 │
//!                          wrapped_key ──open──▶ DEK ──▶ every entry
//! ```
//!
//! One Argon2id derivation unlocks the whole file; each entry then costs a
//! single AES-256-GCM pass. Deriving per entry instead would make one
//! `retrieve` cost a full KDF (~0.5 s) and a 50-entry migration cost fifty of
//! them. The split also buys two properties the per-entry scheme cannot offer:
//! the passphrase can be verified by unwrapping the key (no need to guess which
//! entry to trial-decrypt), and changing the passphrase rewraps 32 bytes rather
//! than re-encrypting every credential.
//!
//! ## File format
//!
//! ```json
//! {
//!   "format_version": 1,
//!   "kdf": "argon2id",
//!   "kdf_params": { "m_cost": 65536, "t_cost": 3, "p_cost": 4 },
//!   "kdf_salt": "<base64, 16 bytes>",
//!   "wrapped_key": "<base64: nonce(12) + DEK ciphertext(32) + tag(16)>",
//!   "entries": {
//!     "rustconn/my-server": "<base64: nonce(12) + ciphertext + tag(16)>"
//!   }
//! }
//! ```
//!
//! `kdf_params` are per-file, so raising the defaults leaves existing files
//! openable: a store keeps the cost it was created with until it is rewritten.
//! Entry blobs and the wrapped key are bound to their role by AAD
//! ([`AAD_ENTRY`] / [`AAD_WRAPPED_KEY`]), so moving one into the other's slot
//! fails authentication instead of silently decrypting.
//!
//! ## Unlock model
//!
//! The backend starts **locked** (no passphrase). Any `store`/`retrieve`/
//! `delete` while locked returns [`SecretError::PassphraseRequired`]; the GUI
//! intercepts it, prompts, and calls
//! [`PortableEncryptedFileBackend::set_passphrase`]. A wrong passphrase is
//! reported as [`SecretError::IncorrectPassphrase`] so the caller can tell "ask
//! again" from "this file is broken".
//!
//! ## Cloud-sync ceiling
//!
//! Every write is a read-modify-write of the whole JSON file, so two machines
//! that both write while offline resolve as last-writer-wins and one set of
//! additions is lost. The DEK cache is keyed on `kdf_salt` *and* `wrapped_key`
//! so a file replaced by the sync client — or rewrapped under the same salt — is
//! noticed rather than decrypted with a stale key, but concurrent edits from
//! different machines are not merged.
//!
//! Within one process the interleaving is closed: `store` and `delete` hold a
//! mutex across the whole read-modify-write, so two overlapping writes queue
//! instead of losing an entry. Nothing analogous exists *between* processes —
//! two RustConn instances on the same synced folder, or a second machine — and
//! an advisory `flock` would not help across the network and FUSE mounts this
//! backend targets.
//!
//! ponytail: whole-file rewrite per entry, fine for the single-writer-at-a-time
//! use this targets; split the map into one file per entry if concurrent writes
//! from several machines become a real workflow, so the sync client merges them.
//!
//! ## Secret hygiene
//!
//! The passphrase is held in a `SecretString`; the DEK lives in
//! `Zeroizing<[u8; 32]>`; all intermediate plaintext is `Zeroizing`.
//! [`StoredCredentials`] (shared with [`super::encrypted_file`]) wipes on drop
//! and redacts in `Debug`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError, RwLock};

use async_trait::async_trait;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use super::backend::SecretBackend;
use super::encrypted_file::StoredCredentials;
use super::local_crypto::{
    AAD_ENTRY, AAD_WRAPPED_KEY, PassphraseKdfParams, SETTINGS_SALT_LEN, derive_passphrase_key,
    fill_random, open_with_key, seal_with_key,
};
use crate::error::{SecretError, SecretResult};
use crate::models::Credentials;

/// Current portable store format version.
const FORMAT_VERSION: u32 = 1;

/// The only KDF this format has ever used.
const KDF_ARGON2ID: &str = "argon2id";

/// Default file name for new portable credential stores.
pub const PORTABLE_STORE_FILE_NAME: &str = "credentials-portable.enc";

/// Owner-only permission bits (`rw-------`).
#[cfg(unix)]
const STORE_FILE_MODE: u32 = 0o600;

/// Largest store file this build will read into memory (8 MiB).
///
/// The file arrives from a shared folder, so its *size* is untrusted input just
/// as much as its header is — and the read happens before any header validation
/// can run. One entry is a base64 blob of a small JSON object (~700 bytes), so
/// 8 MiB still admits an order of magnitude more entries than [`MAX_ENTRIES`]
/// allows. Anything past that is a corrupt or hostile file, and rejecting it by
/// `metadata` costs nothing where `read` would cost the whole allocation.
const MAX_STORE_BYTES: u64 = 8 * 1024 * 1024;

/// Largest entry count this build will accept from a store file.
///
/// Bounds the `BTreeMap` that `serde_json` builds before anything validates the
/// header. Ten thousand credentials is far past any plausible connection list.
const MAX_ENTRIES: usize = 10_000;

/// A 256-bit data-encryption key, wiped on drop.
type Dek = Zeroizing<[u8; 32]>;

// ──────────────────────────────────────────────────────────────────────────────
// File format
// ──────────────────────────────────────────────────────────────────────────────

/// On-disk JSON structure of the portable credential store.
///
/// This is the single declaration of the format; [`super::migration`] reads and
/// writes it through the helpers here rather than restating the shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PortableStoreFile {
    /// Format version (currently 1).
    format_version: u32,
    /// KDF algorithm name (always `"argon2id"`).
    kdf: String,
    /// KDF parameters — per-file, so a cost bump leaves old files openable.
    kdf_params: PassphraseKdfParams,
    /// Base64 salt fed to Argon2id together with the passphrase.
    kdf_salt: String,
    /// Base64 data-encryption key, sealed under the passphrase-derived KEK.
    wrapped_key: String,
    /// Map of `connection_id → base64(nonce + ciphertext + tag)`.
    #[serde(default)]
    pub(crate) entries: BTreeMap<String, String>,
}

impl PortableStoreFile {
    /// Creates an empty store protected by `passphrase`, returning its DEK.
    ///
    /// The data-encryption key is random, so two stores created from the same
    /// passphrase share no key material.
    ///
    /// # Errors
    /// Returns [`SecretError::StoreFailed`] if the RNG, the Argon2 derivation or
    /// the key wrapping fails.
    fn create(passphrase: &[u8]) -> SecretResult<(Self, Dek)> {
        let kdf_params = PassphraseKdfParams::default();

        let mut salt = [0u8; SETTINGS_SALT_LEN];
        fill_random(&mut salt).map_err(SecretError::StoreFailed)?;

        let mut dek: Dek = Zeroizing::new([0u8; 32]);
        fill_random(dek.as_mut()).map_err(SecretError::StoreFailed)?;

        let kek = derive_passphrase_key(passphrase, &salt, &kdf_params)
            .map_err(SecretError::StoreFailed)?;
        let wrapped =
            seal_with_key(&kek, AAD_WRAPPED_KEY, dek.as_ref()).map_err(SecretError::StoreFailed)?;

        Ok((
            Self {
                format_version: FORMAT_VERSION,
                kdf: KDF_ARGON2ID.to_owned(),
                kdf_params,
                kdf_salt: data_encoding::BASE64.encode(&salt),
                wrapped_key: data_encoding::BASE64.encode(&wrapped),
                entries: BTreeMap::new(),
            },
            dek,
        ))
    }

    /// Validates the header this crate is willing to open.
    ///
    /// # Errors
    /// Returns [`SecretError::RetrieveFailed`] for a newer format version or an
    /// unknown KDF — both mean "written by something this build does not
    /// understand", and guessing would corrupt the file on the next write.
    fn check_header(&self) -> SecretResult<()> {
        if self.format_version > FORMAT_VERSION {
            return Err(SecretError::RetrieveFailed(format!(
                "portable store version {} is newer than supported ({FORMAT_VERSION}); \
                 update RustConn to open it",
                self.format_version
            )));
        }
        // Version 0 never existed. Accepting it would mean parsing an unknown
        // shape as v1 — the same "guess and corrupt on the next write" outcome
        // the newer-version check exists to prevent, and the likelier reading is
        // a file whose `format_version` was lost to a bad merge or a truncated
        // sync rather than one this build can open.
        if self.format_version < FORMAT_VERSION {
            return Err(SecretError::RetrieveFailed(format!(
                "portable store declares format version {}, which no RustConn release \
                 has written; refusing to open it",
                self.format_version
            )));
        }
        if self.kdf != KDF_ARGON2ID {
            return Err(SecretError::RetrieveFailed(format!(
                "portable store uses unsupported key derivation '{}'",
                self.kdf
            )));
        }
        self.check_kdf_cost()?;
        // The allocation itself is bounded by MAX_STORE_BYTES in `read_store`,
        // which is the only place that can bound it — by the time there is a
        // struct to check, `serde_json` has already built the map. This ceiling
        // is here for the error message: "too many entries" is actionable where
        // a silent 10k-iteration walk on every resolve is not.
        if self.entries.len() > MAX_ENTRIES {
            return Err(SecretError::RetrieveFailed(format!(
                "portable store holds {} entries, past the {MAX_ENTRIES} ceiling; \
                 refusing to open it",
                self.entries.len()
            )));
        }
        Ok(())
    }

    /// Rejects KDF costs that would exhaust memory or stall the unlock.
    ///
    /// The point of this backend is that the file arrives from somewhere else —
    /// a cloud folder, another machine, a colleague's USB stick — so its header
    /// is untrusted input. `kdf_params` are fed straight to Argon2, and a file
    /// claiming `m_cost: 4194304` would have the process try to allocate 4 GiB
    /// before anyone typed a passphrase.
    ///
    /// The ceilings are four times the defaults (64 MiB / 3 / 4), which leaves
    /// room for a future cost bump while keeping the worst case something a
    /// desktop survives. They are deliberately *not* generous: Argon2 is a
    /// CPU-and-memory-bound loop inside `hash_password_into`, so there is no
    /// honest way to cancel a derivation once it starts — a timeout would report
    /// failure while the thread kept running. The ceiling is therefore the only
    /// real bound on how long an unlock attempt can take, which is why it is
    /// picked to be survivable rather than merely non-fatal.
    ///
    /// # Errors
    /// Returns [`SecretError::RetrieveFailed`] if any cost exceeds its ceiling.
    fn check_kdf_cost(&self) -> SecretResult<()> {
        /// Memory ceiling in KiB (256 MiB — 4× the 64 MiB default).
        const MAX_M_COST: u32 = 256 * 1024;
        /// Iteration ceiling (4× the default of 3, rounded up).
        const MAX_T_COST: u32 = 12;
        /// Parallelism ceiling (4× the default of 4).
        const MAX_P_COST: u32 = 16;

        let p = &self.kdf_params;
        if p.m_cost > MAX_M_COST || p.t_cost > MAX_T_COST || p.p_cost > MAX_P_COST {
            return Err(SecretError::RetrieveFailed(format!(
                "portable store demands implausible key-derivation cost \
                 (m={}, t={}, p={}); refusing to open it",
                p.m_cost, p.t_cost, p.p_cost
            )));
        }
        // Argon2 itself rejects a zero/too-small memory cost, but a clear error
        // beats "Invalid Argon2 passphrase params" leaking out of the KDF.
        if p.m_cost == 0 || p.t_cost == 0 || p.p_cost == 0 {
            return Err(SecretError::RetrieveFailed(
                "portable store has a zero key-derivation cost".to_string(),
            ));
        }
        Ok(())
    }

    /// Returns the raw KDF salt.
    ///
    /// # Errors
    /// Returns [`SecretError::RetrieveFailed`] if the salt is not valid base64.
    fn salt(&self) -> SecretResult<Vec<u8>> {
        data_encoding::BASE64
            .decode(self.kdf_salt.as_bytes())
            .map_err(|e| {
                SecretError::RetrieveFailed(format!("portable store salt is invalid: {e}"))
            })
    }

    /// Derives the KEK from `passphrase` and unwraps the store's DEK.
    ///
    /// # Errors
    /// Returns [`SecretError::IncorrectPassphrase`] when the wrapped key fails
    /// to authenticate — the expected outcome for a mistyped passphrase — and
    /// [`SecretError::RetrieveFailed`] for a malformed header or a failed
    /// derivation.
    pub(crate) fn unlock(&self, passphrase: &[u8]) -> SecretResult<Dek> {
        self.check_header()?;

        let salt = self.salt()?;
        let wrapped = data_encoding::BASE64
            .decode(self.wrapped_key.as_bytes())
            .map_err(|e| {
                SecretError::RetrieveFailed(format!("portable store key is malformed: {e}"))
            })?;

        let kek = derive_passphrase_key(passphrase, &salt, &self.kdf_params)
            .map_err(SecretError::RetrieveFailed)?;
        // A failure here is overwhelmingly "wrong passphrase": the KEK is the
        // only input that varies, and AES-GCM authenticated the rest.
        let plain = open_with_key(&kek, AAD_WRAPPED_KEY, &wrapped)
            .map_err(|_| SecretError::IncorrectPassphrase)?;

        let bytes: [u8; 32] = plain.as_slice().try_into().map_err(|_| {
            SecretError::RetrieveFailed("portable store key has the wrong length".to_string())
        })?;
        Ok(Zeroizing::new(bytes))
    }
}

impl PortableStoreFile {
    /// Opens the store at `path` for a batch write, creating it when absent.
    ///
    /// Used by [`super::migration`], which is one-shot and therefore wants no
    /// part in the backend's session DEK cache. An existing store must open with
    /// `passphrase`: appending under a different one would leave a file whose
    /// halves need two different passphrases and which neither can fully read.
    ///
    /// # Errors
    /// Returns [`SecretError::IncorrectPassphrase`] if an existing store does not
    /// open with `passphrase`, or a `SecretError` if the file cannot be read or a
    /// new store cannot be built.
    pub(crate) fn open_or_create_for_write(
        path: &Path,
        passphrase: &[u8],
    ) -> SecretResult<(Self, Dek)> {
        match read_store(path)? {
            Some(store) => {
                let dek = store.unlock(passphrase)?;
                Ok((store, dek))
            }
            None => Self::create(passphrase),
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// File I/O helpers
// ──────────────────────────────────────────────────────────────────────────────

/// Reads and parses the on-disk store; `None` means the file does not exist yet.
///
/// # Errors
/// Returns [`SecretError::RetrieveFailed`] if the file cannot be read or is not
/// valid JSON.
pub(crate) fn read_store(path: &Path) -> SecretResult<Option<PortableStoreFile>> {
    // Bound the size before reading. `check_header` cannot help here: the
    // allocation it would guard against has already happened by the time there
    // is a struct to validate, so the only place to bound a hostile file is
    // ahead of the read.
    //
    // One handle for both the size check and the read, on purpose. Asking
    // `fs::metadata` and then `fs::read` opens the path twice, and this path is
    // in a folder a sync client rewrites: the file measured is then not
    // necessarily the file read, which is the one thing the ceiling exists to
    // prevent. `take` re-imposes it on the handle that was actually measured, so
    // a growing or non-regular file cannot read past it either.
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(SecretError::RetrieveFailed(format!(
                "cannot read portable store: {e}"
            )));
        }
    };

    let meta = file
        .metadata()
        .map_err(|e| SecretError::RetrieveFailed(format!("cannot read portable store: {e}")))?;
    if !meta.is_file() {
        return Err(SecretError::RetrieveFailed(
            "the portable store path is not a regular file; refusing to read it".to_string(),
        ));
    }
    if meta.len() > MAX_STORE_BYTES {
        return Err(SecretError::RetrieveFailed(format!(
            "portable store is {} bytes, past the {MAX_STORE_BYTES}-byte ceiling; \
             refusing to load it",
            meta.len()
        )));
    }

    let mut bytes = Vec::with_capacity(usize::try_from(meta.len()).unwrap_or(0));
    std::io::Read::read_to_end(
        &mut std::io::Read::take(&mut file, MAX_STORE_BYTES + 1),
        &mut bytes,
    )
    .map_err(|e| SecretError::RetrieveFailed(format!("cannot read portable store: {e}")))?;
    if bytes.len() as u64 > MAX_STORE_BYTES {
        return Err(SecretError::RetrieveFailed(format!(
            "portable store grew past the {MAX_STORE_BYTES}-byte ceiling while it was being \
             read; refusing to load it"
        )));
    }

    let store: PortableStoreFile = serde_json::from_slice(&bytes)
        .map_err(|e| SecretError::RetrieveFailed(format!("portable store is corrupt: {e}")))?;
    store.check_header()?;
    Ok(Some(store))
}

/// Computes a unique sibling temp path for an atomic write.
///
/// The suffix is random rather than a fixed `.tmp` because this file lives in a
/// directory the user shares with a sync client and possibly with other people.
/// A predictable sibling name is a name an attacker can pre-create as a symlink,
/// and it is also the name a second RustConn process would pick, so the two
/// would clobber each other mid-write. Paired with `create_new` in
/// [`create_temp_file`], a random name means the open fails rather than
/// following or truncating anything that is already there.
///
/// # Errors
/// Returns [`SecretError::StoreFailed`] if the RNG is unavailable.
fn tmp_path(path: &Path) -> SecretResult<PathBuf> {
    let mut suffix = [0u8; 8];
    fill_random(&mut suffix).map_err(SecretError::StoreFailed)?;

    let mut name = path.file_name().map_or_else(
        || std::ffi::OsString::from(PORTABLE_STORE_FILE_NAME),
        ToOwned::to_owned,
    );
    name.push(format!(".{}.tmp", data_encoding::HEXLOWER.encode(&suffix)));
    Ok(path.with_file_name(name))
}

/// Creates the temp file with owner-only permissions already in place.
///
/// `create_new` refuses an existing path, so a pre-planted symlink or a leftover
/// temp is an error rather than a redirected write. On unix the mode is set in
/// the `open` call itself: doing it afterwards leaves a window in which the file
/// is world-readable, and what lands in that window is the `kdf_salt` and
/// `wrapped_key` — precisely the material an offline passphrase attack needs.
///
/// # Errors
/// Returns [`SecretError::StoreFailed`] if the file cannot be created.
fn create_temp_file(tmp: &Path) -> SecretResult<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(STORE_FILE_MODE);
    }
    options
        .open(tmp)
        .map_err(|e| SecretError::StoreFailed(format!("cannot create portable store temp: {e}")))
}

/// Atomically writes the store: temp file + `0600` + `fsync` + `rename`.
///
/// The `fsync` matters more here than for the machine-bound store: this file is
/// often the only copy of the user's passwords and frequently lives on a
/// network or FUSE-backed cloud mount, where an unflushed rename can leave a
/// zero-length file behind after a crash. The parent directory is synced too so
/// the rename itself survives.
///
/// # Errors
/// Returns [`SecretError::StoreFailed`] if any directory creation,
/// serialization, write, sync, permission or rename step fails.
pub(crate) fn write_store_atomic(path: &Path, store: &PortableStoreFile) -> SecretResult<()> {
    use std::io::Write;

    let parent = path.parent().filter(|p| !p.as_os_str().is_empty());
    if let Some(dir) = parent {
        std::fs::create_dir_all(dir).map_err(|e| {
            SecretError::StoreFailed(format!("cannot create portable store directory: {e}"))
        })?;
    }

    // Values are ciphertext, so the serialized JSON holds no plaintext secrets.
    let json = serde_json::to_vec_pretty(store)
        .map_err(|e| SecretError::StoreFailed(format!("cannot serialize portable store: {e}")))?;

    let tmp = tmp_path(path)?;
    // Every failure past this point leaves a temp file behind, so each arm
    // removes it: the name is random, so a leftover would never be reused and
    // would accumulate in a directory the user syncs.
    let write_result = (|| -> SecretResult<()> {
        let mut file = create_temp_file(&tmp)?;
        file.write_all(&json)
            .map_err(|e| SecretError::StoreFailed(format!("cannot write portable store: {e}")))?;
        file.sync_all()
            .map_err(|e| SecretError::StoreFailed(format!("cannot flush portable store: {e}")))?;
        Ok(())
    })();
    if let Err(e) = write_result {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        SecretError::StoreFailed(format!("cannot finalize portable store: {e}"))
    })?;
    // No chmod on the destination afterwards. `rename` gives the name to the
    // temp file's inode, which was created 0600, so the mode is already right no
    // matter what the previous file's was — and a `set_permissions` by *path*
    // here would follow whatever the path resolves to a moment later, which in a
    // sync folder is not guaranteed to still be this file.

    // Persist the rename itself. A missing directory handle is not fatal — the
    // data is already synced — so this is best effort.
    if let Some(dir) = parent
        && let Ok(handle) = std::fs::File::open(dir)
    {
        let _ = handle.sync_all();
    }
    Ok(())
}

// ──────────────────────────────────────────────────────────────────────────────
// Per-entry seal / open
// ──────────────────────────────────────────────────────────────────────────────

/// Builds the AAD that binds an entry blob to the name it is filed under.
///
/// [`AAD_ENTRY`] alone separates the two *roles* in the file (a wrapped key
/// cannot be opened as an entry), but every entry shares it, so any blob
/// authenticates in any slot. That matters here and not in the machine-bound
/// store, because this file is meant to sit in a folder something else can
/// write: whoever can edit the JSON could move the blob for a throwaway host
/// into the slot of a production one, and the password would decrypt and be
/// sent to the wrong place. Mixing the entry name into the AAD makes that a
/// failed authentication instead.
///
/// The `0x00` keeps the label from running into the name, so no `connection_id`
/// can produce the same AAD as a different label plus a different name.
fn entry_aad(connection_id: &str) -> Vec<u8> {
    let mut aad = Vec::with_capacity(AAD_ENTRY.len() + 1 + connection_id.len());
    aad.extend_from_slice(AAD_ENTRY);
    aad.push(0x00);
    aad.extend_from_slice(connection_id.as_bytes());
    aad
}

/// Seals one credential entry under the store's DEK, returning base64.
///
/// `connection_id` is the name the entry is filed under; it is authenticated,
/// not encrypted, so [`open_entry`] has to be given the same one.
///
/// # Errors
/// Returns [`SecretError::StoreFailed`] if serialization or sealing fails. No
/// secret value appears in the error.
pub(crate) fn seal_entry(
    dek: &Dek,
    connection_id: &str,
    creds: &Credentials,
) -> SecretResult<String> {
    let stored = StoredCredentials::from_credentials(creds);
    // Plaintext secrets live only inside this wiped-on-drop buffer.
    let plaintext = Zeroizing::new(
        serde_json::to_vec(&stored)
            .map_err(|e| SecretError::StoreFailed(format!("cannot serialize credentials: {e}")))?,
    );
    let blob = seal_with_key(dek, &entry_aad(connection_id), &plaintext)
        .map_err(|e| SecretError::StoreFailed(format!("encryption failed: {e}")))?;
    Ok(data_encoding::BASE64.encode(&blob))
}

/// Opens one base64 entry under the store's DEK.
///
/// # Errors
/// Returns [`SecretError::RetrieveFailed`] if the blob is malformed, was filed
/// under a different name, or fails to authenticate.
pub(crate) fn open_entry(
    dek: &Dek,
    connection_id: &str,
    encoded: &str,
) -> SecretResult<Credentials> {
    let blob = data_encoding::BASE64
        .decode(encoded.as_bytes())
        .map_err(|e| {
            SecretError::RetrieveFailed(format!("portable store entry is malformed: {e}"))
        })?;
    let plaintext = open_with_key(dek, &entry_aad(connection_id), &blob)
        .map_err(|e| SecretError::RetrieveFailed(format!("decryption failed: {e}")))?;
    // The serde error is deliberately dropped: this parses freshly *decrypted*
    // plaintext, and serde puts the offending value in its message, so a schema
    // mismatch could carry a fragment of a credential into an error string that
    // reaches a dialog and the log.
    let stored: StoredCredentials = serde_json::from_slice(&plaintext).map_err(|_| {
        SecretError::RetrieveFailed("portable store entry is not in a known format".to_string())
    })?;
    Ok(stored.into_credentials())
}

// ──────────────────────────────────────────────────────────────────────────────
// Unlock cache
// ──────────────────────────────────────────────────────────────────────────────

/// A DEK cached after the first unlock, tied to the salt it came from.
///
/// The salt guard is what makes the cache safe on a cloud-synced path: if the
/// sync client replaces the file with one created elsewhere, its salt differs
/// and the cache is rederived instead of decrypting with a key from the old
/// file (which would fail authentication and read as corruption).
struct CachedDek {
    /// KDF salt the key was derived against.
    salt: Vec<u8>,
    /// The store's `wrapped_key` at derivation time.
    ///
    /// The salt alone is not enough to identify the key: rewrapping generates a
    /// fresh DEK under the *same* salt, and a cache hit would then seal new
    /// entries with the superseded key and write them to a file that can never
    /// open them again. The wrapped key is an exact, cheap fingerprint of the
    /// DEK, so comparing both is what makes the cache safe to write through.
    wrapped_key: String,
    /// The unwrapped data-encryption key.
    dek: Dek,
}

/// Shared, interior-mutable DEK cache.
type DekCache = Arc<Mutex<Option<CachedDek>>>;

/// Opens the store at `path`, creating it when absent, and yields its DEK.
///
/// Uses `cache` to skip the Argon2id derivation when the file's salt is
/// unchanged since the last unlock.
///
/// # Errors
/// Propagates read, header, unlock and creation failures. A mistyped passphrase
/// surfaces as [`SecretError::IncorrectPassphrase`].
fn open_or_create(
    path: &Path,
    passphrase: &[u8],
    cache: &DekCache,
) -> SecretResult<(PortableStoreFile, Dek)> {
    let Some(store) = read_store(path)? else {
        let (store, dek) = PortableStoreFile::create(passphrase)?;
        cache_store(cache, &store, &dek)?;
        return Ok((store, dek));
    };

    if let Some(dek) = cached_dek(cache, &store)? {
        return Ok((store, dek));
    }

    let dek = store.unlock(passphrase)?;
    cache_store(cache, &store, &dek)?;
    Ok((store, dek))
}

/// Returns the cached DEK if it belongs to exactly this store state.
///
/// # Errors
/// Returns [`SecretError::RetrieveFailed`] if the store's salt cannot be decoded.
fn cached_dek(cache: &DekCache, store: &PortableStoreFile) -> SecretResult<Option<Dek>> {
    let salt = store.salt()?;
    let Ok(guard) = cache.lock() else {
        // A poisoned mutex only costs a rederivation.
        return Ok(None);
    };
    let hit = guard.as_ref().and_then(|cached| {
        (cached.salt == salt && cached.wrapped_key == store.wrapped_key).then(|| cached.dek.clone())
    });
    drop(guard);
    Ok(hit)
}

/// Records `dek` in `cache` against the store's salt and wrapped key.
///
/// # Errors
/// Returns [`SecretError::RetrieveFailed`] if the salt cannot be decoded. A
/// poisoned cache mutex is not an error: the DEK is already in hand, so the
/// operation proceeds without caching and the next call rederives.
fn cache_store(cache: &DekCache, store: &PortableStoreFile, dek: &Dek) -> SecretResult<()> {
    let salt = store.salt()?;
    if let Ok(mut guard) = cache.lock() {
        *guard = Some(CachedDek {
            salt,
            wrapped_key: store.wrapped_key.clone(),
            dek: dek.clone(),
        });
    }
    Ok(())
}

// ──────────────────────────────────────────────────────────────────────────────
// Public helpers for the GUI layer
// ──────────────────────────────────────────────────────────────────────────────

/// Checks whether `passphrase` opens the portable store at `path`.
///
/// A file that does not exist yet is accepted: there is nothing to verify
/// against, and the first write will create it under this passphrase.
///
/// This is what the unlock dialog and the Settings page call, so neither needs
/// access to the crate-private crypto helpers.
///
/// # Errors
/// Returns [`SecretError::IncorrectPassphrase`] if the passphrase does not open
/// the store, or [`SecretError::RetrieveFailed`] if the file is unreadable or
/// corrupt.
pub fn verify_portable_passphrase(path: &Path, passphrase: &SecretString) -> SecretResult<()> {
    let Some(store) = read_store(path)? else {
        return Ok(());
    };
    let pass = Zeroizing::new(passphrase.expose_secret().as_bytes().to_vec());
    store.unlock(&pass).map(|_| ())
}

/// Counts the credential entries in the portable store at `path`.
///
/// Needs no passphrase — the entry keys are not encrypted. A missing file
/// counts as zero.
///
/// # Errors
/// Returns [`SecretError::RetrieveFailed`] if the file is unreadable or corrupt.
pub fn entry_count(path: &Path) -> SecretResult<usize> {
    Ok(read_store(path)?.map_or(0, |store| store.entries.len()))
}

/// Resolves the portable store path from an optional user override.
///
/// Falls back to `dirs::data_dir()/rustconn/credentials-portable.enc`, then to a
/// bare relative file name when no data directory can be resolved. Centralised
/// because the GUI resolved this inline in five places.
#[must_use]
pub fn resolve_portable_store_path(configured: Option<&Path>) -> PathBuf {
    configured.map_or_else(default_store_path, Path::to_path_buf)
}

/// Default portable store location under the XDG data directory.
fn default_store_path() -> PathBuf {
    dirs::data_dir()
        .map(|dir| dir.join("rustconn").join(PORTABLE_STORE_FILE_NAME))
        .unwrap_or_else(|| PathBuf::from(PORTABLE_STORE_FILE_NAME))
}

// ──────────────────────────────────────────────────────────────────────────────
// Keyring storage for the portable passphrase
// ──────────────────────────────────────────────────────────────────────────────

/// Keyring entry name for the portable store passphrase.
const KEY_PORTABLE_PASSPHRASE: &str = "portable-file-passphrase";

/// Stores the portable store passphrase in the system keyring.
///
/// # Errors
/// Returns `SecretError` if the keyring write fails.
pub async fn store_portable_passphrase_in_keyring(passphrase: &SecretString) -> SecretResult<()> {
    super::keyring::store(
        KEY_PORTABLE_PASSPHRASE,
        passphrase.expose_secret(),
        "RustConn Portable Credential File Passphrase",
    )
    .await
}

/// Retrieves the portable store passphrase from the system keyring.
///
/// # Errors
/// Returns `SecretError` if the keyring lookup fails.
pub async fn get_portable_passphrase_from_keyring() -> SecretResult<Option<SecretString>> {
    super::keyring::lookup(KEY_PORTABLE_PASSPHRASE)
        .await
        .map(|opt| opt.map(SecretString::from))
}

/// Deletes the portable store passphrase from the system keyring.
///
/// # Errors
/// Returns `SecretError` if the keyring deletion fails.
pub async fn delete_portable_passphrase_from_keyring() -> SecretResult<()> {
    super::keyring::clear(KEY_PORTABLE_PASSPHRASE).await
}

// ──────────────────────────────────────────────────────────────────────────────
// Backend
// ──────────────────────────────────────────────────────────────────────────────

/// Portable encrypted-file credential backend.
///
/// Encrypts credentials under a passphrase-derived key hierarchy so the file is
/// portable across machines (cloud-syncable).
pub struct PortableEncryptedFileBackend {
    /// Path to the on-disk portable credential store.
    path: PathBuf,
    /// User passphrase — `None` means locked (not yet unlocked this session).
    ///
    /// A `std` lock rather than a `tokio` one: it is only ever held long enough
    /// to copy the bytes out, never across an `.await`. That keeps the unlock
    /// setters synchronous, which matters because the manager is assembled from
    /// synchronous code (`SecretManager::build_from_settings`) that would
    /// otherwise have no way to seed a passphrase it already holds.
    passphrase: Arc<RwLock<Option<SecretString>>>,
    /// DEK cached after the first successful unlock this session.
    dek_cache: DekCache,
    /// Serialises the read-modify-write of the whole file.
    ///
    /// Each `store`/`delete` reads the JSON, mutates one entry and writes it all
    /// back. Two of those interleaving means the second read happens before the
    /// first write lands, and the second write then drops the first entry — a
    /// silently lost credential. `store_bulk` and the migration wizard both
    /// issue writes back to back, so the interleaving is reachable in one
    /// process, not just across machines.
    ///
    /// A `std::sync::Mutex` is correct here because the guard is only ever taken
    /// inside a `spawn_blocking` closure and released before it returns; it is
    /// never held across an `.await`.
    write_lock: Arc<Mutex<()>>,
}

impl std::fmt::Debug for PortableEncryptedFileBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `path` is non-secret; the passphrase and DEK are never rendered.
        f.debug_struct("PortableEncryptedFileBackend")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl PortableEncryptedFileBackend {
    /// Creates a backend with the default path under the XDG data directory.
    #[must_use]
    pub fn new() -> Self {
        Self::with_path(default_store_path())
    }

    /// Creates a backend at an explicit path.
    #[must_use]
    pub fn with_path(path: PathBuf) -> Self {
        Self {
            path,
            passphrase: Arc::new(RwLock::new(None)),
            dek_cache: Arc::new(Mutex::new(None)),
            write_lock: Arc::new(Mutex::new(())),
        }
    }

    /// Sets the passphrase, unlocking the backend for this session.
    ///
    /// Invalidating the cached key here is what makes a re-unlock mean
    /// something: without it a wrong passphrase would still "work" for the rest
    /// of the session, because the cache would answer before anything checked.
    pub fn set_passphrase(&self, passphrase: SecretString) {
        *self
            .passphrase
            .write()
            .unwrap_or_else(PoisonError::into_inner) = Some(passphrase);
        self.invalidate_dek();
    }

    /// Clears the passphrase and cached key, re-locking the backend.
    pub fn clear_passphrase(&self) {
        *self
            .passphrase
            .write()
            .unwrap_or_else(PoisonError::into_inner) = None;
        self.invalidate_dek();
    }

    /// Drops any cached data-encryption key.
    fn invalidate_dek(&self) {
        *self
            .dek_cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = None;
    }

    /// Returns whether the backend is currently unlocked (passphrase set).
    #[must_use]
    pub fn is_unlocked(&self) -> bool {
        self.passphrase
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .is_some()
    }

    /// Returns the path to the credential store file.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Copies the passphrase out of the lock for use on a blocking thread.
    ///
    /// The guard is dropped as the copy is returned, so the Argon2id derivation
    /// that follows never holds the lock and cannot stall a concurrent
    /// `is_unlocked` check.
    ///
    /// # Errors
    /// Returns [`SecretError::PassphraseRequired`] when the backend is locked.
    fn passphrase_bytes(&self) -> SecretResult<Zeroizing<Vec<u8>>> {
        let guard = self
            .passphrase
            .read()
            .unwrap_or_else(PoisonError::into_inner);
        let Some(ref passphrase) = *guard else {
            return Err(SecretError::PassphraseRequired);
        };
        let copy = Zeroizing::new(passphrase.expose_secret().as_bytes().to_vec());
        drop(guard);
        Ok(copy)
    }
}

impl Default for PortableEncryptedFileBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SecretBackend for PortableEncryptedFileBackend {
    async fn store(&self, connection_id: &str, credentials: &Credentials) -> SecretResult<()> {
        let pass = self.passphrase_bytes()?;
        let path = self.path.clone();
        let key = connection_id.to_string();
        let creds = credentials.clone();
        let cache = Arc::clone(&self.dek_cache);
        let write_lock = Arc::clone(&self.write_lock);

        tokio::task::spawn_blocking(move || {
            // Held across read → mutate → write so a concurrent store/delete
            // cannot read a snapshot this one is about to replace.
            let guard = write_lock.lock().unwrap_or_else(PoisonError::into_inner);
            let (mut store, dek) = open_or_create(&path, &pass, &cache)?;
            let sealed = seal_entry(&dek, &key, &creds)?;
            store.entries.insert(key, sealed);
            write_store_atomic(&path, &store)?;
            drop(guard);
            tracing::debug!(
                backend = "portable_encrypted_file",
                "stored credential entry"
            );
            Ok(())
        })
        .await
        .map_err(|e| SecretError::StoreFailed(format!("portable store task panicked: {e}")))?
    }

    async fn retrieve(&self, connection_id: &str) -> SecretResult<Option<Credentials>> {
        let pass = self.passphrase_bytes()?;
        let path = self.path.clone();
        let key = connection_id.to_string();
        let cache = Arc::clone(&self.dek_cache);

        tokio::task::spawn_blocking(move || {
            // A store that does not exist yet holds no credentials; that is a
            // miss, not a failure, so the resolver can fall through. Read once
            // and unlock from that snapshot: reading again would let a file
            // swapped in between the two reads surface as "corrupt".
            let Some(store) = read_store(&path)? else {
                return Ok(None);
            };
            if !store.entries.contains_key(&key) {
                return Ok(None);
            }
            let dek = if let Some(cached) = cached_dek(&cache, &store)? {
                cached
            } else {
                let dek = store.unlock(&pass)?;
                cache_store(&cache, &store, &dek)?;
                dek
            };
            // Presence was just checked, so the entry is there.
            let encoded = store.entries.get(&key).ok_or_else(|| {
                SecretError::RetrieveFailed("portable store entry vanished".to_string())
            })?;
            open_entry(&dek, &key, encoded).map(Some)
        })
        .await
        .map_err(|e| SecretError::RetrieveFailed(format!("portable store task panicked: {e}")))?
    }

    async fn delete(&self, connection_id: &str) -> SecretResult<()> {
        // Deleting needs no key — entry names are stored in the clear — but it
        // still requires an unlocked backend, so a locked session cannot be
        // tricked into destroying entries it cannot read. Checking the flag
        // rather than copying the passphrase keeps no key material alive here.
        if !self.is_unlocked() {
            return Err(SecretError::PassphraseRequired);
        }
        let path = self.path.clone();
        let key = connection_id.to_string();
        let write_lock = Arc::clone(&self.write_lock);

        tokio::task::spawn_blocking(move || {
            // Same read-modify-write as `store`, same lock: a delete racing a
            // store would otherwise resurrect the deleted entry or drop the
            // stored one, depending on which write landed second.
            let guard = write_lock.lock().unwrap_or_else(PoisonError::into_inner);
            let Some(mut store) = read_store(&path)? else {
                return Ok(());
            };
            if store.entries.remove(&key).is_some() {
                write_store_atomic(&path, &store)?;
                tracing::debug!(
                    backend = "portable_encrypted_file",
                    "deleted credential entry"
                );
            }
            drop(guard);
            Ok(())
        })
        .await
        .map_err(|e| SecretError::DeleteFailed(format!("portable store task panicked: {e}")))?
    }

    async fn is_available(&self) -> bool {
        // Available once unlocked. A locked backend is not "unavailable" in the
        // ClientMissing sense — it just needs the passphrase.
        self.is_unlocked()
    }

    fn backend_id(&self) -> &'static str {
        "portable_encrypted_file"
    }

    fn display_name(&self) -> &'static str {
        "Portable encrypted file — cloud-syncable"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cheap KDF parameters so the tests do not pay 64 MiB of Argon2 each.
    fn fast_store(passphrase: &[u8]) -> (PortableStoreFile, Dek) {
        let kdf_params = PassphraseKdfParams {
            m_cost: 1024,
            t_cost: 1,
            p_cost: 1,
        };
        let mut salt = [0u8; SETTINGS_SALT_LEN];
        fill_random(&mut salt).unwrap();
        let mut dek: Dek = Zeroizing::new([0u8; 32]);
        fill_random(dek.as_mut()).unwrap();
        let kek = derive_passphrase_key(passphrase, &salt, &kdf_params).unwrap();
        let wrapped = seal_with_key(&kek, AAD_WRAPPED_KEY, dek.as_ref()).unwrap();
        (
            PortableStoreFile {
                format_version: FORMAT_VERSION,
                kdf: KDF_ARGON2ID.to_owned(),
                kdf_params,
                kdf_salt: data_encoding::BASE64.encode(&salt),
                wrapped_key: data_encoding::BASE64.encode(&wrapped),
                entries: BTreeMap::new(),
            },
            dek,
        )
    }

    fn sample_credentials() -> Credentials {
        Credentials {
            username: Some("alice".to_string()),
            password: Some(SecretString::from("s3cr3t".to_owned())),
            key_passphrase: None,
            domain: None,
        }
    }

    #[test]
    fn entry_round_trips_under_the_data_key() {
        let (_store, dek) = fast_store(b"correct-passphrase");
        let encoded = seal_entry(&dek, "rustconn/host", &sample_credentials()).expect("seal");
        let recovered = open_entry(&dek, "rustconn/host", &encoded).expect("open");

        assert_eq!(recovered.username.as_deref(), Some("alice"));
        assert_eq!(recovered.expose_password(), Some("s3cr3t"));
    }

    #[test]
    fn unlock_recovers_the_same_data_key() {
        let (store, dek) = fast_store(b"correct-passphrase");
        let recovered = store.unlock(b"correct-passphrase").expect("unlock");
        assert_eq!(recovered.as_ref(), dek.as_ref());
    }

    #[test]
    fn unlock_with_wrong_passphrase_reports_incorrect_passphrase() {
        let (store, _dek) = fast_store(b"correct-passphrase");
        let result = store.unlock(b"wrong-passphrase");

        assert!(
            matches!(result, Err(SecretError::IncorrectPassphrase)),
            "a mistyped passphrase must be distinguishable from a corrupt file, got {result:?}"
        );
    }

    /// The AAD labels are the only thing stopping a wrapped key and an entry
    /// blob from being interchangeable, since both are `nonce + ct + tag` under
    /// the same cipher. Swapping the role must fail authentication.
    #[test]
    fn entry_blob_does_not_authenticate_as_a_wrapped_key() {
        let (_store, dek) = fast_store(b"correct-passphrase");
        let encoded = seal_entry(&dek, "rustconn/host", &sample_credentials()).expect("seal");
        let blob = data_encoding::BASE64.decode(encoded.as_bytes()).unwrap();

        assert!(
            open_with_key(&dek, AAD_WRAPPED_KEY, &blob).is_err(),
            "an entry blob must not open under the wrapped-key AAD"
        );
        assert!(
            open_with_key(&dek, &entry_aad("rustconn/host"), &blob).is_ok(),
            "the same blob must open under its own AAD"
        );
    }

    /// The file is meant to live where a sync client — or anyone with write
    /// access to the shared folder — can edit it. Moving a blob between entry
    /// names would otherwise hand the password for one host to another.
    #[test]
    fn an_entry_does_not_open_under_a_different_name() {
        let (_store, dek) = fast_store(b"correct-passphrase");
        let encoded = seal_entry(&dek, "rustconn/staging", &sample_credentials()).expect("seal");

        assert!(
            open_entry(&dek, "rustconn/staging", &encoded).is_ok(),
            "the entry must open under the name it was filed under"
        );
        assert!(
            open_entry(&dek, "rustconn/production", &encoded).is_err(),
            "a relocated entry must fail authentication, not decrypt into the new slot"
        );
    }

    #[test]
    fn header_rejects_a_newer_format_version() {
        let (mut store, _dek) = fast_store(b"pass");
        store.format_version = FORMAT_VERSION + 1;
        assert!(store.check_header().is_err());
    }

    #[test]
    fn header_rejects_an_unknown_kdf() {
        let (mut store, _dek) = fast_store(b"pass");
        store.kdf = "scrypt".to_owned();
        assert!(store.check_header().is_err());
    }

    /// The header arrives from a cloud folder, so its costs are untrusted input:
    /// a 4 GiB `m_cost` must be refused before it reaches Argon2.
    #[test]
    fn header_rejects_an_implausible_kdf_cost() {
        let (mut store, _dek) = fast_store(b"pass");
        store.kdf_params.m_cost = 4 * 1024 * 1024;
        assert!(store.check_header().is_err());

        let (mut zero, _dek) = fast_store(b"pass");
        zero.kdf_params.t_cost = 0;
        assert!(zero.check_header().is_err());
    }

    /// Rewrapping keeps the salt but changes the key. Caching on the salt alone
    /// would seal new entries with the superseded DEK and write them to a file
    /// that can never open them again.
    #[test]
    fn cache_misses_when_the_key_was_rewrapped_under_the_same_salt() {
        let (store, dek) = fast_store(b"pass");
        let cache: DekCache = Arc::new(Mutex::new(None));
        cache_store(&cache, &store, &dek).expect("cache");

        assert!(
            cached_dek(&cache, &store)
                .expect("lookup")
                .is_some_and(|hit| hit.as_ref() == dek.as_ref()),
            "the same store state must hit the cache"
        );

        // Same salt, different wrapped key — as a passphrase change produces.
        let mut rewrapped = store.clone();
        let mut fresh_dek: Dek = Zeroizing::new([0u8; 32]);
        fill_random(fresh_dek.as_mut()).unwrap();
        let kek =
            derive_passphrase_key(b"pass", &store.salt().unwrap(), &store.kdf_params).unwrap();
        rewrapped.wrapped_key = data_encoding::BASE64
            .encode(&seal_with_key(&kek, AAD_WRAPPED_KEY, fresh_dek.as_ref()).unwrap());

        assert_eq!(rewrapped.kdf_salt, store.kdf_salt, "salt is unchanged");
        assert!(
            cached_dek(&cache, &rewrapped).expect("lookup").is_none(),
            "a rewrapped key must miss the cache instead of returning the stale DEK"
        );
    }

    /// M-PUBLIC-DEBUG: the backend owns a passphrase and a derived key, so its
    /// `Debug` must show neither.
    #[test]
    fn backend_debug_does_not_leak_the_passphrase() {
        const SENTINEL: &str = "PortableBackend-LEAK-pass-001";

        let backend = PortableEncryptedFileBackend::with_path(PathBuf::from("/tmp/portable.enc"));
        backend.set_passphrase(SecretString::from(SENTINEL.to_owned()));

        let rendered = format!("{backend:?}");
        assert!(
            !rendered.contains(SENTINEL),
            "backend Debug leaked the passphrase: {rendered}"
        );
        assert!(rendered.contains("portable.enc"), "path stays visible");
    }

    /// Locking must drop the derived key, not just the passphrase: a cached DEK
    /// left behind would keep answering after the user locked the store, and
    /// would let a *wrong* passphrase appear to work on the next unlock.
    #[test]
    fn locking_and_relocking_tracks_the_unlocked_flag() {
        let backend = PortableEncryptedFileBackend::with_path(PathBuf::from("/tmp/portable.enc"));
        assert!(!backend.is_unlocked(), "a fresh backend starts locked");

        backend.set_passphrase(SecretString::from("pass".to_owned()));
        assert!(backend.is_unlocked());

        backend.clear_passphrase();
        assert!(!backend.is_unlocked(), "clearing must re-lock");
    }

    #[test]
    fn store_file_survives_a_json_round_trip() {
        let (mut store, dek) = fast_store(b"pass");
        store.entries.insert(
            "rustconn/host".to_owned(),
            seal_entry(&dek, "rustconn/host", &sample_credentials()).unwrap(),
        );

        let json = serde_json::to_string_pretty(&store).expect("serialize");
        let recovered: PortableStoreFile = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(recovered.format_version, FORMAT_VERSION);
        assert_eq!(recovered.kdf, KDF_ARGON2ID);
        assert_eq!(recovered.entries.len(), 1);
        let dek2 = recovered.unlock(b"pass").expect("unlock after round trip");
        let creds =
            open_entry(&dek2, "rustconn/host", &recovered.entries["rustconn/host"]).expect("open");
        assert_eq!(creds.expose_password(), Some("s3cr3t"));
    }

    #[test]
    fn write_then_read_preserves_entries() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("nested").join("portable.enc");

        let (mut store, dek) = fast_store(b"pass");
        store.entries.insert(
            "rustconn/host".to_owned(),
            seal_entry(&dek, "rustconn/host", &sample_credentials()).unwrap(),
        );
        write_store_atomic(&path, &store).expect("write");

        assert_eq!(entry_count(&path).expect("count"), 1);
        let reread = read_store(&path).expect("read").expect("present");
        assert_eq!(reread.entries.len(), 1);
    }

    #[test]
    fn verify_passphrase_accepts_a_missing_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("absent.enc");
        assert!(
            verify_portable_passphrase(&path, &SecretString::from("anything".to_owned())).is_ok()
        );
        assert_eq!(entry_count(&path).expect("count"), 0);
    }

    #[test]
    fn verify_passphrase_rejects_the_wrong_passphrase() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("portable.enc");
        let (store, _dek) = fast_store(b"right");
        write_store_atomic(&path, &store).expect("write");

        assert!(matches!(
            verify_portable_passphrase(&path, &SecretString::from("wrong".to_owned())),
            Err(SecretError::IncorrectPassphrase)
        ));
        assert!(
            verify_portable_passphrase(&path, &SecretString::from("right".to_owned())).is_ok(),
            "the correct passphrase must still be accepted"
        );
    }

    #[test]
    fn resolve_store_path_prefers_the_configured_path() {
        let configured = PathBuf::from("/tmp/rustconn-test/creds.enc");
        assert_eq!(resolve_portable_store_path(Some(&configured)), configured);
        assert!(
            resolve_portable_store_path(None)
                .to_string_lossy()
                .ends_with(PORTABLE_STORE_FILE_NAME)
        );
    }

    /// Version 0 was never written by any release, so a file claiming it is a
    /// file whose header was damaged — by a bad sync merge or a truncated write.
    /// Parsing it as v1 would mean guessing at an unknown shape and then
    /// rewriting the file in that guess.
    #[test]
    fn header_rejects_a_format_version_below_one() {
        let (mut store, _dek) = fast_store(b"pass");
        store.format_version = 0;
        assert!(store.check_header().is_err());
    }

    /// The size check has to happen before the read: by the time there is a
    /// struct to validate, the allocation `check_header` would guard against has
    /// already been made.
    #[test]
    fn an_oversized_file_is_refused_without_being_loaded() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("huge.enc");
        let filler = vec![b'x'; usize::try_from(MAX_STORE_BYTES).unwrap() + 1];
        std::fs::write(&path, &filler).unwrap();

        let err = read_store(&path).expect_err("an oversized store must be refused");
        assert!(
            matches!(err, SecretError::RetrieveFailed(ref m) if m.contains("ceiling")),
            "the error must say why, got {err:?}"
        );
    }

    #[test]
    fn a_store_with_too_many_entries_is_refused() {
        let (mut store, _dek) = fast_store(b"pass");
        for i in 0..=MAX_ENTRIES {
            store.entries.insert(format!("k{i}"), "x".to_owned());
        }
        let err = store
            .check_header()
            .expect_err("past the entry ceiling the store must be refused");
        assert!(matches!(err, SecretError::RetrieveFailed(ref m) if m.contains("entries")));
    }

    /// The written file holds the KDF salt and the wrapped key — the material an
    /// offline passphrase attack needs — and it lives in a directory the user
    /// shares with a sync client, so it must never be readable by anyone else,
    /// not even for the width of one `write` call.
    #[cfg(unix)]
    #[test]
    fn the_written_store_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("portable.enc");
        let (store, _dek) = fast_store(b"pass");
        write_store_atomic(&path, &store).expect("write");

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, STORE_FILE_MODE,
            "store file must be 0600, got {mode:o}"
        );
    }

    /// A fixed `.tmp` sibling was both a name an attacker could pre-create as a
    /// symlink and the name a second RustConn process would choose. Two writes in
    /// a row must not collide, and neither must leave a temp behind.
    #[test]
    fn repeated_writes_leave_no_temp_file_behind() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("portable.enc");
        let (store, _dek) = fast_store(b"pass");

        write_store_atomic(&path, &store).expect("first write");
        write_store_atomic(&path, &store).expect("second write");

        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| {
                std::path::Path::new(name)
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("tmp"))
            })
            .collect();
        assert!(
            leftovers.is_empty(),
            "atomic write left temp files behind: {leftovers:?}"
        );
    }

    #[test]
    fn temp_paths_are_unique_per_call() {
        let path = PathBuf::from("/tmp/portable.enc");
        let first = tmp_path(&path).expect("first");
        let second = tmp_path(&path).expect("second");
        assert_ne!(
            first, second,
            "a predictable temp name is a symlink target and a collision between processes"
        );
        assert_eq!(
            first.parent(),
            path.parent(),
            "temp stays in the same directory"
        );
    }

    // ──────────────────────────────────────────────────────────────────────
    // `SecretBackend` impl — the async trait surface the resolver actually uses
    // ──────────────────────────────────────────────────────────────────────

    /// A locked backend must refuse every operation rather than answer wrongly.
    ///
    /// `retrieve` returning `Ok(None)` here would read as "this connection has no
    /// stored password", which is what sent the user a password prompt instead of
    /// an unlock prompt.
    #[test]
    fn a_locked_backend_refuses_every_operation() {
        let dir = tempfile::TempDir::new().unwrap();
        let backend = PortableEncryptedFileBackend::with_path(dir.path().join("portable.enc"));
        let rt = tokio::runtime::Runtime::new().unwrap();

        rt.block_on(async {
            assert!(!backend.is_available().await, "locked is not available");
            assert!(matches!(
                backend.store("rustconn/host", &sample_credentials()).await,
                Err(SecretError::PassphraseRequired)
            ));
            assert!(matches!(
                backend.retrieve("rustconn/host").await,
                Err(SecretError::PassphraseRequired)
            ));
            assert!(matches!(
                backend.delete("rustconn/host").await,
                Err(SecretError::PassphraseRequired)
            ));
        });
    }

    /// One pass over the whole trait: store, read back, delete, confirm gone.
    ///
    /// Pays one real Argon2id derivation (the default 64 MiB parameters) because
    /// this is the path the application takes; the session DEK cache means the
    /// three operations after the first are cheap.
    #[test]
    fn backend_round_trips_a_credential_through_the_trait() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("portable.enc");
        let backend = PortableEncryptedFileBackend::with_path(path.clone());
        backend.set_passphrase(SecretString::from("correct horse".to_owned()));
        let rt = tokio::runtime::Runtime::new().unwrap();

        rt.block_on(async {
            assert!(backend.is_available().await);
            assert!(
                backend
                    .retrieve("rustconn/host")
                    .await
                    .expect("miss")
                    .is_none(),
                "a store that does not exist yet is a miss, not a failure"
            );

            backend
                .store("rustconn/host", &sample_credentials())
                .await
                .expect("store");

            let found = backend
                .retrieve("rustconn/host")
                .await
                .expect("retrieve")
                .expect("present");
            assert_eq!(found.username.as_deref(), Some("alice"));
            assert_eq!(found.expose_password(), Some("s3cr3t"));

            assert!(
                backend
                    .retrieve("rustconn/other")
                    .await
                    .expect("miss")
                    .is_none(),
                "an absent key is a miss even when the store opens"
            );

            backend.delete("rustconn/host").await.expect("delete");
            assert!(
                backend
                    .retrieve("rustconn/host")
                    .await
                    .expect("after delete")
                    .is_none()
            );
        });

        assert_eq!(entry_count(&path).expect("count"), 0);
    }

    /// A wrong passphrase must be distinguishable from an empty store, all the
    /// way out through the trait — not just at `PortableStoreFile::unlock`.
    #[test]
    fn the_trait_reports_a_wrong_passphrase_rather_than_a_miss() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("portable.enc");

        // Seed a store with cheap KDF parameters so this test pays no 64 MiB
        // derivation, then point a backend at it with the wrong passphrase.
        let (mut store, dek) = fast_store(b"right");
        store.entries.insert(
            "rustconn/host".to_owned(),
            seal_entry(&dek, "rustconn/host", &sample_credentials()).unwrap(),
        );
        write_store_atomic(&path, &store).expect("write");

        let backend = PortableEncryptedFileBackend::with_path(path);
        backend.set_passphrase(SecretString::from("wrong".to_owned()));
        let rt = tokio::runtime::Runtime::new().unwrap();

        rt.block_on(async {
            assert!(
                matches!(
                    backend.retrieve("rustconn/host").await,
                    Err(SecretError::IncorrectPassphrase)
                ),
                "a wrong passphrase must not read as \"no stored password\""
            );
        });
    }

    /// Two overlapping writes are a read-modify-write of the whole file each, so
    /// without a lock spanning the sequence the second write drops the first
    /// entry. `store_bulk` and the migration wizard both issue writes back to
    /// back, so this is reachable in one process.
    #[test]
    fn concurrent_stores_do_not_lose_an_entry() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("portable.enc");
        let backend = Arc::new(PortableEncryptedFileBackend::with_path(path.clone()));
        backend.set_passphrase(SecretString::from("pass".to_owned()));
        let rt = tokio::runtime::Runtime::new().unwrap();

        rt.block_on(async {
            // Create the store first so both writes below race on an existing
            // file rather than both trying to create one.
            backend
                .store("rustconn/seed", &sample_credentials())
                .await
                .expect("seed");

            let mut handles = Vec::new();
            for i in 0..4 {
                let backend = Arc::clone(&backend);
                handles.push(tokio::spawn(async move {
                    backend
                        .store(&format!("rustconn/host-{i}"), &sample_credentials())
                        .await
                }));
            }
            for handle in handles {
                handle.await.expect("task").expect("store");
            }

            for i in 0..4 {
                assert!(
                    backend
                        .retrieve(&format!("rustconn/host-{i}"))
                        .await
                        .expect("retrieve")
                        .is_some(),
                    "entry {i} was lost to an interleaved whole-file rewrite"
                );
            }
        });

        assert_eq!(entry_count(&path).expect("count"), 5);
    }
}
