//! Remote Desktop Manager (RDM) JSON importer.
//!
//! Parses RDM export files in JSON format.
//! Supports importing SSH, RDP, VNC, and Telnet connections with folder hierarchy.
//!
//! Two dialects of the same format are handled:
//!
//! * The documented import format, where `ConnectionType` is a token name
//!   (`"RDPConfigured"`, `"SSHShell"`, ...) and the destination folder is a
//!   backslash separated path in `Group`.
//! * Real RDM exports, where `ConnectionType` is the numeric value of the
//!   Devolutions `ConnectionType` enum (`1` = `RDPConfigured`, `25` = `Group`,
//!   `77` = `SSHShell`, ...).

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use secrecy::ExposeSecret;
use serde::{Deserialize, Deserializer};
use uuid::Uuid;

use super::normalize::parse_host_port;
use super::traits::{ImportResult, ImportSource, SkippedEntry};
use crate::error::ImportError;
use crate::models::{
    AutomationConfig, Connection, ConnectionGroup, Credentials, PasswordSource, ProtocolConfig,
    ProtocolType, RdpConfig, SshAuthMethod, SshConfig, TelnetConfig, VncConfig, WindowMode,
};
use crate::progress::ProgressReporter;

/// Maps the numeric Devolutions `ConnectionType` enum to its token name.
///
/// RDM serialises the enum as an integer in real exports while the documented
/// import format uses the token name, so both have to be understood. Only the
/// values RustConn can act on are listed; everything else is reported as an
/// unsupported entry type.
fn connection_type_token(value: u64) -> Option<&'static str> {
    match value {
        1 => Some("rdpconfigured"),
        2 => Some("rdpfilename"),
        4 => Some("vnc"),
        8 => Some("putty"),
        25 => Some("group"),
        26 => Some("credential"),
        28 => Some("ftp"),
        32 => Some("website"),
        38 => Some("scp"),
        62 => Some("terminalconsole"),
        67 => Some("securecrt"),
        68 => Some("iterm"),
        74 => Some("telnet"),
        77 => Some("sshshell"),
        92 => Some("root"),
        100 => Some("sftp"),
        104 => Some("novnc"),
        _ => None,
    }
}

/// Deserializes `ConnectionType`, which may be a token name or an enum number.
///
/// Unknown numbers are kept as their decimal representation so the skipped
/// entry reason names the actual value found in the file.
fn deserialize_connection_type<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Raw {
        Token(String),
        Number(i64),
    }

    let raw = Option::<Raw>::deserialize(deserializer)?;
    Ok(raw.map(|raw| match raw {
        Raw::Token(token) => token.trim().to_lowercase(),
        Raw::Number(number) => u64::try_from(number)
            .ok()
            .and_then(connection_type_token)
            .map_or_else(|| number.to_string(), ToString::to_string),
    }))
}

/// Deserializes a port that may be encoded as a number or as a string.
fn deserialize_optional_port<'de, D>(deserializer: D) -> Result<Option<u16>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Raw {
        Number(i64),
        Text(String),
    }

    let raw = Option::<Raw>::deserialize(deserializer)?;
    Ok(raw.and_then(|raw| match raw {
        Raw::Number(number) => u16::try_from(number).ok(),
        Raw::Text(text) => text.trim().parse().ok(),
    }))
}

/// Nested `Credentials` structure RDM uses for entry credentials.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RdmCredentials {
    #[serde(
        default,
        deserialize_with = "crate::secret::serde_helpers::deserialize_tolerant_string"
    )]
    user_name: Option<String>,
    #[serde(
        default,
        deserialize_with = "crate::secret::serde_helpers::deserialize_tolerant_string"
    )]
    domain: Option<String>,
    #[serde(
        default,
        deserialize_with = "crate::secret::serde_helpers::deserialize_tolerant_secret"
    )]
    password: Option<secrecy::SecretString>,
}

/// RDM JSON connection entry
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RdmConnection {
    #[serde(default)]
    name: String,
    #[serde(
        default,
        rename = "ID",
        deserialize_with = "crate::secret::serde_helpers::deserialize_tolerant_string"
    )]
    id: Option<String>,
    #[serde(default, deserialize_with = "deserialize_connection_type")]
    connection_type: Option<String>,
    /// Target host. RDM stores it in `Host` for terminal entries and in `Url`
    /// for RDP and web based entries.
    #[serde(
        default,
        alias = "Url",
        alias = "HostName",
        alias = "Server",
        deserialize_with = "crate::secret::serde_helpers::deserialize_tolerant_string"
    )]
    host: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_port")]
    port: Option<u16>,
    #[serde(
        default,
        alias = "UserName",
        deserialize_with = "crate::secret::serde_helpers::deserialize_tolerant_string"
    )]
    username: Option<String>,
    #[serde(
        default,
        deserialize_with = "crate::secret::serde_helpers::deserialize_tolerant_secret"
    )]
    password: Option<secrecy::SecretString>,
    #[serde(
        default,
        deserialize_with = "crate::secret::serde_helpers::deserialize_tolerant_string"
    )]
    domain: Option<String>,
    #[serde(
        default,
        deserialize_with = "crate::secret::serde_helpers::deserialize_tolerant_string"
    )]
    description: Option<String>,
    /// Backslash separated destination folder, e.g. `Customers\ACME`.
    #[serde(
        default,
        deserialize_with = "crate::secret::serde_helpers::deserialize_tolerant_string"
    )]
    group: Option<String>,
    #[serde(
        default,
        rename = "ParentID",
        deserialize_with = "crate::secret::serde_helpers::deserialize_tolerant_string"
    )]
    parent_id: Option<String>,
    /// Credentials embedded in the entry.
    credentials: Option<RdmCredentials>,
    /// Reference to a separate `Credential` entry.
    #[serde(
        default,
        rename = "CredentialConnectionID",
        deserialize_with = "crate::secret::serde_helpers::deserialize_tolerant_string"
    )]
    credential_connection_id: Option<String>,
    // SSH specific
    #[serde(
        default,
        rename = "PrivateKeyPath",
        deserialize_with = "crate::secret::serde_helpers::deserialize_tolerant_string"
    )]
    private_key_path: Option<String>,
    // VNC specific
    #[serde(rename = "ViewOnly")]
    view_only: Option<bool>,
}

/// RDM JSON folder entry
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RdmFolder {
    #[serde(
        default,
        rename = "ID",
        deserialize_with = "crate::secret::serde_helpers::deserialize_tolerant_string"
    )]
    id: Option<String>,
    #[serde(default)]
    name: String,
    #[serde(
        default,
        rename = "ParentID",
        deserialize_with = "crate::secret::serde_helpers::deserialize_tolerant_string"
    )]
    parent_id: Option<String>,
}

/// RDM JSON export structure
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RdmExport {
    connections: Option<Vec<RdmConnection>>,
    folders: Option<Vec<RdmFolder>>,
}

/// Credential fields collected from an entry or a linked `Credential` entry.
#[derive(Debug, Clone, Default)]
struct EntryCredential {
    username: Option<String>,
    domain: Option<String>,
    password: Option<secrecy::SecretString>,
}

/// Remote Desktop Manager JSON importer
pub struct RdmImporter;

impl ImportSource for RdmImporter {
    fn source_id(&self) -> &'static str {
        "rdm"
    }

    fn display_name(&self) -> &'static str {
        "Remote Desktop Manager (JSON)"
    }

    fn is_available(&self) -> bool {
        // RDM is file-based, so always available for file import
        true
    }

    fn default_paths(&self) -> Vec<PathBuf> {
        // RDM doesn't have standard config paths, return empty
        Vec::new()
    }

    fn import(&self) -> Result<ImportResult, ImportError> {
        // No default paths for RDM, return empty result
        Ok(ImportResult::new())
    }

    fn import_from_path(&self, path: &Path) -> Result<ImportResult, ImportError> {
        let content = fs::read_to_string(path).map_err(ImportError::Io)?;

        self.import_from_content(&content)
    }

    fn import_from_path_with_progress(
        &self,
        path: &Path,
        progress: Option<&dyn ProgressReporter>,
    ) -> Result<ImportResult, ImportError> {
        if let Some(reporter) = progress {
            reporter.report(0, 3, "Reading RDM file...");
            if reporter.is_cancelled() {
                return Err(ImportError::Cancelled);
            }
        }

        let content = fs::read_to_string(path).map_err(ImportError::Io)?;

        if let Some(reporter) = progress {
            reporter.report(1, 3, "Parsing RDM data...");
            if reporter.is_cancelled() {
                return Err(ImportError::Cancelled);
            }
        }

        let result = self.import_from_content(&content)?;

        if let Some(reporter) = progress {
            reporter.report(3, 3, "Import completed");
        }

        Ok(result)
    }
}

impl RdmImporter {
    /// Creates a new RDM importer
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Imports connections from RDM JSON content
    ///
    /// Parses the provided JSON string as an RDM export file and converts the
    /// connections and folders to RustConn format. Folder hierarchy is taken
    /// from the `Folders` array when present and from the `Group` path of each
    /// entry otherwise.
    ///
    /// # Arguments
    ///
    /// * `content` - The JSON content to parse
    ///
    /// # Returns
    ///
    /// An `ImportResult` containing the converted connections, groups and
    /// credentials. Entries of an unsupported type are reported in `skipped`.
    ///
    /// # Errors
    ///
    /// Returns `ImportError::ParseError` if the JSON is malformed or does not
    /// describe an RDM export.
    pub fn import_from_content(&self, content: &str) -> Result<ImportResult, ImportError> {
        let rdm_data: RdmExport =
            serde_json::from_str(content).map_err(|e| ImportError::ParseError {
                source_name: "RDM JSON".to_string(),
                reason: format!("Failed to parse JSON: {e}"),
            })?;

        let mut result = ImportResult::new();
        // Maps RDM folder ID -> RustConn group UUID
        let mut group_map: HashMap<String, Uuid> = HashMap::new();
        // Maps a normalized folder path -> RustConn group UUID
        let mut path_map: HashMap<String, Uuid> = HashMap::new();

        // Explicit folder list (documented format and older exports)
        if let Some(folders) = &rdm_data.folders {
            for folder in folders {
                if let Some(id) = folder.id.as_ref().filter(|id| !id.is_empty()) {
                    group_map.insert(id.clone(), Uuid::new_v4());
                }
            }

            for folder in folders {
                let group = Self::create_group_from_folder(folder, &group_map);
                result.add_group(group);
            }
        }

        let Some(connections) = &rdm_data.connections else {
            return Ok(result);
        };

        // First pass: folder paths and credential entries. Group entries carry
        // the folder tree in real exports; other entries reference their folder
        // through the same `Group` path, which may not have its own entry.
        let mut credentials: HashMap<String, EntryCredential> = HashMap::new();
        for conn in connections {
            let is_group = conn.connection_type.as_deref() == Some("group");
            let path = if is_group {
                Self::group_entry_path(conn)
            } else {
                conn.group.as_deref().unwrap_or_default().trim().to_string()
            };
            if !path.is_empty() {
                Self::ensure_group_path(&path, &mut path_map, &mut result);
            }

            if conn.connection_type.as_deref() == Some("credential")
                && let Some(id) = conn.id.as_ref().filter(|id| !id.is_empty())
            {
                credentials.insert(id.clone(), Self::entry_credential(conn));
            }
        }

        // Second pass: connections.
        for conn in connections {
            // Folders and credential entries were handled above, the document
            // root is not a connection either.
            if matches!(
                conn.connection_type.as_deref(),
                Some("group" | "credential" | "root")
            ) {
                continue;
            }

            match Self::create_connection_from_rdm(conn, &credentials, &group_map, &path_map) {
                Ok((connection, creds)) => {
                    if let Some(creds) = creds {
                        result.add_credentials(connection.id, creds);
                    }
                    result.add_connection(connection);
                }
                Err(reason) => {
                    result.add_skipped(SkippedEntry::new(&conn.name, reason));
                }
            }
        }

        Ok(result)
    }

    /// Collects the credential fields of an entry, flat or nested.
    fn entry_credential(conn: &RdmConnection) -> EntryCredential {
        let nested = conn.credentials.clone().unwrap_or_default();
        EntryCredential {
            username: conn
                .username
                .clone()
                .or(nested.user_name)
                .filter(|u| !u.is_empty()),
            domain: conn
                .domain
                .clone()
                .or(nested.domain)
                .filter(|d| !d.is_empty()),
            password: conn
                .password
                .clone()
                .or(nested.password)
                .filter(|p| !p.expose_secret().is_empty()),
        }
    }

    /// Returns the full folder path of a `Group` entry (`Group` path + `Name`).
    fn group_entry_path(conn: &RdmConnection) -> String {
        let parent = conn.group.as_deref().unwrap_or_default().trim();
        let name = conn.name.trim();
        if name.is_empty() {
            parent.to_string()
        } else if parent.is_empty() {
            name.to_string()
        } else {
            format!("{parent}\\{name}")
        }
    }

    /// Creates (or reuses) the group chain for a backslash separated path.
    ///
    /// Returns the UUID of the deepest group in the path, or `None` when the
    /// path holds no usable segment.
    fn ensure_group_path(
        path: &str,
        path_map: &mut HashMap<String, Uuid>,
        result: &mut ImportResult,
    ) -> Option<Uuid> {
        let mut current_path = String::new();
        let mut parent: Option<Uuid> = None;

        for segment in path.split(['\\', '/']) {
            let segment = segment.trim();
            if segment.is_empty() {
                continue;
            }
            if !current_path.is_empty() {
                current_path.push('\\');
            }
            current_path.push_str(segment);

            if let Some(existing) = path_map.get(&current_path) {
                parent = Some(*existing);
                continue;
            }

            let group = parent.map_or_else(
                || ConnectionGroup::new(segment.to_string()),
                |parent_id| ConnectionGroup::with_parent(segment.to_string(), parent_id),
            );
            let group_id = group.id;
            result.add_group(group);
            path_map.insert(current_path.clone(), group_id);
            parent = Some(group_id);
        }

        parent
    }

    /// Normalizes a `Group` path the same way [`Self::ensure_group_path`] does.
    fn normalize_group_path(path: &str) -> String {
        path.split(['\\', '/'])
            .map(str::trim)
            .filter(|segment| !segment.is_empty())
            .collect::<Vec<_>>()
            .join("\\")
    }

    /// Creates a connection group from RDM folder with parent resolution
    ///
    /// # Arguments
    ///
    /// * `folder` - The RDM folder to convert
    /// * `group_map` - Mapping from RDM folder IDs to RustConn group UUIDs
    fn create_group_from_folder(
        folder: &RdmFolder,
        group_map: &HashMap<String, Uuid>,
    ) -> ConnectionGroup {
        // Resolve parent_id: look up the RDM parent ID in the group map
        let parent_id = folder
            .parent_id
            .as_ref()
            .and_then(|pid| group_map.get(pid).copied());

        let mut group = parent_id.map_or_else(
            || ConnectionGroup::new(folder.name.clone()),
            |parent| ConnectionGroup::with_parent(folder.name.clone(), parent),
        );
        // Use the pre-allocated UUID from the group_map
        if let Some(id) = folder.id.as_ref().and_then(|id| group_map.get(id)).copied() {
            group.id = id;
        }
        group
    }

    /// Parses protocol configuration from RDM connection type
    fn parse_protocol(conn: &RdmConnection) -> Result<(ProtocolType, ProtocolConfig, u16), String> {
        // RDM falls back to RDP when no connection type is serialised.
        let connection_type = conn
            .connection_type
            .as_deref()
            .unwrap_or("rdpconfigured")
            .trim();

        match connection_type {
            "ssh" | "ssh2" | "sshshell" | "putty" | "iterm" | "terminalconsole" | "securecrt" => {
                let key_path = conn
                    .private_key_path
                    .as_ref()
                    .filter(|p| !p.is_empty())
                    .map(|p| std::path::PathBuf::from(shellexpand::tilde(p).into_owned()));
                let auth_method = if key_path.is_some() {
                    SshAuthMethod::PublicKey
                } else {
                    SshAuthMethod::Password
                };
                let ssh_config = SshConfig {
                    auth_method,
                    key_path,
                    x11_forwarding: false,
                    compression: false,
                    ..Default::default()
                };
                Ok((ProtocolType::Ssh, ProtocolConfig::Ssh(ssh_config), 22))
            }
            "rdp" | "rdp2" | "rdpconfigured" | "rdpfilename" => Ok((
                ProtocolType::Rdp,
                ProtocolConfig::Rdp(RdpConfig::default()),
                3389,
            )),
            "vnc" | "novnc" => {
                let vnc_config = VncConfig {
                    view_only: conn.view_only.unwrap_or(false),
                    ..Default::default()
                };
                Ok((ProtocolType::Vnc, ProtocolConfig::Vnc(vnc_config), 5900))
            }
            "telnet" => Ok((
                ProtocolType::Telnet,
                ProtocolConfig::Telnet(TelnetConfig::default()),
                23,
            )),
            other => Err(format!("Unsupported connection type: {other}")),
        }
    }

    /// Creates a connection from RDM connection data
    ///
    /// Returns the connection together with the credentials that have to be
    /// stored in the secret backend, or the reason the entry was skipped.
    fn create_connection_from_rdm(
        conn: &RdmConnection,
        credentials: &HashMap<String, EntryCredential>,
        group_map: &HashMap<String, Uuid>,
        path_map: &HashMap<String, Uuid>,
    ) -> Result<(Connection, Option<Credentials>), String> {
        let (protocol, protocol_config, default_port) = Self::parse_protocol(conn)?;

        let host_raw = conn
            .host
            .as_deref()
            .map(str::trim)
            .filter(|h| !h.is_empty())
            .ok_or_else(|| format!("Connection '{}' has no host", conn.name))?;

        // Use shared utility for host:port parsing
        let (host, parsed_port) = parse_host_port(host_raw);

        // Use parsed port from host string, or connection port, or default
        let port = parsed_port.or(conn.port).unwrap_or(default_port);

        // Credentials on the entry win over the linked Credential entry.
        let own = Self::entry_credential(conn);
        let linked = conn
            .credential_connection_id
            .as_ref()
            .and_then(|id| credentials.get(id))
            .cloned()
            .unwrap_or_default();
        let username = own.username.or(linked.username);
        let domain = own.domain.or(linked.domain);
        let password = own.password.or(linked.password);

        let group_id = conn
            .parent_id
            .as_ref()
            .and_then(|pid| group_map.get(pid).copied())
            .or_else(|| {
                conn.group
                    .as_deref()
                    .map(Self::normalize_group_path)
                    .filter(|path| !path.is_empty())
                    .and_then(|path| path_map.get(&path).copied())
            });

        // A password is only useful when it can be handed to the secret
        // backend, so the connection is switched to the vault source.
        let (password_source, creds) = password.map_or((PasswordSource::None, None), |password| {
            let creds = Credentials {
                username: username.clone(),
                password: Some(password),
                key_passphrase: None,
                domain: domain.clone(),
            };
            (PasswordSource::Vault, Some(creds))
        });

        let now = chrono::Utc::now();

        let connection = Connection {
            id: Uuid::new_v4(),
            name: conn.name.clone(),
            description: conn.description.clone(),
            protocol,
            host,
            port,
            username,
            group_id,
            tags: Vec::new(),
            created_at: now,
            updated_at: now,
            protocol_config,
            automation: AutomationConfig::default(),
            sort_order: 0,
            last_connected: None,
            password_source,
            domain,
            custom_properties: Vec::new(),
            pre_connect_task: None,
            post_disconnect_task: None,
            wol_config: None,
            local_variables: HashMap::new(),
            log_config: None,
            key_sequence: None,
            window_mode: WindowMode::default(),
            remember_window_position: false,
            window_geometry: None,
            skip_port_check: false,
            is_pinned: false,
            pin_order: 0,
            icon: None,
            monitoring_config: None,
            activity_monitor_config: None,
            theme_override: None,
            session_recording_enabled: false,
            highlight_rules: Vec::new(),
            is_dynamic: false,
            retry_config: None,
            knock_sequence: None,
            spa_config: None,
        };

        Ok((connection, creds))
    }
}

impl Default for RdmImporter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_numeric_connection_types() {
        // Real RDM exports serialise ConnectionType as the enum number:
        // 25 = Group, 77 = SSHShell, 1 = RDPConfigured, 4 = VNC.
        let json = r#"{"Connections":[
            {"ConnectionType":25,"ID":"g1","Name":"Prod"},
            {"ConnectionType":77,"Name":"shell","Host":"ssh.example.com","Group":"Prod"},
            {"ConnectionType":1,"Name":"desk","Url":"rdp.example.com","Group":"Prod"},
            {"ConnectionType":4,"Name":"screen","Host":"vnc.example.com","Port":"5901"}
        ]}"#;

        let result = RdmImporter::new()
            .import_from_content(json)
            .expect("import should succeed");

        assert_eq!(result.connections.len(), 3, "{:?}", result.skipped);
        assert_eq!(result.groups.len(), 1);

        let ssh = &result.connections[0];
        assert_eq!(ssh.protocol, ProtocolType::Ssh);
        assert_eq!(ssh.host, "ssh.example.com");
        assert_eq!(ssh.port, 22);
        assert_eq!(ssh.group_id, Some(result.groups[0].id));

        let rdp = &result.connections[1];
        assert_eq!(rdp.protocol, ProtocolType::Rdp);
        assert_eq!(rdp.host, "rdp.example.com");
        assert_eq!(rdp.group_id, Some(result.groups[0].id));

        let vnc = &result.connections[2];
        assert_eq!(vnc.protocol, ProtocolType::Vnc);
        assert_eq!(vnc.port, 5901, "string port must be accepted");
    }

    #[test]
    fn creates_nested_groups_from_group_path() {
        let json = r#"{"Connections":[
            {"ConnectionType":"SSHShell","Name":"web","Host":"web.example.com",
             "Group":"Customers\\ACME\\Web"}
        ]}"#;

        let result = RdmImporter::new()
            .import_from_content(json)
            .expect("import should succeed");

        assert_eq!(result.connections.len(), 1);
        assert_eq!(result.groups.len(), 3);

        let customers = result
            .groups
            .iter()
            .find(|g| g.name == "Customers")
            .expect("Customers group");
        let acme = result
            .groups
            .iter()
            .find(|g| g.name == "ACME")
            .expect("ACME group");
        let web = result
            .groups
            .iter()
            .find(|g| g.name == "Web")
            .expect("Web group");

        assert!(customers.parent_id.is_none());
        assert_eq!(acme.parent_id, Some(customers.id));
        assert_eq!(web.parent_id, Some(acme.id));
        assert_eq!(result.connections[0].group_id, Some(web.id));
    }

    #[test]
    fn imports_nested_and_linked_credentials() {
        let json = r#"{"Connections":[
            {"ConnectionType":"Credential","ID":"cred-1","Name":"Bob",
             "Credentials":{"UserName":"bob","Domain":"corp","Password":"s3cret"}},
            {"ConnectionType":"RDPConfigured","Name":"desk","Url":"rdp.example.com",
             "CredentialConnectionID":"cred-1"},
            {"ConnectionType":"SSHShell","Name":"shell","Host":"ssh.example.com",
             "Credentials":{"UserName":"root","Password":"hunter2"}}
        ]}"#;

        let result = RdmImporter::new()
            .import_from_content(json)
            .expect("import should succeed");

        assert_eq!(result.connections.len(), 2, "{:?}", result.skipped);

        let rdp = &result.connections[0];
        assert_eq!(rdp.username.as_deref(), Some("bob"));
        assert_eq!(rdp.domain.as_deref(), Some("corp"));
        assert_eq!(rdp.password_source, PasswordSource::Vault);
        let rdp_creds = result
            .credentials
            .get(&rdp.id)
            .expect("linked credential stored");
        assert_eq!(
            rdp_creds
                .password
                .as_ref()
                .map(|p| p.expose_secret().to_string()),
            Some("s3cret".to_string())
        );

        let ssh = &result.connections[1];
        assert_eq!(ssh.username.as_deref(), Some("root"));
        assert_eq!(ssh.password_source, PasswordSource::Vault);
        assert!(result.credentials.contains_key(&ssh.id));
    }

    #[test]
    fn reports_unsupported_and_hostless_entries() {
        let json = r#"{"Connections":[
            {"ConnectionType":5,"Name":"portal","Url":"https://example.com"},
            {"ConnectionType":"SSHShell","Name":"broken"}
        ]}"#;

        let result = RdmImporter::new()
            .import_from_content(json)
            .expect("import should succeed");

        assert!(result.connections.is_empty());
        assert_eq!(result.skipped.len(), 2);
        assert!(
            result.skipped[0].reason.contains('5'),
            "unknown numeric type must be named: {}",
            result.skipped[0].reason
        );
        assert!(result.skipped[1].reason.contains("no host"));
    }

    /// Regression test for issue #234: real RDM exports may contain integer
    /// values in fields the documented format encodes as strings (Port inside
    /// a URL field, numeric IDs, numeric passwords for PIN-based devices).
    #[test]
    fn tolerant_parsing_accepts_numeric_field_values() {
        // Simulates a real Devolutions export where:
        // - ConnectionType is numeric (77 = SSHShell)
        // - Folders[].ID and ParentID are bare integers
        // - Port is a bare integer
        // - Description is absent (null)
        // - A connection ParentID is numeric
        // - Password is a numeric PIN
        let json = r#"{"Folders":[
            {"ID":100,"Name":"Servers"},
            {"ID":101,"Name":"Network","ParentID":100}
        ],"Connections":[
            {"ConnectionType":77,"Name":"Router","Host":"192.168.1.1",
             "Port":22,"Username":"admin","Password":1234,
             "ParentID":101,"Description":null},
            {"ConnectionType":1,"Name":"Win2022","Url":"10.0.0.5",
             "Port":3389,"Domain":"CORP","Username":"sysadmin",
             "Credentials":{"UserName":"nested-user","Domain":"NESTED","Password":"s3cret"}}
        ]}"#;

        let result = RdmImporter::new()
            .import_from_content(json)
            .expect("import must not fail on numeric fields");

        // Two explicit folders + 2 connections
        assert_eq!(result.groups.len(), 2);
        assert_eq!(result.connections.len(), 2, "skipped: {:?}", result.skipped);

        let servers = result
            .groups
            .iter()
            .find(|group| group.name == "Servers")
            .expect("parent folder");
        let network = result
            .groups
            .iter()
            .find(|group| group.name == "Network")
            .expect("numeric child folder");
        assert_eq!(network.parent_id, Some(servers.id));

        let router = &result.connections[0];
        assert_eq!(router.group_id, Some(network.id));
        assert_eq!(router.host, "192.168.1.1");
        assert_eq!(router.port, 22);
        assert_eq!(router.username.as_deref(), Some("admin"));
        assert_eq!(router.password_source, PasswordSource::Vault);

        // Verify the numeric password was captured as "1234"
        let router_creds = result.credentials.get(&router.id).expect("credentials");
        assert_eq!(
            router_creds
                .password
                .as_ref()
                .map(|p| p.expose_secret().to_string()),
            Some("1234".to_string())
        );

        let win = &result.connections[1];
        assert_eq!(win.host, "10.0.0.5");
        assert_eq!(win.port, 3389);
        // Flat username wins over nested
        assert_eq!(win.username.as_deref(), Some("sysadmin"));
        assert_eq!(win.domain.as_deref(), Some("CORP"));
    }
}
