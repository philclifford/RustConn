# macOS Port — Changelog

## [0.19.6] - 2026-07-29

### Added

- **Local-only macOS CI and audit gate** — `scripts/macos-ci.sh` runs formatting, strict Clippy, core tests, `cargo deny`, canonical bundle creation, CLI smoke checks, signature verification, `@rpath` validation, and absolute Homebrew dependency detection without modifying GitHub workflows.
- **Developer ID and notarization flow** — the canonical scripts support explicit Developer ID Application signing with hardened runtime, entitlements and timestamps, followed by optional `notarytool` submission, stapling, validation, and Gatekeeper assessment.
- **Explicit ad-hoc mode** — local artifacts can be signed with `--adhoc`; unsigned remains the default and there is no silent fallback from a requested Developer ID signature.

### Fixed

- **Auxiliary Keychain storage no longer invokes `secret-tool`** — macOS auxiliary secret operations delegate to the native Security.framework implementation with blocking work isolated from the async runtime, a 10-second timeout, and zeroizing retrieval intermediates.
- **Cross-platform Clippy failures** — corrected target-specific `statvfs` conversion expectations and removed an unused GTK observer so warnings are denied under the full canonical profile.
- **Bundled OpenH264 was never loaded** — the runtime probed only Linux `.so` paths, so H.264 quietly fell back to non-AVC codecs. macOS now probes `Contents/Frameworks/libopenh264.dylib` first, the producer requires OpenH264, and the local audit verifies the bundled library and its architecture.
- **`--skip-build` could produce a misleading Developer ID DMG** — the packager now strictly verifies the enclosed app's signature, identity and hardened runtime before signing or notarizing.
- **Malformed Keychain values were dropped without zeroization** — auxiliary Keychain and Secret Service decoding now wipes invalid UTF-8 buffers, covered by unit tests.
- **Timed-out Keychain mutations were abandoned silently** — the late outcome is now observed and logged, and the error states the operation may still complete.

### Improved

- **Canonical self-contained `.app` producer** — `scripts/macos-build.sh` owns application creation and relocates 58 recursively discovered non-system dylibs into `Contents/Frameworks`, rewrites them to `@rpath`, embeds runtime resources, and fails on remaining absolute Homebrew references.
- **Single DMG path** — `packaging/macos/build-dmg.sh` packages `dist/RustConn.app`; `build_macos.sh` delegates for compatibility instead of producing a divergent application.
- **Canonical features** — macOS consistently enables `tray-macos,system-keyring,vnc-embedded,rdp-embedded,gfx-h264,rdp-audio,rd-gateway,adw-1-8` and excludes Linux-only Wayland/WebKitGTK and removed embedded SPICE paths.
- **Portable bundle layout** — LaunchServices executes `Contents/MacOS/rustconn`; the optional terminal wrapper moved to `Contents/Resources/bin/rustconn-wrapper`, and schemas, icons, locales, OpenH264 and library lookup are bundle-relative.
- **Supply-chain target coverage** — `deny.toml` audits both Apple Silicon and Intel macOS targets.
- **Native PTY completion** — the isolated `rustconn-pty-sys` helper provides `setsid` and `TIOCSCTTY`, replacing the earlier basic process-group limitation.

### Validation

- A real Apple Silicon ad-hoc release app and DMG were built successfully, with all non-system dylibs relocated, OpenH264 bundled, and signature/linkage checks passing.
- Developer ID signing and notarization remain credential-dependent and cannot be validated without the certificate and notarytool keychain profile.
- Intel and universal runtime behavior still requires an Intel host or explicit universal build; the validated local artifact is arm64.
- Rust reports an upstream future-incompatibility warning in `block 0.1.6`, pulled by the latest `gettext-rs`/`locale_config` chain; no compatible published update currently removes it.

> The limitations and build configuration under the older `0.15.4` port notes are historical. For current commands and supported layout, use [`MACOS_BUILD.md`](MACOS_BUILD.md).

## [0.15.4] - 2026-05-31

### Fixed

- **UI hang when editing connection with broken ssh-agent** — `ConnectionDialog::refresh_agent_keys()` now probes the agent asynchronously on a background thread with a 5-second timeout; if the agent does not respond in time the child process is killed and the dropdown shows "No keys loaded" without blocking the UI.
- **Clippy `useless_conversion` on macOS** — `cfg_attr` for `u64::from()` on `statvfs` fields was inverted (`not(target_os = "macos")` → `target_os = "macos"`); the conversion is identity on macOS (u64→u64) but needed on Linux (u32→u64).
- **Clippy `new_without_default` for `MacOsKeychainBackend`** — added `Default` impl delegating to `new()`.
- **Clippy `redundant_clone` in macOS Keychain** — `bytes.to_vec()` / `bytes.clone()` removed where `bytes` is already an owned `Vec<u8>` passed by value to `String::from_utf8`.
- **Clippy `case_sensitive_file_extension_comparisons`** — `.app` bundle detection now uses `Path::extension().is_some_and(|ext| ext.eq_ignore_ascii_case("app"))`.
- **macOS tray: `single_match_else`** — replaced `match` with `if let ... else` for icon rendering.
- **macOS tray: `while_let_loop`** — replaced `loop { match recv() { ... Err => break } }` with `while let Ok(event) = recv()`.
- **macOS tray: `useless_conversion`** — removed `.into()` on `menu_id` that was already `String`.
- **macOS tray: `if_not_else`** — inverted conditions in `set_active_sessions`, `set_recent_connections`, `set_window_visible` to satisfy clippy.
- **Unused `PtyFlags` import on macOS** — gated with `#[cfg(not(target_os = "macos"))]` since it's only used in the Linux `spawn_async` path.
- **`collapsible_if` in `macos_pty.rs`** — merged nested `if` into let-chain.
- **`collapsible_if` in `window/mod.rs`** — merged nested `if` for bundle icons path check.
- **`needless_return` in `edit_actions.rs`** — removed trailing `return;` in macOS SFTP URI handler.
- **`items_after_statements` in `dialog.rs`** — moved `const AGENT_TIMEOUT` before statements in `refresh_agent_keys()`.
- **`release.sh` crash on macOS** — script used `declare -A` (bash 4+ associative arrays) but macOS ships bash 3.2; replaced with parallel indexed arrays.
- **`release.sh` clippy on macOS** — added platform detection to run clippy with `--no-default-features --features tray-macos,...` on Darwin, avoiding the missing `gtk4-wayland` pkg-config error.

## Changes for macOS Support

### Added

- **macOS Keychain backend** (`rustconn-core/src/secret/macos_keychain.rs`) — native credential storage using Security.framework via `security-framework` crate. Implements `SecretBackend` trait with store/retrieve/delete operations through macOS Keychain Services API. Uses `tokio::task::spawn_blocking` for async compatibility. No CLI tools needed, no PATH issues in `.app` bundles. Added `SecretBackendType::MacOsKeychain` variant with full integration across resolver, manager, detection, settings UI, and CLI.
- **Native PTY spawn for macOS** (`rustconn/src/macos_pty.rs`) — VTE's built-in `spawn_async` does not work on macOS (Homebrew build); the PTY is created but never connected to child process output. New module creates PTY via `nix::pty::openpty()`, spawns the child with slave fd as stdin/stdout/stderr, and hands the master fd to VTE via `Pty::foreign_sync()`. Uses `process_group(0)` for basic job control. Conditional compilation `#[cfg(target_os = "macos")]` ensures zero impact on Linux.
- **macOS PATH extension in `get_extended_path()`** — GUI apps launched via `.app` bundle have minimal PATH (`/usr/bin:/bin`). Added `/opt/homebrew/bin`, `/opt/homebrew/sbin`, `/usr/local/bin`, and `/Applications/KeePassXC.app/Contents/MacOS` to the extended PATH on macOS. This fixes detection of all CLI tools (keepassxc-cli, bw, op, pass, gcloud, kubectl, etc.) without using `set_var`.
- **Unified `detection_command()` helper** — All `detect_*` functions in `detection.rs` now use a shared helper that injects the extended PATH into every spawned `Command`. This ensures all secret backends (KeePassXC, Bitwarden, 1Password, Pass, Passbolt) are discoverable on macOS without per-backend fallback paths.
- **Platform-aware URL opener** — `url_open_command()` returns `open` on macOS and `xdg-open` on Linux. Used by all backends that open web vaults or file managers.
- **macOS .app bundle** — `RustConn.app` with proper `Info.plist`, `.icns` icon, wrapper script setting environment variables, and self-contained binary.
- **macOS tray icon** (`tray-macos` feature) — native NSStatusItem via `tray-icon` + `muda` crates. Provides menu bar icon with Show/Hide Window, Recent Connections, Quick Connect, Local Shell, About, and Quit actions. Replaces the Linux-only `ksni` D-Bus tray on macOS.
- **Homebrew formula** (`packaging/macos/rustconn.rb`) — Complete formula for Homebrew Tap distribution with all dependencies, locale compilation, icon generation, and .app bundle creation.
- **DMG build script** (`packaging/macos/build-dmg.sh`) — Automated script to build release `.dmg` with self-contained `.app` bundle including Adwaita icons, locales, and GSettings schemas. Version is read dynamically from `Cargo.toml`.

### Fixed

- **Cross-platform `statvfs` types** (`rustconn-core/src/rdp_client/rdpdr.rs`) — `fragment_size()`, `blocks()`, `blocks_available()` return different integer types on macOS vs Linux. Added `u64::from()` with `#[cfg_attr(not(target_os = "macos"), allow(clippy::useless_conversion))]` for cross-platform compatibility without clippy warnings on either platform.
- **Local Shell on macOS** — launches with `--login` flag so `.zprofile`/`.zshrc` are sourced (macOS-only via `#[cfg]`).
- **Secret backend detection on macOS** — all backends now use extended PATH, removing the need for per-tool fallback path lists.
- **Removed invalid Cellar path** — `/usr/local/Cellar/keepassxc/keepassxc-cli` never existed (Cellar paths include version). Removed from `status.rs`.
- **Unified extended PATH for all secret backends** — `bitwarden.rs`, `onepassword.rs`, `passbolt.rs`, `pass.rs`, `libsecret.rs`, `keyring.rs`, and `status.rs` now inject `get_extended_path()` into every spawned `Command`. Previously only `detection.rs` (UI panel) used extended PATH; the actual backend operations used bare `Command::new("tool")` which would fail on macOS `.app` bundles.
- **PTY child process cleanup on error** — if `Pty::foreign_sync()` fails after the child is spawned, the child is now killed via `SIGKILL` and reaped via `waitpid()` to prevent zombie processes.
- **PTY child handle race condition** — `std::mem::forget(child)` prevents `Child::drop()` from calling `waitpid()` which would race with GLib's `child_watch_add_local` reaper.
- **PTY fd leak prevention** — `nix::unistd::dup()` returns `OwnedFd` which auto-closes on drop; if a later dup fails, earlier fds are cleaned up automatically via RAII.
- **Essential env vars in macOS PTY** — when `envv` is non-empty, `HOME`, `USER`, `LOGNAME`, `SHELL`, `LANG`, `PATH` are inherited from parent if not already provided. Prevents shell malfunction in `.app` bundles.
- **Reconnect banner on macOS spawn failure** — previously only a toast was shown; now the full reconnect overlay (indicator icon + banner + button) is displayed, matching Linux behavior.
- **`nix` dependency is macOS-only** — moved to `[target.'cfg(target_os = "macos")'.dependencies]` to avoid unnecessary compilation on Linux.
- **Removed hardcoded `GDK_DPI_SCALE=0.5`** — GTK4 handles HiDPI natively on macOS; the forced scale factor caused incorrect sizing on non-Retina displays.
- **Homebrew formula: added `librsvg` build dependency** — required for `rsvg-convert` used during icon generation.
- **Info.plist: added `LSMinimumSystemVersion` (13.0)** — declares macOS Ventura as minimum for GTK4 compatibility.
- **Info.plist: added `NSAppleEventsUsageDescription`** — prevents unexpected permission dialogs when opening URLs.
- **DMG: ad-hoc code signing** — `codesign --force --deep --sign -` applied before DMG creation to reduce Gatekeeper friction.
- **macOS tray: main-thread initialization** — `NSStatusItem` (via `tray-icon` crate) must be created on the main thread. Moved tray creation out of background thread into synchronous main-thread initialization (Linux `ksni` D-Bus tray still uses background thread).
- **macOS tray: dynamic menu rebuild** — `set_active_sessions()`, `set_recent_connections()`, `set_window_visible()` now call `tray_icon.set_menu()` to rebuild the menu on state change. Previously the menu was static after creation.
- **macOS tray: active sessions in menu** — menu now shows "{N} Active Session(s)" as a disabled informational item when sessions are open.
- **X11 renderer fallback skipped on macOS** — `ensure_x11_renderer_fallback()` is now gated with `#[cfg(not(target_os = "macos"))]` to avoid unnecessary code execution on macOS.

### Known Limitations

- **VTE `spawn_async` broken on macOS** — Homebrew VTE build does not connect PTY to child process. Workaround: native PTY via `openpty()` + `Pty::foreign_sync()`.
- **Full session leader not possible without unsafe** — `process_group(0)` provides basic job control (Ctrl-C), but full `setsid` + `TIOCSCTTY` requires unsafe `pre_exec`. This means `Ctrl-Z` (suspend) may not work in all cases.
- **Tray icon not visible when launched from Finder/Dock** — NSStatusItem is created successfully but macOS does not display it when the app is launched via LaunchServices (`open RustConn.app`, Finder double-click, Dock). Works correctly when launched from terminal (`./RustConn.app/Contents/MacOS/rustconn-wrapper`). This is a known interaction between GTK4's GLib main loop and macOS NSApplication activation policy. Workaround: launch from terminal, or use the `rustconn-wrapper` script. Fix planned for a future release via native Objective-C bridge.
- **Wayland not available** — Build without `wayland-native` feature.
- **CSS parser warnings** — libadwaita 1.9 CSS uses features not yet supported by GTK4 4.22 CSS parser. Cosmetic only, no functional impact.
- **libsecret not available** — GNOME Keyring doesn't exist on macOS. Use macOS Keychain (native), KeePassXC, Bitwarden, 1Password, or Pass backends instead.

### Build Configuration for macOS

```bash
cargo build -p rustconn --no-default-features \
  --features "tray-macos,vnc-embedded,rdp-embedded,rdp-audio,adw-1-8"
```

Disabled features: `tray` (Linux D-Bus only), `wayland-native`

Enabled macOS-specific features: `tray-macos` (NSStatusItem via `tray-icon` crate), `adw-1-8` (libadwaita 1.8+ widgets)

### Dependencies (Homebrew)

```bash
brew install gtk4 libadwaita vte3 adwaita-icon-theme openssl@3 dbus gettext
```
