//! Secret-bearing environment for a Custom Command launch (issue #151).
//!
//! A Custom Command template may reference `${password}`. Substituting the
//! password into the template would put it in the argv of the `sh -c` we spawn,
//! where `/proc/<pid>/cmdline` and `ps` expose it to every process of the same
//! user. Instead the placeholder becomes a shell reference to an environment
//! variable and the value is delivered out of band:
//!
//! * Outside Flatpak, VTE sets the variable directly in the child environment —
//!   nothing touches a command line.
//! * Inside Flatpak the command runs on the host through `flatpak-spawn`, which
//!   does **not** forward the sandbox environment (verified against the GNOME 50
//!   runtime). Its `--env=VAR=VALUE` option would move the secret straight back
//!   into argv, so this module writes an `env -0` block to a mode-0600 file in
//!   `$XDG_RUNTIME_DIR` (tmpfs, user-private) that the spawned shell opens as a
//!   file descriptor and immediately unlinks, handing the descriptor to
//!   `flatpak-spawn --env-fd`. Only the path is ever visible in a command line.
//!
//! The residual exposure on Flatpak is the portal `HostCommand` D-Bus call, which
//! carries the child environment by design; that channel is the user's own
//! session bus. The launched program's own argv is the user's choice and outside
//! RustConn's control.

use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use secrecy::{ExposeSecret, SecretString};

/// Builds a validated `NAME=VALUE` environment entry.
///
/// The single guard both delivery paths share. `None` is returned — and the
/// caller launches without the variable — when the name is empty, the name holds
/// a NUL or `=`, or the value holds a NUL: in the `env -0` file a NUL would forge
/// a second variable, and in either encoding it would truncate the value.
///
/// The result is [`zeroize::Zeroizing`] because it is the only place the
/// plaintext is materialised outside `SecretString`.
pub(super) fn env_entry(name: &str, value: &SecretString) -> Option<zeroize::Zeroizing<String>> {
    if name.is_empty() || name.contains(['\0', '=']) || value.expose_secret().contains('\0') {
        tracing::warn!(
            variable = name,
            "Refusing to build command environment: name or value contains NUL or '='"
        );
        return None;
    }
    Some(zeroize::Zeroizing::new(format!(
        "{name}={}",
        value.expose_secret()
    )))
}

/// Appends the NUL separator the `env -0` format requires.
fn terminate(entry: &zeroize::Zeroizing<String>) -> zeroize::Zeroizing<String> {
    zeroize::Zeroizing::new(format!("{}\0", entry.as_str()))
}

/// Single-use `env -0` file consumed by `flatpak-spawn --env-fd`.
///
/// Deliberately has no `Drop` cleanup: the spawn is asynchronous, so removing the
/// file when the guard goes out of scope would race the child that still has to
/// open it. The generated shell command unlinks it as its first action, right
/// after opening the descriptor. A file left behind by a failed spawn lives in
/// tmpfs at mode 0600 and disappears at logout.
pub(super) struct EphemeralCommandEnv {
    path: PathBuf,
}

impl EphemeralCommandEnv {
    /// Returns the path the spawned shell reads the descriptor from.
    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    /// Writes `entries` as a NUL-separated `env -0` block to a fresh runtime file.
    ///
    /// Returns `None` when `$XDG_RUNTIME_DIR` is unusable, a name or value holds a
    /// NUL byte (which would forge an extra variable), or the file cannot be
    /// created — the caller then launches without the variable, leaving
    /// `${password}` unresolved rather than leaking it another way.
    pub(super) fn write(entries: &[(&str, &SecretString)]) -> Option<Self> {
        let dir = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .filter(|path| path.is_dir())?;
        Self::write_in_dir(&dir, entries)
    }

    fn write_in_dir(dir: &Path, entries: &[(&str, &SecretString)]) -> Option<Self> {
        // Build (and thereby validate) every entry before opening, so a rejected
        // one never creates a file.
        let blocks: Vec<zeroize::Zeroizing<String>> = entries
            .iter()
            .map(|(name, value)| env_entry(name, value).map(|entry| terminate(&entry)))
            .collect::<Option<_>>()?;

        let path = dir.join(format!("rustconn-cmdenv-{}", uuid::Uuid::new_v4()));
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&path)
            .map_err(|error| {
                tracing::warn!(
                    path = %path.display(),
                    %error,
                    "Failed to create command environment file"
                );
            })
            .ok()?;

        let guard = Self { path };
        for block in &blocks {
            if let Err(error) = file.write_all(block.as_bytes()) {
                tracing::warn!(
                    path = %guard.path.display(),
                    %error,
                    "Failed to write command environment file"
                );
                // Remove the partial file here: nothing will consume it, so the
                // "the shell unlinks it" contract does not apply.
                let _ = std::fs::remove_file(&guard.path);
                return None;
            }
        }
        Some(guard)
    }
}

impl std::fmt::Debug for EphemeralCommandEnv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EphemeralCommandEnv")
            .field("path", &self.path)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    struct TempRuntimeDir(PathBuf);
    impl TempRuntimeDir {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("rustconn-test-env-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&path).expect("create test runtime dir");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
                .expect("set test runtime permissions");
            Self(path)
        }
        fn path(&self) -> &Path {
            &self.0
        }
        fn assert_empty(&self) {
            assert_eq!(
                std::fs::read_dir(&self.0)
                    .expect("read test runtime dir")
                    .count(),
                0,
                "a rejected entry must not create a file"
            );
        }
    }
    impl Drop for TempRuntimeDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn writes_nul_separated_env_block() {
        let dir = TempRuntimeDir::new();
        let password = SecretString::from("s3cret with spaces".to_string());
        let guard = EphemeralCommandEnv::write_in_dir(dir.path(), &[("RC_PW", &password)])
            .expect("write env file");
        let written = std::fs::read(guard.path()).expect("read env file");
        assert_eq!(written, b"RC_PW=s3cret with spaces\0");
    }

    #[test]
    fn file_mode_is_0600() {
        let dir = TempRuntimeDir::new();
        let password = SecretString::from("any".to_string());
        let guard = EphemeralCommandEnv::write_in_dir(dir.path(), &[("RC_PW", &password)])
            .expect("write env file");
        let mode = std::fs::metadata(guard.path())
            .expect("read env file metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn rejects_nul_in_value_before_open() {
        let dir = TempRuntimeDir::new();
        let password = SecretString::from("secret\0INJECTED=1".to_string());
        assert!(EphemeralCommandEnv::write_in_dir(dir.path(), &[("RC_PW", &password)]).is_none());
        dir.assert_empty();
    }

    #[test]
    fn rejects_malformed_names_before_open() {
        let password = SecretString::from("safe".to_string());
        for name in ["", "RC\0PW", "RC=PW"] {
            let dir = TempRuntimeDir::new();
            assert!(EphemeralCommandEnv::write_in_dir(dir.path(), &[(name, &password)]).is_none());
            dir.assert_empty();
        }
    }

    #[test]
    fn debug_does_not_leak_password() {
        let dir = TempRuntimeDir::new();
        let password = SecretString::from("hunter2-secret".to_string());
        let guard = EphemeralCommandEnv::write_in_dir(dir.path(), &[("RC_PW", &password)])
            .expect("write env file");
        assert!(!format!("{guard:?}").contains("hunter2-secret"));
    }
}
