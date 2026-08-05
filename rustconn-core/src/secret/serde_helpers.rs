//! Serde helpers for secret deserialization.
//!
//! These helpers wrap incoming password fields directly in [`SecretString`]
//! so the plaintext is never materialised as a plain `String` on the heap.
//! Use them on backend response structs (Bitwarden, Passbolt, `KeePassXC`,
//! libvirt XML, RDM imports) to satisfy `secrets-guide.md` rule #6.
//!
//! Example:
//!
//! ```ignore
//! use serde::Deserialize;
//! use crate::secret::serde_helpers::deserialize_optional_secret;
//!
//! #[derive(Deserialize)]
//! struct ApiResponse {
//!     #[serde(default, deserialize_with = "deserialize_optional_secret")]
//!     password: Option<secrecy::SecretString>,
//! }
//! ```

use secrecy::SecretString;
use serde::{Deserialize, Deserializer};

/// Deserializes `Option<String>` into `Option<SecretString>` without
/// keeping the plaintext in a long-lived `String`.
///
/// `serde_json` allocates the `String` for the borrowed JSON text
/// regardless; this helper only ensures the value lives inside a
/// `SecretString` immediately afterwards (which redacts itself in `Debug`
/// and zeroises on drop).
///
/// # Errors
///
/// Returns the underlying deserialization error if the field is not a
/// JSON string or null.
pub fn deserialize_optional_secret<'de, D>(
    deserializer: D,
) -> Result<Option<SecretString>, D::Error>
where
    D: Deserializer<'de>,
{
    let opt: Option<String> = Option::deserialize(deserializer)?;
    Ok(opt.map(SecretString::from))
}

/// Tolerant variant of [`deserialize_optional_secret`] that also accepts
/// numeric JSON values by converting them to their decimal representation.
///
/// Real-world RDM exports occasionally encode fields as integers where the
/// documented schema uses a string (e.g. a numeric PIN stored in the
/// password field). Without this helper, serde rejects the entire entry
/// with "invalid type: integer N, expected a string".
///
/// # Errors
///
/// Returns a deserialization error only for structurally invalid JSON
/// (arrays, objects, etc. — not numbers or booleans).
pub fn deserialize_tolerant_secret<'de, D>(
    deserializer: D,
) -> Result<Option<SecretString>, D::Error>
where
    D: Deserializer<'de>,
{
    let raw = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(raw.and_then(|v| match v {
        serde_json::Value::String(s) if !s.is_empty() => Some(SecretString::from(s)),
        serde_json::Value::Number(n) => Some(SecretString::from(n.to_string())),
        serde_json::Value::Bool(b) => Some(SecretString::from(b.to_string())),
        _ => None,
    }))
}

/// Deserializes an `Option<String>` that may arrive as an integer, boolean,
/// or string in the JSON source.
///
/// Devolutions RDM exports serialise many fields inconsistently depending on
/// the data source type — a GUID may be a string in one export and absent in
/// another, a description may be `null`, a port number written as a bare
/// integer into a nominally-string field, etc. This helper coerces any
/// scalar JSON value to `Some(String)` and maps `null` / empty-string /
/// arrays / objects to `None`, so serde never rejects an entry outright.
///
/// # Errors
///
/// Infallible for valid JSON — always returns `Ok`.
pub fn deserialize_tolerant_string<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let raw = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(raw.and_then(|v| match v {
        serde_json::Value::String(s) if !s.is_empty() => Some(s),
        serde_json::Value::Number(n) => Some(n.to_string()),
        serde_json::Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }))
}

#[cfg(test)]
mod tests {
    use secrecy::ExposeSecret;
    use serde::Deserialize;

    use super::*;

    #[derive(Deserialize)]
    struct Wrapper {
        #[serde(default, deserialize_with = "deserialize_optional_secret")]
        password: Option<SecretString>,
    }

    #[derive(Deserialize)]
    struct TolerantWrapper {
        #[serde(default, deserialize_with = "deserialize_tolerant_secret")]
        password: Option<SecretString>,
    }

    #[derive(Deserialize)]
    struct StringWrapper {
        #[serde(default, deserialize_with = "deserialize_tolerant_string")]
        value: Option<String>,
    }

    #[test]
    fn deserializes_json_string_into_secret() {
        let json = r#"{"password": "hunter2"}"#;
        let parsed: Wrapper = serde_json::from_str(json).expect("parse");
        let secret = parsed.password.expect("Some");
        assert_eq!(secret.expose_secret(), "hunter2");
    }

    #[test]
    fn deserializes_null_as_none() {
        let json = r#"{"password": null}"#;
        let parsed: Wrapper = serde_json::from_str(json).expect("parse");
        assert!(parsed.password.is_none());
    }

    #[test]
    fn deserializes_missing_field_as_none() {
        let json = "{}";
        let parsed: Wrapper = serde_json::from_str(json).expect("parse");
        assert!(parsed.password.is_none());
    }

    #[test]
    fn debug_does_not_leak_secret() {
        let secret = SecretString::from("hunter2");
        let rendered = format!("{secret:?}");
        assert!(
            !rendered.contains("hunter2"),
            "Debug leaked secret: {rendered}"
        );
    }

    #[test]
    fn tolerant_secret_accepts_number() {
        let json = r#"{"password": 1234}"#;
        let parsed: TolerantWrapper = serde_json::from_str(json).expect("parse");
        let secret = parsed.password.expect("Some");
        assert_eq!(secret.expose_secret(), "1234");
    }

    #[test]
    fn tolerant_secret_accepts_string() {
        let json = r#"{"password": "s3cret"}"#;
        let parsed: TolerantWrapper = serde_json::from_str(json).expect("parse");
        let secret = parsed.password.expect("Some");
        assert_eq!(secret.expose_secret(), "s3cret");
    }

    #[test]
    fn tolerant_secret_maps_null_to_none() {
        let json = r#"{"password": null}"#;
        let parsed: TolerantWrapper = serde_json::from_str(json).expect("parse");
        assert!(parsed.password.is_none());
    }

    #[test]
    fn tolerant_secret_maps_empty_string_to_none() {
        let json = r#"{"password": ""}"#;
        let parsed: TolerantWrapper = serde_json::from_str(json).expect("parse");
        assert!(parsed.password.is_none());
    }

    #[test]
    fn tolerant_string_accepts_number() {
        let json = r#"{"value": 42}"#;
        let parsed: StringWrapper = serde_json::from_str(json).expect("parse");
        assert_eq!(parsed.value.as_deref(), Some("42"));
    }

    #[test]
    fn tolerant_string_accepts_string() {
        let json = r#"{"value": "hello"}"#;
        let parsed: StringWrapper = serde_json::from_str(json).expect("parse");
        assert_eq!(parsed.value.as_deref(), Some("hello"));
    }

    #[test]
    fn tolerant_string_maps_empty_to_none() {
        let json = r#"{"value": ""}"#;
        let parsed: StringWrapper = serde_json::from_str(json).expect("parse");
        assert!(parsed.value.is_none());
    }

    #[test]
    fn tolerant_string_maps_null_to_none() {
        let json = r#"{"value": null}"#;
        let parsed: StringWrapper = serde_json::from_str(json).expect("parse");
        assert!(parsed.value.is_none());
    }
}
