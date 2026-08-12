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

    // ========================================================================
    // Terminal Tab Creation
    // ========================================================================

    /// Creates a new terminal tab for an SSH session with default settings
    pub fn create_terminal_tab(
        &self,
        connection_id: Uuid,
        title: &str,
        protocol: &str,
        automation: Option<&AutomationConfig>,
    ) -> Uuid {
        self.create_terminal_tab_with_settings(
            connection_id,
            title,
            protocol,
            automation,
            &rustconn_core::config::TerminalSettings::default(),
            None,
            &[], // no variables for default tab
        )
    }

    /// Creates a new terminal tab with specific settings
    ///
    /// When `theme_override` is `Some`, the per-connection colors are applied
    /// on top of the global theme. When `None`, the global theme is used as-is.
    ///
    /// `global_variables` are used to substitute `${VAR}` references in
    /// Expect-rule responses before the automation session is created.
    #[expect(
        clippy::too_many_arguments,
        reason = "function parameters mirror upstream API or struct fields 1:1; bundling into a struct only restates the field list"
    )]
    pub fn create_terminal_tab_with_settings(
        &self,
        connection_id: Uuid,
        title: &str,
        protocol: &str,
        automation: Option<&AutomationConfig>,
        settings: &rustconn_core::config::TerminalSettings,
        theme_override: Option<&rustconn_core::models::ConnectionThemeOverride>,
        global_variables: &[rustconn_core::Variable],
    ) -> Uuid {
        let session_id = Uuid::new_v4();
        self.remove_welcome_page();

        let terminal = Terminal::new();
        terminal.set_hexpand(true);
        terminal.set_vexpand(true);

        // Focus-based accelerator suspend (#197): when the VTE gains focus,
        // single-Ctrl chords (Ctrl+F/P/N…) must reach the shell instead of the
        // app accelerators; restore them when focus leaves. The actual
        // suspend/restore (and the `terminal_passthrough_ctrl` setting) is
        // decided by the listener wired via `set_on_terminal_focus`.
        self.attach_focus_passthrough(&terminal);

        // Build a VariableManager for substituting ${VAR} in Expect responses
        let var_manager = {
            let mut mgr = rustconn_core::variables::VariableManager::new();
            for var in global_variables {
                mgr.set_global(var.clone());
            }
            mgr
        };

        // Setup automation if configured
        if let Some(cfg) = automation
            && !cfg.expect_rules.is_empty()
        {
            let rules = prepare_rules_from_config(&cfg.expect_rules, &var_manager);

            if !rules.is_empty() {
                let session = AutomationSession::new(terminal.clone(), rules);
                self.automation_sessions
                    .borrow_mut()
                    .insert(session_id, session);
            }
        }

        // Apply user settings
        config::configure_terminal_with_settings(&terminal, settings);

        // Apply per-connection theme override (if present) on top of the global theme
        if let Some(override_colors) = theme_override {
            let base_theme = TerminalTheme::by_name(&settings.color_theme)
                .unwrap_or_else(TerminalTheme::dark_theme);
            config::apply_theme_override_with_base(&terminal, override_colors, &base_theme);
        }

        // VTE implements GtkScrollable natively — no ScrolledWindow needed.
        // Wrapping in ScrolledWindow intercepts mouse events and breaks
        // ncurses apps (mc, htop) that rely on VTE's internal mouse handling.
        // Instead, pair VTE with a standalone GtkScrollbar connected to its
        // vadjustment — the same approach used by GNOME Terminal.
        let terminal_row = GtkBox::new(Orientation::Horizontal, 0);
        terminal_row.set_hexpand(true);
        terminal_row.set_vexpand(true);
        terminal_row.append(&terminal);

        if settings.show_scrollbar {
            let scrollbar =
                gtk4::Scrollbar::new(Orientation::Vertical, terminal.vadjustment().as_ref());
            terminal_row.append(&scrollbar);
        }

        // Wrap terminal_row in an Overlay so the highlight DrawingArea can
        // be layered on top without interfering with VTE input.
        let terminal_overlay = gtk4::Overlay::new();
        terminal_overlay.set_child(Some(&terminal_row));
        terminal_overlay.set_hexpand(true);
        terminal_overlay.set_vexpand(true);

        // Outer vertical container: terminal row on top, monitoring bar below.
        // get_session_container() returns this box so monitoring can append to it.
        let container = GtkBox::new(Orientation::Vertical, 0);
        container.set_hexpand(true);
        container.set_vexpand(true);
        container.append(&terminal_overlay);

        // Right-click context menu actions installed on the terminal widget
        // so they follow it when reparented between TabView and split view.
        config::setup_context_menu(&terminal, &self.snippet_menu_section);

        // Drag-and-drop: insert shell-escaped file paths when files are
        // dragged from a file manager onto the terminal (GNOME Terminal behavior).
        file_drop::setup_file_drop_target(&terminal);

        // Wrap in TabPageContainer to guarantee non-zero allocation for TabOverview
        let tab_container = TabPageContainer::single(&container);

        // Add page to TabView — child is the TabPageContainer outer box
        let page = self.tab_view.append(tab_container.widget());
        page.set_title(title);
        page.set_icon(Some(&gio::ThemedIcon::new(Self::get_protocol_icon(
            protocol,
        ))));
        page.set_tooltip(title);

        // Store session data
        self.sessions.borrow_mut().insert(session_id, page.clone());
        let terminal_for_focus = terminal.clone();
        self.terminals.borrow_mut().insert(session_id, terminal);
        self.terminal_overlays
            .borrow_mut()
            .insert(session_id, terminal_overlay);
        self.tab_containers
            .borrow_mut()
            .insert(session_id, tab_container);

        self.session_info.borrow_mut().insert(
            session_id,
            TerminalSession {
                id: session_id,
                connection_id,
                name: title.to_string(),
                protocol: protocol.to_string(),
                is_embedded: true,
                host: None,
                log_file: None,
                history_entry_id: None,
                tab_group: None,
                tab_color_index: None,
                connected_at: chrono::Utc::now(),
            },
        );

        // Select the new page
        self.tab_view.set_selected_page(&page);

        // Auto-focus the terminal so the user can type immediately (#79).
        // Use idle_add_local_once so the focus request runs after the page
        // is fully mapped, and only if this page is still selected (avoids
        // focus-stealing when multiple tabs open in quick succession).
        let tab_view_focus = self.tab_view.clone();
        let page_focus = page.clone();
        let terminal_focus = terminal_for_focus;
        glib::idle_add_local_once(move || {
            if tab_view_focus.selected_page().as_ref() == Some(&page_focus) {
                terminal_focus.grab_focus();
            }
        });

        // Apply protocol color indicator if enabled
        if *self.color_tabs_by_protocol.borrow() {
            self.apply_protocol_color(session_id, protocol);
        }

        // Resolve any pending cluster registration for this connection
        self.resolve_cluster_pending(connection_id, session_id);

        // Notify listeners that a new terminal session was created.
        // Single choke point for per-session wiring (activity monitoring):
        // fires for every terminal protocol and for both synchronous and
        // async (port-checked) connection paths, regardless of which connect
        // action started the session.
        if let Some(ref callback) = *self.on_session_created.borrow() {
            callback(session_id, connection_id);
        }

        self.notify_tab_added(session_id, connection_id);

        session_id
    }

    // ========================================================================
    // VNC Tab Creation
    // ========================================================================

    /// Creates a new VNC session tab
    pub fn create_vnc_session_tab(&self, connection_id: Uuid, title: &str) -> Uuid {
        self.create_vnc_session_tab_with_host(connection_id, title, "")
    }

    /// Creates a new VNC session tab with host information
    pub fn create_vnc_session_tab_with_host(
        &self,
        connection_id: Uuid,
        title: &str,
        host: &str,
    ) -> Uuid {
        let session_id = Uuid::new_v4();
        self.remove_welcome_page();

        let vnc_widget = Rc::new(VncSessionWidget::new());

        // #197: suspend single-Ctrl accelerators while the viewer has focus.
        self.attach_focus_passthrough(vnc_widget.widget());

        let container = GtkBox::new(Orientation::Vertical, 0);
        container.set_hexpand(true);
        container.set_vexpand(true);
        container.append(vnc_widget.widget());

        let tab_container = TabPageContainer::single(&container);
        let page = self.tab_view.append(tab_container.widget());
        page.set_title(title);
        page.set_icon(Some(&gio::ThemedIcon::new(
            "video-joined-displays-symbolic",
        )));
        // The host is stored on the session below, so a tab rebuilt after a
        // detach or a rename produces this very tooltip again.
        let session_host = (!host.is_empty()).then(|| host.to_owned());
        page.set_tooltip(&Self::tab_tooltip(title, session_host.as_deref(), None));

        self.sessions.borrow_mut().insert(session_id, page.clone());
        // Register the container so split (switch_tab_to_split) and unsplit /
        // close-pane (reparent_terminal_to_tab) can swap this tab's content.
        self.tab_containers
            .borrow_mut()
            .insert(session_id, tab_container);
        self.session_widgets
            .borrow_mut()
            .insert(session_id, SessionWidgetStorage::Vnc(vnc_widget));

        self.session_info.borrow_mut().insert(
            session_id,
            TerminalSession {
                id: session_id,
                connection_id,
                name: title.to_string(),
                protocol: "vnc".to_string(),
                is_embedded: true,
                host: session_host,
                log_file: None,
                history_entry_id: None,
                tab_group: None,
                tab_color_index: None,
                connected_at: chrono::Utc::now(),
            },
        );

        self.tab_view.set_selected_page(&page);
        // Apply protocol color indicator if enabled
        if *self.color_tabs_by_protocol.borrow() {
            self.apply_protocol_color(session_id, "vnc");
        }
        self.notify_tab_added(session_id, connection_id);
        session_id
    }

    // ========================================================================
    // Embedded Session Tab Creation (RDP, Web, External)
    // ========================================================================

    /// Adds an embedded RDP tab with the EmbeddedRdpWidget
    pub fn add_embedded_rdp_tab(
        &self,
        session_id: Uuid,
        connection_id: Uuid,
        title: &str,
        widget: Rc<EmbeddedRdpWidget>,
    ) {
        self.remove_welcome_page();

        // #197: suspend single-Ctrl accelerators while the viewer has focus.
        self.attach_focus_passthrough(widget.widget());

        // Wrap in ToastOverlay for file DnD notifications
        let toast_overlay = libadwaita::ToastOverlay::new();
        toast_overlay.set_child(Some(widget.widget()));
        toast_overlay.set_hexpand(true);
        toast_overlay.set_vexpand(true);
        widget.set_toast_overlay(toast_overlay.clone());

        let container = GtkBox::new(Orientation::Vertical, 0);
        container.set_hexpand(true);
        container.set_vexpand(true);
        container.append(&toast_overlay);

        let tab_container = TabPageContainer::single(&container);
        let page = self.tab_view.append(tab_container.widget());
        page.set_title(title);
        page.set_icon(Some(&gio::ThemedIcon::new("computer-symbolic")));
        page.set_tooltip(title);

        self.sessions.borrow_mut().insert(session_id, page.clone());
        // Register the container so split (switch_tab_to_split) and unsplit /
        // close-pane (reparent_terminal_to_tab) can swap this tab's content.
        self.tab_containers
            .borrow_mut()
            .insert(session_id, tab_container);
        self.session_widgets
            .borrow_mut()
            .insert(session_id, SessionWidgetStorage::EmbeddedRdp(widget));

        self.session_info.borrow_mut().insert(
            session_id,
            TerminalSession {
                id: session_id,
                connection_id,
                name: title.to_string(),
                protocol: "rdp".to_string(),
                is_embedded: true,
                host: None,
                log_file: None,
                history_entry_id: None,
                tab_group: None,
                tab_color_index: None,
                connected_at: chrono::Utc::now(),
            },
        );

        self.tab_view.set_selected_page(&page);
        // Apply protocol color indicator if enabled
        if *self.color_tabs_by_protocol.borrow() {
            self.apply_protocol_color(session_id, "rdp");
        }
        self.notify_tab_added(session_id, connection_id);
    }

    /// Adds an embedded Web browser tab with the `EmbeddedWebWidget`.
    ///
    /// Creates a new tab page, stores the widget as
    /// `SessionWidgetStorage::EmbeddedWeb`, and selects the page.
    #[cfg(feature = "web-embedded")]
    pub fn add_embedded_web_tab(
        &self,
        session_id: Uuid,
        connection_id: Uuid,
        title: &str,
        widget: Rc<crate::embedded_web::EmbeddedWebWidget>,
    ) {
        self.remove_welcome_page();

        // #197: suspend single-Ctrl accelerators while the viewer has focus.
        self.attach_focus_passthrough(widget.widget());

        let container = GtkBox::new(Orientation::Vertical, 0);
        container.set_hexpand(true);
        container.set_vexpand(true);
        container.append(widget.widget());

        let tab_container = TabPageContainer::single(&container);
        let page = self.tab_view.append(tab_container.widget());
        page.set_title(title);
        page.set_icon(Some(&gio::ThemedIcon::new("web-browser-symbolic")));
        page.set_tooltip(title);

        self.sessions.borrow_mut().insert(session_id, page.clone());
        self.tab_containers
            .borrow_mut()
            .insert(session_id, tab_container);
        self.session_widgets
            .borrow_mut()
            .insert(session_id, SessionWidgetStorage::EmbeddedWeb(widget));

        self.session_info.borrow_mut().insert(
            session_id,
            TerminalSession {
                id: session_id,
                connection_id,
                name: title.to_string(),
                protocol: "web".to_string(),
                is_embedded: true,
                host: None,
                log_file: None,
                history_entry_id: None,
                tab_group: None,
                tab_color_index: None,
                connected_at: chrono::Utc::now(),
            },
        );

        self.tab_view.set_selected_page(&page);
        // Apply protocol color indicator if enabled
        if *self.color_tabs_by_protocol.borrow() {
            self.apply_protocol_color(session_id, "web");
        }
        self.notify_tab_added(session_id, connection_id);
    }

    /// Adds an embedded session tab (for RDP/VNC external processes)
    pub fn add_embedded_session_tab(
        &self,
        session_id: Uuid,
        connection_id: Uuid,
        title: &str,
        protocol: &str,
        widget: &GtkBox,
        process: Option<Rc<RefCell<Option<std::process::Child>>>>,
    ) {
        self.remove_welcome_page();

        let tab_container = TabPageContainer::single(widget);
        let page = self.tab_view.append(tab_container.widget());
        page.set_title(title);
        page.set_icon(Some(&gio::ThemedIcon::new(Self::get_protocol_icon(
            protocol,
        ))));
        page.set_tooltip(title);

        self.sessions.borrow_mut().insert(session_id, page.clone());

        // Store external process for cleanup on tab close
        if let Some(proc) = process {
            self.session_widgets
                .borrow_mut()
                .insert(session_id, SessionWidgetStorage::ExternalProcess(proc));
        }

        self.session_info.borrow_mut().insert(
            session_id,
            TerminalSession {
                id: session_id,
                connection_id,
                name: title.to_string(),
                protocol: protocol.to_string(),
                is_embedded: false,
                host: None,
                log_file: None,
                history_entry_id: None,
                tab_group: None,
                tab_color_index: None,
                connected_at: chrono::Utc::now(),
            },
        );

        self.tab_view.set_selected_page(&page);
        // Apply protocol color indicator if enabled
        if *self.color_tabs_by_protocol.borrow() {
            self.apply_protocol_color(session_id, protocol);
        }
        self.notify_tab_added(session_id, connection_id);
    }
}
