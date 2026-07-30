//! Royal TS configuration importer.
//!
//! Parses Royal TS / Royal TSX documents (`.rtsz`, `.rtsx`).
//!
//! The document is a flat list of `Royal*` objects linked by `ParentID`. The
//! object element names follow the scripting object model: `RoyalSSHConnection`
//! for terminal connections, `RoyalRDSConnection` (formerly
//! `RoyalTerminalServicesConnection`) for RDP and `RoyalVNCConnection` for VNC.
//! `.rtsz` may be a ZIP container around that XML, which is unpacked here.
//!
//! Passwords are stored encrypted inside the document, so only usernames and
//! domains are imported; such connections are marked as "prompt for password".

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use quick_xml::Reader;
use quick_xml::events::Event;
use uuid::Uuid;

use super::traits::{ImportResult, ImportSource, SkippedEntry, read_import_bytes};
use crate::error::ImportError;
use crate::models::{
    Connection, ConnectionGroup, PasswordSource, ProtocolConfig, RdpConfig, SshAuthMethod,
    SshConfig, VncConfig,
};

/// An all-zero GUID, used by Royal TS for "no object assigned".
const EMPTY_GUID: &str = "00000000-0000-0000-0000-000000000000";

/// Maximum folder levels walked when resolving inherited credentials.
///
/// Royal TS documents are trees, but a hand-edited file could contain a cycle;
/// the cap keeps the walk terminating.
const MAX_CREDENTIAL_INHERITANCE_DEPTH: usize = 32;

/// Protocol of a Royal TS connection object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RoyalProtocol {
    Ssh,
    Rdp,
    Vnc,
}

impl RoyalProtocol {
    /// Returns the protocol for a Royal TS object element name.
    fn from_element(name: &str) -> Option<Self> {
        match name {
            "RoyalSSHConnection" => Some(Self::Ssh),
            // RoyalRDSConnection is the current name, RoyalTerminalServicesConnection
            // the legacy one; RoyalRDPConnection is written by older RustConn exports.
            "RoyalRDSConnection" | "RoyalTerminalServicesConnection" | "RoyalRDPConnection" => {
                Some(Self::Rdp)
            }
            "RoyalVNCConnection" => Some(Self::Vnc),
            _ => None,
        }
    }

    /// Default port when the document does not specify one.
    const fn default_port(self) -> u16 {
        match self {
            Self::Ssh => 22,
            Self::Rdp => 3389,
            Self::Vnc => 5900,
        }
    }
}

/// How a Royal TS object refers to its credentials.
#[derive(Debug, Clone, Default)]
struct CredentialRef {
    /// `CredentialId` - GUID of a `RoyalCredential` object.
    id: Option<String>,
    /// `CredentialName` - name of a `RoyalCredential` object.
    name: Option<String>,
    /// `CredentialUsername` - username specified directly on the object.
    username: Option<String>,
    /// `CredentialMode`: 0 none, 1 from parent, 2 inline, 3 by GUID, 4 by name.
    mode: Option<u8>,
    /// `CredentialFromParent`
    from_parent: bool,
}

impl CredentialRef {
    /// Whether the object carries no credential information of its own.
    fn is_empty(&self) -> bool {
        self.id.is_none() && self.name.is_none() && self.username.is_none()
    }

    /// Whether credentials should be looked up in the parent folder.
    ///
    /// Explicit inheritance (`CredentialFromParent`, `CredentialMode` = 1) is
    /// honoured; an object without any credential fields also falls back to the
    /// folder, which is how shared Royal TS documents are usually organised.
    fn inherits(&self) -> bool {
        self.from_parent || self.mode == Some(1) || self.is_empty()
    }
}

/// Royal TS connection object.
#[derive(Debug, Clone, Default)]
struct ConnectionData {
    name: String,
    uri: Option<String>,
    port: Option<u16>,
    parent_id: Option<String>,
    credential: CredentialRef,
    /// Path to private key file
    private_key_path: Option<String>,
}

/// Royal TS folder data
#[derive(Debug, Clone, Default)]
struct FolderData {
    id: String,
    name: String,
    parent_id: Option<String>,
    credential: CredentialRef,
}

/// Royal TS credential data
#[derive(Debug, Clone, Default)]
struct CredentialData {
    id: String,
    name: String,
    username: Option<String>,
    domain: Option<String>,
}

/// Credentials resolved for a connection.
#[derive(Debug, Clone, Default)]
struct ResolvedCredential {
    username: Option<String>,
    domain: Option<String>,
}

/// The object currently being parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Context {
    None,
    Connection(RoyalProtocol),
    Folder,
    Credential,
    Trash,
    /// A `Royal*Connection` element RustConn cannot map. Only the fact that we
    /// are inside one matters; the element name comes from the end event.
    Unsupported,
}

/// Importer for Royal TS documents (`.rtsz`, `.rtsx`).
pub struct RoyalTsImporter {
    custom_paths: Vec<PathBuf>,
}

impl RoyalTsImporter {
    /// Creates a new Royal TS importer
    #[must_use]
    pub const fn new() -> Self {
        Self {
            custom_paths: Vec::new(),
        }
    }

    /// Creates a new Royal TS importer with custom paths
    #[must_use]
    pub const fn with_paths(paths: Vec<PathBuf>) -> Self {
        Self {
            custom_paths: paths,
        }
    }

    /// Parses Royal TS XML content using event-based parsing
    #[expect(
        clippy::too_many_lines,
        reason = "long match/dispatch over many enum variants; splitting per variant only relocates the boilerplate"
    )]
    #[must_use]
    pub fn parse_xml(&self, content: &str, source_path: &str) -> ImportResult {
        let mut result = ImportResult::new();

        // Remove BOM if present
        let content = content.trim_start_matches('\u{feff}');

        let mut reader = Reader::from_str(content);
        // Text is not trimmed per event: a value split by an entity reference
        // ("Dev &amp; Test") arrives as several events and the inner spaces must
        // survive. The assembled value is trimmed when the element closes.
        reader.config_mut().trim_text(false);

        let mut connections: Vec<(RoyalProtocol, ConnectionData)> = Vec::new();
        let mut folders: Vec<FolderData> = Vec::new();
        let mut credentials: Vec<CredentialData> = Vec::new();
        let mut trash_id: Option<String> = None;

        let mut current_field = String::new();
        // Text of the field element being read. quick-xml reports entity
        // references (&amp;, &#39;, ...) as separate events, so the value has to
        // be accumulated and committed when the field element closes.
        let mut current_value = String::new();
        let mut object = Context::None;
        let mut current_connection = ConnectionData::default();
        let mut current_folder = FolderData::default();
        let mut current_credential = CredentialData::default();
        let mut current_trash_id = String::new();
        let mut current_unsupported_name = String::new();

        loop {
            match reader.read_event() {
                Ok(Event::Start(e)) => {
                    let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                    if let Some(protocol) = RoyalProtocol::from_element(&name) {
                        object = Context::Connection(protocol);
                        current_connection = ConnectionData::default();
                    } else if name == "RoyalFolder" {
                        object = Context::Folder;
                        current_folder = FolderData::default();
                    } else if name == "RoyalCredential" {
                        object = Context::Credential;
                        current_credential = CredentialData::default();
                    } else if name == "RoyalTrash" {
                        object = Context::Trash;
                        current_trash_id.clear();
                    } else if Self::is_connection_element(&name) {
                        // Any other connection object type (web page, file
                        // transfer, TeamViewer, ...) is reported, not dropped.
                        object = Context::Unsupported;
                        current_unsupported_name.clear();
                    } else {
                        current_field = name;
                        current_value.clear();
                    }
                }
                Ok(Event::End(e)) => {
                    let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                    if let Some(protocol) = RoyalProtocol::from_element(&name) {
                        connections.push((protocol, current_connection.clone()));
                        object = Context::None;
                    } else if name == "RoyalFolder" {
                        folders.push(current_folder.clone());
                        object = Context::None;
                    } else if name == "RoyalCredential" {
                        credentials.push(current_credential.clone());
                        object = Context::None;
                    } else if name == "RoyalTrash" {
                        if !current_trash_id.is_empty() {
                            trash_id = Some(current_trash_id.clone());
                        }
                        object = Context::None;
                    } else if Self::is_connection_element(&name) {
                        let identifier = if current_unsupported_name.is_empty() {
                            name.clone()
                        } else {
                            current_unsupported_name.clone()
                        };
                        result.add_skipped(SkippedEntry::with_location(
                            &identifier,
                            format!("Unsupported connection type: {name}"),
                            source_path,
                        ));
                        object = Context::None;
                    } else if name == current_field {
                        // The field element closed: commit the accumulated text.
                        let value = current_value.trim();
                        if !value.is_empty() {
                            Self::set_field(
                                &object,
                                &name,
                                value,
                                &mut ParserState {
                                    connection: &mut current_connection,
                                    folder: &mut current_folder,
                                    credential: &mut current_credential,
                                    trash_id: &mut current_trash_id,
                                    unsupported_name: &mut current_unsupported_name,
                                },
                            );
                        }
                    }
                    current_field.clear();
                    current_value.clear();
                }
                Ok(Event::Text(e)) => {
                    current_value.push_str(&e.xml10_content().unwrap_or_default());
                }
                Ok(Event::CData(e)) => {
                    current_value.push_str(&e.decode().unwrap_or_default());
                }
                Ok(Event::GeneralRef(e)) => {
                    // &amp;, &#39; and friends arrive as their own event.
                    if let Some(resolved) = Self::resolve_reference(&e) {
                        current_value.push_str(&resolved);
                    }
                }
                Ok(Event::Eof) => break,
                Err(e) => {
                    result.add_error(ImportError::ParseError {
                        source_name: "Royal TS".to_string(),
                        reason: format!("XML parse error: {e}"),
                    });
                    return result;
                }
                _ => {}
            }
        }

        // Build credential lookups (by GUID and by name)
        let mut creds_by_id: HashMap<String, CredentialData> = HashMap::new();
        let mut creds_by_name: HashMap<String, CredentialData> = HashMap::new();
        for cred in credentials {
            if !cred.name.is_empty() {
                creds_by_name.insert(cred.name.clone(), cred.clone());
            }
            if !cred.id.is_empty() {
                creds_by_id.insert(cred.id.clone(), cred);
            }
        }

        let folders_by_id: HashMap<String, FolderData> = folders
            .iter()
            .filter(|f| !f.id.is_empty())
            .map(|f| (f.id.clone(), f.clone()))
            .collect();

        // Build folder map and create groups
        let (folder_map, groups) = Self::build_folder_hierarchy(&folders);
        for group in groups {
            result.add_group(group);
        }

        for (protocol, conn) in &connections {
            // Skip connections in trash
            if let Some(ref tid) = trash_id
                && conn.parent_id.as_ref() == Some(tid)
            {
                continue;
            }

            let resolved = Self::resolve_credentials(
                &conn.credential,
                conn.parent_id.as_deref(),
                &creds_by_id,
                &creds_by_name,
                &folders_by_id,
            );

            if let Some(c) = Self::convert(*protocol, conn, resolved.as_ref(), &folder_map) {
                result.add_connection(c);
            } else {
                result.add_skipped(SkippedEntry::with_location(
                    &conn.name,
                    "Missing host",
                    source_path,
                ));
            }
        }

        result
    }

    /// Whether the element is a Royal TS connection object.
    fn is_connection_element(name: &str) -> bool {
        name.starts_with("Royal") && name.ends_with("Connection")
    }

    /// Resolves an entity or character reference to its text.
    ///
    /// Returns `None` for entities that are not predefined in XML, since the
    /// documents carry no DTD to resolve them against.
    fn resolve_reference(reference: &quick_xml::events::BytesRef<'_>) -> Option<String> {
        if let Ok(Some(character)) = reference.resolve_char_ref() {
            return Some(character.to_string());
        }

        let name = reference.decode().ok()?;
        let text = match name.as_ref() {
            "amp" => "&",
            "lt" => "<",
            "gt" => ">",
            "quot" => "\"",
            "apos" => "'",
            _ => return None,
        };
        Some(text.to_string())
    }

    /// Dispatches a text value to the object currently being parsed.
    fn set_field(context: &Context, field: &str, value: &str, state: &mut ParserState<'_>) {
        match context {
            Context::Connection(protocol) => {
                Self::set_connection_field(*protocol, state.connection, field, value);
            }
            Context::Folder => Self::set_folder_field(state.folder, field, value),
            Context::Credential => Self::set_credential_field(state.credential, field, value),
            Context::Trash => {
                if field == "ID" {
                    *state.trash_id = value.to_string();
                }
            }
            Context::Unsupported => {
                if field == "Name" {
                    *state.unsupported_name = value.to_string();
                }
            }
            Context::None => {}
        }
    }

    fn set_connection_field(
        protocol: RoyalProtocol,
        conn: &mut ConnectionData,
        field: &str,
        value: &str,
    ) {
        // RDS connections store the port in RDPPort, VNC in Port or VNCPort.
        let is_port_field = match protocol {
            RoyalProtocol::Ssh => field == "Port",
            RoyalProtocol::Rdp => field == "Port" || field == "RDPPort",
            RoyalProtocol::Vnc => field == "Port" || field == "VNCPort",
        };
        if is_port_field {
            if let Ok(port) = value.parse() {
                conn.port = Some(port);
            }
            return;
        }

        match field {
            "Name" => conn.name = value.to_string(),
            "URI" => conn.uri = Some(value.to_string()),
            "ParentID" => conn.parent_id = Some(value.to_string()),
            "PrivateKeyFile" | "KeyFilePath" | "PrivateKeyPath" if !value.is_empty() => {
                conn.private_key_path = Some(value.to_string());
            }
            _ => Self::set_credential_ref_field(&mut conn.credential, field, value),
        }
    }

    fn set_folder_field(folder: &mut FolderData, field: &str, value: &str) {
        match field {
            "ID" => folder.id = value.to_string(),
            "Name" => folder.name = value.to_string(),
            "ParentID" => folder.parent_id = Some(value.to_string()),
            _ => Self::set_credential_ref_field(&mut folder.credential, field, value),
        }
    }

    /// Fills the credential reference fields shared by folders and connections.
    fn set_credential_ref_field(credential: &mut CredentialRef, field: &str, value: &str) {
        match field {
            "CredentialId" | "CredentialID" => credential.id = Self::non_empty_guid(value),
            "CredentialName" if !value.is_empty() => credential.name = Some(value.to_string()),
            "CredentialUsername" | "CredentialUserName" if !value.is_empty() => {
                credential.username = Some(value.to_string());
            }
            "CredentialMode" => credential.mode = value.parse().ok(),
            "CredentialFromParent" => credential.from_parent = value.eq_ignore_ascii_case("true"),
            _ => {}
        }
    }

    fn set_credential_field(cred: &mut CredentialData, field: &str, value: &str) {
        match field {
            "ID" => cred.id = value.to_string(),
            "Name" => cred.name = value.to_string(),
            "UserName" | "Username" => cred.username = Some(value.to_string()),
            "Domain" => cred.domain = Some(value.to_string()),
            _ => {}
        }
    }

    /// Returns the GUID unless it is empty or the all-zero placeholder.
    fn non_empty_guid(value: &str) -> Option<String> {
        let value = value.trim();
        if value.is_empty() || value.eq_ignore_ascii_case(EMPTY_GUID) {
            None
        } else {
            Some(value.to_string())
        }
    }

    /// Resolves the credentials of a connection, following folder inheritance.
    fn resolve_credentials(
        credential: &CredentialRef,
        parent_id: Option<&str>,
        creds_by_id: &HashMap<String, CredentialData>,
        creds_by_name: &HashMap<String, CredentialData>,
        folders: &HashMap<String, FolderData>,
    ) -> Option<ResolvedCredential> {
        if let Some(resolved) =
            Self::resolve_own_credentials(credential, creds_by_id, creds_by_name)
        {
            return Some(resolved);
        }

        if !credential.inherits() {
            return None;
        }

        let mut current = parent_id.map(ToString::to_string);
        for _ in 0..MAX_CREDENTIAL_INHERITANCE_DEPTH {
            let folder = folders.get(current.as_deref()?)?;
            if let Some(resolved) =
                Self::resolve_own_credentials(&folder.credential, creds_by_id, creds_by_name)
            {
                return Some(resolved);
            }
            if !folder.credential.inherits() {
                return None;
            }
            current = folder.parent_id.clone();
        }

        None
    }

    /// Resolves a credential reference without walking the folder chain.
    fn resolve_own_credentials(
        credential: &CredentialRef,
        creds_by_id: &HashMap<String, CredentialData>,
        creds_by_name: &HashMap<String, CredentialData>,
    ) -> Option<ResolvedCredential> {
        if let Some(cred) = credential.id.as_ref().and_then(|id| creds_by_id.get(id)) {
            return Some(ResolvedCredential {
                username: cred.username.clone(),
                domain: cred.domain.clone(),
            });
        }
        if let Some(cred) = credential
            .name
            .as_ref()
            .and_then(|name| creds_by_name.get(name))
        {
            return Some(ResolvedCredential {
                username: cred.username.clone(),
                domain: cred.domain.clone(),
            });
        }
        credential
            .username
            .clone()
            .map(|username| ResolvedCredential {
                username: Some(username),
                domain: None,
            })
    }

    fn build_folder_hierarchy(
        folders: &[FolderData],
    ) -> (HashMap<String, Uuid>, Vec<ConnectionGroup>) {
        let mut id_map: HashMap<String, Uuid> = HashMap::new();
        let mut groups = Vec::new();

        // First pass: create UUIDs
        for folder in folders {
            if !folder.id.is_empty() {
                id_map.insert(folder.id.clone(), Uuid::new_v4());
            }
        }

        // Second pass: create groups
        for folder in folders {
            if folder.id.is_empty() || folder.name.is_empty() {
                continue;
            }
            let new_id = id_map.get(&folder.id).copied().unwrap_or_else(Uuid::new_v4);
            let parent_uuid = folder
                .parent_id
                .as_ref()
                .and_then(|pid| id_map.get(pid).copied());

            let group = parent_uuid.map_or_else(
                || {
                    let mut g = ConnectionGroup::new(folder.name.clone());
                    g.id = new_id;
                    g
                },
                |parent_id| {
                    let mut g = ConnectionGroup::with_parent(folder.name.clone(), parent_id);
                    g.id = new_id;
                    g
                },
            );
            groups.push(group);
        }

        (id_map, groups)
    }

    /// Converts a Royal TS connection object into a RustConn connection.
    ///
    /// Returns `None` when the object has no target host.
    fn convert(
        protocol: RoyalProtocol,
        conn: &ConnectionData,
        credentials: Option<&ResolvedCredential>,
        folder_map: &HashMap<String, Uuid>,
    ) -> Option<Connection> {
        let host = conn.uri.as_ref().filter(|h| !h.is_empty())?;
        let port = conn.port.unwrap_or_else(|| protocol.default_port());

        let protocol_config = match protocol {
            RoyalProtocol::Ssh => {
                let key_path = conn
                    .private_key_path
                    .as_ref()
                    .filter(|p| !p.is_empty())
                    .map(|p| PathBuf::from(shellexpand::tilde(p).into_owned()));
                let auth_method = if key_path.is_some() {
                    SshAuthMethod::PublicKey
                } else {
                    SshAuthMethod::Password
                };
                ProtocolConfig::Ssh(SshConfig {
                    auth_method,
                    key_path,
                    ..Default::default()
                })
            }
            RoyalProtocol::Rdp => ProtocolConfig::Rdp(RdpConfig::default()),
            RoyalProtocol::Vnc => ProtocolConfig::Vnc(VncConfig::default()),
        };

        let mut connection =
            Connection::new(conn.name.clone(), host.clone(), port, protocol_config);

        if let Some(resolved) = credentials {
            connection.username.clone_from(&resolved.username);
            if protocol == RoyalProtocol::Rdp {
                connection.domain.clone_from(&resolved.domain);
            }
            // Royal TS keeps passwords encrypted in the document, so they
            // cannot be imported - ask the user on connect instead.
            connection.password_source = PasswordSource::Prompt;
        }

        if let Some(parent_id) = &conn.parent_id
            && let Some(group_id) = folder_map.get(parent_id)
        {
            connection.group_id = Some(*group_id);
        }

        Some(connection)
    }

    /// Reads a Royal TS document, unpacking the `.rtsz` ZIP container if needed.
    fn read_document(path: &Path) -> Result<String, ImportError> {
        let bytes = read_import_bytes(path, "Royal TS")?;

        if bytes.starts_with(b"PK\x03\x04") {
            return Self::extract_zipped_document(bytes);
        }

        String::from_utf8(bytes).map_err(|_| ImportError::ParseError {
            source_name: "Royal TS".to_string(),
            reason: format!(
                "{} is neither XML nor a ZIP document. Encrypted and lockdown documents cannot be imported.",
                path.display()
            ),
        })
    }

    /// Extracts the document XML from a compressed `.rtsz` file.
    fn extract_zipped_document(bytes: Vec<u8>) -> Result<String, ImportError> {
        let parse_error = |reason: String| ImportError::ParseError {
            source_name: "Royal TS".to_string(),
            reason,
        };

        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
            .map_err(|e| parse_error(format!("Cannot open compressed document: {e}")))?;

        // Prefer the XML entry, fall back to the largest one (Royal TS stores a
        // single document entry, but the name is not part of the format).
        let mut candidate: Option<(usize, u64)> = None;
        for index in 0..archive.len() {
            let Ok(entry) = archive.by_index(index) else {
                continue;
            };
            if entry.is_dir() {
                continue;
            }
            let size = entry.size();
            let extension = std::path::Path::new(entry.name())
                .extension()
                .and_then(std::ffi::OsStr::to_str)
                .unwrap_or_default();
            if extension.eq_ignore_ascii_case("xml") || extension.eq_ignore_ascii_case("rtsx") {
                candidate = Some((index, size));
                break;
            }
            if candidate.is_none_or(|(_, best)| size > best) {
                candidate = Some((index, size));
            }
        }

        let (index, _) =
            candidate.ok_or_else(|| parse_error("Compressed document is empty".to_string()))?;

        let mut entry = archive
            .by_index(index)
            .map_err(|e| parse_error(format!("Cannot read compressed document: {e}")))?;
        let mut content = String::new();
        entry
            .read_to_string(&mut content)
            .map_err(|e| parse_error(format!("Cannot read compressed document: {e}")))?;

        Ok(content)
    }
}

/// Mutable parser accumulators handed to [`RoyalTsImporter::set_field`].
struct ParserState<'a> {
    connection: &'a mut ConnectionData,
    folder: &'a mut FolderData,
    credential: &'a mut CredentialData,
    trash_id: &'a mut String,
    unsupported_name: &'a mut String,
}

impl Default for RoyalTsImporter {
    fn default() -> Self {
        Self::new()
    }
}

impl ImportSource for RoyalTsImporter {
    fn source_id(&self) -> &'static str {
        "royalts"
    }

    fn display_name(&self) -> &'static str {
        "Royal TS"
    }

    fn is_available(&self) -> bool {
        !self.custom_paths.is_empty() && self.custom_paths.iter().any(|p| p.exists())
    }

    fn default_paths(&self) -> Vec<PathBuf> {
        if !self.custom_paths.is_empty() {
            return self.custom_paths.clone();
        }
        Vec::new()
    }

    fn import(&self) -> Result<ImportResult, ImportError> {
        let paths = self.default_paths();
        if paths.is_empty() {
            return Err(ImportError::FileNotFound(PathBuf::from(
                "No Royal TS file specified",
            )));
        }

        let mut combined_result = ImportResult::new();
        for path in paths {
            match self.import_from_path(&path) {
                Ok(result) => combined_result.merge(result),
                Err(e) => combined_result.add_error(e),
            }
        }
        Ok(combined_result)
    }

    fn import_from_path(&self, path: &Path) -> Result<ImportResult, ImportError> {
        if !path.exists() {
            return Err(ImportError::FileNotFound(path.to_path_buf()));
        }

        let content = Self::read_document(path)?;

        Ok(self.parse_xml(&content, &path.display().to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_ssh_connection() {
        let importer = RoyalTsImporter::new();
        let content = r#"<?xml version="1.0" encoding="utf-8"?>
<RTSZDocument>
  <RoyalSSHConnection>
    <ID>conn1</ID>
    <Name>My SSH Server</Name>
    <URI>192.168.1.100</URI>
    <Port>22</Port>
  </RoyalSSHConnection>
</RTSZDocument>"#;

        let result = importer.parse_xml(content, "test.rtsz");
        assert_eq!(result.connections.len(), 1);
        assert!(result.errors.is_empty());

        let conn = &result.connections[0];
        assert_eq!(conn.name, "My SSH Server");
        assert_eq!(conn.host, "192.168.1.100");
        assert_eq!(conn.port, 22);
    }

    #[test]
    fn test_parse_multiple_ssh_connections() {
        let importer = RoyalTsImporter::new();
        let content = r#"<?xml version="1.0" encoding="utf-8"?>
<RTSZDocument>
  <RoyalSSHConnection>
    <ID>conn1</ID>
    <Name>Server 1</Name>
    <URI>server1.example.com</URI>
    <Port>22</Port>
  </RoyalSSHConnection>
  <RoyalSSHConnection>
    <ID>conn2</ID>
    <Name>Server 2</Name>
    <URI>server2.example.com</URI>
    <Port>2222</Port>
  </RoyalSSHConnection>
  <RoyalSSHConnection>
    <ID>conn3</ID>
    <Name>Server 3</Name>
    <URI>server3.example.com</URI>
  </RoyalSSHConnection>
</RTSZDocument>"#;

        let result = importer.parse_xml(content, "test.rtsz");
        assert_eq!(result.connections.len(), 3);
        assert!(result.errors.is_empty());

        assert_eq!(result.connections[0].name, "Server 1");
        assert_eq!(result.connections[1].name, "Server 2");
        assert_eq!(result.connections[1].port, 2222);
        assert_eq!(result.connections[2].name, "Server 3");
        assert_eq!(result.connections[2].port, 22); // default
    }

    #[test]
    fn test_parse_with_credential() {
        let importer = RoyalTsImporter::new();
        let content = r#"<?xml version="1.0" encoding="utf-8"?>
<RTSZDocument>
  <RoyalCredential>
    <ID>cred1</ID>
    <Name>Root</Name>
    <UserName>root</UserName>
  </RoyalCredential>
  <RoyalSSHConnection>
    <ID>conn1</ID>
    <Name>Server</Name>
    <URI>server.example.com</URI>
    <CredentialId>cred1</CredentialId>
  </RoyalSSHConnection>
</RTSZDocument>"#;

        let result = importer.parse_xml(content, "test.rtsz");
        assert_eq!(result.connections.len(), 1);

        let conn = &result.connections[0];
        assert_eq!(conn.username, Some("root".to_string()));
        assert_eq!(conn.password_source, PasswordSource::Prompt);
    }

    #[test]
    fn test_parse_folder_hierarchy() {
        let importer = RoyalTsImporter::new();
        let content = r#"<?xml version="1.0" encoding="utf-8"?>
<RTSZDocument>
  <RoyalFolder>
    <ID>folder1</ID>
    <Name>Production</Name>
  </RoyalFolder>
  <RoyalFolder>
    <ID>folder2</ID>
    <Name>Web Servers</Name>
    <ParentID>folder1</ParentID>
  </RoyalFolder>
  <RoyalSSHConnection>
    <ID>conn1</ID>
    <Name>Web01</Name>
    <URI>web01.example.com</URI>
    <ParentID>folder2</ParentID>
  </RoyalSSHConnection>
</RTSZDocument>"#;

        let result = importer.parse_xml(content, "test.rtsz");
        assert_eq!(result.groups.len(), 2);
        assert_eq!(result.connections.len(), 1);

        let production = result
            .groups
            .iter()
            .find(|g| g.name == "Production")
            .expect("Production group");
        let web_servers = result
            .groups
            .iter()
            .find(|g| g.name == "Web Servers")
            .expect("Web Servers group");
        assert!(production.parent_id.is_none());
        assert_eq!(web_servers.parent_id, Some(production.id));

        let conn = &result.connections[0];
        assert_eq!(conn.group_id, Some(web_servers.id));
    }

    #[test]
    fn test_skip_no_host() {
        let importer = RoyalTsImporter::new();
        let content = r#"<?xml version="1.0" encoding="utf-8"?>
<RTSZDocument>
  <RoyalSSHConnection>
    <ID>conn1</ID>
    <Name>No Host</Name>
  </RoyalSSHConnection>
</RTSZDocument>"#;

        let result = importer.parse_xml(content, "test.rtsz");
        assert_eq!(result.connections.len(), 0);
        assert_eq!(result.skipped.len(), 1);
    }

    #[test]
    fn test_skip_trashed_connections() {
        let importer = RoyalTsImporter::new();
        let content = r#"<?xml version="1.0" encoding="utf-8"?>
<RTSZDocument>
  <RoyalTrash>
    <ID>trash-folder-id</ID>
    <Name>Trash</Name>
  </RoyalTrash>
  <RoyalSSHConnection>
    <ID>conn1</ID>
    <Name>Active Server</Name>
    <URI>active.example.com</URI>
  </RoyalSSHConnection>
  <RoyalSSHConnection>
    <ID>conn2</ID>
    <Name>Deleted Server</Name>
    <URI>deleted.example.com</URI>
    <ParentID>trash-folder-id</ParentID>
  </RoyalSSHConnection>
  <RoyalRDSConnection>
    <ID>rdp1</ID>
    <Name>Deleted RDP</Name>
    <URI>deleted-rdp.example.com</URI>
    <ParentID>trash-folder-id</ParentID>
  </RoyalRDSConnection>
</RTSZDocument>"#;

        let result = importer.parse_xml(content, "test.rtsz");
        // Only the active server should be imported, trashed ones skipped
        assert_eq!(result.connections.len(), 1);
        assert_eq!(result.connections[0].name, "Active Server");
    }

    #[test]
    fn test_parse_ssh_with_private_key() {
        let importer = RoyalTsImporter::new();
        let content = r#"<?xml version="1.0" encoding="utf-8"?>
<RTSZDocument>
  <RoyalSSHConnection>
    <ID>conn1</ID>
    <Name>Server with Key</Name>
    <URI>server.example.com</URI>
    <Port>22</Port>
    <PrivateKeyFile>/home/user/.ssh/id_rsa</PrivateKeyFile>
  </RoyalSSHConnection>
  <RoyalSSHConnection>
    <ID>conn2</ID>
    <Name>Server with KeyFilePath</Name>
    <URI>server2.example.com</URI>
    <KeyFilePath>~/.ssh/id_ed25519</KeyFilePath>
  </RoyalSSHConnection>
  <RoyalSSHConnection>
    <ID>conn3</ID>
    <Name>Server without Key</Name>
    <URI>server3.example.com</URI>
  </RoyalSSHConnection>
</RTSZDocument>"#;

        let result = importer.parse_xml(content, "test.rtsz");
        assert_eq!(result.connections.len(), 3);
        assert!(result.errors.is_empty());

        // First connection: has PrivateKeyFile
        let conn1 = &result.connections[0];
        assert_eq!(conn1.name, "Server with Key");
        if let ProtocolConfig::Ssh(ref ssh) = conn1.protocol_config {
            assert_eq!(ssh.auth_method, SshAuthMethod::PublicKey);
            let key_path = ssh.key_path.as_ref().expect("key path");
            assert!(key_path.to_string_lossy().contains(".ssh/id_rsa"));
        } else {
            panic!("Expected SSH config");
        }

        // Second connection: has KeyFilePath with tilde expansion
        let conn2 = &result.connections[1];
        assert_eq!(conn2.name, "Server with KeyFilePath");
        if let ProtocolConfig::Ssh(ref ssh) = conn2.protocol_config {
            assert_eq!(ssh.auth_method, SshAuthMethod::PublicKey);
            // Tilde should be expanded
            let key_path = ssh.key_path.as_ref().expect("key path");
            assert!(!key_path.to_string_lossy().starts_with('~'));
        } else {
            panic!("Expected SSH config");
        }

        // Third connection: no key, should use password auth
        let conn3 = &result.connections[2];
        assert_eq!(conn3.name, "Server without Key");
        if let ProtocolConfig::Ssh(ref ssh) = conn3.protocol_config {
            assert_eq!(ssh.auth_method, SshAuthMethod::Password);
            assert!(ssh.key_path.is_none());
        } else {
            panic!("Expected SSH config");
        }
    }

    #[test]
    fn imports_rds_and_legacy_rdp_elements() {
        let importer = RoyalTsImporter::new();
        let content = r#"<?xml version="1.0" encoding="utf-8"?>
<RTSZDocument>
  <RoyalRDSConnection>
    <ID>rds1</ID>
    <Name>Terminal Server</Name>
    <URI>ts.example.com</URI>
    <RDPPort>3390</RDPPort>
  </RoyalRDSConnection>
  <RoyalTerminalServicesConnection>
    <ID>rds2</ID>
    <Name>Legacy TS</Name>
    <URI>legacy.example.com</URI>
  </RoyalTerminalServicesConnection>
  <RoyalVNCConnection>
    <ID>vnc1</ID>
    <Name>Desktop</Name>
    <URI>vnc.example.com</URI>
    <Port>5901</Port>
  </RoyalVNCConnection>
</RTSZDocument>"#;

        let result = importer.parse_xml(content, "test.rtsz");
        assert_eq!(result.connections.len(), 3, "{:?}", result.skipped);

        let rds = &result.connections[0];
        assert!(matches!(rds.protocol_config, ProtocolConfig::Rdp(_)));
        assert_eq!(rds.port, 3390, "RDS connections use RDPPort");

        let legacy = &result.connections[1];
        assert!(matches!(legacy.protocol_config, ProtocolConfig::Rdp(_)));
        assert_eq!(legacy.port, 3389);

        let vnc = &result.connections[2];
        assert!(matches!(vnc.protocol_config, ProtocolConfig::Vnc(_)));
        assert_eq!(vnc.port, 5901);
    }

    #[test]
    fn inherits_credentials_from_folder() {
        let importer = RoyalTsImporter::new();
        let content = r#"<?xml version="1.0" encoding="utf-8"?>
<RTSZDocument>
  <RoyalCredential>
    <ID>cred1</ID>
    <Name>Domain Admin</Name>
    <UserName>administrator</UserName>
    <Domain>CORP</Domain>
  </RoyalCredential>
  <RoyalFolder>
    <ID>folder1</ID>
    <Name>Datacenter</Name>
    <CredentialMode>3</CredentialMode>
    <CredentialId>cred1</CredentialId>
  </RoyalFolder>
  <RoyalFolder>
    <ID>folder2</ID>
    <Name>Hypervisors</Name>
    <ParentID>folder1</ParentID>
    <CredentialFromParent>True</CredentialFromParent>
  </RoyalFolder>
  <RoyalRDSConnection>
    <ID>rds1</ID>
    <Name>ESXi Host</Name>
    <URI>esxi.example.com</URI>
    <ParentID>folder2</ParentID>
    <CredentialMode>1</CredentialMode>
    <CredentialId>00000000-0000-0000-0000-000000000000</CredentialId>
  </RoyalRDSConnection>
  <RoyalSSHConnection>
    <ID>ssh1</ID>
    <Name>Inline User</Name>
    <URI>shell.example.com</URI>
    <CredentialMode>2</CredentialMode>
    <CredentialUsername>deploy</CredentialUsername>
  </RoyalSSHConnection>
</RTSZDocument>"#;

        let result = importer.parse_xml(content, "test.rtsz");
        assert_eq!(result.connections.len(), 2, "{:?}", result.skipped);

        let rds = &result.connections[0];
        assert_eq!(rds.username.as_deref(), Some("administrator"));
        assert_eq!(rds.domain.as_deref(), Some("CORP"));
        assert_eq!(rds.password_source, PasswordSource::Prompt);

        let ssh = &result.connections[1];
        assert_eq!(ssh.username.as_deref(), Some("deploy"));
    }

    #[test]
    fn reports_unsupported_connection_objects() {
        let importer = RoyalTsImporter::new();
        let content = r#"<?xml version="1.0" encoding="utf-8"?>
<RTSZDocument>
  <RoyalWebConnection>
    <ID>web1</ID>
    <Name>Intranet</Name>
    <URI>https://intranet.example.com</URI>
  </RoyalWebConnection>
</RTSZDocument>"#;

        let result = importer.parse_xml(content, "test.rtsz");
        assert!(result.connections.is_empty());
        assert_eq!(result.skipped.len(), 1);
        assert_eq!(result.skipped[0].identifier, "Intranet");
        assert!(
            result.skipped[0].reason.contains("RoyalWebConnection"),
            "reason should name the object type: {}",
            result.skipped[0].reason
        );
    }

    #[test]
    fn unescapes_xml_entities() {
        let importer = RoyalTsImporter::new();
        let content = r#"<?xml version="1.0" encoding="utf-8"?>
<RTSZDocument>
  <RoyalSSHConnection>
    <ID>conn1</ID>
    <Name>Dev &amp; Test</Name>
    <URI>dev.example.com</URI>
  </RoyalSSHConnection>
</RTSZDocument>"#;

        let result = importer.parse_xml(content, "test.rtsz");
        assert_eq!(result.connections.len(), 1);
        assert_eq!(result.connections[0].name, "Dev & Test");
    }
}
