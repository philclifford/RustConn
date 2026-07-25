//! Ephemeral FreeRDP args file for connection arguments.
//!
//! FreeRDP requires that `/args-from:` is the **only** argument on the command
//! line — it cannot be combined with other CLI arguments ([FreeRDP#12697]).
//! All connection parameters (including secrets like `/p:<password>`) are
//! written into a single-use file in `$XDG_RUNTIME_DIR` (mode 0600), keeping
//! everything out of `/proc/<pid>/cmdline`.
//!
//! The switch itself is spelled differently across FreeRDP releases —
//! `/args-from:<path>` everywhere, `/args-from:file:<path>` only from 3.26 on.
//! [`super::detect::args_from_argument`] picks the form the installed binary
//! understands.
//!
//! [FreeRDP#12697]: https://github.com/FreeRDP/FreeRDP/pull/12697
//!
//! # Lifecycle
//!
//! [`EphemeralRdpArgs`] writes the args file in [`Self::write_all`] and
//! removes it on `Drop` (best-effort). Callers must hold the guard
//! alive until the spawned FreeRDP process has actually consumed
//! the file (a fraction of a second after `spawn`). Because FreeRDP
//! reads the file during argument parsing, dropping the guard
//! immediately after `child.try_wait()` returns `None` is safe.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use rustconn_core::error::SecretResult;
use secrecy::{ExposeSecret, SecretString};

/// Single-use args file containing all FreeRDP connection arguments.
///
/// FreeRDP requires `/args-from:` to be the sole CLI argument.
/// All parameters (secret and plain) are written one-per-line into this
/// file. The file is created with mode `0600` so only the owning user can
/// read it. It is removed when the guard is dropped, even if the
/// launcher panics partway through `spawn`.
pub(super) struct EphemeralRdpArgs {
    path: PathBuf,
}

impl EphemeralRdpArgs {
    /// Returns the path the spawned FreeRDP client should read its args
    /// from via `/args-from:`.
    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    /// Writes all connection arguments (plain + secret) to a fresh file in
    /// `$XDG_RUNTIME_DIR` and returns a guard that removes the file on drop.
    ///
    /// FreeRDP requires `/args-from:` to be the **only** CLI argument — it
    /// cannot be combined with other arguments. All connection parameters are
    /// therefore written into this file, one per line.
    ///
    /// # Arguments
    ///
    /// * `plain_args` — non-secret arguments (e.g. `/v:host`, `/w:1920`)
    /// * `secret_args` — secret arguments written as `/<flag>:<secret>`
    ///   (e.g. `/p:<password>`); the in-memory copy is zeroized after write
    ///
    /// # Errors
    ///
    /// Returns `SecretError::Pass` when the runtime directory cannot
    /// be located or the file cannot be created with the requested
    /// permissions.
    pub(super) fn write_all(
        plain_args: &[String],
        secret_args: &[(&str, &SecretString)],
    ) -> SecretResult<Self> {
        use rustconn_core::error::SecretError;

        // $XDG_RUNTIME_DIR is the natural choice on Linux desktops:
        // tmpfs, mode 0700, owned by the user, cleared on logout.
        let dir = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .filter(|p| p.is_dir())
            .ok_or_else(|| {
                SecretError::Pass(
                    "XDG_RUNTIME_DIR is not set or is not a directory; \
                     cannot create ephemeral RDP args file"
                        .to_string(),
                )
            })?;

        Self::write_all_in_dir(&dir, plain_args, secret_args)
    }

    /// Writes all connection arguments into a specific directory.
    /// Used by `write_all` (with `$XDG_RUNTIME_DIR`) and by the tests.
    fn write_all_in_dir(
        dir: &Path,
        plain_args: &[String],
        secret_args: &[(&str, &SecretString)],
    ) -> SecretResult<Self> {
        use rustconn_core::error::SecretError;

        // Avoid name collisions across concurrent RDP launches by
        // suffixing with a random UUID.
        let path = dir.join(format!("rustconn-rdp-{}.args", uuid::Uuid::new_v4()));

        let mut file: File = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&path)
            .map_err(|e| {
                SecretError::Pass(format!(
                    "failed to create ephemeral RDP args file at {}: {e}",
                    path.display()
                ))
            })?;

        // The FreeRDP args-file format is one argument per line. The file must
        // contain ALL arguments — nothing else may appear on the command line
        // alongside the `/args-from:` switch.

        // Write plain-text arguments first (non-secret, e.g. /v:host)
        for arg in plain_args {
            file.write_all(arg.as_bytes()).map_err(|e| {
                SecretError::Pass(format!(
                    "failed to write ephemeral RDP args file at {}: {e}",
                    path.display()
                ))
            })?;
            file.write_all(b"\n").map_err(|e| {
                SecretError::Pass(format!(
                    "failed to write ephemeral RDP args file at {}: {e}",
                    path.display()
                ))
            })?;
        }

        // Write secret arguments last; wrap in Zeroizing so the heap copy
        // is wiped once the write completes.
        if !secret_args.is_empty() {
            let mut secret_content = String::new();
            for (flag, secret) in secret_args {
                secret_content.push('/');
                secret_content.push_str(flag);
                secret_content.push(':');
                secret_content.push_str(secret.expose_secret());
                secret_content.push('\n');
            }
            let zline = zeroize::Zeroizing::new(secret_content);
            file.write_all(zline.as_bytes()).map_err(|e| {
                SecretError::Pass(format!(
                    "failed to write ephemeral RDP args file at {}: {e}",
                    path.display()
                ))
            })?;
        }

        Ok(Self { path })
    }

    /// Legacy helper used by tests.
    #[cfg(test)]
    fn write_in_dir(dir: &Path, args: &[(&str, &SecretString)]) -> SecretResult<Self> {
        Self::write_all_in_dir(dir, &[], args)
    }
}

impl Drop for EphemeralRdpArgs {
    fn drop(&mut self) {
        // Best-effort: if the file was already moved or the runtime
        // directory was wiped under our feet, there is nothing
        // sensible we can do here. We deliberately ignore the result.
        if self.path.exists()
            && let Err(e) = std::fs::remove_file(&self.path)
        {
            tracing::warn!(
                path = %self.path.display(),
                error = %e,
                "failed to remove ephemeral RemoteApp args file; \
                 it will be cleaned up at logout via XDG_RUNTIME_DIR"
            );
        }
    }
}

impl std::fmt::Debug for EphemeralRdpArgs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EphemeralRdpArgs")
            .field("path", &self.path)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    /// Creates a temporary directory mode 0700 to mimic `$XDG_RUNTIME_DIR`.
    /// The directory and its contents are removed when the returned guard
    /// drops.
    struct TempRuntimeDir(PathBuf);

    impl TempRuntimeDir {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("rustconn-test-rt-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&path).expect("create test runtime dir");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
                .expect("set 0700 on test runtime dir");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempRuntimeDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn drop_removes_file_with_normal_password() {
        let dir = TempRuntimeDir::new();
        let path_after_drop;
        {
            let pwd = SecretString::from("hunter2".to_string());
            let guard = EphemeralRdpArgs::write_in_dir(dir.path(), &[("p", &pwd)])
                .expect("write args file");
            let p = guard.path().to_path_buf();
            assert!(p.starts_with(dir.path()));
            assert!(p.exists(), "args file should exist while guard is alive");
            path_after_drop = p;
        }
        assert!(
            !path_after_drop.exists(),
            "args file should be removed when guard drops"
        );
    }

    #[test]
    fn file_mode_is_0600() {
        let dir = TempRuntimeDir::new();
        let pwd = SecretString::from("any".to_string());
        let guard =
            EphemeralRdpArgs::write_in_dir(dir.path(), &[("p", &pwd)]).expect("write args file");
        let mode = std::fs::metadata(guard.path())
            .expect("stat")
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "args file must be readable only by the owner"
        );
    }

    #[test]
    fn drop_removes_file_for_password_with_special_characters() {
        // Tests a payload that includes characters which would historically
        // have been awkward on the command line (`'`, `"`, `\n`, `\t`, etc.).
        // The file format is line-based but xfreerdp consumes the whole line
        // verbatim, so we only need to ensure the cleanup path runs.
        let dir = TempRuntimeDir::new();
        let path_after_drop;
        {
            let pwd = SecretString::from(
                "p@ss\twith\nnew\rlines and 'quotes' and \"escapes\\\"".to_string(),
            );
            let guard = EphemeralRdpArgs::write_in_dir(dir.path(), &[("p", &pwd)])
                .expect("write args file");
            let p = guard.path().to_path_buf();
            assert!(p.exists());
            path_after_drop = p;
        }
        assert!(!path_after_drop.exists());
    }

    #[test]
    fn write_fails_for_nonexistent_dir() {
        let pwd = SecretString::from("any".to_string());
        let nope = std::path::Path::new("/this/path/does/not/exist/and/should/not");
        let res = EphemeralRdpArgs::write_in_dir(nope, &[("p", &pwd)]);
        assert!(res.is_err());
    }

    #[test]
    fn debug_does_not_leak_password() {
        let dir = TempRuntimeDir::new();
        let pwd = SecretString::from("hunter2-secret".to_string());
        let guard =
            EphemeralRdpArgs::write_in_dir(dir.path(), &[("p", &pwd)]).expect("write args file");
        let rendered = format!("{guard:?}");
        // Path is non-secret (it's in $XDG_RUNTIME_DIR with a UUID), so it
        // may appear; the password must not.
        assert!(
            !rendered.contains("hunter2-secret"),
            "Debug output leaked the password: {rendered}"
        );
    }

    #[test]
    fn writes_multiple_secret_args_one_per_line() {
        let dir = TempRuntimeDir::new();
        let session = SecretString::from("session-pw".to_string());
        let gateway = SecretString::from("gateway-pw".to_string());
        let guard =
            EphemeralRdpArgs::write_in_dir(dir.path(), &[("p", &session), ("gp", &gateway)])
                .expect("write args file");
        let content = std::fs::read_to_string(guard.path()).expect("read args file");
        assert_eq!(content, "/p:session-pw\n/gp:gateway-pw\n");
    }

    #[test]
    fn write_all_combines_plain_and_secret_args() {
        let dir = TempRuntimeDir::new();
        let plain = vec![
            "/v:myhost".to_string(),
            "/u:admin".to_string(),
            "/w:1920".to_string(),
            "/h:1080".to_string(),
            "+clipboard".to_string(),
        ];
        let password = SecretString::from("s3cret".to_string());
        let guard = EphemeralRdpArgs::write_all_in_dir(dir.path(), &plain, &[("p", &password)])
            .expect("write args file");
        let content = std::fs::read_to_string(guard.path()).expect("read args file");
        assert_eq!(
            content,
            "/v:myhost\n/u:admin\n/w:1920\n/h:1080\n+clipboard\n/p:s3cret\n"
        );
    }

    #[test]
    fn write_all_no_secrets_writes_only_plain_args() {
        let dir = TempRuntimeDir::new();
        let plain = vec!["/v:host".to_string(), "/cert:ignore".to_string()];
        let guard =
            EphemeralRdpArgs::write_all_in_dir(dir.path(), &plain, &[]).expect("write args file");
        let content = std::fs::read_to_string(guard.path()).expect("read args file");
        assert_eq!(content, "/v:host\n/cert:ignore\n");
    }
}
