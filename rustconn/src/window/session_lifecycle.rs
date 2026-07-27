//! Session lifecycle management: logging, activity monitoring, and child-exit handling.
//!
//! Extracted from `window/mod.rs` — sets up session logging, activity
//! monitoring with notifications, and child-exited cleanup/reconnect logic.

use super::*;

impl MainWindow {
    /// Sets up session logging for a terminal session
    ///
    /// Directory creation and file opening are performed asynchronously
    /// to avoid blocking the GTK main thread on slow storage.
    pub fn setup_session_logging(
        state: &SharedAppState,
        notebook: &SharedNotebook,
        session_id: Uuid,
        connection_id: Uuid,
        connection_name: &str,
    ) {
        // Get the log directory and logging modes from settings
        let (log_dir, log_activity, log_input, log_output) =
            if let Ok(state_ref) = state.try_borrow() {
                let settings = state_ref.settings();
                let dir = if settings.logging.log_directory.is_absolute() {
                    settings.logging.log_directory.clone()
                } else {
                    state_ref
                        .config_manager()
                        .config_dir()
                        .join(&settings.logging.log_directory)
                };
                (
                    dir,
                    settings.logging.log_activity,
                    settings.logging.log_input,
                    settings.logging.log_output,
                )
            } else {
                return;
            };

        // Create log file path with timestamp
        let timestamp = chrono::Local::now().format("%Y-%m-%d_%H-%M-%S");
        let sanitized_name: String = connection_name
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .take(64)
            .collect();
        let log_filename = format!("{}_{}.log", sanitized_name, timestamp);
        let log_path = log_dir.join(&log_filename);

        // Clone data for the background thread (must be owned/static)
        let connection_name_for_header = connection_name.to_string();
        let connection_name_for_callback = connection_name.to_string();
        let log_dir_clone = log_dir.clone();
        let log_path_clone = log_path.clone();

        // Clone notebook for the callback
        let notebook_clone = notebook.clone();

        // Perform directory creation and file opening in background thread
        crate::utils::spawn_blocking_with_callback(
            move || {
                // Ensure log directory exists
                if let Err(e) = std::fs::create_dir_all(&log_dir_clone) {
                    return Err(format!(
                        "Failed to create log directory '{}': {}",
                        log_dir_clone.display(),
                        e
                    ));
                }

                // Create the log file and write header
                match std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&log_path_clone)
                {
                    Ok(mut file) => {
                        use std::io::Write;
                        let header = format!(
                            "=== Session Log ===\nConnection: {}\nConnection ID: {}\nSession ID: {}\nStarted: {}\n\n",
                            connection_name_for_header,
                            connection_id,
                            session_id,
                            chrono::Local::now().format("%Y-%m-%d %H:%M:%S")
                        );
                        if let Err(e) = file.write_all(header.as_bytes()) {
                            return Err(format!("Failed to write log header: {}", e));
                        }
                        Ok(log_path_clone)
                    }
                    Err(e) => Err(format!(
                        "Failed to create log file '{}': {}",
                        log_path_clone.display(),
                        e
                    )),
                }
            },
            move |result: Result<std::path::PathBuf, String>| {
                match result {
                    Ok(log_path) => {
                        tracing::info!(
                            connection_name = %connection_name_for_callback,
                            log_path = %log_path.display(),
                            "Session logging enabled"
                        );

                        // Store log file path in session info
                        notebook_clone.set_log_file(session_id, log_path.clone());

                        // Set up logging handlers based on settings
                        Self::setup_logging_handlers(
                            &notebook_clone,
                            session_id,
                            &log_path,
                            log_activity,
                            log_input,
                            log_output,
                        );
                    }
                    Err(e) => {
                        tracing::error!(%e, "Session logging setup failed");
                    }
                }
            },
        );
    }

    /// Resolves the effective activity-monitor settings for a connection.
    ///
    /// Returns `(mode, quiet_period_secs, silence_timeout_secs, connection_name)`
    /// from the per-connection override layered over the global defaults.
    ///
    /// Returns `None` if the application state is currently borrowed or the
    /// connection no longer exists.
    fn resolve_activity_config(
        state: &SharedAppState,
        connection_id: Uuid,
    ) -> Option<(
        rustconn_core::activity_monitor::MonitorMode,
        u32,
        u32,
        String,
    )> {
        let state_ref = state.try_borrow().ok()?;
        let defaults = &state_ref.settings().activity_monitor;
        let conn = state_ref.get_connection(connection_id)?;
        let name = conn.name.clone();
        Some(if let Some(ref config) = conn.activity_monitor_config {
            (
                config.effective_mode(defaults),
                config.effective_quiet_period(defaults),
                config.effective_silence_timeout(defaults),
                name,
            )
        } else {
            (
                defaults.mode,
                defaults.effective_quiet_period(),
                defaults.effective_silence_timeout(),
                name,
            )
        })
    }

    /// Re-registers activity monitoring for a session after an in-place reconnect.
    ///
    /// In-place reconnect reuses the existing VTE terminal widget, so the
    /// `contents_changed` / `child_exited` handlers wired in
    /// [`Self::setup_activity_monitoring`] persist. Only the coordinator's
    /// session entry needs recreating — it was removed when the previous child
    /// process exited. Re-wiring the handlers here would double them.
    pub(crate) fn reactivate_activity_monitoring(
        state: &SharedAppState,
        activity: &types::SharedActivityCoordinator,
        connection_id: Uuid,
        session_id: Uuid,
    ) {
        let Some((mode, quiet, silence, _name)) =
            Self::resolve_activity_config(state, connection_id)
        else {
            return;
        };
        activity.start(session_id, mode, quiet, silence);
        tracing::debug!(
            %session_id,
            %connection_id,
            ?mode,
            "Activity monitoring re-armed after in-place reconnect"
        );
    }

    /// Sets up activity monitoring for a terminal session.
    ///
    /// Called once per session from the notebook's `on_session_created` hook,
    /// so it covers every terminal protocol (SSH, Telnet, serial, Kubernetes,
    /// Mosh, Zero Trust) and both synchronous and async connection paths.
    ///
    /// Resolves per-connection or global defaults, registers the session with
    /// the coordinator (even when the mode is Off, so the per-tab menu can
    /// enable it later), connects VTE `contents_changed` to `on_output()`, and
    /// delivers notifications (tab indicator, toast, desktop notification).
    /// Also wires `connect_child_exited` for cleanup.
    pub(crate) fn setup_activity_monitoring(
        state: &SharedAppState,
        notebook: &SharedNotebook,
        activity: &types::SharedActivityCoordinator,
        session_id: Uuid,
        connection_id: Uuid,
    ) {
        // Resolve effective config from per-connection override + global defaults
        let Some((mode, quiet, silence, conn_name)) =
            Self::resolve_activity_config(state, connection_id)
        else {
            return;
        };

        // Always register the session with the coordinator, even when the mode
        // is Off. This lets the per-tab "Monitor" menu cycle the mode on a live
        // session (Off → Activity → Silence) without reconnecting (issue #180).
        // `start` only arms the silence timer for Silence mode, and `on_output`
        // is a no-op for Off, so Off mode carries no meaningful runtime cost.
        activity.start(session_id, mode, quiet, silence);

        // Set up silence callback for timer-based silence notifications.
        // The callback is global to the coordinator, so it resolves the session
        // name dynamically rather than capturing a single connection's name
        // (otherwise every session would report the most recently wired name).
        let notebook_for_silence = notebook.clone();
        let sessions_for_silence = notebook.sessions_map();
        activity.set_silence_callback(move |sid, ntype| {
            let name = notebook_for_silence
                .get_session_info(sid)
                .map_or_else(String::new, |info| info.name);
            Self::deliver_activity_notification(
                &notebook_for_silence,
                &sessions_for_silence,
                sid,
                ntype,
                &name,
            );
        });

        // 4.2: Connect contents_changed to on_output() for activity detection
        let activity_for_output = Rc::clone(activity);
        let notebook_for_output = notebook.clone();
        let sessions_for_output = notebook.sessions_map();
        let conn_name_for_output = conn_name.clone();
        notebook.connect_contents_changed(session_id, move || {
            if let Some(ntype) = activity_for_output.on_output(session_id) {
                Self::deliver_activity_notification(
                    &notebook_for_output,
                    &sessions_for_output,
                    session_id,
                    ntype,
                    &conn_name_for_output,
                );
            }
        });

        // 4.7: On child exit, stop the coordinator to clean up timers
        let activity_for_exit = Rc::clone(activity);
        notebook.connect_child_exited(session_id, move |_exit_status| {
            activity_for_exit.stop(session_id);
        });

        tracing::debug!(
            %session_id,
            %connection_id,
            ?mode,
            quiet_period = quiet,
            silence_timeout = silence,
            "Activity monitoring started"
        );
    }

    /// Delivers an activity/silence notification through all channels:
    /// tab indicator icon, toast, and desktop notification (when unfocused).
    fn deliver_activity_notification(
        notebook: &SharedNotebook,
        sessions: &Rc<RefCell<std::collections::HashMap<Uuid, adw::TabPage>>>,
        session_id: Uuid,
        ntype: crate::activity_coordinator::NotificationType,
        session_name: &str,
    ) {
        use crate::activity_coordinator::NotificationType;
        use crate::i18n::i18n_f;

        let (icon_name, toast_msg) = match ntype {
            NotificationType::Activity => (
                "dialog-information-symbolic",
                i18n_f("Activity detected: {}", &[session_name]),
            ),
            NotificationType::Silence => (
                "dialog-warning-symbolic",
                i18n_f("Silence detected: {}", &[session_name]),
            ),
        };

        // 4.3: Set tab indicator icon
        if let Some(page) = sessions.borrow().get(&session_id) {
            page.set_indicator_icon(Some(&gio::ThemedIcon::new(icon_name)));
        }

        // 4.4: Show toast via existing ToastOverlay — on the window the session
        // actually lives in, which is its own one while it is detached
        // (Requirement 5.4, issue #236).
        let host_window: Option<gtk4::Window> =
            super::detach_actions::detached_host_window(session_id)
                .map(gtk4::prelude::Cast::upcast)
                .or_else(|| notebook.widget().root().and_downcast::<gtk4::Window>());
        if let Some(ref window) = host_window {
            let toast_type = match ntype {
                NotificationType::Activity => crate::toast::ToastType::Info,
                NotificationType::Silence => crate::toast::ToastType::Warning,
            };
            crate::toast::show_toast_on_window(window, &toast_msg, toast_type);

            // 4.5: Send desktop notification when window is unfocused
            if !window.is_active()
                && let Some(app) = window.application()
            {
                let notification = gio::Notification::new(&toast_msg);
                notification.set_icon(&gio::ThemedIcon::new(icon_name));
                app.send_notification(Some(&format!("activity-{session_id}")), &notification);
            }
        }
    }

    /// Sets up the child exited handler for session cleanup
    pub fn setup_child_exited_handler(
        state: &SharedAppState,
        notebook: &SharedNotebook,
        sidebar: &SharedSidebar,
        session_id: Uuid,
        connection_id: Uuid,
    ) {
        let state_clone = state.clone();
        let sidebar_clone = sidebar.clone();
        let notebook_clone = notebook.clone();
        let connection_id_str = connection_id.to_string();

        // Capture post-disconnect task and connection info before entering the closure
        let post_disconnect_conn = state
            .try_borrow()
            .ok()
            .and_then(|s| s.get_connection(connection_id).cloned());
        let post_disconnect_task = post_disconnect_conn
            .as_ref()
            .and_then(|c| c.post_disconnect_task.clone());

        let post_disconnect_variables = state
            .try_borrow()
            .ok()
            .map(|s| crate::state::resolve_global_variables(s.settings()))
            .unwrap_or_default();
        let post_disconnect_folder_id = post_disconnect_conn.as_ref().and_then(|c| c.group_id);

        let post_disconnect_folder_tracker = state
            .try_borrow()
            .ok()
            .map(|s| Arc::clone(s.folder_tracker()))
            .unwrap_or_default();

        // A Custom Command (Generic Zero Trust) is a one-shot launcher, not a
        // session: RustDesk, WinBox or `xdg-open` return as soon as their own
        // window is up. Leaving a dead terminal with a reconnect banner behind
        // is pure noise (#151), so such a tab always closes on a clean exit —
        // a failure still keeps the tab so its output stays readable.
        let is_custom_command = post_disconnect_conn.as_ref().is_some_and(|c| {
            matches!(
                &c.protocol_config,
                rustconn_core::ProtocolConfig::ZeroTrust(zt)
                    if matches!(zt.provider, rustconn_core::models::ZeroTrustProvider::Generic)
            )
        });

        // Capture close-on-clean-exit setting before entering the closure
        let close_on_clean_exit = is_custom_command
            || state
                .try_borrow()
                .ok()
                .is_some_and(|s| s.settings().terminal.close_on_clean_exit);

        notebook.connect_child_exited(session_id, move |exit_status| {
            // Execute post-disconnect task if configured
            if let Some(ref task) = post_disconnect_task {
                tracing::info!(
                    %connection_id,
                    command = %task.command,
                    "Executing post-disconnect task"
                );

                let mut var_manager = VariableManager::new();
                for var in &post_disconnect_variables {
                    var_manager.set_global(var.clone());
                }
                // Add connection-scoped synthetic variables (host, port, username, name)
                if let Some(ref conn) = post_disconnect_conn {
                    var_manager.set_connection(
                        connection_id,
                        rustconn_core::Variable::new("host", &conn.host),
                    );
                    var_manager.set_connection(
                        connection_id,
                        rustconn_core::Variable::new("port", conn.port.to_string()),
                    );
                    if let Some(ref user) = conn.username {
                        var_manager.set_connection(
                            connection_id,
                            rustconn_core::Variable::new("username", user),
                        );
                    }
                    var_manager.set_connection(
                        connection_id,
                        rustconn_core::Variable::new("name", &conn.name),
                    );
                }

                let executor =
                    TaskExecutor::with_tracker(Arc::new(var_manager), Arc::clone(&post_disconnect_folder_tracker));

                let result = crate::async_utils::with_runtime(|rt| {
                    rt.block_on(async {
                        // ponytail: 60s ceiling protects the GTK main thread;
                        // the task's own timeout (if configured) fires first.
                        let ceiling = std::time::Duration::from_mins(1);
                        tokio::time::timeout(ceiling, executor.execute_post_disconnect(
                            task,
                            VariableScope::Connection(connection_id),
                            post_disconnect_folder_id,
                        ))
                        .await
                        .unwrap_or(Err(rustconn_core::automation::TaskError::Timeout(60_000)))
                    })
                });

                match result {
                    Ok(Ok(_)) => {
                        tracing::info!(
                            %connection_id,
                            "Post-disconnect task completed successfully"
                        );
                    }
                    Ok(Err(e)) => {
                        tracing::warn!(
                            %connection_id,
                            command = %task.command,
                            error = %e,
                            "Post-disconnect task failed"
                        );
                    }
                    Err(runtime_err) => {
                        tracing::warn!(
                            %connection_id,
                            error = %runtime_err,
                            "Failed to create async runtime for post-disconnect task"
                        );
                    }
                }
            }

            // Get history entry ID before session info is removed
            let history_entry_id = notebook_clone
                .get_session_info(session_id)
                .and_then(|info| info.history_entry_id);

            // Update session status in state manager
            // This also closes the session logger and finalizes the log file
            if let Ok(mut state_mut) = state_clone.try_borrow_mut()
                && let Err(e) = state_mut.terminate_session(session_id) { tracing::warn!(?e, %session_id, "Failed to terminate session"); }

            // Check if session still exists in notebook
            // If it doesn't, the tab was closed by user
            if notebook_clone.get_session_info(session_id).is_none() {
                // Record connection end in history (user closed tab)
                if let Some(entry_id) = history_entry_id
                    && let Ok(mut state_mut) = state_clone.try_borrow_mut() {
                        state_mut.record_connection_end(entry_id);
                    }
                // Decrement session count - status changes only if no other sessions active
                sidebar_clone.decrement_session_count(&connection_id_str, false);
                return;
            }

            // If the app is shutting down, suppress failure handling — the session
            // exits are expected because close_all_control_sockets() kills SSH connections.
            if crate::app::is_shutting_down() {
                sidebar_clone.decrement_session_count(&connection_id_str, false);
                return;
            }

            // Parse waitpid status to determine if exit was a failure or intentional kill
            // WIFSIGNALED: (status & 0x7f) != 0
            // WTERMSIG: status & 0x7f
            // WIFEXITED: (status & 0x7f) == 0
            // WEXITSTATUS: (status >> 8) & 0xff

            let term_sig = exit_status & 0x7f;
            let is_signaled = term_sig != 0;
            let exit_code = (exit_status >> 8) & 0xff;

            // Consider it a failure if:
            // 1. Killed by a signal that isn't a standard termination signal (HUP, INT, KILL, TERM)
            // 2. Exited normally with non-zero code, UNLESS that code indicates a standard signal kill (128+N)
            let is_failure = if is_signaled {
                !matches!(term_sig, 1 | 2 | 9 | 15)
            } else {
                exit_code != 0 && !matches!(exit_code, 129 | 130 | 137 | 143)
            };

            // Record connection end/failure in history
            if let Some(entry_id) = history_entry_id
                && let Ok(mut state_mut) = state_clone.try_borrow_mut() {
                    if is_failure {
                        let error_msg =
                            format!("Exit status: {exit_status} (Signal: {term_sig}, Code: {exit_code})");
                        state_mut.record_connection_failed(entry_id, &error_msg);
                    } else {
                        state_mut.record_connection_end(entry_id);
                    }
                }

            if is_failure {
                tracing::error!(%session_id, exit_status, term_sig, exit_code, "Session exited with failure");
            }

            // Stop recording if active before marking as disconnected
            notebook_clone.stop_recording(session_id);

            // If the session exited cleanly and close-on-clean-exit is enabled,
            // close the tab automatically instead of showing the reconnect overlay.
            if !is_failure && close_on_clean_exit {
                tracing::info!(
                    %session_id,
                    %connection_id,
                    "Session exited cleanly, auto-closing tab (close_on_clean_exit=true)"
                );
                // Defer close_tab to the next idle iteration of the main loop.
                // Closing the VTE widget synchronously from within its own
                // `child-exited` signal can race with a pending GTK snapshot,
                // causing a use-after-free SIGSEGV in libvte/pango (#171).
                let nb = notebook_clone.clone();
                let sb = sidebar_clone.clone();
                let cid = connection_id_str.clone();
                glib::idle_add_local_once(move || {
                    // Guard: if user already closed the tab before idle fires,
                    // the session no longer exists — skip to avoid double
                    // decrement_session_count.
                    if nb.get_session_info(session_id).is_none() {
                        return;
                    }
                    nb.close_tab(session_id);
                    sb.decrement_session_count(&cid, false);
                });
                return;
            }

            // Defer all remaining widget/VTE work (disconnect indicator,
            // terminal.reset(), reconnect banner, auto-reconnect setup) to
            // the next main-loop idle. `child-exited` is emitted from inside
            // VTE — resetting VTE state or mutating the widget tree during
            // the emission can race with the pending GTK snapshot of the
            // current frame and crash in libvte/pango (#171).
            let notebook_clone = notebook_clone.clone();
            let state_clone = state_clone.clone();
            let sidebar_clone = sidebar_clone.clone();
            let connection_id_str = connection_id_str.clone();
            glib::idle_add_local_once(move || {
            // Guard: the tab may have been closed before this idle ran.
            if notebook_clone.get_session_info(session_id).is_none() {
                sidebar_clone.decrement_session_count(&connection_id_str, is_failure);
                return;
            }

            // Mark tab as disconnected and show reconnect overlay
            notebook_clone.mark_tab_disconnected(session_id);
            notebook_clone.show_reconnect_overlay(session_id);

            // Auto-reconnect: poll host and reconnect when it comes back online
            // Only for non-intentional disconnects (failures, not user-initiated)
            // Skip auto-reconnect for SSH authentication failures (exit code 255)
            // because the host is reachable but credentials are wrong — polling
            // would instantly trigger reconnect creating an infinite loop.
            let is_ssh_auth_failure = exit_code == 255
                && notebook_clone
                    .get_session_info(session_id)
                    .is_some_and(|info| info.protocol == "ssh");

            // Skip auto-reconnect for rapid crashes (session lived < 5 seconds).
            // This prevents infinite reconnect loops when the terminal process
            // crashes immediately (e.g., SIGSEGV in VTE on macOS).
            let is_rapid_crash = notebook_clone
                .get_session_info(session_id)
                .is_some_and(|info| {
                    let elapsed = chrono::Utc::now()
                        .signed_duration_since(info.connected_at)
                        .num_seconds();
                    elapsed < 5
                });

            if is_rapid_crash {
                tracing::warn!(
                    %session_id,
                    %connection_id,
                    "Skipping auto-reconnect: session crashed within 5 seconds of start"
                );
            }

            if is_failure
                && !is_ssh_auth_failure
                && !is_rapid_crash
                && let Ok(state_ref) = state_clone.try_borrow()
                && let Some(conn) = state_ref.get_connection(connection_id)
            {
                // Use per-connection retry config or default
                let retry_config = conn.retry_config.clone()
                    .unwrap_or_default();

                // If retry is explicitly disabled, skip auto-reconnect
                if !retry_config.enabled {
                    drop(state_ref);
                    sidebar_clone.decrement_session_count(&connection_id_str, is_failure);
                    return;
                }

                let host = conn.host.clone();
                let port = conn.port;
                drop(state_ref);

                let cancel = std::sync::Arc::new(
                    std::sync::atomic::AtomicBool::new(false),
                );
                // Register cancel token so closing the tab cancels polling
                notebook_clone.register_poll_cancel(session_id, cancel.clone());

                tracing::info!(
                    %connection_id,
                    %host,
                    %port,
                    max_attempts = retry_config.max_attempts,
                    initial_delay_ms = retry_config.initial_delay_ms,
                    "Starting auto-reconnect with exponential backoff"
                );

                // Update the reconnect banner to show auto-reconnect status
                notebook_clone.update_reconnect_banner_status(session_id, true);

                // Channel for attempt progress updates (background → main thread)
                let (attempt_tx, attempt_rx) = std::sync::mpsc::channel::<u32>();
                let notebook_attempt = notebook_clone.clone();
                let max_attempts_for_ui = retry_config.max_attempts;
                gtk4::glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
                    match attempt_rx.try_recv() {
                        Ok(attempt) => {
                            notebook_attempt.update_reconnect_banner_attempt(
                                session_id,
                                attempt,
                                max_attempts_for_ui,
                            );
                            gtk4::glib::ControlFlow::Continue
                        }
                        Err(std::sync::mpsc::TryRecvError::Empty) => {
                            gtk4::glib::ControlFlow::Continue
                        }
                        Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                            gtk4::glib::ControlFlow::Break
                        }
                    }
                });

                // Clone notebook's on_reconnect callback for use in the polling result
                let on_reconnect = notebook_clone.reconnect_callback();
                let notebook_cleanup = notebook_clone.clone();

                crate::utils::spawn_blocking_with_callback(
                    move || {
                        let rt = tokio::runtime::Runtime::new()
                            .map_err(rustconn_core::host_check::HostCheckError::Io)?;
                        rt.block_on(
                            rustconn_core::host_check::poll_until_online_with_backoff(
                                &host,
                                port,
                                &retry_config,
                                &cancel,
                                |attempt, is_online| {
                                    tracing::debug!(
                                        attempt,
                                        is_online,
                                        "Auto-reconnect probe"
                                    );
                                    let _ = attempt_tx.send(attempt);
                                },
                            ),
                        )
                    },
                    move |result| {
                        // Clean up the cancel token
                        notebook_cleanup.cancel_poll(session_id);
                        if matches!(result, Ok(true)) {
                            // Guard: if the user closed the tab while polling
                            // was active, the session no longer exists — skip
                            // reconnect to avoid creating an orphan tab. A
                            // detached session has no tab but is very much
                            // alive, so it counts as existing (issue #236).
                            let session_exists = notebook_cleanup
                                .sessions_map()
                                .borrow()
                                .contains_key(&session_id)
                                || notebook_cleanup.is_detached(session_id);
                            if !session_exists {
                                tracing::debug!(
                                    %session_id,
                                    "Tab closed during polling, skipping reconnect"
                                );
                                return;
                            }
                            tracing::info!(
                                %connection_id,
                                "Host is back online, triggering reconnect"
                            );
                            if let Some(ref cb) = *on_reconnect.borrow() {
                                cb(session_id, connection_id);
                            }
                        }
                    },
                );
            }

            // Decrement session count - status changes only if no other sessions active
            sidebar_clone.decrement_session_count(&connection_id_str, is_failure);
            });
        });
    }

    /// Sets up logging handlers for a terminal session based on settings
    ///
    /// Supports three logging modes:
    /// - Activity: logs change counts (default, lightweight)
    /// - Input: logs user commands sent to terminal
    /// - Output: logs full terminal transcript
    fn setup_logging_handlers(
        notebook: &SharedNotebook,
        session_id: Uuid,
        log_path: &std::path::Path,
        log_activity: bool,
        log_input: bool,
        log_output: bool,
    ) {
        use std::cell::RefCell;
        use std::fs::OpenOptions;
        use std::io::Write;
        use std::rc::Rc;

        // Create a shared writer for the log file
        let log_writer: Rc<RefCell<Option<std::io::BufWriter<std::fs::File>>>> =
            Rc::new(RefCell::new(None));

        // Open the log file for appending
        match OpenOptions::new().append(true).open(log_path) {
            Ok(file) => {
                *log_writer.borrow_mut() = Some(std::io::BufWriter::new(file));
            }
            Err(e) => {
                tracing::error!(
                    %e,
                    log_path = %log_path.display(),
                    "Failed to open log file for session logging"
                );
                return;
            }
        }

        // Set up activity logging (change counts)
        if log_activity {
            let log_writer_clone = log_writer.clone();
            let change_counter: Rc<RefCell<u32>> = Rc::new(RefCell::new(0));
            let last_log_time: Rc<RefCell<std::time::Instant>> =
                Rc::new(RefCell::new(std::time::Instant::now()));
            let flush_counter: Rc<RefCell<u32>> = Rc::new(RefCell::new(0));

            notebook.connect_contents_changed(session_id, move || {
                let mut counter = change_counter.borrow_mut();
                *counter += 1;

                let mut flush_count = flush_counter.borrow_mut();
                *flush_count += 1;

                let now = std::time::Instant::now();
                let elapsed = now.duration_since(*last_log_time.borrow());

                if *counter >= 100 || elapsed.as_secs() >= 5 {
                    if let Ok(mut writer_opt) = log_writer_clone.try_borrow_mut()
                        && let Some(ref mut writer) = *writer_opt
                    {
                        let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
                        let _ = writeln!(
                            writer,
                            "[{}] Terminal activity ({} changes)",
                            timestamp, *counter
                        );

                        if *flush_count >= 10 || elapsed.as_secs() >= 30 {
                            let _ = writer.flush();
                            *flush_count = 0;
                        }
                    }

                    *counter = 0;
                    *last_log_time.borrow_mut() = now;
                }
            });
        }

        // Set up input logging (user commands)
        if log_input {
            let log_writer_clone = log_writer.clone();
            let input_buffer: Rc<RefCell<String>> = Rc::new(RefCell::new(String::new()));
            let last_flush: Rc<RefCell<std::time::Instant>> =
                Rc::new(RefCell::new(std::time::Instant::now()));

            notebook.connect_commit(session_id, move |text| {
                let mut buffer = input_buffer.borrow_mut();

                // Handle special characters
                for ch in text.chars() {
                    match ch {
                        '\r' | '\n' => {
                            // End of command - log it
                            if !buffer.is_empty() {
                                if let Ok(mut writer_opt) = log_writer_clone.try_borrow_mut()
                                    && let Some(ref mut writer) = *writer_opt
                                {
                                    let timestamp =
                                        chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
                                    let _ = writeln!(writer, "[{}] INPUT: {}", timestamp, *buffer);
                                    let _ = writer.flush();
                                }
                                buffer.clear();
                            }
                        }
                        '\x7f' | '\x08' => {
                            // Backspace - remove last char
                            buffer.pop();
                        }
                        '\x03' => {
                            // Ctrl+C
                            if let Ok(mut writer_opt) = log_writer_clone.try_borrow_mut()
                                && let Some(ref mut writer) = *writer_opt
                            {
                                let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
                                let _ = writeln!(writer, "[{}] INPUT: ^C", timestamp);
                                let _ = writer.flush();
                            }
                            buffer.clear();
                        }
                        '\x04' => {
                            // Ctrl+D
                            if let Ok(mut writer_opt) = log_writer_clone.try_borrow_mut()
                                && let Some(ref mut writer) = *writer_opt
                            {
                                let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
                                let _ = writeln!(writer, "[{}] INPUT: ^D", timestamp);
                                let _ = writer.flush();
                            }
                        }
                        _ if ch.is_control() => {
                            // Skip other control characters
                        }
                        _ => {
                            buffer.push(ch);
                        }
                    }
                }

                // Periodic flush for long-running commands
                let now = std::time::Instant::now();
                if now.duration_since(*last_flush.borrow()).as_secs() >= 30 && !buffer.is_empty() {
                    if let Ok(mut writer_opt) = log_writer_clone.try_borrow_mut()
                        && let Some(ref mut writer) = *writer_opt
                    {
                        let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
                        let _ = writeln!(writer, "[{}] INPUT (partial): {}", timestamp, *buffer);
                        let _ = writer.flush();
                    }
                    *last_flush.borrow_mut() = now;
                }
            });
        }

        // Set up output logging (full transcript)
        if log_output {
            let log_writer_clone = log_writer.clone();
            let notebook_clone = notebook.clone();
            let last_content: Rc<RefCell<String>> = Rc::new(RefCell::new(String::new()));
            let last_log_time: Rc<RefCell<std::time::Instant>> =
                Rc::new(RefCell::new(std::time::Instant::now()));

            notebook.connect_contents_changed(session_id, move || {
                let now = std::time::Instant::now();
                let elapsed = now.duration_since(*last_log_time.borrow());

                // Only capture transcript every 5 seconds to avoid performance issues
                if elapsed.as_secs() >= 5 {
                    if let Some(current_text) = notebook_clone.get_terminal_text(session_id) {
                        let mut last = last_content.borrow_mut();

                        // Only log if content changed
                        if current_text != *last {
                            // Find new content (simple diff - just log new lines)
                            let new_lines: Vec<&str> =
                                current_text.lines().skip(last.lines().count()).collect();

                            if !new_lines.is_empty()
                                && let Ok(mut writer_opt) = log_writer_clone.try_borrow_mut()
                                && let Some(ref mut writer) = *writer_opt
                            {
                                let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
                                let _ = writeln!(writer, "[{}] OUTPUT:", timestamp);
                                for line in new_lines {
                                    let _ = writeln!(writer, "  {}", line);
                                }
                                let _ = writer.flush();
                            }

                            *last = current_text;
                        }
                    }

                    *last_log_time.borrow_mut() = now;
                }
            });
        }
    }
}
