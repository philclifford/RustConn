//! FreeRDP detection utilities
//!
//! This module provides functions for detecting available FreeRDP clients.
//! Detection prefers the maintained SDL3 client (`sdl-freerdp3`); the
//! deprecated `wlfreerdp` is kept only as a fallback and for embedded mode.

use std::process::{Command, Stdio};

/// Ordered candidate list for FreeRDP detection.
///
/// SDL3 (`sdl-freerdp3`) comes first: it is the maintained FreeRDP 3.x client
/// and works natively on both Wayland and X11. The Wayland-native `wlfreerdp`
/// is deprecated upstream (FreeRDP 3.x prints a deprecation warning and points
/// users at the SDL3 client), so it is only a fallback here. X11 `xfreerdp`
/// variants come last on Wayland sessions.
const WAYLAND_FIRST_CANDIDATES: &[&str] = &[
    "sdl-freerdp3", // FreeRDP 3.x SDL3 — maintained, Wayland + X11 (versioned)
    "sdl-freerdp",  // FreeRDP SDL3 (unversioned, e.g. Flatpak)
    "wlfreerdp3",   // FreeRDP 3.x Wayland-native — deprecated, fallback
    "wlfreerdp",    // FreeRDP Wayland-native (unversioned) — deprecated, fallback
    "xfreerdp3",    // FreeRDP 3.x X11
    "xfreerdp",     // FreeRDP 2.x X11
];

/// X11-first candidate order (used when not running under Wayland).
const X11_FIRST_CANDIDATES: &[&str] = &[
    "xfreerdp3",    // FreeRDP 3.x X11
    "xfreerdp",     // FreeRDP 2.x X11
    "sdl-freerdp3", // FreeRDP 3.x SDL3 (versioned)
    "sdl-freerdp",  // FreeRDP SDL3 (unversioned, e.g. Flatpak)
    "wlfreerdp3",   // FreeRDP 3.x Wayland (still usable as fallback)
    "wlfreerdp",    // FreeRDP Wayland (unversioned)
];

/// Returns `true` if the current session is Wayland.
fn is_wayland_session() -> bool {
    std::env::var("XDG_SESSION_TYPE")
        .map(|v| v == "wayland")
        .unwrap_or(false)
        || std::env::var("WAYLAND_DISPLAY").is_ok()
}

/// Syntax accepted by the installed FreeRDP for `/args-from:`.
///
/// FreeRDP has two spellings for reading arguments from a file:
///
/// * `/args-from:<path>` — accepted by every FreeRDP 3.x release. Marked
///   deprecated from 3.26 on, but still functional unless the distro builds
///   with `WITHOUT_FREERDP_3x_DEPRECATED`.
/// * `/args-from:file:<path>` — only exists from FreeRDP 3.26 on. Older
///   releases (3.24, 3.25 — the versions shipped by current Debian/Ubuntu)
///   try to open a file literally named `file:/run/...` and exit with 255.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgsFromForm {
    /// `/args-from:<path>` — universal, deprecated from 3.26 on.
    BarePath,
    /// `/args-from:file:<path>` — FreeRDP 3.26 and newer only.
    FilePrefix,
}

/// First FreeRDP 3.x minor release that understands the `file:` prefix.
const ARGS_FROM_FILE_PREFIX_MIN_MINOR: u32 = 26;

/// Extracts `(major, minor, patch)` from a FreeRDP `--version` banner.
///
/// The banner looks like `This is FreeRDP version 3.24.2 (3.24.2)`; a missing
/// patch component is treated as `0`.
fn parse_freerdp_version(output: &str) -> Option<(u32, u32, u32)> {
    let tail = output.split("version").nth(1)?;
    let token = tail.split_whitespace().next()?;
    let mut parts = token.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().unwrap_or("0").parse().ok()?;
    // The patch component may carry a suffix (e.g. "2-rc1"); keep the digits.
    let patch = parts
        .next()
        .map(|p| {
            p.chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
        })
        .and_then(|p| p.parse().ok())
        .unwrap_or(0);
    Some((major, minor, patch))
}

/// Picks the `/args-from:` syntax for a detected FreeRDP version.
///
/// An unknown version falls back to [`ArgsFromForm::BarePath`]: it is the form
/// that works on every FreeRDP 3.x released so far, whereas `file:` silently
/// breaks the launch on anything older than 3.26.
const fn args_from_form_for_version(version: Option<(u32, u32, u32)>) -> ArgsFromForm {
    match version {
        Some((major, minor, _))
            if major > 3 || (major == 3 && minor >= ARGS_FROM_FILE_PREFIX_MIN_MINOR) =>
        {
            ArgsFromForm::FilePrefix
        }
        _ => ArgsFromForm::BarePath,
    }
}

/// Queries `binary --version` and parses the FreeRDP version.
///
/// A `host:` prefix (Flatpak) is resolved through `flatpak-spawn --host`.
/// Returns `None` when the binary cannot be run or the banner is unparseable.
#[must_use]
pub fn freerdp_version(binary: &str) -> Option<(u32, u32, u32)> {
    let output = if let Some(host_bin) = binary.strip_prefix("host:") {
        Command::new("flatpak-spawn")
            .args(["--host", host_bin, "--version"])
            .output()
    } else {
        Command::new(binary).arg("--version").output()
    }
    .ok()?;

    let version = parse_freerdp_version(&String::from_utf8_lossy(&output.stdout))
        .or_else(|| parse_freerdp_version(&String::from_utf8_lossy(&output.stderr)));
    tracing::debug!(
        protocol = "rdp",
        binary = %binary,
        version = ?version,
        "Detected FreeRDP version"
    );
    version
}

/// Builds the `/args-from:` argument for `binary`, using the syntax that this
/// binary's FreeRDP version actually understands.
///
/// See [`ArgsFromForm`] for why this cannot be a single hardcoded string.
#[must_use]
pub fn args_from_argument(binary: &str, path: &std::path::Path) -> String {
    match args_from_form_for_version(freerdp_version(binary)) {
        ArgsFromForm::FilePrefix => format!("/args-from:file:{}", path.display()),
        ArgsFromForm::BarePath => format!("/args-from:{}", path.display()),
    }
}

/// Checks whether a binary is available on `PATH`.
fn binary_exists(name: &str) -> bool {
    Command::new("which")
        .arg(name)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Detects the best available FreeRDP binary.
///
/// On Wayland sessions, Wayland-native variants (`wlfreerdp3`, `wlfreerdp`)
/// are tried first. On X11 sessions, `xfreerdp` variants take priority.
/// Returns `None` if no FreeRDP client is found.
#[must_use]
pub fn detect_best_freerdp() -> Option<String> {
    let wayland = is_wayland_session();
    let candidates = if wayland {
        WAYLAND_FIRST_CANDIDATES
    } else {
        X11_FIRST_CANDIDATES
    };

    for candidate in candidates {
        if binary_exists(candidate) {
            tracing::debug!(
                protocol = "rdp",
                binary = %candidate,
                wayland,
                "Selected external FreeRDP binary"
            );
            return Some((*candidate).to_string());
        }
    }
    tracing::warn!(
        protocol = "rdp",
        wayland,
        "No FreeRDP client found on PATH (wlfreerdp3/sdl-freerdp3/xfreerdp3)"
    );
    None
}

/// Detects if a Wayland-native FreeRDP variant is available for embedded mode.
///
/// Embedded mode (the managed `wlfreerdp` subsurface in [`super::thread`]) only
/// makes sense on a Wayland session with an actual `wlfreerdp` binary, so this
/// checks both directly instead of going through [`detect_best_freerdp`] — that
/// now prefers the maintained SDL3 client, which cannot do subsurface embedding.
#[must_use]
pub fn detect_wlfreerdp() -> bool {
    is_wayland_session() && (binary_exists("wlfreerdp3") || binary_exists("wlfreerdp"))
}

/// Detects the best FreeRDP binary for RemoteApp (RAIL) sessions.
///
/// `wlfreerdp` and `sdl-freerdp` do not support RAIL/RemoteApp — they render
/// a full desktop into their own surface and cannot create individual
/// application windows. Only `xfreerdp` variants support RAIL because they
/// use X11 window management for per-app windows.
///
/// In Flatpak, `xfreerdp` is typically not bundled (only `wlfreerdp`/`sdl-freerdp`
/// are included). This function checks the host system via `flatpak-spawn --host`
/// when running inside a Flatpak sandbox.
#[must_use]
pub fn detect_best_freerdp_for_remoteapp() -> Option<String> {
    // Only xfreerdp variants support RAIL/RemoteApp
    const REMOTEAPP_CANDIDATES: &[&str] = &["xfreerdp3", "xfreerdp"];

    // First check inside the sandbox
    for candidate in REMOTEAPP_CANDIDATES {
        if binary_exists(candidate) {
            return Some((*candidate).to_string());
        }
    }

    // In Flatpak, check host system via flatpak-spawn
    if rustconn_core::flatpak::is_flatpak() {
        for candidate in REMOTEAPP_CANDIDATES {
            if host_binary_exists(candidate) {
                // Return a marker that launch() will interpret as host-side binary
                return Some(format!("host:{candidate}"));
            }
        }
    }

    None
}

/// Checks whether a binary exists on the host system (via flatpak-spawn --host).
fn host_binary_exists(name: &str) -> bool {
    Command::new("flatpak-spawn")
        .args(["--host", "which", name])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Detects if any FreeRDP client is available for external mode
///
/// Returns the name of the best available FreeRDP client.
#[must_use]
pub fn detect_xfreerdp() -> Option<String> {
    detect_best_freerdp()
}

/// Checks if IronRDP native client is available
///
/// This is determined at compile time via the rdp-embedded feature flag.
#[must_use]
pub fn is_ironrdp_available() -> bool {
    rustconn_core::is_embedded_rdp_available()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wayland_candidates_prefer_sdl3() {
        assert!(WAYLAND_FIRST_CANDIDATES.contains(&"wlfreerdp3"));
        assert!(WAYLAND_FIRST_CANDIDATES.contains(&"wlfreerdp"));
        assert!(WAYLAND_FIRST_CANDIDATES.contains(&"sdl-freerdp3"));
        assert!(WAYLAND_FIRST_CANDIDATES.contains(&"sdl-freerdp"));
        // Maintained SDL3 first, deprecated wlfreerdp as fallback, X11 last.
        let sdl_pos = WAYLAND_FIRST_CANDIDATES
            .iter()
            .position(|c| *c == "sdl-freerdp3")
            .unwrap();
        let wl_pos = WAYLAND_FIRST_CANDIDATES
            .iter()
            .position(|c| *c == "wlfreerdp3")
            .unwrap();
        let x11_pos = WAYLAND_FIRST_CANDIDATES
            .iter()
            .position(|c| *c == "xfreerdp3")
            .unwrap();
        assert!(sdl_pos < wl_pos);
        assert!(wl_pos < x11_pos);
    }

    #[test]
    fn test_x11_candidates_prefer_xfreerdp() {
        let x11_pos = X11_FIRST_CANDIDATES
            .iter()
            .position(|c| *c == "xfreerdp3")
            .unwrap();
        let sdl_pos = X11_FIRST_CANDIDATES
            .iter()
            .position(|c| *c == "sdl-freerdp3")
            .unwrap();
        let wl_pos = X11_FIRST_CANDIDATES
            .iter()
            .position(|c| *c == "wlfreerdp3")
            .unwrap();
        assert!(x11_pos < sdl_pos);
        assert!(sdl_pos < wl_pos);
    }

    #[test]
    fn test_binary_exists_returns_false_for_nonexistent() {
        assert!(!binary_exists("this_binary_does_not_exist_12345"));
    }

    #[test]
    fn parses_freerdp_version_banner() {
        assert_eq!(
            parse_freerdp_version("This is FreeRDP version 3.24.2 (3.24.2)"),
            Some((3, 24, 2))
        );
        assert_eq!(
            parse_freerdp_version("This is FreeRDP version 3.26.0 (3.26.0)"),
            Some((3, 26, 0))
        );
    }

    #[test]
    fn parses_freerdp_version_with_suffix_and_missing_patch() {
        assert_eq!(
            parse_freerdp_version("This is FreeRDP version 3.27.0-rc1"),
            Some((3, 27, 0))
        );
        assert_eq!(
            parse_freerdp_version("FreeRDP version 4.0"),
            Some((4, 0, 0))
        );
    }

    #[test]
    fn rejects_unparseable_version_output() {
        assert_eq!(parse_freerdp_version(""), None);
        assert_eq!(parse_freerdp_version("command not found"), None);
        assert_eq!(parse_freerdp_version("version unknown"), None);
    }

    #[test]
    fn old_freerdp_gets_bare_args_from_path() {
        // 3.24/3.25 do not know the `file:` prefix.
        assert_eq!(
            args_from_form_for_version(Some((3, 24, 2))),
            ArgsFromForm::BarePath
        );
        assert_eq!(
            args_from_form_for_version(Some((3, 25, 0))),
            ArgsFromForm::BarePath
        );
    }

    #[test]
    fn new_freerdp_gets_file_prefix() {
        assert_eq!(
            args_from_form_for_version(Some((3, 26, 0))),
            ArgsFromForm::FilePrefix
        );
        assert_eq!(
            args_from_form_for_version(Some((4, 0, 0))),
            ArgsFromForm::FilePrefix
        );
    }

    #[test]
    fn unknown_version_falls_back_to_bare_path() {
        assert_eq!(args_from_form_for_version(None), ArgsFromForm::BarePath);
        // FreeRDP 2.x never had the prefix either.
        assert_eq!(
            args_from_form_for_version(Some((2, 11, 7))),
            ArgsFromForm::BarePath
        );
    }

    #[test]
    fn args_from_argument_uses_bare_path_for_unknown_binary() {
        let arg = args_from_argument(
            "this_binary_does_not_exist_12345",
            std::path::Path::new("/run/user/1000/rustconn-rdp.args"),
        );
        assert_eq!(arg, "/args-from:/run/user/1000/rustconn-rdp.args");
    }
}
