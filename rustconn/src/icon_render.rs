//! Rendering rules for user-supplied icon values.
//!
//! Connections, groups and templates store their icon in a single string field
//! that may hold either an emoji glyph or a GTK icon name. Deciding which of
//! the two it is has to happen identically everywhere a row is drawn, otherwise
//! the same value shows up in one view and disappears in another.
//!
//! Use [`rustconn_core::dialog_utils::is_glyph_icon`] to pick the branch, and
//! [`theme_icon_or`] for the icon-name branch so a name the current theme does
//! not carry falls back to a visible default instead of empty space. That case
//! is routine in a Flatpak sandbox: the GNOME runtime ships only the Adwaita
//! theme, so names the user found in a host icon browser cannot be resolved.

/// Returns `true` when the current icon theme can resolve `name`.
///
/// Answers `true` when no display is available (headless unit runs), so a
/// caller never silently swaps a user's icon for the default in that case.
#[must_use]
pub fn theme_has_icon(name: &str) -> bool {
    gtk4::gdk::Display::default().is_none_or(|display| {
        let theme = gtk4::IconTheme::for_display(&display);
        theme.has_icon(name)
    })
}

/// Returns `name` when the icon theme can resolve it, otherwise `fallback`.
///
/// Keeps a row legible when a stored icon name is missing from the active
/// theme, which the icon widget would otherwise render as blank space.
#[must_use]
pub fn theme_icon_or<'a>(name: &'a str, fallback: &'a str) -> &'a str {
    if theme_has_icon(name) {
        name
    } else {
        tracing::debug!(icon = %name, %fallback, "icon name not in theme, using fallback");
        fallback
    }
}
