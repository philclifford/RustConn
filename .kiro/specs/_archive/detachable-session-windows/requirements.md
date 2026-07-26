# Requirements Document

## Introduction

This specification describes detachable session windows for RustConn.

The trigger is issue #236: a user asks for a way to move an SSH tab into a separate window (as Ásbrú Connection Manager allows), which is mainly useful on a multi-monitor desktop. The request is generalized here to every protocol that RustConn renders in-process, plus the inverse operation — returning a detached session to the main window's tab list.

A feasibility review of the current code established the following starting position:

- Every session tab lives in a single `adw::TabView` owned by one `TerminalNotebook`, which also owns all per-session state maps (`sessions`, `terminals`, `session_widgets`, `session_info`, `tab_containers`, …).
- Tab tearoff is explicitly disabled: `TabView::connect_create_window` returns `None` (`rustconn/src/terminal/mod.rs`).
- A complete but unused `ExternalWindow` / `ExternalWindowManager` pair already exists (`rustconn/src/external_window.rs`), including terminal reparenting and geometry capture on close.
- The "remove a tab without tearing the session down, then recreate it later" mechanism already exists and is in production use for split-view guests: `park_session_tab` / `restore_session_tab` / `reparent_terminal_to_tab` / `reparent_embedded_to_tab`.
- Sessions delegated to an external viewer process (SPICE always; RDP/VNC in external mode) hold only an `adw::StatusPage` placeholder in their tab — their real window is already a separate operating-system window, and re-embedding a foreign toplevel is not possible on Wayland (no XEmbed, no `GtkSocket` in GTK4).

The feature is therefore scoped as **one session per detached window**, reusing the proven park/reparent machinery, with no second `adw::TabView` and no drag-and-drop tearoff. Multi-tab detached windows and drag-and-drop between windows are explicit non-goals of this phase (see Out of Scope).

All GUI and window-management code stays in the `rustconn` crate. Any pure decision logic (for example the predicate "can this session be detached") belongs in `rustconn-core` (no gtk4/adw/vte4).

## Glossary

- **Session**: an active connection tracked by `TerminalNotebook` and identified by a `session_id` (`Uuid`).
- **Detach**: moving a session's live widget out of its tab in the main window into a new top-level window, without interrupting the underlying connection.
- **Attach (Reattach)**: the inverse of detach — moving a detached session's live widget back into a tab in the main window's `adw::TabView`.
- **Detached Window**: a top-level `adw::ApplicationWindow` created by a detach operation that hosts exactly one session.
- **Detached Session**: a session whose widget currently lives in a detached window and which therefore has no tab in the main window.
- **In-Process Session**: a session whose content is rendered by RustConn itself — a `vte4::Terminal` (SSH, SFTP, Telnet, Serial, Kubernetes, Mosh, ZeroTrust, local shell) or an embedded viewer widget (embedded RDP, embedded VNC, embedded Web).
- **External Viewer Session**: a session whose display is delegated to a separate process (SPICE via `remote-viewer`; RDP/VNC in external mode), as defined by the existing external-session tracking feature.
- **Parking**: the existing mechanism (`park_session_tab`) that removes a session's `adw::TabPage` while keeping all session state alive, by marking the session so the `close-page` handler skips teardown.
- **Split Owner Tab**: a tab whose `TabPageContainer` hosts a split layout (a `SplitViewBridge`), possibly containing guest sessions parked from their own tabs.
- **Session Teardown**: the full cleanup performed today by the `close-page` handler — disconnecting embedded widgets, killing the VTE child process group, dropping SSH tunnels, clearing session maps, and notifying the sidebar and history.
- **RustConn**: the connection-manager application these requirements apply to.

## Out of Scope

The following are deliberately excluded from this phase and, where relevant, are listed as follow-up candidates:

1. Detached windows hosting more than one session (no `adw::TabView` in the detached window).
2. Drag-and-drop tab tearoff and drag-back between windows (`TabView::create-window` / `transfer_page`).
3. Detaching a split owner tab or an individual split pane.
4. Re-embedding an external viewer's window (SPICE, external RDP/VNC) into RustConn.
5. Persisting the detached layout across application restarts (workspace save/restore of detached windows).
6. Programmatic window positioning on a specific monitor by coordinates (not available on Wayland).

## Requirements

### Requirement 1: Detach an in-process session into its own window

**User Story:** As a user working across two monitors, I want to move a session out of the main window into its own window, so that I can keep it visible on the second monitor while continuing to work in the main window.

#### Acceptance Criteria

1. WHERE a session is an in-process session, THE RustConn SHALL provide a detach operation that moves that session's live widget into a new detached window.
2. WHEN a session is detached, THE RustConn SHALL keep the underlying connection uninterrupted — the same widget instance is moved, no protocol reconnect or process restart occurs, and no terminal scrollback is lost.
3. WHEN a session is detached, THE RustConn SHALL remove that session's tab from the main window's `adw::TabView` without performing session teardown.
4. WHEN a session is detached, THE RustConn SHALL present the detached window and give input focus to the session content inside it.
5. THE RustConn SHALL support detaching every in-process session type: VTE-based sessions (SSH, SFTP, Telnet, Serial, Kubernetes, Mosh, ZeroTrust, local shell), embedded RDP, embedded VNC, and embedded Web.
6. WHEN an embedded viewer session (RDP, VNC, Web) is detached, THE RustConn SHALL render its live content in the detached window within 1 second of the window being mapped, with no blank viewer area.
7. THE RustConn SHALL allow at most one detached window per session and SHALL ignore a repeated detach request for an already detached session.
8. IF a detach operation fails at any step, THEN THE RustConn SHALL leave the session in its previous location with a working widget and SHALL report the failure to the user.

### Requirement 2: Attach a detached session back into the main window

**User Story:** As a user, I want to return a detached session to the main window's tab list, so that I can consolidate my sessions again without losing the connection.

#### Acceptance Criteria

1. WHERE a session is detached, THE RustConn SHALL provide an attach operation that moves that session's live widget back into a tab in the main window's `adw::TabView`.
2. WHEN a session is attached, THE RustConn SHALL keep the underlying connection uninterrupted, using the same guarantees as Requirement 1.2.
3. WHEN a session is attached, THE RustConn SHALL recreate that session's tab with the same title, protocol icon, tooltip, tab group, and group or protocol color indicator it had before detaching.
4. WHEN a session is attached, THE RustConn SHALL select the recreated tab in the main window, present the main window, and close the now-empty detached window.
5. WHEN an embedded viewer session is attached, THE RustConn SHALL render its live content in the tab within 1 second, with no blank viewer area.
6. WHEN the last remaining session is attached back and the main window shows the Welcome tab, THE RustConn SHALL remove the Welcome tab, consistent with normal tab creation.
7. IF an attach operation fails at any step, THEN THE RustConn SHALL keep the session alive in its detached window and SHALL report the failure to the user.

### Requirement 3: Discoverability of detach and attach

**User Story:** As a user, I want to find the detach and attach actions where I would expect them, so that I do not need to read documentation to use the feature.

#### Acceptance Criteria

1. THE RustConn SHALL offer the detach action in the tab context menu of the main window, labelled in header capitalization and placed in a section above the close section.
2. WHERE the right-clicked tab cannot be detached, THE RustConn SHALL either hide the detach menu item or present it as insensitive, and SHALL NOT present an item that does nothing when activated.
3. THE RustConn SHALL offer the attach action in the detached window's `adw::HeaderBar` as an icon-only button with both a tooltip and an accessible label.
4. THE RustConn SHALL register the detach action and the attach action as window actions so they are reachable by keyboard, and SHALL list them in the keyboard shortcuts dialog under an existing category.
5. THE RustConn SHALL bind the same default accelerator to detach in the main window and to attach in a detached window, so that one key combination toggles the session between the two locations depending on which window has focus.
6. WHEN a detached session is activated from the sidebar (the action that would normally select its tab), THE RustConn SHALL present its detached window instead of creating a second session or a new tab.

### Requirement 4: Sessions that cannot be detached

**User Story:** As a user with SPICE or external-viewer connections, I want RustConn to tell me clearly why detaching does not apply, so that I do not think the feature is broken.

#### Acceptance Criteria

1. WHERE a session is an external viewer session, THE RustConn SHALL NOT offer a detach operation for it, because its display already runs in a separate operating-system window.
2. WHERE a tab holds an external-window placeholder (for example an `adw::StatusPage` for a delegated RDP, VNC, or SPICE session), THE RustConn SHALL treat that tab as not detachable.
3. WHERE a tab is a split owner tab, THE RustConn SHALL treat it as not detachable in this phase, and IF the user attempts to detach it THEN THE RustConn SHALL explain that the split layout must be removed first.
4. WHERE a session is a split guest (its widget currently lives in another tab's split layout), THE RustConn SHALL treat it as not detachable in this phase.
5. WHERE the Welcome tab is the target, THE RustConn SHALL treat it as not detachable.
6. THE RustConn SHALL decide detachability through a single shared predicate that returns the same result for the same session state, used by every call site (context menu population, keyboard action, sidebar routing).

### Requirement 5: A detached window behaves like a first-class application window

**User Story:** As a user, I want a detached session window to behave like the rest of the application, so that shortcuts, notifications, and window management do not silently break.

#### Acceptance Criteria

1. THE RustConn SHALL give each detached window an `adw::ToolbarView` with an `adw::HeaderBar` whose title identifies the session (connection name), following the existing header bar conventions.
2. THE RustConn SHALL make the application's existing window actions resolvable inside a detached window, so that a keyboard shortcut that acts on the focused session (for example copy, paste, terminal search, zoom in, zoom out, reset zoom) operates on the session in that window.
3. THE RustConn SHALL NOT let a window action invoked from a detached window act on an unrelated session in the main window.
4. THE RustConn SHALL provide an `adw::ToastOverlay` in each detached window, so that a toast raised while the detached window is active is visible to the user rather than discarded.
5. THE RustConn SHALL apply the same accelerator handling as the main window to each detached window, including the layout-independent accelerator support and the focus-based single-`Ctrl` accelerator suspension used for terminals and embedded viewers.
6. THE RustConn SHALL apply the compact-interface preferences to each detached window at creation and keep them in sync when the preference changes.
7. WHEN the application is re-activated (for example a second launch attempt or D-Bus activation), THE RustConn SHALL present the main window, not a detached window.
8. THE RustConn SHALL give each detached window a default size of at least 800×600 logical pixels and SHALL allow it to be resized, maximized, and fullscreened.

### Requirement 6: Window lifecycle and shutdown

**User Story:** As a user, I want closing windows to do the obvious thing, so that I never end up with orphaned windows or silently killed sessions.

#### Acceptance Criteria

1. WHEN the user closes a detached window, THE RustConn SHALL perform the standard session teardown for its session — exactly the same cleanup a tab close performs today — and SHALL NOT leave the session's widget, child process, or SSH tunnel alive.
2. THE RustConn SHALL perform session teardown exactly once per session, whether the session ends by tab close, by detached-window close, or by remote disconnect.
3. WHEN the user closes the main window, THE RustConn SHALL close all detached windows and terminate their sessions, so that no detached window outlives the main window.
4. WHILE at least one detached session exists, THE RustConn SHALL include those sessions in the open-session count used by the main window's close confirmation dialog.
5. WHEN a detached session's underlying connection ends on its own (remote disconnect, child process exit), THE RustConn SHALL apply the same behavior it applies to a tab: update the sidebar status, close the connection's history entry, and close the detached window.
6. WHEN minimize-to-tray is enabled and the main window is hidden to the tray, THE RustConn SHALL keep detached windows and their sessions in their current state.

### Requirement 7: Session state integrity across detach and attach

**User Story:** As a user, I want everything attached to a session — monitoring, recording, history, indicators — to keep working after I move the session between windows, so that detaching has no hidden cost.

#### Acceptance Criteria

1. WHILE a session is detached, THE RustConn SHALL keep its sidebar connection status, session count, and connection history entry in the same state as an equivalent non-detached session.
2. WHEN a session is detached or attached, THE RustConn SHALL preserve its activity or silence monitoring mode, and THE monitoring status bar SHALL remain functional in the session's new location.
3. WHEN a session is detached or attached, THE RustConn SHALL preserve an in-progress session recording without gaps or duplicate files.
4. WHEN a session is detached or attached, THE RustConn SHALL preserve its highlight rules and highlight overlay behavior.
5. WHEN a session is detached or attached, THE RustConn SHALL keep its SSH tunnel, automation (expect) session, and auto-reconnect cancel token associated with the session.
6. WHEN a detached session reconnects (manually or through auto-reconnect), THE RustConn SHALL show the reconnected session in the same detached window.
7. THE RustConn SHALL keep the main window's tab count, Tab Overview, and split-view session picker free of entries for detached sessions, while continuing to show those sessions in the sidebar as connected.

### Requirement 8: Multi-monitor support

**User Story:** As a user with two monitors, I want a detached session to be easy to put on my second monitor, so that the feature solves the problem I actually have.

#### Acceptance Criteria

1. THE RustConn SHALL allow a detached window to be moved and resized by the user through the desktop's normal window management.
2. WHERE more than one monitor is connected, THE RustConn SHALL offer an option to open the detached window fullscreen on a chosen monitor.
3. WHERE exactly one monitor is connected, THE RustConn SHALL NOT present a monitor choice.
4. WHEN a detached window is moved to a monitor with a different scale factor, THE RustConn SHALL render the session content at the correct scale for that monitor, including fractional scaling for embedded viewers.
5. THE RustConn SHALL NOT attempt to position a detached window by coordinates on Wayland, and SHALL NOT log an error or warning for the absence of that capability.

### Requirement 9: Internationalization and accessibility

**User Story:** As a user of a localized or assistive-technology-driven desktop, I want the new interface elements to follow the same rules as the rest of the application.

#### Acceptance Criteria

1. THE RustConn SHALL pass every user-facing string introduced by this feature through `i18n()` or `i18n_f()`, using `{}` placeholders.
2. THE RustConn SHALL give every icon-only control introduced by this feature both a tooltip and an accessible label.
3. THE RustConn SHALL give each detached window an accessible window title that identifies the session.
4. THE RustConn SHALL keep the new menu items and window titles consistent with the project's capitalization rules — header capitalization for menu items, buttons, and window titles.

### Requirement 10: Quality gates

**User Story:** As a maintainer, I want this feature to meet the project's existing quality bar, so that it does not become a source of regressions.

#### Acceptance Criteria

1. THE RustConn SHALL build with no new `cargo clippy --all-targets` warnings under the project's lint configuration.
2. THE RustConn SHALL cover the detachability predicate and any other pure decision logic with unit or property tests in the crate that owns it.
3. THE RustConn SHALL keep the existing split-view park and restore behavior working unchanged, verified by the existing tests plus a regression check that a split guest is still returned to its own tab.
4. THE RustConn SHALL NOT introduce a reference cycle that keeps a `TerminalNotebook`, a detached window, or a session widget alive after its session ends.
5. THE RustConn SHALL document the feature in `CHANGELOG.md` under the current release section, following the project's changelog format, and SHALL reference issue #236.
