//! Single-key reads of `config.toml`, for decisions that predate the config system.
//!
//! Two startup choices have to be made before GTK is initialised: the interface
//! language (`LANGUAGE` must be set before the first translatable string is
//! evaluated) and the GSK renderer (`GSK_RENDERER` must be set before
//! `gtk_init`). Both come from `[ui]` in `~/.config/rustconn/config.toml`, but
//! the settings the rest of the application uses are loaded much later, when the
//! application state is built in `build_ui`. Rather than load the whole settings
//! tree that early, each of these decisions reads the one key it needs.
//!
//! The scan is deliberately not a TOML parser. It understands exactly what
//! RustConn itself writes — `key = "value"` inside a `[table]` — because that is
//! all it is asked to read, and because a malformed section elsewhere in the file
//! must not prevent the language from being applied. Anything it does not
//! understand reads as "not set", which lands on the same defaults as a missing
//! file.

/// Reads one string key from the `[ui]` table of the user's `config.toml`.
///
/// Returns `None` if the file is missing or unreadable, the `[ui]` table has no
/// such key, or the value is empty.
pub fn read_ui_string(key: &str) -> Option<String> {
    let path = dirs::config_dir()?.join("rustconn").join("config.toml");
    let content = std::fs::read_to_string(path).ok()?;
    find_table_string(&content, "ui", key)
}

/// Finds `key = "value"` inside `[table]`, without parsing the rest of the file.
///
/// Split from [`read_ui_string`] so the scan can be tested without touching the
/// user's real configuration directory.
fn find_table_string(content: &str, table: &str, key: &str) -> Option<String> {
    let wanted_header = format!("[{table}]");
    let mut in_table = false;

    for line in content.lines() {
        let trimmed = line.trim();

        if trimmed.starts_with('[') {
            in_table = trimmed == wanted_header;
            continue;
        }
        if !in_table {
            continue;
        }

        let Some(rest) = trimmed.strip_prefix(key) else {
            continue;
        };
        // `renderer_extra = …` starts with `renderer` but is a different key,
        // so the separator has to be the next non-space character.
        let Some(value) = rest.trim_start().strip_prefix('=') else {
            continue;
        };

        // Quoted, so an inline comment after the value is not part of it.
        let value = value.trim_start().strip_prefix('"')?;
        let value = value.split('"').next()?;
        if value.is_empty() {
            return None;
        }
        return Some(value.to_string());
    }

    None
}

#[cfg(test)]
mod tests {
    use super::find_table_string;

    /// The shape RustConn itself writes.
    #[test]
    fn finds_a_key_in_the_requested_table() {
        let content = "[ui]\nlanguage = \"uk\"\nrenderer = \"software\"\n";

        assert_eq!(
            find_table_string(content, "ui", "language").as_deref(),
            Some("uk")
        );
        assert_eq!(
            find_table_string(content, "ui", "renderer").as_deref(),
            Some("software")
        );
    }

    /// The same key name in another table must not answer for `[ui]`.
    /// `[terminal]` and `[ui]` both have keys RustConn reads at startup.
    #[test]
    fn ignores_the_same_key_in_another_table() {
        let content = "[terminal]\nrenderer = \"gpu\"\n\n[ui]\nlanguage = \"de\"\n";

        assert_eq!(find_table_string(content, "ui", "renderer"), None);
        assert_eq!(
            find_table_string(content, "ui", "language").as_deref(),
            Some("de")
        );
    }

    /// A longer key that merely starts with the one asked for is not a match —
    /// the separator must be the next non-space character.
    #[test]
    fn does_not_match_a_longer_key_with_the_same_prefix() {
        let content = "[ui]\nrenderer_debug = \"1\"\n";

        assert_eq!(find_table_string(content, "ui", "renderer"), None);
    }

    /// Spacing around `=` is the writer's choice, not the reader's.
    #[test]
    fn tolerates_missing_spaces_around_the_separator() {
        let content = "[ui]\n  renderer=\"software\"\n";

        assert_eq!(
            find_table_string(content, "ui", "renderer").as_deref(),
            Some("software")
        );
    }

    /// A hand-added trailing comment belongs to the file, not to the value.
    #[test]
    fn stops_the_value_at_the_closing_quote() {
        let content = "[ui]\nrenderer = \"software\" # slow GPU in this VM\n";

        assert_eq!(
            find_table_string(content, "ui", "renderer").as_deref(),
            Some("software")
        );
    }

    /// An empty value is not a choice; the caller's default must win.
    #[test]
    fn treats_an_empty_value_as_unset() {
        let content = "[ui]\nlanguage = \"\"\n";

        assert_eq!(find_table_string(content, "ui", "language"), None);
    }

    /// A key before any table header is not in `[ui]`.
    #[test]
    fn ignores_keys_outside_any_table() {
        let content = "renderer = \"software\"\n[ui]\nlanguage = \"fr\"\n";

        assert_eq!(find_table_string(content, "ui", "renderer"), None);
    }

    /// Absent key, absent table, empty file — all the same answer.
    #[test]
    fn returns_none_when_the_key_is_absent() {
        assert_eq!(find_table_string("", "ui", "renderer"), None);
        assert_eq!(find_table_string("[ui]\n", "ui", "renderer"), None);
        assert_eq!(
            find_table_string("[secrets]\nbackend = \"none\"\n", "ui", "renderer"),
            None
        );
    }
}
