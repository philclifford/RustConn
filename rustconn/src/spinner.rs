//! The one place that knows whether this build has `AdwSpinner`.
//!
//! `AdwSpinner` arrived in libadwaita 1.6 and is the widget GNOME expects a
//! modern application to use. Two supported build targets are still below that
//! version — Ubuntu 24.04 ships libadwaita 1.5.0, and the snap's `core24`
//! `gnome-46-2404` platform ships 1.5 — so the default build has to keep
//! working against `GtkSpinner`. Rather than repeat a `#[cfg]` pair at each of
//! the six construction sites, the choice is made once here and every caller
//! spells the type [`Spinner`].
//!
//! # Why there is no `set_spinning`
//!
//! `AdwSpinner` animates whenever it is mapped and offers no way to stop it;
//! `GtkSpinner` animates only while its `spinning` property is set. The two are
//! made to behave the same by handing out a `GtkSpinner` that is *already*
//! spinning: GTK advances a CSS animation only for a mapped widget, so a
//! spinner hidden by its own `visible` flag or by an ancestor costs nothing on
//! either path. Callers therefore control a spinner the same way on both — by
//! showing and hiding it — and nothing outside this module mentions `spinning`.
//!
//! Retire this module once no supported target is below libadwaita 1.6: delete
//! the `adw-1-6` feature, keep the `adw` arm and drop the indirection.

use gtk4::prelude::*;

/// The spinner widget this build uses.
///
/// `adw::Spinner` with the `adw-1-6` feature, `gtk4::Spinner` without it. Both
/// are `GtkWidget`s, so callers can size them, show and hide them, and put them
/// in a container without caring which one they hold.
#[cfg(feature = "adw-1-6")]
pub type Spinner = libadwaita::Spinner;

/// The spinner widget this build uses.
///
/// `adw::Spinner` with the `adw-1-6` feature, `gtk4::Spinner` without it. Both
/// are `GtkWidget`s, so callers can size them, show and hide them, and put them
/// in a container without caring which one they hold.
#[cfg(not(feature = "adw-1-6"))]
pub type Spinner = gtk4::Spinner;

/// Creates a spinner that animates as soon as it is shown.
#[must_use]
pub fn new() -> Spinner {
    #[cfg(feature = "adw-1-6")]
    {
        Spinner::new()
    }
    #[cfg(not(feature = "adw-1-6"))]
    {
        // See the module docs: spinning is set once and never cleared, so the
        // 1.5 path reacts to `visible` exactly like AdwSpinner does.
        Spinner::builder().spinning(true).build()
    }
}

/// Creates a spinner of a fixed size, for the places that need one larger than
/// the natural 16 px — a progress page rather than a list row, say.
#[must_use]
pub fn sized(width: i32, height: i32) -> Spinner {
    let spinner = Spinner::builder()
        .width_request(width)
        .height_request(height);
    #[cfg(feature = "adw-1-6")]
    {
        spinner.build()
    }
    #[cfg(not(feature = "adw-1-6"))]
    {
        spinner.spinning(true).build()
    }
}

/// Gives a spinner an accessible name, for the ones that carry meaning on their
/// own rather than sitting next to a label that already says it.
///
/// Goes through `GtkWidget` because libadwaita 0.9 does not implement
/// `IsA<gtk4::Accessible>` for `adw::Spinner`, so the property cannot be set on
/// the concrete type on the 1.6 path. Every `GtkWidget` is a `GtkAccessible` in
/// C, so the upcast is sound and works on both paths.
pub fn set_accessible_label(spinner: &Spinner, label: &str) {
    spinner
        .upcast_ref::<gtk4::Widget>()
        .update_property(&[gtk4::accessible::Property::Label(label)]);
}
