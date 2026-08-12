//! Tab creation, closing, parking, and reparenting.
//!
//! Extracted from `terminal/mod.rs` to reduce module complexity.
//! Contains methods for creating terminal/VNC/RDP/Web tabs and managing
//! their lifecycle (close, park, restore, reparent).

use super::*;

impl TerminalNotebook {
    // ========================================================================
    // Welcome Tab
    // ========================================================================

    /// Creates the welcome tab content — uses the full welcome screen with features.
    pub(super) fn create_welcome_tab() -> GtkBox {
        let container = GtkBox::new(Orientation::Vertical, 0);
        container.set_hexpand(true);
        container.set_vexpand(true);

        // Use the full welcome content from SplitViewBridge for consistency
        let status_page = crate::split_view::SplitViewBridge::create_welcome_content_static();
        container.append(&status_page);
        container
    }

    /// Appends the Welcome tab to an empty `TabView`.
    ///
    /// Shared by both paths that can empty the tab bar: a normal tab close and
    /// parking the last tab into a detached window (issue #236). The caller
    /// checks the user preference and that no pages are left.
    pub(super) fn append_welcome_page(tab_view: &adw::TabView) {
        let welcome = Self::create_welcome_tab();
        let welcome_wrap = TabPageContainer::welcome(&welcome.upcast::<gtk4::Widget>());
        let welcome_page = tab_view.append(welcome_wrap.widget());
        welcome_page.set_title(&i18n("Welcome"));
        welcome_page.set_icon(Some(&gio::ThemedIcon::new("go-home-symbolic")));
    }

    /// Gets the icon name for a protocol.
    pub(super) fn get_protocol_icon(protocol: &str) -> &'static str {
        rustconn_core::get_protocol_icon_by_name(protocol)
    }

    /// Removes the welcome page if it exists.
    pub(super) fn remove_welcome_page(&self) {
        if self.sessions.borrow().is_empty() && self.tab_view.n_pages() > 0 {
            // Find and remove welcome page
            for i in 0..self.tab_view.n_pages() {
                let page = self.tab_view.nth_page(i);
                if page.title() == i18n("Welcome") {
                    self.tab_view.close_page(&page);
                    break;
                }
            }
        }
    }

    /// Restores the Welcome page when the configured empty-notebook conditions hold.
    pub(super) fn ensure_welcome_page(&self) {
        if self.show_welcome.get()
            && self.sessions.borrow().is_empty()
            && self.tab_view.n_pages() == 0
        {
            Self::append_welcome_page(&self.tab_view);
        }
    }

    // ========================================================================
    // Tab Tooltip Helper
    // ========================================================================

    /// Builds a tab tooltip from a session title, its host, and its group.
    ///
    /// One place decides the layout — title, then the host line the embedded
    /// creation paths add, then the group line `set_tab_group` appends — so a tab
    /// recreated after a park or a rename is indistinguishable from the original
    /// (Requirement 2.3).
    pub(super) fn tab_tooltip(title: &str, host: Option<&str>, group: Option<&str>) -> String {
        use std::fmt::Write;

        let mut tooltip = title.to_owned();
        if let Some(host) = host.filter(|host| !host.is_empty()) {
            tooltip.push('\n');
            tooltip.push_str(host);
        }
        if let Some(group) = group {
            // Writing into a String never fails; the result is discarded the
            // same way the other string builders in the GUI do it.
            let _ = write!(tooltip, "\n[{group}]");
        }
        tooltip
    }
}
