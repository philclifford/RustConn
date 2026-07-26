//! Detach and attach window actions.
//!
//! Registers the main-window half of the feature (`win.detach-session`,
//! `win.detach-session-to-monitor`, `win.attach-session`, `win.toggle-detach`)
//! and the session-scoped half installed on every detached window, so a
//! shortcut pressed there acts on that window's own session and never on the
//! main window's selection.
//!
//! `win.toggle-detach` is deliberately one action name registered on both
//! windows with different handlers: GTK resolves `win.*` against the focused
//! window, so a single accelerator detaches from the main window and attaches
//! from a detached one.
//!
//! Every closure holds `Weak` handles to the notebook, the registry, and the
//! main window, so nothing registered here can keep a session alive past its
//! teardown (Requirement 10.4).

use std::rc::Weak;

use gtk4::gdk;
use rustconn_core::DetachVerdict;

use super::*;
use crate::detached_window::{DetachedSessionWindow, DetachedWindowParams, DetachedWindowRegistry};
use crate::i18n::i18n;
use crate::toast::ToastType;

/// Variant type of `win.detach-session-to-monitor`: session id plus monitor index.
const DETACH_TO_MONITOR_TYPE: &str = "(su)";

/// Weak handles the detach and attach helpers need.
///
/// The toast overlay is held strongly: it is a plain widget wrapper that owns
/// nothing pointing back here, so it cannot form a cycle.
#[derive(Clone)]
struct DetachHandles {
    notebook: Weak<TerminalNotebook>,
    registry: Weak<DetachedWindowRegistry>,
    main_window: glib::WeakRef<adw::ApplicationWindow>,
    toasts: SharedToastOverlay,
}

/// The upgraded counterparts of [`DetachHandles`].
struct DetachTargets {
    notebook: Rc<TerminalNotebook>,
    registry: Rc<DetachedWindowRegistry>,
    main_window: adw::ApplicationWindow,
}

impl DetachHandles {
    /// Upgrades every handle, or reports `None` once any of them is gone.
    fn upgrade(&self) -> Option<DetachTargets> {
        Some(DetachTargets {
            notebook: self.notebook.upgrade()?,
            registry: self.registry.upgrade()?,
            main_window: self.main_window.upgrade()?,
        })
    }
}

/// Returns the translated explanation for a verdict that blocks a detach.
///
/// Mirrors `DetachVerdict::reason_key()` one-to-one — `detach-already-detached`,
/// `detach-external-viewer`, `detach-split-owner`, `detach-split-guest` — while
/// `detach-allowed` stays unexplained, because nothing is shown for it. Matched
/// on the enum so a future verdict cannot silently fall into a generic message.
/// Sentence case, per the project rules for explanations.
fn verdict_explanation(verdict: DetachVerdict) -> Option<String> {
    match verdict {
        DetachVerdict::Allowed => None,
        DetachVerdict::AlreadyDetached => Some(i18n("This session already has its own window.")),
        DetachVerdict::ExternalViewer => Some(i18n(
            "This session is displayed by a separate viewer, which already runs in its own window.",
        )),
        DetachVerdict::SplitOwner => Some(i18n(
            "Remove the split layout from this tab before moving the session to its own window.",
        )),
        DetachVerdict::SplitGuest => Some(i18n(
            "This session is displayed in a split layout. Return it to its own tab first.",
        )),
    }
}

/// Parses a session id from an action parameter.
fn session_id_from(text: &str) -> Option<Uuid> {
    match Uuid::parse_str(text) {
        Ok(session_id) => Some(session_id),
        Err(error) => {
            tracing::warn!(%error, text, "detach action received an invalid session id");
            None
        }
    }
}

/// Returns the monitor at `index` in the default display's monitor list.
///
/// A monitor that disappeared between building the menu and activating it
/// yields `None`, and the caller falls back to a normal window.
fn monitor_at(index: u32) -> Option<gdk::Monitor> {
    let monitor = gdk::Display::default()?
        .monitors()
        .item(index)
        .and_downcast::<gdk::Monitor>();
    if monitor.is_none() {
        tracing::debug!(index, "requested monitor is no longer connected");
    }
    monitor
}

/// Shows a high-priority error toast inside a detached window.
fn show_error_in_window(window: &DetachedSessionWindow, message: &str) {
    let toast = adw::Toast::new(message);
    toast.set_priority(adw::ToastPriority::High);
    window.toast_overlay().add_toast(toast);
}

/// Moves a session out of its tab into a new detached window.
///
/// A rejected verdict is explained as a toast and changes nothing. Every other
/// failure leaves the session in its tab, because `take_session_content` rolls
/// itself back (Requirement 1.8).
fn detach_session(handles: &DetachHandles, session_id: Uuid, monitor: Option<u32>) {
    let Some(targets) = handles.upgrade() else {
        return;
    };

    let verdict = targets.notebook.detach_verdict(session_id);
    if let Some(explanation) = verdict_explanation(verdict) {
        tracing::info!(
            session = %session_id,
            reason = verdict.reason_key(),
            "detach rejected"
        );
        handles
            .toasts
            .show_toast_with_type(&explanation, ToastType::Warning);
        return;
    }

    let Some(info) = targets.notebook.get_session_info(session_id) else {
        tracing::warn!(session = %session_id, "detach failed: no session metadata");
        handles
            .toasts
            .show_error(&i18n("Could not move the session to its own window."));
        return;
    };
    // Resolved before the content is taken, so a missing application cannot
    // leave the session without a home.
    let Some(app) = targets
        .main_window
        .application()
        .and_downcast::<adw::Application>()
    else {
        tracing::warn!(session = %session_id, "detach failed: no application on the main window");
        handles
            .toasts
            .show_error(&i18n("Could not move the session to its own window."));
        return;
    };

    let Some(content) = targets.notebook.take_session_content(session_id) else {
        handles
            .toasts
            .show_error(&i18n("Could not move the session to its own window."));
        return;
    };

    let params = DetachedWindowParams {
        session_id,
        connection_id: info.connection_id,
        title: &info.name,
        protocol: &info.protocol.to_uppercase(),
    };
    let detached = DetachedSessionWindow::new(&app, &params, &content);
    install_detached_window_actions(&detached, handles);
    wire_detached_window_callbacks(&detached, handles);

    match monitor.and_then(monitor_at) {
        Some(ref monitor) => detached.present_fullscreen_on(monitor),
        None => detached.present(),
    }
    targets.registry.insert(detached);
}

/// Moves a detached session back into a tab of the main window.
///
/// On failure the session keeps running in its detached window and the error is
/// reported there (Requirement 2.7).
fn attach_session(handles: &DetachHandles, session_id: Uuid) {
    let Some(targets) = handles.upgrade() else {
        return;
    };
    if !targets.notebook.is_detached(session_id) {
        tracing::debug!(session = %session_id, "attach ignored: session is not detached");
        return;
    }

    if !targets.notebook.attach_session(session_id) {
        let message = i18n("Could not move the session back to the main window.");
        let shown = targets
            .registry
            .with_window(session_id, |window| {
                show_error_in_window(window, &message);
            })
            .is_some();
        if !shown {
            handles.toasts.show_error(&message);
        }
        return;
    }

    if let Some(window) = targets.registry.take(session_id) {
        // Marked first, so the close handler treats this as a move rather than
        // as the user ending the session.
        window.begin_attach();
        window.close();
    }
    targets.main_window.present();
}

/// Ends a detached session and closes its window.
///
/// `close_session` runs the standard tab-close teardown, so the window must not
/// run it a second time: it is marked as "closing without teardown" first.
fn close_detached_session(handles: &DetachHandles, session_id: Uuid) {
    let Some(targets) = handles.upgrade() else {
        return;
    };
    let window = targets.registry.take(session_id);
    targets.notebook.close_session(session_id);
    if let Some(window) = window {
        window.begin_attach();
        window.close();
    }
}

/// Drops a session's detached window on idle, without a second teardown.
///
/// Deferred to idle so a window is never closed from inside its own close
/// handler, and idempotent: whichever path reaches the registry first takes the
/// entry, the others find nothing. `begin_attach` marks the close as a move so
/// the window's close handler does not run `close_session` again.
fn close_detached_window_later(registry: &Weak<DetachedWindowRegistry>, session_id: Uuid) {
    let registry = registry.clone();
    glib::idle_add_local_once(move || {
        if let Some(registry) = registry.upgrade()
            && let Some(window) = registry.take(session_id)
        {
            window.begin_attach();
            window.close();
            tracing::debug!(session = %session_id, "detached window dropped after session end");
        }
    });
}

/// Wires the attach button and the close handler of a fresh detached window.
fn wire_detached_window_callbacks(detached: &DetachedSessionWindow, handles: &DetachHandles) {
    let handles_attach = handles.clone();
    detached.set_on_attach(move |session_id| {
        attach_session(&handles_attach, session_id);
    });

    // The user closing the window ends the session, exactly as closing its tab
    // would. The registry entry goes away on idle, so it is not removed from
    // inside the window's own close handler.
    let notebook = handles.notebook.clone();
    let registry = handles.registry.clone();
    detached.set_on_close(move |session_id| {
        if let Some(notebook) = notebook.upgrade() {
            notebook.close_session(session_id);
        }
        close_detached_window_later(&registry, session_id);
    });
}

/// Installs the session-scoped actions of a detached window.
///
/// Every handler targets `detached.session_id()`, so a shortcut pressed here
/// never reaches the main window's selected tab (Requirements 5.2, 5.3).
fn install_detached_window_actions(detached: &DetachedSessionWindow, handles: &DetachHandles) {
    let window = detached.window().clone();
    let session_id = detached.session_id();

    // Same accelerator as detach in the main window, opposite direction.
    let toggle_action = gio::SimpleAction::new("toggle-detach", None);
    let handles_toggle = handles.clone();
    toggle_action.connect_activate(move |_, _| {
        attach_session(&handles_toggle, session_id);
    });
    window.add_action(&toggle_action);

    // Scoped to this window's session: a parameter naming another session is
    // ignored, so a stray target cannot act on a different window.
    let attach_action = gio::SimpleAction::new("attach-session", Some(glib::VariantTy::STRING));
    let handles_attach = handles.clone();
    attach_action.connect_activate(move |_, param| {
        if let Some(requested) = param
            .and_then(glib::Variant::get::<String>)
            .as_deref()
            .and_then(session_id_from)
            && requested != session_id
        {
            tracing::debug!(
                session = %session_id,
                requested = %requested,
                "detached window ignored an attach request for another session"
            );
            return;
        }
        attach_session(&handles_attach, session_id);
    });
    window.add_action(&attach_action);

    let copy_action = gio::SimpleAction::new("copy", None);
    let notebook_copy = handles.notebook.clone();
    copy_action.connect_activate(move |_, _| {
        // Embedded viewers keep their own copy handling on the widget, which
        // travelled into this window with the session.
        if let Some(notebook) = notebook_copy.upgrade()
            && let Some(terminal) = notebook.get_terminal(session_id)
            && let Some(text) = terminal.text_selected(vte4::Format::Text)
        {
            terminal.display().clipboard().set_text(&text);
        }
    });
    window.add_action(&copy_action);

    let paste_action = gio::SimpleAction::new("paste", None);
    let notebook_paste = handles.notebook.clone();
    paste_action.connect_activate(move |_, _| {
        if let Some(notebook) = notebook_paste.upgrade()
            && let Some(terminal) = notebook.get_terminal(session_id)
        {
            terminal.paste_clipboard();
        }
    });
    window.add_action(&paste_action);

    let search_action = gio::SimpleAction::new("terminal-search", None);
    let notebook_search = handles.notebook.clone();
    let window_search = window.downgrade();
    search_action.connect_activate(move |_, _| {
        if let Some(notebook) = notebook_search.upgrade()
            && let Some(parent) = window_search.upgrade()
            && let Some(terminal) = notebook.get_terminal(session_id)
        {
            let dialog =
                crate::dialogs::TerminalSearchDialog::new(Some(&parent.clone().upcast()), terminal);
            dialog.show();
        }
    });
    window.add_action(&search_action);

    // Ctrl+W in a detached window ends only this session.
    let close_action = gio::SimpleAction::new("close-tab", None);
    let handles_close = handles.clone();
    close_action.connect_activate(move |_, _| {
        close_detached_session(&handles_close, session_id);
    });
    window.add_action(&close_action);

    // Stateful, like the main window's, so the checkmark semantics stay
    // identical if the action is ever surfaced in a menu.
    let fullscreen_action =
        gio::SimpleAction::new_stateful("toggle-fullscreen", None, &false.to_variant());
    let window_fullscreen = window.downgrade();
    fullscreen_action.connect_activate(move |action, _| {
        if let Some(window) = window_fullscreen.upgrade() {
            let is_fullscreen = window.is_fullscreen();
            if is_fullscreen {
                window.unfullscreen();
            } else {
                window.fullscreen();
            }
            action.set_state(&(!is_fullscreen).to_variant());
        }
    });
    window.add_action(&fullscreen_action);

    // Passthrough is application-wide (it drops accelerators from the whole
    // application), so this forwards to the main window's stateful action
    // instead of keeping a second copy of the state and the indicator.
    let passthrough_action = gio::SimpleAction::new("toggle-passthrough", None);
    let main_window = handles.main_window.clone();
    passthrough_action.connect_activate(move |_, _| {
        if let Some(main_window) = main_window.upgrade() {
            let _ = WidgetExt::activate_action(&main_window, "win.toggle-passthrough", None);
        }
    });
    window.add_action(&passthrough_action);
}

impl MainWindow {
    /// Registers the main-window detach and attach actions and the notebook hooks.
    pub(crate) fn setup_detach_actions(&self, window: &adw::ApplicationWindow) {
        let handles = DetachHandles {
            notebook: Rc::downgrade(&self.terminal_notebook),
            registry: Rc::downgrade(&self.detached_windows),
            main_window: window.downgrade(),
            toasts: Rc::clone(&self.toast_overlay),
        };

        let detach_action = gio::SimpleAction::new("detach-session", Some(glib::VariantTy::STRING));
        let handles_detach = handles.clone();
        detach_action.connect_activate(move |_, param| {
            if let Some(session_id) = param
                .and_then(glib::Variant::get::<String>)
                .as_deref()
                .and_then(session_id_from)
            {
                detach_session(&handles_detach, session_id, None);
            }
        });
        window.add_action(&detach_action);

        match glib::VariantTy::new(DETACH_TO_MONITOR_TYPE) {
            Ok(variant_type) => {
                let monitor_action =
                    gio::SimpleAction::new("detach-session-to-monitor", Some(variant_type));
                let handles_monitor = handles.clone();
                monitor_action.connect_activate(move |_, param| {
                    if let Some((session, monitor)) =
                        param.and_then(glib::Variant::get::<(String, u32)>)
                        && let Some(session_id) = session_id_from(&session)
                    {
                        detach_session(&handles_monitor, session_id, Some(monitor));
                    }
                });
                window.add_action(&monitor_action);
            }
            Err(error) => {
                tracing::error!(%error, "could not register win.detach-session-to-monitor");
            }
        }

        let attach_action = gio::SimpleAction::new("attach-session", Some(glib::VariantTy::STRING));
        let handles_attach = handles.clone();
        attach_action.connect_activate(move |_, param| {
            if let Some(session_id) = param
                .and_then(glib::Variant::get::<String>)
                .as_deref()
                .and_then(session_id_from)
            {
                attach_session(&handles_attach, session_id);
            }
        });
        window.add_action(&attach_action);

        // Same accelerator as attach in a detached window; GTK routes it to
        // whichever window has focus.
        let toggle_action = gio::SimpleAction::new("toggle-detach", None);
        let handles_toggle = handles.clone();
        toggle_action.connect_activate(move |_, _| {
            let Some(notebook) = handles_toggle.notebook.upgrade() else {
                return;
            };
            let Some(session_id) = notebook.get_active_session_id() else {
                tracing::debug!("toggle-detach ignored: no active session");
                return;
            };
            detach_session(&handles_toggle, session_id, None);
        });
        window.add_action(&toggle_action);

        // Focusing a detached session presents its window instead of selecting a
        // tab, so sidebar activation, the session manager, and workspace restore
        // need no changes of their own.
        let registry_focus = handles.registry.clone();
        self.terminal_notebook
            .set_on_focus_detached(move |session_id| {
                if let Some(registry) = registry_focus.upgrade()
                    && !registry.present(session_id)
                {
                    tracing::warn!(
                        session = %session_id,
                        "detached session has no window to present"
                    );
                }
            });

        // A detached session can also end without the window being involved: a
        // remote disconnect, its child exiting, or a terminate from the session
        // manager all tear it down inside the notebook. Closing the window from
        // here is what keeps no empty detached window behind (Requirement 6.5).
        let registry_ended = handles.registry.clone();
        self.terminal_notebook
            .set_on_session_ended(move |session_id| {
                close_detached_window_later(&registry_ended, session_id);
            });

        let handles_request = handles;
        self.terminal_notebook
            .set_on_detach_request(move |session_id, monitor| {
                detach_session(&handles_request, session_id, monitor);
            });
    }
}

#[cfg(test)]
mod tests {
    use rustconn_core::DetachVerdict;

    use super::{DETACH_TO_MONITOR_TYPE, session_id_from, verdict_explanation};

    #[test]
    fn every_blocking_verdict_has_an_explanation() {
        for verdict in [
            DetachVerdict::AlreadyDetached,
            DetachVerdict::ExternalViewer,
            DetachVerdict::SplitOwner,
            DetachVerdict::SplitGuest,
        ] {
            let explanation = verdict_explanation(verdict)
                .unwrap_or_else(|| panic!("{} needs an explanation", verdict.reason_key()));
            assert!(!explanation.is_empty());
        }
    }

    #[test]
    fn an_allowed_verdict_explains_nothing() {
        assert!(verdict_explanation(DetachVerdict::Allowed).is_none());
    }

    #[test]
    fn session_ids_round_trip_and_garbage_is_rejected() {
        let id = uuid::Uuid::new_v4();
        assert_eq!(session_id_from(&id.to_string()), Some(id));
        assert!(session_id_from("not-a-uuid").is_none());
        assert!(session_id_from("").is_none());
    }

    #[test]
    fn monitor_action_type_is_a_valid_variant_type() {
        assert!(gtk4::glib::VariantTy::new(DETACH_TO_MONITOR_TYPE).is_ok());
    }
}
