//! Split view window actions (split, unsplit, resize, navigate panes)
//!
//! Extracted from `window/mod.rs` to reduce module complexity.

use super::*;

/// Reports whether a "Select Tab" placement must be refused, and says why.
///
/// Asked **before** the session's widget is resolved and reparented: once
/// `move_session_to_panel` has run, a refusal can only report corruption instead
/// of preventing it. Both pickers already filter these sessions out, so this
/// guards any other route into the commit callback — a detached session above all
/// (issue #236).
fn refuses_split_placement(
    notebook: &SharedNotebook,
    session_id: Uuid,
    orientation: &'static str,
) -> bool {
    if notebook.may_place_in_split(session_id) {
        return false;
    }
    tracing::warn!(
        session = %session_id,
        orientation,
        detached = notebook.is_detached(session_id),
        "Select Tab refused: session cannot be placed in a split"
    );
    true
}

/// Wires the callbacks behind a split layout's panel buttons and context menu.
///
/// Both entry points need the same "focus this pane first, then let the window
/// action work on it" behaviour, so they are wired together: `win.close-pane`
/// for the × button and "Close Connection", `win.pop-pane-to-tab` for the
/// "Remove from Split" button and menu item (issue #252). Focusing is what makes
/// an action target the panel the user pointed at — the panel's own focus
/// gesture declines clicks that land on a button.
pub(super) fn wire_panel_action_callbacks(bridge: &Rc<SplitViewBridge>) {
    let focus_pane = {
        let bridge = Rc::clone(bridge);
        move |pane_uuid: Uuid| {
            bridge.set_focused_pane(Some(pane_uuid));

            // Update focus styling via the adapter
            if let Some(panel_id) = bridge.get_panel_id_for_uuid(pane_uuid)
                && let Err(e) = bridge.adapter_set_focus(panel_id)
            {
                tracing::warn!("Failed to set focus on panel: {}", e);
            }
        }
    };
    bridge.setup_close_panel_callback(focus_pane.clone());
    bridge.setup_pop_panel_callback(focus_pane);
}

/// The window handles needed to take a split layout apart.
///
/// Grouped into one struct because three actions dismantle splits —
/// `win.unsplit`, `win.pop-pane-to-tab` and `win.close-pane` — and each would
/// otherwise capture the same five clones individually in its `'static` closure.
struct SplitTeardown {
    notebook: SharedNotebook,
    bridges: SessionSplitBridges,
    monitoring: Rc<MonitoringCoordinator>,
    /// The window-global split view, hidden whenever a per-tab split collapses.
    global_split_view: SharedSplitView,
    /// The window-global split container, hidden alongside `global_split_view`.
    split_container: gtk4::Box,
}

impl SplitTeardown {
    /// Looks up the split layout of the active tab, if it has one.
    ///
    /// Returns the owning session id together with a *cloned* `Rc` so no borrow
    /// on `bridges` is held: [`Self::collapse`] takes `borrow_mut` on the same
    /// map, and returning a session to its tab re-enters the notebook, whose
    /// `close-page` handler borrows it too.
    fn active_split(&self) -> Option<(Uuid, Rc<SplitViewBridge>)> {
        let owner = self.notebook.get_active_session_id()?;
        let bridge = self.bridges.borrow().get(&owner).cloned()?;
        Some((owner, bridge))
    }

    /// Returns the sessions currently shown in the layout's panes.
    fn displayed(bridge: &SplitViewBridge) -> Vec<Uuid> {
        bridge
            .pane_ids()
            .iter()
            .filter_map(|&pane_id| bridge.get_pane_session(pane_id))
            .collect()
    }

    /// Returns a session that was living in a split panel to its own tab.
    ///
    /// The live widget is moved, never rebuilt: `reparent_terminal_to_tab`
    /// recreates the standalone tab a split guest gave up when it was parked and
    /// re-parents the same instance into it, so the PTY, the child process and an
    /// embedded viewer's protocol connection all survive. The split colour
    /// indicator is dropped and monitoring, suspended when the session entered
    /// the split, resumes against the *new* container.
    fn return_to_tab(&self, session_id: Uuid) {
        self.notebook.clear_tab_split_color(session_id);
        self.notebook.reparent_terminal_to_tab(session_id);
        if self.monitoring.is_suspended(session_id)
            && let Some(container) = self.notebook.get_session_container(session_id)
        {
            self.monitoring.resume_monitoring(session_id, &container);
        }
    }

    /// Dismantles a split layout, returning every session in it to its own tab.
    ///
    /// `departing` names a session the caller handles itself — the pane being
    /// popped out, or the one `win.close-pane` is about to terminate — so it is
    /// skipped here instead of being processed twice.
    ///
    /// The owner goes last: its `switch_to_single` clears the tab container that
    /// holds the bridge widget, so the guests come out of a tree that is still
    /// attached.
    fn collapse(&self, owner: Uuid, bridge: &Rc<SplitViewBridge>, departing: Option<Uuid>) {
        let mut returning: Vec<Uuid> = Self::displayed(bridge)
            .into_iter()
            .filter(|&sid| sid != owner && Some(sid) != departing)
            .collect();
        // The owner is normally in a pane, but its pane may already have been
        // cleared; it always needs its content rebuilt regardless, because the
        // bridge widget is what its tab currently shows.
        if Some(owner) != departing {
            returning.push(owner);
        }

        tracing::debug!(
            owner = %owner,
            ?departing,
            ?returning,
            "collapsing split layout"
        );

        // Broadcast dies with the layout: the per-terminal `commit` handlers stay
        // connected for the life of each session and read this flag, so leaving
        // it set would keep mirroring into a split that no longer exists.
        bridge.broadcast_active.set(false);

        // Clear the "wired" set so a session entering a *different* split later
        // is properly re-wired for the new bridge. The old handler stays
        // connected but becomes inert (is_session_displayed returns false).
        bridge.broadcast_wired_sessions.borrow_mut().clear();

        for session_id in &returning {
            self.return_to_tab(*session_id);
        }

        bridge.widget().set_visible(false);
        self.global_split_view.widget().set_visible(false);
        self.split_container.set_visible(false);
        self.notebook.show_tab_view_content();

        // Drop the bridge entry of every participant. `get_or_create_session_bridge`
        // reuses whatever it finds, so a stale entry would make the next split on
        // any of these sessions reuse this hidden, half-wired layout.
        let mut bridges = self.bridges.borrow_mut();
        bridges.remove(&owner);
        for session_id in &returning {
            bridges.remove(session_id);
        }
        if let Some(departing) = departing {
            bridges.remove(&departing);
        }
    }

    /// Reports whether a layout has stopped being a split after a pane removal.
    ///
    /// One pane holding one session is an ordinary tab, so it collapses rather
    /// than leaving the user with a single-pane "split".
    fn is_spent(bridge: &SplitViewBridge, removed_last_panel: bool) -> bool {
        let remaining = Self::displayed(bridge);
        removed_last_panel
            || bridge.is_empty()
            || (bridge.pane_count() == 1 && remaining.len() == 1)
    }
}

impl MainWindow {
    /// Clones the handles the split-teardown actions need.
    ///
    /// Called once per action so each `'static` closure owns its own set.
    fn split_teardown(&self) -> SplitTeardown {
        SplitTeardown {
            notebook: self.terminal_notebook.clone(),
            bridges: self.session_split_bridges.clone(),
            monitoring: Rc::clone(&self.monitoring),
            global_split_view: self.split_view.clone(),
            split_container: self.split_container.clone(),
        }
    }

    pub(crate) fn setup_split_view_actions(&self, window: &adw::ApplicationWindow) {
        // Helper function to get or create a split bridge for a session
        // Each tab maintains its own independent split layout
        // A session gets its own bridge when it initiates a split.
        // If the session is already displayed in another bridge, we still create
        // a new bridge for it (the session will be moved to the new bridge).
        fn get_or_create_session_bridge(
            session_id: Uuid,
            session_split_bridges: &SessionSplitBridges,
            color_pool: &SharedColorPool,
        ) -> Rc<SplitViewBridge> {
            let mut bridges = session_split_bridges.borrow_mut();
            // Check if this session already owns a bridge
            if let Some(bridge) = bridges.get(&session_id) {
                // Session already has its own bridge - use it
                tracing::debug!(
                    "get_or_create_session_bridge: REUSING existing bridge for session {:?}, \
                     pool_ptr={:p}, pool_allocated={}",
                    session_id,
                    &*color_pool.borrow(),
                    color_pool.borrow().allocated_count()
                );
                bridge.clone()
            } else {
                // Create a new bridge for this session with the shared color pool
                // This ensures different split containers get different colors
                tracing::debug!(
                    "get_or_create_session_bridge: CREATING new bridge for session {:?}, \
                     pool_ptr={:p}, pool_allocated={}",
                    session_id,
                    &*color_pool.borrow(),
                    color_pool.borrow().allocated_count()
                );
                let new_bridge = Rc::new(SplitViewBridge::with_color_pool(Rc::clone(color_pool)));
                bridges.insert(session_id, new_bridge.clone());
                new_bridge
            }
        }

        // Helper closure that refreshes the broadcast toggle button state.
        // Cloned into every callback that may change a split's session count
        // (split, unsplit, close pane, move to panel) so the toggle accurately
        // reflects the active tab's split layout.
        //
        // It also drives a one-shot discoverability toast: the very first
        // time a split has ≥2 active panels in this app session we tell the
        // user that broadcast is now an option. The flag lives on
        // `MainWindow::broadcast_hint_shown` and is not persisted.
        let refresh_broadcast: Rc<dyn Fn()> = {
            let window_weak = window.downgrade();
            let notebook = self.terminal_notebook.clone();
            let bridges = self.session_split_bridges.clone();
            let toggle = self.broadcast_toggle.clone();
            let toast_overlay = self.toast_overlay.clone();
            let hint_shown = self.broadcast_hint_shown.clone();
            Rc::new(move || {
                if let Some(win) = window_weak.upgrade() {
                    super::navigation_actions::refresh_broadcast_toggle(
                        &win, &notebook, &bridges, &toggle,
                    );

                    // First-time hint: only when the toggle just became
                    // visible (i.e. a split with ≥2 active panels exists)
                    // and we haven't shown the hint yet this session.
                    if !hint_shown.get() && toggle.is_visible() {
                        hint_shown.set(true);
                        let hint = crate::i18n::i18n(
                            "Tip: press Ctrl+Shift+B or use the Broadcast button to mirror keystrokes across split panels",
                        );
                        toast_overlay
                            .widget()
                            .add_toast(adw::Toast::builder().title(&hint).timeout(6).build());
                    }
                }
            })
        };

        // Split horizontal action
        let split_horizontal_action = gio::SimpleAction::new("split-horizontal", None);
        let session_bridges = self.session_split_bridges.clone();
        let notebook_for_split_h = self.terminal_notebook.clone();
        let split_container_h = self.split_container.clone();
        let global_split_view_h = self.split_view.clone();
        let color_pool_h = self.global_color_pool.clone();
        let window_weak_h = window.downgrade();
        let monitoring_h = self.monitoring.clone();
        let refresh_broadcast_h = refresh_broadcast.clone();
        split_horizontal_action.connect_activate(move |_, _| {
            // Get current active session before splitting
            let Some(current_session) = notebook_for_split_h.get_active_session_id() else {
                return; // No active session to split
            };

            // Gate split on the session's eligibility rather than a hardcoded
            // protocol allowlist: VTE terminals and in-process embedded viewers
            // (RDP/VNC/SPICE) are Embeddable; external-process viewers are declined.
            match notebook_for_split_h.split_eligibility(current_session) {
                crate::terminal::SplitEligibility::Embeddable => {}
                crate::terminal::SplitEligibility::ExternalViewer => {
                    tracing::debug!(
                        "split-horizontal: session {:?} uses an external viewer, declining split",
                        current_session
                    );
                    if let Some(win) = window_weak_h.upgrade() {
                        crate::toast::show_toast_on_window(
                            &win,
                            &crate::i18n::i18n("Split view is not available for external-viewer sessions. Switch this connection to embedded mode to use split."),
                            crate::toast::ToastType::Warning,
                        );
                    }
                    return;
                }
                crate::terminal::SplitEligibility::None => return,
            }

            tracing::debug!("split-horizontal: splitting session {:?}", current_session);

            // Get or create a split bridge for this session (with shared color pool)
            let split_view =
                get_or_create_session_bridge(current_session, &session_bridges, &color_pool_h);

            // Wire the content provider so the bridge can place any session's
            // display widget (VTE terminal or embedded RDP/VNC/SPICE viewer)
            // through the uniform content-widget path.
            let notebook_for_content_provider_h = notebook_for_split_h.clone();
            split_view.set_content_provider(Rc::new(move |sid| {
                notebook_for_content_provider_h.get_session_display_widget(sid)
            }));

            // Check if this is the first split (bridge has only 1 panel)
            // If bridge already has multiple panels, we don't need to show the current session
            // because restore_panel_contents() already restored all terminals
            let is_first_split = split_view.pane_count() == 1;

            // Clone for close callback
            let sv_for_close = split_view.clone();
            if let Some((new_pane_id, new_color_index, original_color_index)) = split_view
                .split_with_close_callback(SplitDirection::Horizontal, move || {
                    let _ = sv_for_close.close_pane();
                })
            {
                tracing::debug!(
                    "split-horizontal: session {:?} got original_color={}, new_color={}, \
                     is_first_split={}",
                    current_session,
                    original_color_index,
                    new_color_index,
                    is_first_split
                );

                let notebook = notebook_for_split_h.clone();
                let notebook_for_drop = notebook_for_split_h.clone();
                let sv_for_click = split_view.clone();

                // Per spec: Split transforms current tab into Container Tab
                // Only show current session in the original pane if this is the FIRST split
                // For subsequent splits, restore_panel_contents() already restored all terminals
                if is_first_split {
                    // Ensure session is registered in split_view
                    if let Some(info) = notebook_for_split_h.get_session_info(current_session) {
                        split_view.add_session(info);
                    }
                    // Show in the focused (original) pane
                    let _ = split_view.show_session(current_session);

                    // Use the original pane's color (properly allocated during split)
                    split_view.set_session_color(current_session, original_color_index);
                    notebook_for_split_h.set_tab_split_color(current_session, original_color_index);
                    tracing::debug!(
                        "split-horizontal: applied color {} to tab for session {:?}",
                        original_color_index,
                        current_session
                    );

                    // Suspend monitoring — bar is not visible in split view
                    monitoring_h.suspend_monitoring(current_session);
                }

                // Place split view widget inside the TabPage via TabPageContainer
                split_view.widget().set_vexpand(true);
                split_view.widget().set_hexpand(true);
                notebook_for_split_h.switch_tab_to_split(current_session, split_view.widget());

                // Also hide global split view (we're using per-tab now)
                global_split_view_h.widget().set_visible(false);
                split_container_h.set_visible(false);

                // Setup drop target for the new (empty) pane
                let sv_for_drop = split_view.clone();
                split_view.setup_pane_drop_target_with_callbacks(
                    new_pane_id,
                    move |session_id| {
                        let info = notebook.get_session_info(session_id)?;
                        let terminal = notebook.get_terminal(session_id);
                        Some((info, terminal))
                    },
                    move |session_id, color_index| {
                        // Store session color in split_view for tracking
                        sv_for_drop.set_session_color(session_id, color_index);
                        // Set tab color indicator when session is dropped into pane
                        notebook_for_drop.set_tab_split_color(session_id, color_index);
                    },
                );

                // Setup click handlers for ALL panes (both original and new)
                // This ensures focus rectangle moves correctly when clicking any pane
                let sv_for_focus = sv_for_click.clone();
                let sv_for_session = sv_for_click.clone();
                let sv_for_terminal = sv_for_click.clone();
                sv_for_click.setup_all_panel_click_handlers(move |clicked_pane_uuid| {
                    // Update the bridge's focused pane state (handles all focus styling)
                    sv_for_focus.set_focused_pane(Some(clicked_pane_uuid));
                    // Get session_id from the clicked pane via adapter
                    let session_to_focus = sv_for_session.get_pane_session(clicked_pane_uuid);
                    // Grab focus on the terminal in the clicked pane.
                    // Do NOT call switch_to_tab() here — the split widget lives on the
                    // split-owner's TabPage. Switching to another session's tab would
                    // navigate away from the split widget and make the content disappear.
                    if let Some(session_id) = session_to_focus
                        && let Some(widget) = sv_for_terminal.content_widget(session_id)
                    {
                        widget.grab_focus();
                    }
                });

                // Setup select tab callback for this per-session bridge
                let split_view_for_select = split_view.clone();
                let notebook_for_select = notebook_for_split_h.clone();
                let notebook_for_provider = notebook_for_split_h.clone();
                let notebook_for_terminal = notebook_for_split_h.clone();
                let notebook_for_placeholder_h = notebook_for_split_h.clone();
                // Clone session_bridges so we can register the new session in the map
                let session_bridges_for_select = session_bridges.clone();
                // Clone for clearing from previous split
                let session_bridges_for_clear = session_bridges.clone();
                // Clone for provider closure
                let split_view_for_provider = split_view.clone();
                let monitoring_for_select_h = monitoring_h.clone();
                let split_colors_h = Rc::clone(notebook_for_split_h.split_colors());
                // Refresh broadcast toggle once a new session is placed via Select Tab —
                // until this point the bridge has 1 active session and the toggle is hidden.
                let refresh_broadcast_select_h = refresh_broadcast_h.clone();
                // Notebook clone for wiring broadcast on the freshly-placed session
                // (needed because the wired commit handler also calls send_text_to_session).
                let notebook_for_broadcast_h = notebook_for_split_h.clone();
                split_view.setup_select_tab_callback_with_provider(
                    move || {
                        // Get all sessions from the notebook, excluding those already in THIS split.
                        // Include VTE terminals and in-process embedded viewers (Embeddable);
                        // external-process viewers are excluded via eligibility (R4.3).
                        notebook_for_provider
                            .get_all_sessions()
                            .into_iter()
                            .filter(|s| {
                                matches!(
                                    notebook_for_provider.split_eligibility(s.id),
                                    crate::terminal::SplitEligibility::Embeddable
                                )
                            })
                            // A detached session's widget lives in its own
                            // window; offering it here would rip it out and
                            // leave an empty window behind (issue #236). Same
                            // predicate the commit callback refuses with.
                            .filter(|s| notebook_for_provider.may_place_in_split(s.id))
                            .map(|s| (s.id, s.name, s.protocol))
                            .filter(|(id, _, _)| !split_view_for_provider.is_session_displayed(*id))
                            .collect()
                    },
                    move |panel_uuid, session_id| {
                        tracing::debug!(
                            "Select Tab callback (horizontal): moving session {} to panel {}",
                            session_id,
                            panel_uuid
                        );

                        // Refuse before anything moves — see
                        // `refuses_split_placement`.
                        if refuses_split_placement(&notebook_for_terminal, session_id, "horizontal")
                        {
                            return;
                        }

                        // First, clear this session from any previous split view
                        {
                            let bridges = session_bridges_for_clear.borrow();
                            for (other_session_id, other_bridge) in bridges.iter() {
                                // Skip if this is the same bridge we're adding to
                                if Rc::ptr_eq(other_bridge, &split_view_for_select) {
                                    continue;
                                }
                                // Check if this session is displayed in another bridge
                                if other_bridge.is_session_displayed(session_id) {
                                    tracing::debug!(
                                        "Select Tab callback (horizontal): clearing session {} \
                                         from previous split (owner: {})",
                                        session_id,
                                        other_session_id
                                    );
                                    other_bridge.clear_session_from_panes(session_id);
                                    // Clear the old tab color
                                    notebook_for_select.clear_tab_split_color(session_id);
                                    break;
                                }
                            }
                        }

                        // Resolve the session's display widget from the notebook
                        // (terminal or embedded viewer) — not from the bridge's
                        // internal map.
                        let Some(content) =
                            notebook_for_terminal.get_session_display_widget(session_id)
                        else {
                            tracing::warn!(
                                "Select Tab callback (horizontal): no content widget for session {}",
                                session_id
                            );
                            return;
                        };

                        // Move the session to the panel with its content widget.
                        // This returns the color index on success
                        match split_view_for_select
                            .move_session_to_panel(panel_uuid, session_id, &content)
                        {
                            Ok(color_index) => {
                                // Register this session in session_split_bridges
                                session_bridges_for_select
                                    .borrow_mut()
                                    .insert(session_id, split_view_for_select.clone());

                                // Set tab color indicator using the color from the panel
                                notebook_for_select.set_tab_split_color(session_id, color_index);

                                // Remove the moved session's standalone tab — it
                                // now lives in this split, so a placeholder tab
                                // would only clutter the tab bar and Tab Overview.
                                notebook_for_placeholder_h.park_session_tab(session_id);

                                // Suspend monitoring — session is now in split view
                                monitoring_for_select_h.suspend_monitoring(session_id);

                                tracing::debug!(
                                    "Select Tab callback (horizontal): moved session {} to panel {} with color {}",
                                    session_id,
                                    panel_uuid,
                                    color_index
                                );

                                // Layout changed — the bridge now has ≥2 active panels,
                                // so the broadcast toggle should appear in the header bar.
                                refresh_broadcast_select_h();

                                // If broadcast is already on, the freshly-placed session
                                // must be wired immediately — otherwise it would silently
                                // miss keystroke mirroring until broadcast is toggled off
                                // and back on.
                                if split_view_for_select.broadcast_active.get() {
                                    super::navigation_actions::wire_broadcast_for_session(
                                        &split_view_for_select,
                                        &notebook_for_broadcast_h,
                                        session_id,
                                    );
                                }
                            }
                            Err(e) => {
                                tracing::warn!("Failed to move session to panel: {}", e);
                            }
                        }

                        // Note: Do NOT call switch_to_tab() here - the terminal should be
                        // displayed in the split panel, not switched to as the active tab
                    },
                    split_colors_h,
                );

                // Wire the panel buttons and panel context menu
                wire_panel_action_callbacks(&split_view);
            }

            // Layout changed — refresh the broadcast toggle so it reflects the
            // new session count of the active tab's split.
            refresh_broadcast_h();
        });
        window.add_action(&split_horizontal_action);

        // Split vertical action
        let split_vertical_action = gio::SimpleAction::new("split-vertical", None);
        let session_bridges_v = self.session_split_bridges.clone();
        let notebook_for_split_v = self.terminal_notebook.clone();
        let split_container_v = self.split_container.clone();
        let global_split_view_v = self.split_view.clone();
        let color_pool_v = self.global_color_pool.clone();
        let window_weak_v = window.downgrade();
        let monitoring_v = self.monitoring.clone();
        let refresh_broadcast_v = refresh_broadcast.clone();
        split_vertical_action.connect_activate(move |_, _| {
            // Get current active session before splitting
            let Some(current_session) = notebook_for_split_v.get_active_session_id() else {
                return; // No active session to split
            };

            // Gate split on the session's eligibility rather than a hardcoded
            // protocol allowlist: VTE terminals and in-process embedded viewers
            // (RDP/VNC/SPICE) are Embeddable; external-process viewers are declined.
            match notebook_for_split_v.split_eligibility(current_session) {
                crate::terminal::SplitEligibility::Embeddable => {}
                crate::terminal::SplitEligibility::ExternalViewer => {
                    tracing::debug!(
                        "split-vertical: session {:?} uses an external viewer, declining split",
                        current_session
                    );
                    if let Some(win) = window_weak_v.upgrade() {
                        crate::toast::show_toast_on_window(
                            &win,
                            &crate::i18n::i18n("Split view is not available for external-viewer sessions. Switch this connection to embedded mode to use split."),
                            crate::toast::ToastType::Warning,
                        );
                    }
                    return;
                }
                crate::terminal::SplitEligibility::None => return,
            }

            tracing::debug!("split-vertical: splitting session {:?}", current_session);

            // Get or create a split bridge for this session (with shared color pool)
            let split_view =
                get_or_create_session_bridge(current_session, &session_bridges_v, &color_pool_v);

            // Wire the content provider so the bridge can place any session's
            // display widget (VTE terminal or embedded RDP/VNC/SPICE viewer)
            // through the uniform content-widget path.
            let notebook_for_content_provider_v = notebook_for_split_v.clone();
            split_view.set_content_provider(Rc::new(move |sid| {
                notebook_for_content_provider_v.get_session_display_widget(sid)
            }));

            // Check if this is the first split (bridge has only 1 panel)
            // If bridge already has multiple panels, we don't need to show the current session
            // because restore_panel_contents() already restored all terminals
            let is_first_split = split_view.pane_count() == 1;

            // Clone for close callback
            let sv_for_close = split_view.clone();
            if let Some((new_pane_id, new_color_index, original_color_index)) = split_view
                .split_with_close_callback(SplitDirection::Vertical, move || {
                    let _ = sv_for_close.close_pane();
                })
            {
                tracing::debug!(
                    "split-vertical: session {:?} got original_color={}, new_color={}, \
                     is_first_split={}",
                    current_session,
                    original_color_index,
                    new_color_index,
                    is_first_split
                );

                let notebook = notebook_for_split_v.clone();
                let notebook_for_drop = notebook_for_split_v.clone();
                let sv_for_click = split_view.clone();

                // Per spec: Split transforms current tab into Container Tab
                // Only show current session in the original pane if this is the FIRST split
                // For subsequent splits, restore_panel_contents() already restored all terminals
                if is_first_split {
                    // Ensure session is registered in split_view
                    if let Some(info) = notebook_for_split_v.get_session_info(current_session) {
                        split_view.add_session(info);
                    }
                    // Show in the focused (original) pane
                    let _ = split_view.show_session(current_session);

                    // Use the original pane's color (properly allocated during split)
                    split_view.set_session_color(current_session, original_color_index);
                    notebook_for_split_v.set_tab_split_color(current_session, original_color_index);
                    tracing::debug!(
                        "split-vertical: applied color {} to tab for session {:?}",
                        original_color_index,
                        current_session
                    );

                    // Suspend monitoring — bar is not visible in split view
                    monitoring_v.suspend_monitoring(current_session);
                }

                // Place split view widget inside the TabPage via TabPageContainer
                split_view.widget().set_vexpand(true);
                split_view.widget().set_hexpand(true);
                notebook_for_split_v.switch_tab_to_split(current_session, split_view.widget());

                // Also hide global split view (we're using per-tab now)
                global_split_view_v.widget().set_visible(false);
                split_container_v.set_visible(false);

                // Setup drop target for the new (empty) pane
                let sv_for_drop = split_view.clone();
                split_view.setup_pane_drop_target_with_callbacks(
                    new_pane_id,
                    move |session_id| {
                        let info = notebook.get_session_info(session_id)?;
                        let terminal = notebook.get_terminal(session_id);
                        Some((info, terminal))
                    },
                    move |session_id, color_index| {
                        // Store session color in split_view for tracking
                        sv_for_drop.set_session_color(session_id, color_index);
                        // Set tab color indicator when session is dropped into pane
                        notebook_for_drop.set_tab_split_color(session_id, color_index);
                    },
                );

                // Setup click handlers for ALL panes (both original and new)
                // This ensures focus rectangle moves correctly when clicking any pane
                let sv_for_focus = sv_for_click.clone();
                let sv_for_session = sv_for_click.clone();
                let sv_for_terminal = sv_for_click.clone();
                sv_for_click.setup_all_panel_click_handlers(move |clicked_pane_uuid| {
                    // Update the bridge's focused pane state (handles all focus styling)
                    sv_for_focus.set_focused_pane(Some(clicked_pane_uuid));
                    // Get session_id from the clicked pane via adapter
                    let session_to_focus = sv_for_session.get_pane_session(clicked_pane_uuid);
                    // Grab focus on the terminal in the clicked pane.
                    // Do NOT call switch_to_tab() here — the split widget lives on the
                    // split-owner's TabPage. Switching to another session's tab would
                    // navigate away from the split widget and make the content disappear.
                    if let Some(session_id) = session_to_focus
                        && let Some(widget) = sv_for_terminal.content_widget(session_id)
                    {
                        widget.grab_focus();
                    }
                });

                // Setup select tab callback for this per-session bridge
                let split_view_for_select = split_view.clone();
                let notebook_for_select = notebook_for_split_v.clone();
                let notebook_for_provider = notebook_for_split_v.clone();
                let notebook_for_terminal = notebook_for_split_v.clone();
                // Clone session_bridges so we can register the new session in the map
                let session_bridges_for_select = session_bridges_v.clone();
                // Clone for clearing from previous split
                let session_bridges_for_clear = session_bridges_v.clone();
                // Clone for provider closure
                let split_view_for_provider = split_view.clone();
                let monitoring_for_select_v = monitoring_v.clone();
                let split_colors_v = Rc::clone(notebook_for_split_v.split_colors());
                let notebook_for_placeholder_v = notebook_for_split_v.clone();
                // Refresh broadcast toggle once a new session is placed via Select Tab —
                // until this point the bridge has 1 active session and the toggle is hidden.
                let refresh_broadcast_select_v = refresh_broadcast_v.clone();
                // Notebook clone for wiring broadcast on the freshly-placed session
                // (needed because the wired commit handler also calls send_text_to_session).
                let notebook_for_broadcast_v = notebook_for_split_v.clone();
                split_view.setup_select_tab_callback_with_provider(
                    move || {
                        // Get all sessions from the notebook, excluding those already in THIS split.
                        // Include VTE terminals and in-process embedded viewers (Embeddable);
                        // external-process viewers are excluded via eligibility (R4.3).
                        notebook_for_provider
                            .get_all_sessions()
                            .into_iter()
                            .filter(|s| {
                                matches!(
                                    notebook_for_provider.split_eligibility(s.id),
                                    crate::terminal::SplitEligibility::Embeddable
                                )
                            })
                            // A detached session's widget lives in its own
                            // window; offering it here would rip it out and
                            // leave an empty window behind (issue #236). Same
                            // predicate the commit callback refuses with.
                            .filter(|s| notebook_for_provider.may_place_in_split(s.id))
                            .map(|s| (s.id, s.name, s.protocol))
                            .filter(|(id, _, _)| !split_view_for_provider.is_session_displayed(*id))
                            .collect()
                    },
                    move |panel_uuid, session_id| {
                        tracing::debug!(
                            "Select Tab callback (vertical): moving session {} to panel {}",
                            session_id,
                            panel_uuid
                        );

                        // Refuse before anything moves — see
                        // `refuses_split_placement`.
                        if refuses_split_placement(&notebook_for_terminal, session_id, "vertical") {
                            return;
                        }

                        // First, clear this session from any previous split view
                        {
                            let bridges = session_bridges_for_clear.borrow();
                            for (other_session_id, other_bridge) in bridges.iter() {
                                // Skip if this is the same bridge we're adding to
                                if Rc::ptr_eq(other_bridge, &split_view_for_select) {
                                    continue;
                                }
                                // Check if this session is displayed in another bridge
                                if other_bridge.is_session_displayed(session_id) {
                                    tracing::debug!(
                                        "Select Tab callback (vertical): clearing session {} \
                                         from previous split (owner: {})",
                                        session_id,
                                        other_session_id
                                    );
                                    other_bridge.clear_session_from_panes(session_id);
                                    // Clear the old tab color
                                    notebook_for_select.clear_tab_split_color(session_id);
                                    break;
                                }
                            }
                        }

                        // Resolve the session's display widget from the notebook
                        // (terminal or embedded viewer) — not from the bridge's
                        // internal map.
                        let Some(content) =
                            notebook_for_terminal.get_session_display_widget(session_id)
                        else {
                            tracing::warn!(
                                "Select Tab callback (vertical): no content widget for session {}",
                                session_id
                            );
                            return;
                        };

                        // Move the session to the panel with its content widget.
                        // This returns the color index on success
                        match split_view_for_select
                            .move_session_to_panel(panel_uuid, session_id, &content)
                        {
                            Ok(color_index) => {
                                // Register this session in session_split_bridges
                                session_bridges_for_select
                                    .borrow_mut()
                                    .insert(session_id, split_view_for_select.clone());

                                // Set tab color indicator using the color from the panel
                                notebook_for_select.set_tab_split_color(session_id, color_index);

                                // Remove the moved session's standalone tab — it
                                // now lives in this split, so a placeholder tab
                                // would only clutter the tab bar and Tab Overview.
                                notebook_for_placeholder_v.park_session_tab(session_id);

                                // Suspend monitoring — session is now in split view
                                monitoring_for_select_v.suspend_monitoring(session_id);

                                tracing::debug!(
                                    "Select Tab callback (vertical): moved session {} to panel {} with color {}",
                                    session_id,
                                    panel_uuid,
                                    color_index
                                );

                                // Layout changed — the bridge now has ≥2 active panels,
                                // so the broadcast toggle should appear in the header bar.
                                refresh_broadcast_select_v();

                                // If broadcast is already on, the freshly-placed session
                                // must be wired immediately — otherwise it would silently
                                // miss keystroke mirroring until broadcast is toggled off
                                // and back on.
                                if split_view_for_select.broadcast_active.get() {
                                    super::navigation_actions::wire_broadcast_for_session(
                                        &split_view_for_select,
                                        &notebook_for_broadcast_v,
                                        session_id,
                                    );
                                }
                            }
                            Err(e) => {
                                tracing::warn!("Failed to move session to panel: {}", e);
                            }
                        }

                        // Note: Do NOT call switch_to_tab() here - the terminal should be
                        // displayed in the split panel, not switched to as the active tab
                    },
                    split_colors_v,
                );

                // Wire the panel buttons and panel context menu
                wire_panel_action_callbacks(&split_view);
            }

            // Layout changed — refresh broadcast toggle.
            refresh_broadcast_v();
        });
        window.add_action(&split_vertical_action);

        // Close pane action
        let close_pane_action = gio::SimpleAction::new("close-pane", None);
        let teardown_close = self.split_teardown();
        let refresh_broadcast_close = refresh_broadcast.clone();
        close_pane_action.connect_activate(move |_, _| {
            if let Some((owner, bridge)) = teardown_close.active_split() {
                // The session in the focused pane — the one about to be closed.
                let departing = bridge.get_focused_session();

                tracing::debug!(
                    owner = %owner,
                    ?departing,
                    pane_count_before = bridge.pane_count(),
                    "close-pane: closing focused pane"
                );

                match bridge.close_pane() {
                    Ok(removed_last_panel) => {
                        if let Some(session_id) = departing {
                            teardown_close.notebook.clear_tab_split_color(session_id);
                        }

                        if SplitTeardown::is_spent(&bridge, removed_last_panel) {
                            teardown_close.collapse(owner, &bridge, departing);
                        } else {
                            // Panels remain — the rebuild emptied them, so put the
                            // surviving sessions' widgets back.
                            bridge.restore_panel_contents();
                        }

                        // Terminate the session whose pane was just closed: a split
                        // guest has no standalone tab to fall back to, so closing its
                        // pane closes the session. `win.pop-pane-to-tab` is the
                        // variant that keeps it alive (issue #252).
                        if let Some(session_id) = departing {
                            teardown_close.notebook.close_session(session_id);
                        }
                    }
                    Err(e) => {
                        tracing::warn!("Failed to close pane: {}", e);
                    }
                }
            }

            // Layout changed — refresh broadcast toggle.
            refresh_broadcast_close();
        });
        window.add_action(&close_pane_action);

        // Remove the focused pane's session from the split without closing it:
        // the session goes back to its own tab with the connection intact
        // (issue #252).
        let pop_pane_action = gio::SimpleAction::new("pop-pane-to-tab", None);
        let teardown_pop = self.split_teardown();
        let refresh_broadcast_pop = refresh_broadcast.clone();
        pop_pane_action.connect_activate(move |_, _| {
            let Some((owner, bridge)) = teardown_pop.active_split() else {
                tracing::debug!("pop-pane-to-tab: the active tab has no split layout");
                return;
            };
            let Some(departing) = bridge.get_focused_session() else {
                tracing::debug!("pop-pane-to-tab: the focused pane holds no session");
                return;
            };

            // The bridge widget lives in the owner's tab container, so the owner
            // cannot leave on its own — the layout would have no host left. The
            // last remaining pane is the same situation. Either way the whole
            // split comes down, which is what "back to a single tab" means.
            if departing == owner || bridge.pane_count() <= 1 {
                teardown_pop.collapse(owner, &bridge, None);
                refresh_broadcast_pop();
                return;
            }

            // `close_pane` only removes the focused panel and its bookkeeping.
            // The session teardown that `win.close-pane` runs afterwards is
            // precisely what this action must skip.
            if let Err(e) = bridge.close_pane() {
                tracing::warn!(error = %e, "pop-pane-to-tab: could not remove the focused pane");
                return;
            }

            teardown_pop.return_to_tab(departing);
            teardown_pop.bridges.borrow_mut().remove(&departing);

            // Allow re-wiring: the old commit handler stays connected but is
            // inert (is_session_displayed returns false). Removing the entry
            // lets wire_broadcast_for_session run again if the session is placed
            // back into a split later.
            bridge
                .broadcast_wired_sessions
                .borrow_mut()
                .remove(&departing);

            if SplitTeardown::is_spent(&bridge, false) {
                teardown_pop.collapse(owner, &bridge, Some(departing));
            } else {
                bridge.restore_panel_contents();
            }

            // Show the session where it now lives, so the move is visible rather
            // than the pane just vanishing.
            teardown_pop.notebook.switch_to_tab(departing);
            tracing::info!(
                session = %departing,
                owner = %owner,
                "session removed from split and returned to its own tab"
            );
            refresh_broadcast_pop();
        });
        window.add_action(&pop_pane_action);

        // Remove the whole split layout, returning every session in it to its own
        // tab. None of them are closed (issue #252).
        let unsplit_action = gio::SimpleAction::new("unsplit", None);
        let teardown_unsplit = self.split_teardown();
        let refresh_broadcast_unsplit = refresh_broadcast.clone();
        unsplit_action.connect_activate(move |_, _| {
            let Some((owner, bridge)) = teardown_unsplit.active_split() else {
                tracing::debug!("unsplit: the active tab has no split layout");
                return;
            };
            if bridge.pane_count() <= 1 {
                tracing::debug!(session = %owner, "unsplit: layout has a single pane");
                return;
            }
            teardown_unsplit.collapse(owner, &bridge, None);
            tracing::info!(owner = %owner, "split layout removed, sessions returned to tabs");
            refresh_broadcast_unsplit();
        });
        window.add_action(&unsplit_action);

        // Focus next pane action
        let focus_next_pane_action = gio::SimpleAction::new("focus-next-pane", None);
        let session_bridges_focus = self.session_split_bridges.clone();
        let notebook_for_focus = self.terminal_notebook.clone();
        focus_next_pane_action.connect_activate(move |_, _| {
            if let Some(session_id) = notebook_for_focus.get_active_session_id() {
                let bridges = session_bridges_focus.borrow();
                if let Some(bridge) = bridges.get(&session_id) {
                    let _ = bridge.focus_next_pane();
                }
            }
        });
        window.add_action(&focus_next_pane_action);
    }
}
