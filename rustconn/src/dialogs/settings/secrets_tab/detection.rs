//! Background CLI detection for secret backends.
//!
//! All functions in this module are `Send` and perform no GTK calls,
//! making them safe to run on a background thread.

use crate::i18n::{i18n, i18n_f};

/// Results of background CLI detection for all secret backends
#[derive(Clone)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "settings/flags struct mirrors persisted config 1:1; bools represent independent toggles, not a state machine"
)]
pub(crate) struct SecretCliDetection {
    pub keepassxc_version: Option<String>,
    pub bitwarden_installed: bool,
    pub bitwarden_cmd: String,
    pub bitwarden_version: Option<String>,
    pub bitwarden_status: Option<(String, &'static str)>,
    pub onepassword_installed: bool,
    pub onepassword_cmd: String,
    pub onepassword_version: Option<String>,
    pub onepassword_status: Option<(String, &'static str)>,
    pub passbolt_installed: bool,
    pub passbolt_version: Option<String>,
    pub passbolt_status: Option<(String, &'static str)>,
    pub passbolt_server_url: Option<String>,
    pub pass_version: Option<String>,
    pub pass_status: Option<(String, &'static str)>,
    /// Whether `secret-tool` binary is available (for keyring operations)
    pub secret_tool_available: bool,
    /// Fine-grained availability of the platform system-keyring backend
    /// (libsecret/Secret Service on Linux/BSD, Keychain on macOS). Lets the
    /// Secrets tab show whether the keyring is genuinely usable, not just
    /// whether the client binary exists (#201).
    pub system_keyring_availability: rustconn_core::secret::BackendAvailability,
}

/// Whether the selected backend can actually store and read a password.
///
/// The Secrets page already showed a version number and, for some backends, a
/// status line — but the version row answered "is the client installed", which is
/// the least interesting of the prerequisites, and the status line existed for
/// four backends out of eight. Nothing anywhere answered the question the user is
/// really asking when they pick from that list. This is that answer, in one shape
/// for every row, so the page can show one line per backend instead of a
/// different arrangement per backend.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum BackendReadiness {
    /// Usable right now.
    ///
    /// Carries a detail when the probe learned something worth repeating — which
    /// account is signed in, where the store lives — and an empty string when
    /// there is nothing to add beyond "ready".
    Ready(String),
    /// The client is there, but something has to happen before it will work —
    /// logging in, unlocking, initialising a store, choosing a file.
    ///
    /// This is the state the three-variant `BackendAvailability` cannot express,
    /// and the reason a logged-out Bitwarden looks the same to the startup check
    /// as a working one: `is_available()` is a bool, and a CLI that runs at all
    /// answers `true`.
    NeedsAction(String),
    /// The client program is not installed, so nothing can be done in RustConn.
    NotInstalled,
    /// Detection has not finished yet.
    Unknown,
}

impl BackendReadiness {
    /// The line to show the user.
    pub(crate) fn label(&self) -> String {
        match self {
            Self::Ready(detail) if detail.is_empty() => i18n("Ready"),
            Self::Ready(detail) | Self::NeedsAction(detail) => detail.clone(),
            Self::NotInstalled => i18n("Not installed"),
            Self::Unknown => i18n("Checking..."),
        }
    }

    /// The css class to render this verdict with.
    ///
    /// The label always names the state as well, so status is never carried by
    /// colour alone (GNOME HIG / WCAG).
    pub(crate) const fn css_class(&self) -> &'static str {
        match self {
            Self::Ready(_) => "success",
            Self::NeedsAction(_) => "warning",
            Self::NotInstalled => "error",
            Self::Unknown => "dim-label",
        }
    }

    /// Whether saving a password to this backend can be expected to work.
    ///
    /// `Unknown` counts as usable: an unfinished probe is not evidence of a
    /// problem, and treating a `--version` call that has not returned yet as a
    /// fault would make the page argue with the user on a slow machine.
    pub(crate) const fn is_usable(&self) -> bool {
        matches!(self, Self::Ready(_) | Self::Unknown)
    }
}

/// The parts of a backend's readiness that live in the dialog, not in a probe.
///
/// Read from the widgets rather than from the saved `SecretSettings`, and that is
/// the point: someone who has just chosen a database file has not saved it yet,
/// so a verdict computed from the stored configuration would report the state
/// they are in the middle of leaving. Three fields because these are the only
/// prerequisites a probe cannot see.
pub(crate) struct LocalBackendState {
    /// The "Use KeePass integration" switch.
    pub kdbx_enabled: bool,
    /// The database path currently in the entry, expanded.
    pub kdbx_path: Option<std::path::PathBuf>,
    /// Whether the portable file's passphrase field has anything in it.
    pub portable_passphrase_entered: bool,
}

impl LocalBackendState {
    /// Reads the same three prerequisites out of saved settings.
    ///
    /// For callers outside the dialog — the startup check and the post-save
    /// re-check — where there are no widgets to read and the saved configuration
    /// *is* the state in force.
    pub(crate) fn from_settings(secrets: &rustconn_core::config::SecretSettings) -> Self {
        Self {
            kdbx_enabled: secrets.kdbx_enabled,
            kdbx_path: secrets.kdbx_path.clone(),
            portable_passphrase_entered: secrets.portable_passphrase.is_some(),
        }
    }
}

/// Renders the readiness of `backend` from a finished detection pass.
///
/// `detection` is `None` while the background probe is still running, which is
/// the only source of [`BackendReadiness::Unknown`].
///
/// The per-backend status strings this reuses are the ones the page already
/// computed and displayed; what is new is that every backend produces a verdict,
/// including the four that previously had no status line at all — the two file
/// backends, KeePassXC and the system keyring, whose row existed but was shown
/// for one selection only.
pub(crate) fn backend_readiness(
    detection: Option<&SecretCliDetection>,
    backend: rustconn_core::config::SecretBackendType,
    local: &LocalBackendState,
) -> BackendReadiness {
    use rustconn_core::config::SecretBackendType;
    use rustconn_core::secret::BackendAvailability;

    let Some(det) = detection else {
        return BackendReadiness::Unknown;
    };

    // The status pairs carry a css class alongside the text, and the class is
    // already the backend's own verdict: "success" means usable, "warning" means
    // a step is missing, "error" means it cannot work. Reading it keeps this
    // function from re-deriving conclusions the probes already reached.
    let from_status = |status: Option<&(String, &'static str)>| match status {
        // Keep the detail — "Signed in: someone@example.com" and
        // "Initialized at /home/…/.password-store" tell the user which account
        // and which store, which is worth more than a bare "Ready".
        Some((text, "success")) => BackendReadiness::Ready(text.clone()),
        Some((text, _)) => BackendReadiness::NeedsAction(text.clone()),
        None => BackendReadiness::NotInstalled,
    };

    match backend {
        SecretBackendType::Bitwarden => from_status(det.bitwarden_status.as_ref()),
        SecretBackendType::OnePassword => from_status(det.onepassword_status.as_ref()),
        SecretBackendType::Passbolt => from_status(det.passbolt_status.as_ref()),
        SecretBackendType::Pass => from_status(det.pass_status.as_ref()),

        SecretBackendType::LibSecret | SecretBackendType::MacOsKeychain => {
            match det.system_keyring_availability {
                BackendAvailability::Available => BackendReadiness::Ready(String::new()),
                BackendAvailability::ServiceUnavailable => {
                    BackendReadiness::NeedsAction(i18n("No keyring service responding"))
                }
                BackendAvailability::ClientMissing => BackendReadiness::NotInstalled,
            }
        }

        // KeePassXC needs three things and the page only ever reported the first.
        // A database that is not configured is the common case for someone who
        // has just selected the backend, and it is not "not installed".
        SecretBackendType::KeePassXc | SecretBackendType::KdbxFile => {
            if det.keepassxc_version.is_none() {
                return BackendReadiness::NotInstalled;
            }
            if !local.kdbx_enabled {
                return BackendReadiness::NeedsAction(i18n("Turn on KeePass integration below"));
            }
            match local.kdbx_path.as_ref() {
                None => BackendReadiness::NeedsAction(i18n("Choose a database file below")),
                Some(path) if !path.exists() => BackendReadiness::NeedsAction(i18n_f(
                    "Database file not found: {}",
                    &[&path.display().to_string()],
                )),
                Some(_) => BackendReadiness::Ready(String::new()),
            }
        }

        // Needs nothing outside RustConn — the key is derived from the machine.
        SecretBackendType::EncryptedFile => BackendReadiness::Ready(String::new()),

        // Usable once a passphrase has been supplied this session. Whether one
        // has is session state the page does not hold, so this reports the part
        // it can see: a passphrase is configured or it is not.
        SecretBackendType::PortableEncryptedFile => {
            if local.portable_passphrase_entered {
                BackendReadiness::Ready(String::new())
            } else {
                BackendReadiness::NeedsAction(i18n("Enter the file's passphrase below"))
            }
        }
    }
}

/// Cached detection result: probing spawns ~10 child processes, so reuse
/// the result when the settings dialog is reopened shortly after.
/// Vault lock/unlock actions in the dialog refresh their status labels
/// directly (not through this cache), so staleness is bounded to reopen.
static DETECTION_CACHE: std::sync::Mutex<
    Option<(std::time::Instant, Option<String>, SecretCliDetection)>,
> = std::sync::Mutex::new(None);

/// 30s keeps reopen instant while bounding stale backend status.
const DETECTION_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(30);

/// Runs all secret backend CLI detection on a background thread.
/// This function is `Send` and performs no GTK calls.
///
/// Results are cached for [`DETECTION_CACHE_TTL`]; independent backends are
/// probed in parallel so total latency equals the slowest probe, not the sum.
pub(crate) fn detect_secret_backends(pass_store_dir: Option<String>) -> SecretCliDetection {
    // The `pass` store directory is part of the cache key, not just an argument.
    // The cache is one slot, so without this a probe made with the configured
    // directory would be served to a caller asking about a different one — and the
    // 30-second window is exactly long enough to cover someone changing the
    // directory and looking at the Status row.
    if let Ok(guard) = DETECTION_CACHE.lock()
        && let Some((detected_at, probed_store_dir, cached)) = guard.as_ref()
        && detected_at.elapsed() < DETECTION_CACHE_TTL
        && *probed_store_dir == pass_store_dir
    {
        return cached.clone();
    }

    let detection = run_detection(pass_store_dir.as_deref());

    if let Ok(mut guard) = DETECTION_CACHE.lock() {
        *guard = Some((std::time::Instant::now(), pass_store_dir, detection.clone()));
    }
    detection
}

/// Probes every backend in parallel scoped threads.
///
/// Each probe only spawns short-lived child processes (`--version`,
/// `status`), so a panic is a programming bug; in that case the backend is
/// reported as not installed rather than poisoning the whole detection.
fn run_detection(pass_store_dir: Option<&str>) -> SecretCliDetection {
    std::thread::scope(|scope| {
        let keepassxc = scope.spawn(detect_keepassxc);
        let bitwarden = scope.spawn(detect_bitwarden);
        let onepassword = scope.spawn(detect_onepassword);
        let passbolt = scope.spawn(detect_passbolt);
        let pass = scope.spawn(move || detect_pass(pass_store_dir));
        let secret_tool = scope.spawn(detect_secret_tool);
        let keyring_avail = scope.spawn(detect_system_keyring_availability);

        let keepassxc_version = keepassxc.join().unwrap_or_default();
        let (bitwarden_installed, bitwarden_cmd, bitwarden_version, bitwarden_status) = bitwarden
            .join()
            .unwrap_or_else(|_| (false, "bw".to_string(), None, None));
        let (onepassword_installed, onepassword_cmd, onepassword_version, onepassword_status) =
            onepassword
                .join()
                .unwrap_or_else(|_| (false, "op".to_string(), None, None));
        let (passbolt_installed, passbolt_version, passbolt_status, passbolt_server_url) =
            passbolt.join().unwrap_or_default();
        let (pass_version, pass_status) = pass.join().unwrap_or_default();
        let secret_tool_available = secret_tool.join().unwrap_or_default();
        let system_keyring_availability = keyring_avail
            .join()
            .unwrap_or(rustconn_core::secret::BackendAvailability::ServiceUnavailable);

        SecretCliDetection {
            keepassxc_version,
            bitwarden_installed,
            bitwarden_cmd,
            bitwarden_version,
            bitwarden_status,
            onepassword_installed,
            onepassword_cmd,
            onepassword_version,
            onepassword_status,
            passbolt_installed,
            passbolt_version,
            passbolt_status,
            passbolt_server_url,
            pass_version,
            pass_status,
            secret_tool_available,
            system_keyring_availability,
        }
    })
}

/// How long any single CLI probe on this page is given before it is killed.
///
/// None of them had a deadline, and the shape of `run_detection` is what made
/// that expensive: the probes are scoped threads and the scope is not joined
/// until the last of them returns, so one unresponsive CLI kept the whole
/// Secrets page empty rather than costing it one row. The three most likely to
/// stall are the three that do I/O — `bw status` reaches the network once its
/// session has expired, `op whoami` can sit on a biometric prompt, and
/// `passbolt list user` talks to a server.
///
/// Five seconds: nobody is waiting on a credential here, the answers only
/// populate rows, and a probe that gives up reads as "not installed", which is
/// already what a failed probe reads as.
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Runs one CLI probe, returning `None` rather than blocking the page.
///
/// `what` goes into a log line, so it is `&'static str`: a `&str` would let a
/// caller interpolate a resolved path — and these paths include `$HOME` — or a
/// credential into it.
fn probe(cmd: &mut std::process::Command, what: &'static str) -> Option<std::process::Output> {
    let child = cmd
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .ok()?;
    rustconn_core::proc::wait_bounded(child, PROBE_TIMEOUT, what)
        .ok()?
        .output()
}

/// Detects the KeePassXC CLI version.
///
/// Delegates to the core detector, which resolves `keepassxc-cli` on the host
/// via `flatpak-spawn --host` when running inside a Flatpak sandbox (#182).
fn detect_keepassxc() -> Option<String> {
    rustconn_core::secret::KeePassStatus::detect().keepassxc_version
}

/// Detects the Bitwarden CLI: `(installed, cmd, version, status)`
fn detect_bitwarden() -> (bool, String, Option<String>, Option<(String, &'static str)>) {
    let mut bw_paths: Vec<String> = vec!["bw".to_string()];
    if !rustconn_core::flatpak::is_flatpak() {
        bw_paths.extend(["/snap/bin/bw".to_string(), "/usr/local/bin/bw".to_string()]);
    }
    if let Some(cli_dir) = rustconn_core::cli_download::get_cli_install_dir() {
        let flatpak_bw = cli_dir.join("bitwarden").join("bw");
        if flatpak_bw.exists() {
            bw_paths.push(flatpak_bw.to_string_lossy().to_string());
        }
    }
    let mut bitwarden_installed = false;
    let mut bitwarden_cmd = "bw".to_string();
    for path in &bw_paths {
        if probe(
            std::process::Command::new(path).arg("--version"),
            "bw --version",
        )
        .is_some_and(|output| output.status.success())
        {
            bitwarden_installed = true;
            bitwarden_cmd = path.clone();
            break;
        }
    }
    if !bitwarden_installed && let Some(path) = rustconn_core::which::find_in_path("bw") {
        bitwarden_installed = true;
        bitwarden_cmd = path.display().to_string();
    }
    let bitwarden_version = if bitwarden_installed {
        get_cli_version(&bitwarden_cmd, &["--version"])
    } else {
        None
    };
    let bitwarden_status = if bitwarden_installed {
        Some(check_bitwarden_status_sync(&bitwarden_cmd))
    } else {
        None
    };

    (
        bitwarden_installed,
        bitwarden_cmd,
        bitwarden_version,
        bitwarden_status,
    )
}

/// Detects the 1Password CLI: `(installed, cmd, version, status)`
fn detect_onepassword() -> (bool, String, Option<String>, Option<(String, &'static str)>) {
    let mut op_paths: Vec<String> = vec!["op".to_string()];
    if !rustconn_core::flatpak::is_flatpak() {
        op_paths.push("/usr/local/bin/op".to_string());
    }
    if let Some(cli_dir) = rustconn_core::cli_download::get_cli_install_dir() {
        let flatpak_op = cli_dir.join("1password").join("op");
        if flatpak_op.exists() {
            op_paths.push(flatpak_op.to_string_lossy().to_string());
        }
    }
    let mut onepassword_installed = false;
    let mut onepassword_cmd = "op".to_string();
    for path in &op_paths {
        if probe(
            std::process::Command::new(path).arg("--version"),
            "op --version",
        )
        .is_some_and(|output| output.status.success())
        {
            onepassword_installed = true;
            onepassword_cmd = path.clone();
            break;
        }
    }
    if !onepassword_installed && let Some(path) = rustconn_core::which::find_in_path("op") {
        onepassword_installed = true;
        onepassword_cmd = path.display().to_string();
    }
    let onepassword_version = if onepassword_installed {
        get_cli_version(&onepassword_cmd, &["--version"])
    } else {
        None
    };
    let onepassword_status = if onepassword_installed {
        Some(check_onepassword_status_sync(&onepassword_cmd))
    } else {
        None
    };

    (
        onepassword_installed,
        onepassword_cmd,
        onepassword_version,
        onepassword_status,
    )
}

/// Detects the Passbolt CLI: `(installed, version, status, server_url)`
fn detect_passbolt() -> (
    bool,
    Option<String>,
    Option<(String, &'static str)>,
    Option<String>,
) {
    let mut passbolt_paths: Vec<String> = vec!["passbolt".to_string()];
    if !rustconn_core::flatpak::is_flatpak() {
        passbolt_paths.push("/usr/local/bin/passbolt".to_string());
    }
    if let Some(cli_dir) = rustconn_core::cli_download::get_cli_install_dir() {
        let flatpak_pb = cli_dir.join("passbolt").join("passbolt");
        if flatpak_pb.exists() {
            passbolt_paths.push(flatpak_pb.to_string_lossy().to_string());
        }
    }
    let mut passbolt_installed = false;
    for path in &passbolt_paths {
        if probe(
            std::process::Command::new(path).arg("--version"),
            "passbolt --version",
        )
        .is_some_and(|output| output.status.success())
        {
            passbolt_installed = true;
            break;
        }
    }
    if !passbolt_installed && rustconn_core::which::is_available("passbolt") {
        passbolt_installed = true;
    }
    let passbolt_version = if passbolt_installed {
        get_cli_version("passbolt", &["--version"])
    } else {
        None
    };
    let passbolt_status = if passbolt_installed {
        Some(check_passbolt_status_sync())
    } else {
        None
    };
    let passbolt_server_url = read_passbolt_server_url_sync();

    (
        passbolt_installed,
        passbolt_version,
        passbolt_status,
        passbolt_server_url,
    )
}

/// Detects the `pass` password store: `(version, status)`
///
/// `configured_store_dir` is the directory the user has set for this backend, if
/// any — it has to be passed in because the probe runs on a background thread and
/// the value lives in a GTK entry.
fn detect_pass(
    configured_store_dir: Option<&str>,
) -> (Option<String>, Option<(String, &'static str)>) {
    let pass_version = if let Some(output) = probe(
        std::process::Command::new("pass").arg("--version"),
        "pass --version",
    ) {
        if output.status.success() {
            let version_str = String::from_utf8_lossy(&output.stdout);
            // Extract version number from output like "v1.7.4"
            // Find the line containing 'v' followed by digits
            version_str
                .lines()
                .find(|line| line.contains('v') && line.chars().any(|c| c.is_ascii_digit()))
                .and_then(|line| {
                    // Extract just the version part: find 'v' and capture digits/dots after it
                    line.split_whitespace()
                        .find(|word| {
                            word.starts_with('v')
                                && word[1..].chars().next().is_some_and(|c| c.is_ascii_digit())
                        })
                        .map(|v| v.trim_start_matches('v').to_string())
                })
        } else {
            None
        }
    } else {
        None
    };

    let pass_status = if pass_version.is_some() {
        // Which store to look at, in the same order the backend resolves it:
        // the directory configured in this very dialog first, then
        // `$PASSWORD_STORE_DIR`, then pass's own default.
        //
        // The configured value was missing from this list, so the probe read the
        // *ambient* environment while `PassBackend::setup_command` puts the
        // configured directory into the child's environment. A user with a custom
        // store was therefore told "Not initialized (run 'pass init <gpg-id>')"
        // about a healthy store, or "Initialized at ~/.password-store" while the
        // backend read somewhere else entirely. That verdict is not cosmetic — it
        // feeds `BackendReadiness::is_usable`.
        let store_dir = configured_store_dir
            .map(|dir| dir.to_string())
            .or_else(|| std::env::var("PASSWORD_STORE_DIR").ok())
            .or_else(|| {
                dirs::home_dir().map(|h| h.join(".password-store").to_string_lossy().to_string())
            });

        if let Some(dir) = store_dir {
            let store_path = std::path::PathBuf::from(&dir);
            if store_path.exists() && store_path.join(".gpg-id").exists() {
                Some((
                    i18n_f("Initialized at {}", &[&store_path.display().to_string()]),
                    "success",
                ))
            } else {
                Some((
                    i18n("Not initialized (run 'pass init &lt;gpg-id&gt;')"),
                    "warning",
                ))
            }
        } else {
            Some((i18n("Cannot determine store directory"), "error"))
        }
    } else {
        None
    };

    (pass_version, pass_status)
}

/// Checks `secret-tool` availability (for system keyring operations)
fn detect_secret_tool() -> bool {
    rustconn_core::which::is_available("secret-tool")
}

/// Probes the platform system-keyring backend for fine-grained availability.
///
/// Runs the same read-only probe the keyring backend uses (`availability()`),
/// so the Secrets tab can show whether the keyring is genuinely usable —
/// distinguishing a missing client from an unresponsive Secret Service (#201) —
/// rather than only whether the client binary exists. Bounded by the same 5s
/// budget as the startup `has_secret_backend` check.
fn detect_system_keyring_availability() -> rustconn_core::secret::BackendAvailability {
    use rustconn_core::secret::{BackendAvailability, SecretBackend};

    // 5s mirrors the startup availability budget (R2.4).
    const KEYRING_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

    crate::async_utils::with_runtime(|rt| {
        rt.block_on(async {
            #[cfg(target_os = "macos")]
            let probe = {
                let backend = rustconn_core::secret::MacOsKeychainBackend::new();
                tokio::time::timeout(KEYRING_PROBE_TIMEOUT, async move {
                    backend.availability().await
                })
                .await
            };
            #[cfg(not(target_os = "macos"))]
            let probe = {
                let backend = rustconn_core::secret::LibSecretBackend::new("rustconn");
                tokio::time::timeout(KEYRING_PROBE_TIMEOUT, async move {
                    backend.availability().await
                })
                .await
            };
            probe.unwrap_or(BackendAvailability::ServiceUnavailable)
        })
    })
    .unwrap_or(BackendAvailability::ServiceUnavailable)
}

/// Gets CLI version from command output
fn get_cli_version(command: &str, args: &[&str]) -> Option<String> {
    probe(
        std::process::Command::new(command).args(args),
        "cli --version",
    )
    .filter(|o| o.status.success())
    .and_then(|o| {
        let output = String::from_utf8_lossy(&o.stdout);
        parse_version(&output)
    })
}

/// Parses version from output string
fn parse_version(output: &str) -> Option<String> {
    rustconn_core::secret::VERSION_REGEX
        .captures(output)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
}

/// Checks Bitwarden vault status synchronously
pub(super) fn check_bitwarden_status_sync(bw_cmd: &str) -> (String, &'static str) {
    let output = probe(
        std::process::Command::new(bw_cmd).arg("status"),
        "bw status",
    );

    match output {
        Some(o) if o.status.success() => {
            let status_str = String::from_utf8_lossy(&o.stdout);
            if let Ok(status) = serde_json::from_str::<serde_json::Value>(&status_str)
                && let Some(status_val) = status.get("status").and_then(|v| v.as_str())
            {
                return match status_val {
                    "unlocked" => (i18n("Unlocked"), "success"),
                    "locked" => (i18n("Locked"), "warning"),
                    "unauthenticated" => (i18n("Not logged in"), "error"),
                    _ => (i18n_f("Status: {}", &[status_val]), "dim-label"),
                };
            }
            (i18n("Unknown"), "dim-label")
        }
        _ => (i18n("Error checking status"), "error"),
    }
}

/// Checks 1Password account status synchronously
fn check_onepassword_status_sync(op_cmd: &str) -> (String, &'static str) {
    let output = probe(
        std::process::Command::new(op_cmd).args(["whoami", "--format", "json"]),
        "op whoami",
    );

    match output {
        Some(o) if o.status.success() => {
            let stdout = String::from_utf8_lossy(&o.stdout);
            if let Ok(whoami) = serde_json::from_str::<serde_json::Value>(&stdout)
                && let Some(email) = whoami.get("email").and_then(|v| v.as_str())
            {
                return (i18n_f("Signed in: {}", &[email]), "success");
            }
            (i18n("Signed in"), "success")
        }
        Some(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr);
            if stderr.contains("not signed in") || stderr.contains("sign in") {
                (i18n("Not signed in"), "error")
            } else if stderr.contains("session expired") {
                (i18n("Session expired"), "warning")
            } else {
                (i18n("Not signed in"), "error")
            }
        }
        None => (i18n("Error checking status"), "error"),
    }
}

/// Checks Passbolt CLI configuration status synchronously
fn check_passbolt_status_sync() -> (String, &'static str) {
    let output = probe(
        std::process::Command::new("passbolt").args(["list", "user", "--json"]),
        "passbolt list user",
    );

    match output {
        Some(o) if o.status.success() => (i18n("Configured"), "success"),
        Some(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr);
            if stderr.contains("no configuration") {
                (i18n("Not configured"), "error")
            } else if stderr.contains("authentication") || stderr.contains("passphrase") {
                (i18n("Authentication failed"), "warning")
            } else {
                (i18n("Not configured"), "error")
            }
        }
        None => (i18n("Error checking status"), "error"),
    }
}

/// Reads the Passbolt server URL from the CLI configuration file (sync)
pub(super) fn read_passbolt_server_url_sync() -> Option<String> {
    let home = std::env::var("HOME").ok()?;
    let config_path = std::path::PathBuf::from(home)
        .join(".config")
        .join("go-passbolt-cli")
        .join("config.json");

    let content = std::fs::read_to_string(config_path).ok()?;
    let config: serde_json::Value = serde_json::from_str(&content).ok()?;
    config
        .get("serverAddress")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from)
}

/// Extracts session key from `bw unlock` output
pub(super) fn extract_session_key(output: &str) -> Option<String> {
    // Output format: export BW_SESSION="<session_key>"
    // or: $ export BW_SESSION="<session_key>"
    for line in output.lines() {
        if line.contains("BW_SESSION=") {
            // Extract the value between quotes
            if let Some(start) = line.find('"')
                && let Some(end) = line.rfind('"')
                && end > start
            {
                return Some(line[start + 1..end].to_string());
            }
            // Try without quotes (BW_SESSION=value)
            if let Some(pos) = line.find("BW_SESSION=") {
                let value_start = pos + "BW_SESSION=".len();
                let value = line[value_start..].trim().trim_matches('"');
                if !value.is_empty() {
                    return Some(value.to_string());
                }
            }
        }
    }
    None
}
