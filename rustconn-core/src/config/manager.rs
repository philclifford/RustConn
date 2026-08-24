//! Configuration manager for TOML file operations
//!
//! This module provides the `ConfigManager` which handles loading and saving
//! configuration files for connections, groups, snippets, and application settings.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use fs2::FileExt;

use super::settings::AppSettings;
use crate::cluster::Cluster;
use crate::error::{ConfigError, ConfigResult};
use crate::models::{
    Connection, ConnectionGroup, ConnectionHistoryEntry, ConnectionTemplate, Snippet,
    WorkspaceProfile,
};
use crate::sync::tombstone::Tombstone;

/// File names for configuration files
const CONNECTIONS_FILE: &str = "connections.toml";
const GROUPS_FILE: &str = "groups.toml";
const SNIPPETS_FILE: &str = "snippets.toml";
const CLUSTERS_FILE: &str = "clusters.toml";
const TEMPLATES_FILE: &str = "templates.toml";
const HISTORY_FILE: &str = "history.toml";
const TRASH_FILE: &str = "trash.toml";
const WORKSPACE_PROFILES_FILE: &str = "workspace_profiles.toml";
const TOMBSTONES_FILE: &str = "tombstones.toml";
const CONFIG_FILE: &str = "config.toml";

/// How long a config write waits for the `.lock` another *process* holds.
///
/// Bounded on purpose. `save_settings` runs synchronously on the GTK main
/// thread, so an unbounded `flock(LOCK_EX)` there is an unbounded UI freeze —
/// and a stale lock is easy to come by: a `rustconn-cli` stopped in a debugger,
/// a second instance wedged on a hung network filesystem. Generous next to the
/// work it protects (serialize a few hundred KB, fsync, rename).
const LOCK_WAIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Poll interval while waiting for the lock.
///
/// `fs2` offers no timed acquire, so the wait is a poll. Short enough that the
/// ordinary case — a lock held for the length of one fsync — is barely delayed.
const LOCK_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(25);

/// Serializes config writes inside this process.
///
/// `flock(2)` is held per *open file description*, so two `acquire_lock()` calls
/// contend even on the same thread — and the app has four independent writers:
/// the three debounce workers in [`crate::connection::ConnectionManager`]
/// (connections, groups, trash), the history flusher on its own thread, and the
/// synchronous `save_settings` calls from GTK callbacks. A single connect starts
/// two of those 2-second debounces at the same instant, so they woke together
/// and one found the lock taken — which is what produced a steady stream of
/// "waiting for another rustconn instance" with no other instance running.
///
/// Taking this first means the in-process writers queue instead of racing, and
/// the `flock` below is left doing the job it is actually for: keeping *other
/// processes* out. Held only inside [`ConfigManager::write_locked`], which does
/// not call itself, so it cannot deadlock against itself.
static CONFIG_WRITE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Wrapper for serializing a list of connections
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct ConnectionsFile {
    #[serde(default)]
    connections: Vec<Connection>,
}

/// Wrapper for serializing a list of groups
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct GroupsFile {
    #[serde(default)]
    groups: Vec<ConnectionGroup>,
}

/// Wrapper for serializing a list of snippets
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct SnippetsFile {
    #[serde(default)]
    snippets: Vec<Snippet>,
}

/// Wrapper for serializing a list of clusters
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct ClustersFile {
    #[serde(default)]
    clusters: Vec<Cluster>,
}

/// Wrapper for serializing a list of templates
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct TemplatesFile {
    #[serde(default)]
    templates: Vec<ConnectionTemplate>,
}

/// Wrapper for serializing connection history
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct HistoryFile {
    #[serde(default)]
    entries: Vec<ConnectionHistoryEntry>,
}

/// Wrapper for serializing Simple Sync tombstones
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct TombstonesFile {
    #[serde(default)]
    tombstones: Vec<Tombstone>,
}

/// Wrapper for serializing trash (deleted items)
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub(super) struct TrashFile {
    #[serde(default)]
    pub connections: Vec<(Connection, chrono::DateTime<chrono::Utc>)>,
    #[serde(default)]
    pub groups: Vec<(ConnectionGroup, chrono::DateTime<chrono::Utc>)>,
}

/// Wrapper for serializing workspace profiles
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct WorkspaceProfilesFile {
    #[serde(default)]
    profiles: Vec<WorkspaceProfile>,
}

/// Configuration manager for `RustConn`
///
/// Handles loading and saving configuration files in TOML format.
/// Configuration is stored in `~/.config/rustconn/` by default.
#[derive(Debug, Clone)]
pub struct ConfigManager {
    /// Base directory for configuration files
    config_dir: PathBuf,
    /// Whether `ensure_config_dir()` has already succeeded (avoids repeated syscalls)
    dir_ensured: std::sync::Arc<AtomicBool>,
}

impl ConfigManager {
    /// Creates a new `ConfigManager` with the default configuration directory
    ///
    /// The default directory is `~/.config/rustconn/`
    ///
    /// # Errors
    ///
    /// Returns an error if the home directory cannot be determined.
    pub fn new() -> ConfigResult<Self> {
        let config_dir = dirs::config_dir()
            .ok_or_else(|| ConfigError::NotFound(PathBuf::from("~/.config")))?
            .join("rustconn");
        Ok(Self {
            config_dir,
            dir_ensured: std::sync::Arc::new(AtomicBool::new(false)),
        })
    }

    /// Creates a new `ConfigManager` with a custom configuration directory
    ///
    /// This is useful for testing or non-standard configurations.
    #[must_use]
    pub fn with_config_dir(config_dir: PathBuf) -> Self {
        Self {
            config_dir,
            dir_ensured: std::sync::Arc::new(AtomicBool::new(false)),
        }
    }

    /// Returns the configuration directory path
    #[must_use]
    pub fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    /// Ensures the configuration directory exists
    ///
    /// Creates the directory and any parent directories if they don't exist.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory cannot be created.
    pub fn ensure_config_dir(&self) -> ConfigResult<()> {
        // Fast path: directory already ensured in this process lifetime
        if self.dir_ensured.load(Ordering::Relaxed) {
            return Ok(());
        }

        if !self.config_dir.exists() {
            fs::create_dir_all(&self.config_dir).map_err(|e| {
                ConfigError::Write(format!(
                    "Failed to create config directory {}: {}",
                    self.config_dir.display(),
                    e
                ))
            })?;
        }

        // Restrict directory permissions to owner-only (0700)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&self.config_dir, fs::Permissions::from_mode(0o700)).map_err(
                |e| {
                    ConfigError::Write(format!(
                        "Failed to set permissions on {}: {}",
                        self.config_dir.display(),
                        e
                    ))
                },
            )?;
        }

        self.dir_ensured.store(true, Ordering::Relaxed);
        Ok(())
    }

    /// Acquires an exclusive advisory lock on the configuration directory.
    ///
    /// Returns the lock file handle, which holds the lock until dropped. When the
    /// lock is held elsewhere this waits for it, but only up to
    /// [`LOCK_WAIT_TIMEOUT`] — it used to block forever, which on the GTK main
    /// thread means a frozen window with no way out.
    ///
    /// Callers that write should go through [`Self::write_locked`], which also
    /// takes [`CONFIG_WRITE_LOCK`] so this process's own writers do not contend
    /// here.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::Lock`] if the lock file cannot be created, if
    /// locking fails outright, or if the lock is still held after
    /// [`LOCK_WAIT_TIMEOUT`].
    pub fn acquire_lock(&self) -> ConfigResult<fs::File> {
        self.ensure_config_dir()?;
        let lock_path = self.config_dir.join(".lock");
        let lock_file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)
            .map_err(|e| {
                ConfigError::Lock(format!(
                    "Failed to open lock file {}: {}",
                    lock_path.display(),
                    e
                ))
            })?;

        if lock_file.try_lock_exclusive().is_ok() {
            return Ok(lock_file);
        }

        // Busy. Poll to a deadline rather than blocking indefinitely. The message
        // no longer claims another *instance* holds it: with CONFIG_WRITE_LOCK in
        // front of every write, reaching here does mean another process, but this
        // function is public and says only what it can actually observe.
        tracing::info!(
            lock = %lock_path.display(),
            timeout_secs = LOCK_WAIT_TIMEOUT.as_secs(),
            "Config lock is held elsewhere; waiting"
        );
        let deadline = std::time::Instant::now() + LOCK_WAIT_TIMEOUT;
        loop {
            std::thread::sleep(LOCK_POLL_INTERVAL);
            if lock_file.try_lock_exclusive().is_ok() {
                return Ok(lock_file);
            }
            if std::time::Instant::now() >= deadline {
                return Err(ConfigError::Lock(format!(
                    "timed out after {}s waiting for the config lock on {}; \
                     another process may be holding it",
                    LOCK_WAIT_TIMEOUT.as_secs(),
                    lock_path.display()
                )));
            }
        }
    }

    /// Writes `content` to `path` atomically, holding both write locks.
    ///
    /// The single place config bytes reach the disk: temp file, owner-only
    /// permissions, fsync, rename. [`Self::save_toml_file`] and
    /// [`Self::save_toml_file_async`] both funnel through here, which is how the
    /// two stay in step — they used to be separate copies of this sequence, each
    /// commented "matches the other".
    fn write_locked(&self, path: &Path, content: &str) -> ConfigResult<()> {
        // In-process writers queue here; see CONFIG_WRITE_LOCK. The guard holds
        // `()`, so a poisoned mutex carries no invalid state and recovering is
        // strictly better than propagating a panic from an unrelated writer.
        let _serialized = CONFIG_WRITE_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        // Advisory lock against other processes (released on drop)
        let _lock = self.acquire_lock()?;

        let temp_path = path.with_extension("tmp");

        fs::write(&temp_path, content).map_err(|e| {
            ConfigError::Write(format!("Failed to write {}: {}", temp_path.display(), e))
        })?;

        // Restrict file permissions to owner-only (0600)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&temp_path, fs::Permissions::from_mode(0o600)).map_err(|e| {
                ConfigError::Write(format!(
                    "Failed to set permissions on {}: {}",
                    temp_path.display(),
                    e
                ))
            })?;
        }

        // Sync data to disk before rename
        {
            let file = fs::File::open(&temp_path).map_err(|e| {
                ConfigError::Write(format!(
                    "Failed to open {} for sync: {}",
                    temp_path.display(),
                    e
                ))
            })?;
            file.sync_all().map_err(|e| {
                ConfigError::Write(format!("Failed to sync {}: {}", temp_path.display(), e))
            })?;
        }

        fs::rename(&temp_path, path).map_err(|e| {
            ConfigError::Write(format!(
                "Failed to rename {} to {}: {}",
                temp_path.display(),
                path.display(),
                e
            ))
        })?;

        Ok(())
    }

    /// Ensures the logs directory exists
    ///
    /// # Errors
    ///
    /// Returns an error if the directory cannot be created.
    pub fn ensure_logs_dir(&self) -> ConfigResult<PathBuf> {
        let logs_dir = self.config_dir.join("logs");
        if !logs_dir.exists() {
            fs::create_dir_all(&logs_dir).map_err(|e| {
                ConfigError::Write(format!(
                    "Failed to create logs directory {}: {}",
                    logs_dir.display(),
                    e
                ))
            })?;
        }
        Ok(logs_dir)
    }

    // ========== Connections ==========

    /// Loads connections from the configuration file
    ///
    /// Returns an empty vector if the file doesn't exist.
    ///
    /// # Errors
    ///
    /// Returns an error if the file exists but cannot be parsed.
    pub fn load_connections(&self) -> ConfigResult<Vec<Connection>> {
        let path = self.config_dir.join(CONNECTIONS_FILE);
        Self::load_toml_file::<ConnectionsFile>(&path).map(|f| f.connections)
    }

    /// Saves connections to the configuration file
    ///
    /// Creates the configuration directory if it doesn't exist.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be written.
    pub fn save_connections(&self, connections: &[Connection]) -> ConfigResult<()> {
        self.ensure_config_dir()?;
        let path = self.config_dir.join(CONNECTIONS_FILE);
        let file = ConnectionsFile {
            connections: connections.to_vec(),
        };
        self.save_toml_file(&path, &file)
    }

    /// Saves connections to the configuration file asynchronously
    ///
    /// Creates the configuration directory if it doesn't exist.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be written.
    pub async fn save_connections_async(&self, connections: &[Connection]) -> ConfigResult<()> {
        self.ensure_config_dir()?;
        let path = self.config_dir.join(CONNECTIONS_FILE);
        let file = ConnectionsFile {
            connections: connections.to_vec(),
        };
        self.save_toml_file_async(&path, &file).await
    }

    // ========== Groups ==========

    /// Loads connection groups from the configuration file
    ///
    /// Returns an empty vector if the file doesn't exist.
    ///
    /// # Errors
    ///
    /// Returns an error if the file exists but cannot be parsed.
    pub fn load_groups(&self) -> ConfigResult<Vec<ConnectionGroup>> {
        let path = self.config_dir.join(GROUPS_FILE);
        Self::load_toml_file::<GroupsFile>(&path).map(|f| f.groups)
    }

    /// Saves connection groups to the configuration file
    ///
    /// Creates the configuration directory if it doesn't exist.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be written.
    pub fn save_groups(&self, groups: &[ConnectionGroup]) -> ConfigResult<()> {
        self.ensure_config_dir()?;
        let path = self.config_dir.join(GROUPS_FILE);
        let file = GroupsFile {
            groups: groups.to_vec(),
        };
        self.save_toml_file(&path, &file)
    }

    /// Saves connection groups to the configuration file asynchronously
    ///
    /// Creates the configuration directory if it doesn't exist.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be written.
    pub async fn save_groups_async(&self, groups: &[ConnectionGroup]) -> ConfigResult<()> {
        self.ensure_config_dir()?;
        let path = self.config_dir.join(GROUPS_FILE);
        let file = GroupsFile {
            groups: groups.to_vec(),
        };
        self.save_toml_file_async(&path, &file).await
    }

    // ========== Snippets ==========

    /// Loads snippets from the configuration file
    ///
    /// Returns an empty vector if the file doesn't exist.
    ///
    /// # Errors
    ///
    /// Returns an error if the file exists but cannot be parsed.
    pub fn load_snippets(&self) -> ConfigResult<Vec<Snippet>> {
        let path = self.config_dir.join(SNIPPETS_FILE);
        Self::load_toml_file::<SnippetsFile>(&path).map(|f| f.snippets)
    }

    /// Saves snippets to the configuration file
    ///
    /// Creates the configuration directory if it doesn't exist.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be written.
    pub fn save_snippets(&self, snippets: &[Snippet]) -> ConfigResult<()> {
        self.ensure_config_dir()?;
        let path = self.config_dir.join(SNIPPETS_FILE);
        let file = SnippetsFile {
            snippets: snippets.to_vec(),
        };
        self.save_toml_file(&path, &file)
    }

    // ========== Clusters ==========

    /// Loads clusters from the configuration file
    ///
    /// Returns an empty vector if the file doesn't exist.
    ///
    /// # Errors
    ///
    /// Returns an error if the file exists but cannot be parsed.
    pub fn load_clusters(&self) -> ConfigResult<Vec<Cluster>> {
        let path = self.config_dir.join(CLUSTERS_FILE);
        Self::load_toml_file::<ClustersFile>(&path).map(|f| f.clusters)
    }

    /// Saves clusters to the configuration file
    ///
    /// Creates the configuration directory if it doesn't exist.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be written.
    pub fn save_clusters(&self, clusters: &[Cluster]) -> ConfigResult<()> {
        self.ensure_config_dir()?;
        let path = self.config_dir.join(CLUSTERS_FILE);
        let file = ClustersFile {
            clusters: clusters.to_vec(),
        };
        self.save_toml_file(&path, &file)
    }

    // ========== Templates ==========

    /// Loads templates from the configuration file
    ///
    /// Returns an empty vector if the file doesn't exist.
    ///
    /// # Errors
    ///
    /// Returns an error if the file exists but cannot be parsed.
    pub fn load_templates(&self) -> ConfigResult<Vec<ConnectionTemplate>> {
        let path = self.config_dir.join(TEMPLATES_FILE);
        Self::load_toml_file::<TemplatesFile>(&path).map(|f| f.templates)
    }

    /// Saves templates to the configuration file
    ///
    /// Creates the configuration directory if it doesn't exist.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be written.
    pub fn save_templates(&self, templates: &[ConnectionTemplate]) -> ConfigResult<()> {
        self.ensure_config_dir()?;
        let path = self.config_dir.join(TEMPLATES_FILE);
        let file = TemplatesFile {
            templates: templates.to_vec(),
        };
        self.save_toml_file(&path, &file)
    }

    // ========== Workspace Profiles ==========

    /// Loads workspace profiles from the configuration file
    ///
    /// Returns an empty vector if the file doesn't exist.
    ///
    /// # Errors
    ///
    /// Returns an error if the file exists but cannot be parsed.
    pub fn load_workspace_profiles(&self) -> ConfigResult<Vec<WorkspaceProfile>> {
        let path = self.config_dir.join(WORKSPACE_PROFILES_FILE);
        Self::load_toml_file::<WorkspaceProfilesFile>(&path).map(|f| f.profiles)
    }

    /// Saves workspace profiles to the configuration file
    ///
    /// Creates the configuration directory if it doesn't exist.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be written.
    pub fn save_workspace_profiles(&self, profiles: &[WorkspaceProfile]) -> ConfigResult<()> {
        self.ensure_config_dir()?;
        let path = self.config_dir.join(WORKSPACE_PROFILES_FILE);
        let file = WorkspaceProfilesFile {
            profiles: profiles.to_vec(),
        };
        self.save_toml_file(&path, &file)
    }

    // ========== Connection History ==========

    /// Loads connection history from the configuration file
    ///
    /// Returns an empty list if the file doesn't exist.
    ///
    /// # Errors
    ///
    /// Returns an error if the file exists but cannot be parsed.
    pub fn load_history(&self) -> ConfigResult<Vec<ConnectionHistoryEntry>> {
        let path = self.config_dir.join(HISTORY_FILE);
        Self::load_toml_file::<HistoryFile>(&path).map(|f| f.entries)
    }

    /// Saves connection history to the configuration file
    ///
    /// Creates the configuration directory if it doesn't exist.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be written.
    pub fn save_history(&self, entries: &[ConnectionHistoryEntry]) -> ConfigResult<()> {
        self.ensure_config_dir()?;
        let path = self.config_dir.join(HISTORY_FILE);
        let file = HistoryFile {
            entries: entries.to_vec(),
        };
        self.save_toml_file(&path, &file)
    }

    // ========== Simple Sync Tombstones ==========

    /// Loads Simple Sync tombstones from the configuration file.
    ///
    /// Returns an empty list if the file doesn't exist.
    ///
    /// # Errors
    ///
    /// Returns an error if the file exists but cannot be parsed.
    pub fn load_tombstones(&self) -> ConfigResult<Vec<Tombstone>> {
        let path = self.config_dir.join(TOMBSTONES_FILE);
        Self::load_toml_file::<TombstonesFile>(&path).map(|f| f.tombstones)
    }

    /// Saves Simple Sync tombstones to the configuration file.
    ///
    /// Creates the configuration directory if it doesn't exist.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be written.
    pub fn save_tombstones(&self, tombstones: &[Tombstone]) -> ConfigResult<()> {
        self.ensure_config_dir()?;
        let path = self.config_dir.join(TOMBSTONES_FILE);
        let file = TombstonesFile {
            tombstones: tombstones.to_vec(),
        };
        self.save_toml_file(&path, &file)
    }

    // ========== Trash ==========

    /// Loads trash (deleted items) from the configuration file
    ///
    /// # Errors
    ///
    /// Returns an error if the file exists but cannot be parsed.
    #[expect(
        clippy::type_complexity,
        reason = "internal helper signature documents the exact tuple layout used by the caller; aliasing would obscure the data flow"
    )]
    pub fn load_trash(
        &self,
    ) -> ConfigResult<(
        Vec<(Connection, chrono::DateTime<chrono::Utc>)>,
        Vec<(ConnectionGroup, chrono::DateTime<chrono::Utc>)>,
    )> {
        let path = self.config_dir.join(TRASH_FILE);
        let file = Self::load_toml_file::<TrashFile>(&path)?;
        Ok((file.connections, file.groups))
    }

    /// Saves trash items to the configuration file asynchronously
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be written.
    pub async fn save_trash_async(
        &self,
        connections: &[(Connection, chrono::DateTime<chrono::Utc>)],
        groups: &[(ConnectionGroup, chrono::DateTime<chrono::Utc>)],
    ) -> ConfigResult<()> {
        self.ensure_config_dir()?;
        let path = self.config_dir.join(TRASH_FILE);
        let file = TrashFile {
            connections: connections.to_vec(),
            groups: groups.to_vec(),
        };
        self.save_toml_file_async(&path, &file).await
    }

    // ========== Application Settings ==========

    /// Loads application settings from the configuration file
    ///
    /// Returns default settings if the file doesn't exist.
    ///
    /// # Errors
    ///
    /// Returns an error if the file exists but cannot be parsed.
    pub fn load_settings(&self) -> ConfigResult<AppSettings> {
        let path = self.config_dir.join(CONFIG_FILE);
        if !path.exists() {
            return Ok(AppSettings::default());
        }
        Self::load_toml_file(&path)
    }

    /// Saves application settings to the configuration file
    ///
    /// Creates the configuration directory if it doesn't exist.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be written.
    pub fn save_settings(&self, settings: &AppSettings) -> ConfigResult<()> {
        self.ensure_config_dir()?;
        let path = self.config_dir.join(CONFIG_FILE);
        self.save_toml_file(&path, settings)
    }

    // ========== Global Variables ==========

    /// Loads global variables from the settings file
    ///
    /// Returns an empty vector if no variables are configured.
    ///
    /// # Errors
    ///
    /// Returns an error if the settings file cannot be read.
    pub fn load_variables(&self) -> ConfigResult<Vec<crate::variables::Variable>> {
        let settings = self.load_settings()?;
        Ok(settings.global_variables)
    }

    /// Saves global variables to the settings file
    ///
    /// # Errors
    ///
    /// Returns an error if the settings file cannot be written.
    pub fn save_variables(&self, variables: &[crate::variables::Variable]) -> ConfigResult<()> {
        let mut settings = self.load_settings()?;
        settings.global_variables = variables.to_vec();
        self.save_settings(&settings)
    }

    // ========== Generic TOML Operations ==========

    /// Loads and parses a TOML file
    ///
    /// Returns the default value if the file doesn't exist.
    fn load_toml_file<T>(path: &Path) -> ConfigResult<T>
    where
        T: serde::de::DeserializeOwned + Default,
    {
        if !path.exists() {
            return Ok(T::default());
        }

        let content = fs::read_to_string(path)
            .map_err(|e| ConfigError::Parse(format!("Failed to read {}: {}", path.display(), e)))?;

        Self::parse_toml(&content, path)
    }

    /// Parses TOML content with validation
    fn parse_toml<T>(content: &str, path: &Path) -> ConfigResult<T>
    where
        T: serde::de::DeserializeOwned,
    {
        toml::from_str(content).map_err(|e| {
            ConfigError::Deserialize(format!("Failed to parse {}: {}", path.display(), e))
        })
    }

    /// Saves data to a TOML file with atomic write (temp file + rename).
    ///
    /// Acquires an exclusive advisory lock before writing to prevent
    /// concurrent modifications from other processes (GUI + CLI).
    fn save_toml_file<T>(&self, path: &Path, data: &T) -> ConfigResult<()>
    where
        T: serde::Serialize,
    {
        let content = toml::to_string_pretty(data)
            .map_err(|e| ConfigError::Serialize(format!("Failed to serialize: {e}")))?;

        self.write_locked(path, &content)
    }

    /// Saves data to a TOML file from async context, without blocking the runtime.
    ///
    /// The write itself is [`Self::write_locked`] on a blocking-pool thread. It
    /// used to be a second, hand-maintained copy of that sequence built out of
    /// `tokio::fs`, which looked asynchronous but opened with a synchronous
    /// `flock(LOCK_EX)` — parking a runtime worker for as long as another writer's
    /// fsync took, and making the caller's `tokio::time::timeout` useless: a timer
    /// only fires when the future yields, and a future stuck in a syscall never
    /// does. `spawn_blocking` puts the blocking work where blocking work belongs
    /// and restores the yield point the timeout needs.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::Serialize`] if `data` cannot be rendered as TOML,
    /// [`ConfigError::Write`] if the blocking task could not be joined, or
    /// whatever [`Self::write_locked`] reports.
    async fn save_toml_file_async<T>(&self, path: &Path, data: &T) -> ConfigResult<()>
    where
        T: serde::Serialize + Sync,
    {
        let content = toml::to_string_pretty(data)
            .map_err(|e| ConfigError::Serialize(format!("Failed to serialize: {e}")))?;

        // Owned copies: `spawn_blocking` needs 'static + Send.
        let path = path.to_path_buf();
        let manager = self.clone();
        tokio::task::spawn_blocking(move || manager.write_locked(&path, &content))
            .await
            .map_err(|e| ConfigError::Write(format!("Config write task failed: {e}")))?
    }

    // ========== Validation ==========

    /// Validates a connection configuration
    ///
    /// # Errors
    ///
    /// Returns an error if the connection is invalid.
    pub fn validate_connection(connection: &Connection) -> ConfigResult<()> {
        use crate::models::ProtocolConfig;

        if connection.name.trim().is_empty() {
            return Err(ConfigError::Validation {
                field: "name".to_string(),
                reason: "Connection name cannot be empty".to_string(),
            });
        }

        // Host and port are optional for Zero Trust connections
        // (the target is defined in the provider config), Serial connections
        // (the target is a local device path, not a network host), and Kubernetes
        // connections (the target is a pod/container, not a network host).
        let is_zerotrust = matches!(connection.protocol_config, ProtocolConfig::ZeroTrust(_));
        let is_serial = matches!(connection.protocol_config, ProtocolConfig::Serial(_));
        let is_kubernetes = matches!(connection.protocol_config, ProtocolConfig::Kubernetes(_));
        let skip_host_port = is_zerotrust || is_serial || is_kubernetes;

        if !skip_host_port && connection.host.trim().is_empty() {
            return Err(ConfigError::Validation {
                field: "host".to_string(),
                reason: "Host cannot be empty".to_string(),
            });
        }

        if !skip_host_port && connection.port == 0 {
            return Err(ConfigError::Validation {
                field: "port".to_string(),
                reason: "Port must be greater than 0".to_string(),
            });
        }

        Ok(())
    }

    /// Validates a connection group
    ///
    /// # Errors
    ///
    /// Returns an error if the group is invalid.
    pub fn validate_group(group: &ConnectionGroup) -> ConfigResult<()> {
        if group.name.trim().is_empty() {
            return Err(ConfigError::Validation {
                field: "name".to_string(),
                reason: "Group name cannot be empty".to_string(),
            });
        }

        Ok(())
    }

    /// Validates a snippet
    ///
    /// # Errors
    ///
    /// Returns an error if the snippet is invalid.
    pub fn validate_snippet(snippet: &Snippet) -> ConfigResult<()> {
        if snippet.name.trim().is_empty() {
            return Err(ConfigError::Validation {
                field: "name".to_string(),
                reason: "Snippet name cannot be empty".to_string(),
            });
        }

        if snippet.command.trim().is_empty() {
            return Err(ConfigError::Validation {
                field: "command".to_string(),
                reason: "Snippet command cannot be empty".to_string(),
            });
        }

        Ok(())
    }

    /// Validates a cluster
    ///
    /// # Errors
    ///
    /// Returns an error if the cluster is invalid.
    pub fn validate_cluster(cluster: &Cluster) -> ConfigResult<()> {
        if cluster.name.trim().is_empty() {
            return Err(ConfigError::Validation {
                field: "name".to_string(),
                reason: "Cluster name cannot be empty".to_string(),
            });
        }

        Ok(())
    }

    /// Validates all connections and returns errors for invalid ones
    #[must_use]
    pub fn validate_connections(connections: &[Connection]) -> Vec<(usize, ConfigError)> {
        connections
            .iter()
            .enumerate()
            .filter_map(|(i, conn)| Self::validate_connection(conn).err().map(|e| (i, e)))
            .collect()
    }

    /// Validates all groups and returns errors for invalid ones
    #[must_use]
    pub fn validate_groups(groups: &[ConnectionGroup]) -> Vec<(usize, ConfigError)> {
        groups
            .iter()
            .enumerate()
            .filter_map(|(i, group)| Self::validate_group(group).err().map(|e| (i, e)))
            .collect()
    }

    /// Validates all snippets and returns errors for invalid ones
    #[must_use]
    pub fn validate_snippets(snippets: &[Snippet]) -> Vec<(usize, ConfigError)> {
        snippets
            .iter()
            .enumerate()
            .filter_map(|(i, snippet)| Self::validate_snippet(snippet).err().map(|e| (i, e)))
            .collect()
    }

    /// Validates all clusters and returns errors for invalid ones
    #[must_use]
    pub fn validate_clusters(clusters: &[Cluster]) -> Vec<(usize, ConfigError)> {
        clusters
            .iter()
            .enumerate()
            .filter_map(|(i, cluster)| Self::validate_cluster(cluster).err().map(|e| (i, e)))
            .collect()
    }

    /// Validates a template
    ///
    /// # Errors
    ///
    /// Returns an error if the template is invalid.
    pub fn validate_template(template: &ConnectionTemplate) -> ConfigResult<()> {
        if template.name.trim().is_empty() {
            return Err(ConfigError::Validation {
                field: "name".to_string(),
                reason: "Template name cannot be empty".to_string(),
            });
        }

        Ok(())
    }

    /// Validates all templates and returns errors for invalid ones
    #[must_use]
    pub fn validate_templates(templates: &[ConnectionTemplate]) -> Vec<(usize, ConfigError)> {
        templates
            .iter()
            .enumerate()
            .filter_map(|(i, template)| Self::validate_template(template).err().map(|e| (i, e)))
            .collect()
    }

    // ========== Backup / Restore ==========

    /// Files included in a settings backup archive.
    const BACKUP_FILES: &[&str] = &[
        CONNECTIONS_FILE,
        GROUPS_FILE,
        SNIPPETS_FILE,
        CLUSTERS_FILE,
        TEMPLATES_FILE,
        HISTORY_FILE,
        CONFIG_FILE,
    ];

    /// Creates a ZIP backup of all configuration files.
    ///
    /// Only files that exist on disk are included. The archive can be
    /// restored with [`restore_from_archive`].
    ///
    /// # Errors
    ///
    /// Returns an error if the archive cannot be created or written.
    pub fn backup_to_archive(&self, dest: &Path) -> ConfigResult<u32> {
        let file = fs::File::create(dest).map_err(|e| {
            ConfigError::Write(format!(
                "Failed to create backup file {}: {e}",
                dest.display()
            ))
        })?;
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);

        let mut count = 0u32;
        for name in Self::BACKUP_FILES {
            let path = self.config_dir.join(name);
            if path.exists() {
                let content = fs::read(&path).map_err(|e| {
                    ConfigError::Parse(format!("Failed to read {}: {e}", path.display()))
                })?;
                zip.start_file(*name, options).map_err(|e| {
                    ConfigError::Write(format!("Failed to add {name} to archive: {e}"))
                })?;
                std::io::Write::write_all(&mut zip, &content).map_err(|e| {
                    ConfigError::Write(format!("Failed to write {name} to archive: {e}"))
                })?;
                count += 1;
            }
        }

        zip.finish()
            .map_err(|e| ConfigError::Write(format!("Failed to finalize backup archive: {e}")))?;

        tracing::info!(path = %dest.display(), files = count, "Settings backup created");
        Ok(count)
    }

    /// Restores configuration files from a ZIP backup archive.
    ///
    /// Only known configuration file names are extracted; unknown entries
    /// are silently skipped. Existing files are overwritten.
    ///
    /// # Errors
    ///
    /// Returns an error if the archive cannot be read or files cannot be written.
    pub fn restore_from_archive(&self, src: &Path) -> ConfigResult<u32> {
        self.ensure_config_dir()?;

        let file = fs::File::open(src).map_err(|e| {
            ConfigError::Parse(format!("Failed to open backup file {}: {e}", src.display()))
        })?;
        let mut archive = zip::ZipArchive::new(file).map_err(|e| {
            ConfigError::Deserialize(format!("Invalid backup archive {}: {e}", src.display()))
        })?;

        let allowed: std::collections::HashSet<&str> = Self::BACKUP_FILES.iter().copied().collect();

        let mut count = 0u32;
        for i in 0..archive.len() {
            let mut entry = archive.by_index(i).map_err(|e| {
                ConfigError::Parse(format!("Failed to read archive entry {i}: {e}"))
            })?;
            let Some(name) = entry.enclosed_name() else {
                continue;
            };
            let name_str = name.to_string_lossy();
            if !allowed.contains(name_str.as_ref()) {
                continue;
            }
            let dest_path = self.config_dir.join(&*name_str);
            let mut content = Vec::new();
            std::io::Read::read_to_end(&mut entry, &mut content).map_err(|e| {
                ConfigError::Parse(format!("Failed to read {name_str} from archive: {e}"))
            })?;
            fs::write(&dest_path, &content).map_err(|e| {
                ConfigError::Write(format!("Failed to write {}: {e}", dest_path.display()))
            })?;
            count += 1;
        }

        tracing::info!(path = %src.display(), files = count, "Settings restored from backup");
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::models::{ProtocolConfig, SshConfig};

    fn create_test_manager() -> (ConfigManager, TempDir) {
        let temp_dir = TempDir::new().unwrap();
        let manager = ConfigManager::with_config_dir(temp_dir.path().to_path_buf());
        (manager, temp_dir)
    }

    #[test]
    fn test_ensure_config_dir() {
        let (manager, _temp) = create_test_manager();
        assert!(manager.ensure_config_dir().is_ok());
        assert!(manager.config_dir().exists());
    }

    #[test]
    fn test_load_empty_connections() {
        let (manager, _temp) = create_test_manager();
        let connections = manager.load_connections().unwrap();
        assert!(connections.is_empty());
    }

    #[test]
    fn test_save_and_load_connections() {
        let (manager, _temp) = create_test_manager();

        let conn = Connection::new(
            "Test Server".to_string(),
            "example.com".to_string(),
            22,
            ProtocolConfig::Ssh(SshConfig::default()),
        );

        manager
            .save_connections(std::slice::from_ref(&conn))
            .unwrap();
        let loaded = manager.load_connections().unwrap();

        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name, conn.name);
        assert_eq!(loaded[0].host, conn.host);
        assert_eq!(loaded[0].port, conn.port);
    }

    #[tokio::test]
    async fn test_save_connections_async() {
        let (manager, _temp) = create_test_manager();

        let conn = Connection::new(
            "Test Async".to_string(),
            "async.example.com".to_string(),
            22,
            ProtocolConfig::Ssh(SshConfig::default()),
        );

        manager
            .save_connections_async(std::slice::from_ref(&conn))
            .await
            .unwrap();
        let loaded = manager.load_connections().unwrap();

        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name, "Test Async");
    }

    #[test]
    fn test_save_and_load_groups() {
        let (manager, _temp) = create_test_manager();

        let group = ConnectionGroup::new("Production".to_string());

        manager.save_groups(std::slice::from_ref(&group)).unwrap();
        let loaded = manager.load_groups().unwrap();

        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name, group.name);
    }

    #[test]
    fn test_save_and_load_snippets() {
        let (manager, _temp) = create_test_manager();

        let snippet = Snippet::new("List files".to_string(), "ls -la".to_string());

        manager
            .save_snippets(std::slice::from_ref(&snippet))
            .unwrap();
        let loaded = manager.load_snippets().unwrap();

        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name, snippet.name);
        assert_eq!(loaded[0].command, snippet.command);
    }

    #[test]
    fn test_save_and_load_settings() {
        let (manager, _temp) = create_test_manager();

        let mut settings = AppSettings::default();
        settings.terminal.font_size = 14;
        settings.logging.enabled = true;

        manager.save_settings(&settings).unwrap();
        let loaded = manager.load_settings().unwrap();

        assert_eq!(loaded.terminal.font_size, 14);
        assert!(loaded.logging.enabled);
    }

    #[test]
    fn test_validate_connection_empty_name() {
        let conn = Connection::new(
            String::new(),
            "example.com".to_string(),
            22,
            ProtocolConfig::Ssh(SshConfig::default()),
        );

        let result = ConfigManager::validate_connection(&conn);
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_connection_empty_host() {
        let conn = Connection::new(
            "Test".to_string(),
            String::new(),
            22,
            ProtocolConfig::Ssh(SshConfig::default()),
        );

        let result = ConfigManager::validate_connection(&conn);
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_group_empty_name() {
        let mut group = ConnectionGroup::new("Test".to_string());
        group.name = String::new();

        let result = ConfigManager::validate_group(&group);
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_snippet_empty_command() {
        let mut snippet = Snippet::new("Test".to_string(), "ls".to_string());
        snippet.command = String::new();

        let result = ConfigManager::validate_snippet(&snippet);
        assert!(result.is_err());
    }

    #[test]
    fn test_save_and_load_clusters() {
        use uuid::Uuid;

        use crate::cluster::Cluster;

        let (manager, _temp) = create_test_manager();

        let mut cluster = Cluster::new("Production Servers".to_string());
        cluster.add_connection(Uuid::new_v4());
        cluster.add_connection(Uuid::new_v4());
        cluster.broadcast_enabled = true;

        manager
            .save_clusters(std::slice::from_ref(&cluster))
            .unwrap();
        let loaded = manager.load_clusters().unwrap();

        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name, cluster.name);
        assert_eq!(loaded[0].id, cluster.id);
        assert_eq!(loaded[0].connection_ids.len(), 2);
        assert!(loaded[0].broadcast_enabled);
    }

    #[test]
    fn test_save_and_load_tombstones() {
        use uuid::Uuid;

        use crate::sync::tombstone::{SyncEntityType, Tombstone};

        let (manager, _temp) = create_test_manager();

        // Empty when no file exists.
        assert!(manager.load_tombstones().unwrap().is_empty());

        let conn_id = Uuid::new_v4();
        let group_id = Uuid::new_v4();
        let tombstones = vec![
            Tombstone::new(SyncEntityType::Connection, conn_id),
            Tombstone::new(SyncEntityType::Group, group_id),
        ];

        manager.save_tombstones(&tombstones).unwrap();
        let loaded = manager.load_tombstones().unwrap();

        assert_eq!(loaded.len(), 2);
        assert!(
            loaded
                .iter()
                .any(|t| t.entity_type == SyncEntityType::Connection && t.id == conn_id)
        );
        assert!(
            loaded
                .iter()
                .any(|t| t.entity_type == SyncEntityType::Group && t.id == group_id)
        );
    }

    #[test]
    fn test_validate_cluster_empty_name() {
        use crate::cluster::Cluster;

        let mut cluster = Cluster::new("Test".to_string());
        cluster.name = String::new();

        let result = ConfigManager::validate_cluster(&cluster);
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_cluster_whitespace_name() {
        use crate::cluster::Cluster;

        let mut cluster = Cluster::new("Test".to_string());
        cluster.name = "   ".to_string();

        let result = ConfigManager::validate_cluster(&cluster);
        assert!(result.is_err());
    }

    #[test]
    fn test_acquire_lock_exclusive() {
        let (manager, _temp) = create_test_manager();
        manager.ensure_config_dir().unwrap();

        // First lock should succeed
        let lock1 = manager.acquire_lock();
        assert!(lock1.is_ok());

        // Second lock from same process should block or fail with try_lock
        let lock_path = manager.config_dir().join(".lock");
        let lock_file2 = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)
            .unwrap();
        // try_lock_exclusive should fail because lock1 is held
        assert!(fs2::FileExt::try_lock_exclusive(&lock_file2).is_err());

        // Drop lock1 — now lock2 should succeed
        drop(lock1);
        assert!(fs2::FileExt::try_lock_exclusive(&lock_file2).is_ok());
    }

    #[test]
    fn test_concurrent_save_with_lock() {
        use std::sync::Arc;
        use std::thread;

        let temp_dir = TempDir::new().unwrap();
        let config_dir = temp_dir.path().to_path_buf();

        let manager1 = ConfigManager::with_config_dir(config_dir.clone());
        let manager2 = ConfigManager::with_config_dir(config_dir);

        manager1.ensure_config_dir().unwrap();

        let m1 = Arc::new(manager1);
        let m2 = Arc::new(manager2);

        let m1_clone = Arc::clone(&m1);
        let m2_clone = Arc::clone(&m2);

        // Two threads saving connections concurrently — no lost updates
        let handle1 = thread::spawn(move || {
            let conn = Connection::new(
                "Server A".to_string(),
                "a.example.com".to_string(),
                22,
                ProtocolConfig::Ssh(SshConfig::default()),
            );
            m1_clone
                .save_connections(std::slice::from_ref(&conn))
                .unwrap();
        });

        let handle2 = thread::spawn(move || {
            let conn = Connection::new(
                "Server B".to_string(),
                "b.example.com".to_string(),
                22,
                ProtocolConfig::Ssh(SshConfig::default()),
            );
            m2_clone
                .save_connections(std::slice::from_ref(&conn))
                .unwrap();
        });

        handle1.join().unwrap();
        handle2.join().unwrap();

        // One of the two writes wins — file is valid TOML with exactly 1 connection
        let loaded = m1.load_connections().unwrap();
        assert_eq!(loaded.len(), 1);
        assert!(loaded[0].name == "Server A" || loaded[0].name == "Server B");
    }
}
