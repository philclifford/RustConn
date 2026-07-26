---
inclusion: fileMatch
fileMatchPattern: "rustconn/src/window/**/*.rs"
---

# Window / Sessions — Development Rules

You are editing a file in `rustconn/src/window/`.

## State Management

- `SharedAppState = Rc<RefCell<AppState>>` — pass as `&SharedAppState`
- NEVER hold a borrow across async boundaries or GTK callbacks
- Use `with_state()` / `with_state_mut()` helpers instead of direct `.borrow()`
- For callbacks with RefCell → take-invoke-restore pattern (as in `handle_ironrdp_error`)

## Sidebar

- Statuses: yellow = connecting, green = connected, red = failed, gray = disconnected
- Reconnect → reuse existing tab (don't create a new one)
- Context menu → GNOME HIG order: primary action at top, destructive at bottom

## Toasts

- `adw::ToastOverlay` with severity icons
- Use `i18n_f()` with `{}` placeholders for dynamic values

## Tabs

- Tab Overview → `AdwTabOverview`, terminals always inside `TabPage`
- Split view → layout lives inside TabPage, not in a global container

## Detached Session Windows

- Three placements, one at a time: tab, split panel, detached window (one session per window)
- `TerminalNotebook` stays the single owner of session state — a detached window only borrows the
  session's widget subtree; teardown always goes through the notebook's `close-page` path
- `DetachedWindowRegistry` (`rustconn/src/detached_window.rs`) owns the window values by session id;
  actions live in `window/detach_actions.rs` (`win.detach-session`, `win.detach-session-to-monitor`,
  `win.attach-session`, `win.toggle-detach`)
- Detachability is decided only by `rustconn_core::detach_verdict()` — never re-derive it inline
- Any "open sessions" count must be `session_count() + detached_count()` (+ external sessions)
- Window actions in a detached window are scoped to that window's `session_id`, never to the main
  window's selection; callbacks capture `Weak` handles only (no `Rc` cycle back to the notebook)
- Monitor choice → `present_fullscreen_on()`; never position a window by coordinates (Wayland)

## Auto-Reconnect

- Uses `poll_until_online_with_backoff()` from `rustconn-core/src/host_check.rs`
- Exponential backoff via `RetryConfig` / `RetryState` (per-connection or default)
- Runs in background thread via `spawn_blocking_with_callback`
- Cancel token registered per session — closing tab cancels polling
- Never use `.expect()` for `Runtime::new()` — use `.map_err(HostCheckError::Io)?`
- Skip reconnect if: SSH auth failure, rapid crash (<5s), or retry disabled
