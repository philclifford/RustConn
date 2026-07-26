//! Top-level windows that host a single detached session.
//!
//! A detached window is a pure **view host**: it borrows the widget subtree of a
//! session that [`crate::terminal::TerminalNotebook`] still owns. It stores no
//! session state beyond the two ids it needs for its own callbacks, so every
//! per-session map keeps working while the session lives outside the main
//! window.
//!
//! Replaces the never-constructed `ExternalWindow` / `ExternalWindowManager`
//! pair: that one reparented a bare `vte4::Terminal`, had no toast overlay, no
//! per-window accelerator wiring, and no way back into a tab.
//!
//! Callbacks installed here are plain boxed closures. The window layer wires
//! them with `Weak` handles to the notebook and the registry, so nothing in this
//! module can form an `Rc` cycle that outlives a session.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use gtk4::prelude::*;
use gtk4::{Box as GtkBox, Button, DirectionType, gdk, glib};
use libadwaita as adw;
use libadwaita::prelude::*;
use uuid::Uuid;

use crate::i18n::{i18n, i18n_f};

/// Default window width, matching a comfortable 80-column terminal plus chrome.
const DEFAULT_WIDTH: i32 = 900;
/// Default window height (Requirement 5.8 asks for at least 800x600).
const DEFAULT_HEIGHT: i32 = 650;
/// Smallest width a detached window may be shrunk to before the session content
/// stops being usable.
const MIN_WIDTH: i32 = 400;
/// Smallest usable height for a detached window.
const MIN_HEIGHT: i32 = 300;

/// Callback slot invoked with the window's own session id.
type SessionCallback = Rc<RefCell<Option<Box<dyn Fn(Uuid)>>>>;

/// Identity and labelling of the session a detached window is built for.
pub struct DetachedWindowParams<'a> {
    /// Session whose widget the window hosts.
    pub session_id: Uuid,
    /// Connection the session belongs to.
    pub connection_id: Uuid,
    /// Connection name, shown in the window and header bar title.
    pub title: &'a str,
    /// Human-readable protocol name, shown as the header bar subtitle.
    pub protocol: &'a str,
}

/// Builds the window title for a detached session (pure, testable).
///
/// Uses an em dash, matching the GNOME convention of "document — application".
fn detached_window_title(connection: &str) -> String {
    i18n_f("{} — RustConn", &[connection])
}

/// A top-level window hosting exactly one detached session.
pub struct DetachedSessionWindow {
    window: adw::ApplicationWindow,
    session_id: Uuid,
    connection_id: Uuid,
    toast_overlay: adw::ToastOverlay,
    window_title: adw::WindowTitle,
    /// Kept alive for its `clicked` handler — dropping it breaks the attach
    /// button (see the widget-lifecycle note in `main.rs`).
    attach_button: Button,
    /// Set while the window is closing because its session moved back into a
    /// tab, so the close handler skips teardown. Same distinction
    /// `parked_in_split` makes for tabs.
    attaching: Rc<Cell<bool>>,
    on_attach: SessionCallback,
    on_close: SessionCallback,
}

impl DetachedSessionWindow {
    /// Creates a window around a session's content box and wires its chrome.
    ///
    /// `content` is the box handed over by
    /// [`crate::terminal::TerminalNotebook::take_session_content`]; it is placed
    /// as-is so the session's monitoring bar and highlight overlay travel with
    /// it. The window is not presented yet — call [`Self::present`] or
    /// [`Self::present_fullscreen_on`].
    #[must_use]
    pub fn new(
        app: &adw::Application,
        params: &DetachedWindowParams<'_>,
        content: &GtkBox,
    ) -> Self {
        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title(detached_window_title(params.title))
            .default_width(DEFAULT_WIDTH)
            .default_height(DEFAULT_HEIGHT)
            .width_request(MIN_WIDTH)
            .height_request(MIN_HEIGHT)
            .build();

        let window_title = adw::WindowTitle::new(params.title, params.protocol);
        let attach_button = build_attach_button();

        let header = adw::HeaderBar::new();
        header.set_title_widget(Some(&window_title));
        header.pack_start(&attach_button);

        let toolbar_view = adw::ToolbarView::new();
        toolbar_view.add_top_bar(&header);
        toolbar_view.set_content(Some(content));

        let toast_overlay = adw::ToastOverlay::new();
        toast_overlay.set_child(Some(&toolbar_view));
        window.set_content(Some(&toast_overlay));

        // Per-window wiring the main window gets in `app.rs`: the non-Latin
        // layout accelerator fallback plus the compact chrome. `set_compact_prefs`
        // already iterates every application window, so later preference changes
        // reach this window without extra code.
        crate::app::install_layout_independent_accels(&window, app);
        crate::app::watch_window_for_compact(window.upcast_ref());
        crate::app::recompute_window_compact(window.upcast_ref());

        let detached = Self {
            window,
            session_id: params.session_id,
            connection_id: params.connection_id,
            toast_overlay,
            window_title,
            attach_button,
            attaching: Rc::new(Cell::new(false)),
            on_attach: Rc::new(RefCell::new(None)),
            on_close: Rc::new(RefCell::new(None)),
        };
        detached.connect_attach_button();
        detached.connect_close_handler();
        detached.focus_content_on_idle(content);
        tracing::info!(
            session = %params.session_id,
            connection = %params.connection_id,
            protocol = params.protocol,
            "detached window created"
        );
        detached
    }

    /// Fires the attach callback when the header bar button is clicked.
    ///
    /// The button deliberately carries no `GAction`: the window layer owns the
    /// `win.attach-session` wiring and installs it as this callback, which keeps
    /// this module free of action-name coupling.
    fn connect_attach_button(&self) {
        let on_attach = Rc::clone(&self.on_attach);
        let session_id = self.session_id;
        self.attach_button.connect_clicked(move |_button| {
            if let Some(ref callback) = *on_attach.borrow() {
                callback(session_id);
            } else {
                tracing::warn!(session = %session_id, "attach requested with no handler installed");
            }
        });
    }

    /// Runs session teardown when the user closes the window.
    ///
    /// A close that follows [`Self::begin_attach`] means the session moved back
    /// into a tab, so it proceeds silently.
    fn connect_close_handler(&self) {
        let attaching = Rc::clone(&self.attaching);
        let on_close = Rc::clone(&self.on_close);
        let session_id = self.session_id;
        self.window.connect_close_request(move |_window| {
            if attaching.get() {
                tracing::debug!(session = %session_id, "detached window closing for attach");
                return glib::Propagation::Proceed;
            }
            tracing::info!(session = %session_id, "detached window closed by user");
            if let Some(ref callback) = *on_close.borrow() {
                callback(session_id);
            }
            glib::Propagation::Proceed
        });
    }

    /// Nudges a repaint and moves input focus into the session content.
    ///
    /// Runs on idle so GTK has settled the re-parented widget's allocation
    /// first: embedded viewers keep their frames in a Rust-side buffer, so
    /// nothing else triggers the first draw in the new window.
    fn focus_content_on_idle(&self, content: &GtkBox) {
        let content = content.clone();
        let session_id = self.session_id;
        glib::idle_add_local_once(move || {
            content.queue_draw();
            if !content.child_focus(DirectionType::TabForward) {
                tracing::debug!(
                    session = %session_id,
                    "detached window: session content took no focus"
                );
            }
        });
    }

    /// Shows the window and gives it keyboard focus.
    pub fn present(&self) {
        self.window.present();
    }

    /// Shows the window fullscreen on a specific monitor.
    ///
    /// Wayland offers no window positioning by coordinates, so a monitor choice
    /// is expressed as fullscreen-on-monitor, which the compositor honors.
    pub fn present_fullscreen_on(&self, monitor: &gdk::Monitor) {
        self.window.fullscreen_on_monitor(monitor);
        self.window.present();
    }

    /// Updates the window and header bar title after a connection rename.
    pub fn set_session_title(&self, title: &str) {
        self.window.set_title(Some(&detached_window_title(title)));
        self.window_title.set_title(title);
    }

    /// Returns the overlay that toasts for this session must be shown on.
    #[must_use]
    pub const fn toast_overlay(&self) -> &adw::ToastOverlay {
        &self.toast_overlay
    }

    /// Returns the underlying window, for parenting dialogs to it.
    #[must_use]
    pub const fn window(&self) -> &adw::ApplicationWindow {
        &self.window
    }

    /// Returns the session hosted by this window.
    #[must_use]
    pub const fn session_id(&self) -> Uuid {
        self.session_id
    }

    /// Returns the connection the hosted session belongs to.
    #[must_use]
    pub const fn connection_id(&self) -> Uuid {
        self.connection_id
    }

    /// Marks the pending close as an attach, so no session teardown runs.
    ///
    /// Call immediately before [`Self::close`] once the session's content is
    /// back in a tab.
    pub fn begin_attach(&self) {
        self.attaching.set(true);
    }

    /// Closes the window.
    pub fn close(&self) {
        self.window.close();
    }

    /// Sets the callback invoked when the attach button is clicked.
    pub fn set_on_attach<F>(&self, callback: F)
    where
        F: Fn(Uuid) + 'static,
    {
        *self.on_attach.borrow_mut() = Some(Box::new(callback));
    }

    /// Sets the callback invoked when the user closes the window.
    ///
    /// The window layer runs the standard session teardown from it, so a
    /// detached-window close ends the session exactly as a tab close does.
    pub fn set_on_close<F>(&self, callback: F)
    where
        F: Fn(Uuid) + 'static,
    {
        *self.on_close.borrow_mut() = Some(Box::new(callback));
    }
}

/// Builds the header bar button that moves the session back into a tab.
fn build_attach_button() -> Button {
    let button = Button::from_icon_name("view-restore-symbolic");
    button.add_css_class("flat");
    let label = i18n("Move to Main Window");
    button.set_tooltip_text(Some(&label));
    button.update_property(&[gtk4::accessible::Property::Label(&label)]);
    button
}

/// The detached windows currently open, keyed by session.
///
/// Owned by the main window and shared as an `Rc`. It holds the only strong
/// reference to each [`DetachedSessionWindow`] value; dropping the entry and
/// closing the window releases it.
#[derive(Default)]
pub struct DetachedWindowRegistry {
    windows: RefCell<HashMap<Uuid, DetachedSessionWindow>>,
}

impl DetachedWindowRegistry {
    /// Creates an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a window under its own session id.
    pub fn insert(&self, window: DetachedSessionWindow) {
        self.windows.borrow_mut().insert(window.session_id, window);
    }

    /// Removes and returns the window of a session, if it has one.
    pub fn take(&self, session_id: Uuid) -> Option<DetachedSessionWindow> {
        self.windows.borrow_mut().remove(&session_id)
    }

    /// Presents the window of a session; reports whether one was found.
    ///
    /// This is what a focus request for a detached session resolves to, so
    /// sidebar activation and the session manager keep working unchanged.
    pub fn present(&self, session_id: Uuid) -> bool {
        self.with_window(session_id, DetachedSessionWindow::present)
            .is_some()
    }

    /// Reports whether a session currently has a detached window.
    #[must_use]
    pub fn contains(&self, session_id: Uuid) -> bool {
        self.windows.borrow().contains_key(&session_id)
    }

    /// Returns how many detached windows are open.
    #[must_use]
    pub fn count(&self) -> usize {
        self.windows.borrow().len()
    }

    /// Closes every detached window, running the normal session teardown.
    ///
    /// Entries are removed before anything is closed, so a close handler that
    /// calls back into the registry cannot hit an active borrow.
    pub fn close_all(&self) {
        let windows: Vec<DetachedSessionWindow> =
            self.windows.borrow_mut().drain().map(|(_, w)| w).collect();
        tracing::info!(count = windows.len(), "closing all detached windows");
        for window in &windows {
            window.close();
        }
    }

    /// Runs a closure on the window of a session, if it has one.
    ///
    /// `DetachedSessionWindow` is not cloneable, so this is how callers reach a
    /// registered window.
    pub fn with_window<F, R>(&self, session_id: Uuid, f: F) -> Option<R>
    where
        F: FnOnce(&DetachedSessionWindow) -> R,
    {
        self.windows.borrow().get(&session_id).map(f)
    }
}

#[cfg(test)]
mod tests {
    use super::detached_window_title;

    #[test]
    fn window_title_names_the_connection_and_the_application() {
        assert_eq!(detached_window_title("prod-db"), "prod-db — RustConn");
    }

    #[test]
    fn window_title_keeps_an_empty_connection_name_harmless() {
        assert_eq!(detached_window_title(""), " — RustConn");
    }
}
