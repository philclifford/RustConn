//! Internationalization support via gettext
//!
//! This module initializes gettext for the RustConn GUI application
//! and provides convenience macros for translatable strings.
//!
//! # Usage
//!
//! ```ignore
//! use crate::i18n::i18n;
//!
//! let msg = i18n("Connection failed");
//! let msg = i18n_f("Deleted '{}'", &[&name]);
//! let msg = ni18n("1 connection", "{} connections", count);
//! ```

use gettextrs::{gettext, ngettext};

/// The gettext domain for RustConn
pub const GETTEXT_DOMAIN: &str = "rustconn";

/// Initializes gettext for the application.
///
/// Must be called once at startup before any translatable strings are used.
/// Sets up the locale, text domain, and locale directory.
pub fn init() {
    // Set locale from environment.
    //
    // Routed through `rustconn-locale-sys` because `setlocale` is `unsafe` and
    // sound only while the process is single-threaded (RUSTSEC-2026-0244). That
    // crate checks the precondition and panics if it no longer holds, so the
    // requirement "call this first in `main()`" is enforced rather than assumed.
    //
    // The applied locale is dropped rather than reported, deliberately:
    //
    //  * there is nowhere to report it to. This runs as the first statement of
    //    `main()`, which is the whole point — the tracing subscriber is
    //    installed several statements later, so a `warn!` here is swallowed.
    //  * `None` is not an error state we can act on. It means the locale named
    //    by the environment is not installed, which is routine in a Flatpak
    //    sandbox (the runtime ships only the host language, issue #158). The
    //    previous locale stays in effect, gettext falls back to the untranslated
    //    strings, and the user sees English — visible without a log line.
    //  * where the outcome *is* actionable — a specific language configured by
    //    the user — it is checked: `apply_language_setlocale` below inspects the
    //    result and falls back to the system locale.
    let _startup_locale =
        rustconn_locale_sys::init_locale(rustconn_locale_sys::LocaleCategory::LcAll, "");

    // Bind text domain to locale directory
    // In Flatpak: /app/share/locale
    // Native install: /usr/share/locale or ~/.local/share/locale
    // Development: OUT_DIR/locale (compiled by build.rs)
    let locale_dir = locale_dir();
    tracing::debug!(locale_dir, "gettext locale directory");
    gettextrs::bindtextdomain(GETTEXT_DOMAIN, locale_dir).expect("bindtextdomain");
    gettextrs::bind_textdomain_codeset(GETTEXT_DOMAIN, "UTF-8").expect("bind_textdomain_codeset");
    gettextrs::textdomain(GETTEXT_DOMAIN).expect("textdomain");
}

/// Reads the saved language from `config.toml` and applies it at startup.
///
/// If a non-system language is configured and the `LANGUAGE` env var is
/// not already set to it, this function re-executes the current process
/// with `LANGUAGE` set. This is the only reliable way to make GNU gettext
/// use a specific language without calling `std::env::set_var` (which is
/// `unsafe` in Rust 2024 edition).
///
/// The re-exec happens before GTK or tokio start, so it is safe.
/// A sentinel env var (`_RUSTCONN_LANG_SET`) prevents infinite re-exec loops.
///
/// # Thread safety
///
/// This is the **only** place that applies a locale after [`init`], and it must
/// stay that way: `setlocale` mutates process-global locale state with no
/// synchronisation, so it is only sound while the process is still
/// single-threaded (RUSTSEC-2026-0244). Every path below therefore performs its
/// locale change here, called from `main()` before GTK, tokio or the tracing
/// subscriber exist. Applying a locale later — for example from the GTK
/// `activate` handler, where the GIO worker thread is already running — would
/// reintroduce the unsoundness.
///
/// On return the locale is sealed, so that "later" is a panic during
/// development rather than memory corruption in the field. The only path that
/// does not seal is the successful re-exec, which replaces the process image and
/// starts this sequence over in the child.
pub fn apply_language_from_config() {
    apply_configured_language();

    // The startup locale is final. Nothing in the rest of the process lifetime
    // may change it — the Settings dialog only persists the choice for the next
    // start — so close the window `rustconn-locale-sys` guards.
    rustconn_locale_sys::seal_locale();
}

/// Applies the configured language, re-execing if that is the only way.
///
/// Split from [`apply_language_from_config`] so that every one of its early
/// returns is followed by the seal, without repeating the call five times.
fn apply_configured_language() {
    use std::os::unix::process::CommandExt;

    let lang = read_language_from_config().unwrap_or_default();
    if lang.is_empty() || lang == "system" {
        // `init()` already applied the system locale with
        // `setlocale(LC_ALL, "")` and bound the domain, and `LC_ALL` covers
        // `LC_MESSAGES`. Re-applying the system locale here would be a no-op.
        return;
    }

    // LANGUAGE is already correct — normally because this is the re-execed
    // child, or because the desktop set it. The env var alone is not enough:
    // gettext ignores LANGUAGE when LC_MESSAGES resolves to "C", which is what
    // happens in a Flatpak sandbox when the host locale is not installed
    // (issue #158). So the locale still has to be applied — and it is applied
    // here, in main(), rather than later from the GTK activate handler.
    if std::env::var("LANGUAGE").ok().as_deref() == Some(lang.as_str()) {
        apply_language_setlocale(&lang);
        return;
    }

    // Check sentinel to avoid infinite re-exec loop
    if std::env::var("_RUSTCONN_LANG_SET").ok().as_deref() == Some("1") {
        // We already re-execed once — don't loop. Fall through to
        // best-effort setlocale below.
        apply_language_setlocale(&lang);
        return;
    }

    // Re-exec ourselves with LANGUAGE set. This replaces the current
    // process image, so nothing after this line executes on success.
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(?e, "Cannot determine current exe for language re-exec");
            apply_language_setlocale(&lang);
            return;
        }
    };

    let args: Vec<String> = std::env::args().collect();

    // Only set LANGUAGE — gettext uses it as the primary lookup mechanism.
    //
    // We deliberately do NOT set LC_MESSAGES (or LC_ALL/LANG) here. Inside
    // a Flatpak sandbox the user's chosen locale (e.g. `fr_FR.UTF-8`) is
    // very often not installed: Flatpak ships only the host system's
    // language via the `org.gnome.Platform.Locale` extension. If we set
    // LC_MESSAGES to an uninstalled locale, glibc falls back to the C
    // locale, and **gettext ignores LANGUAGE when LC_MESSAGES=C**. The
    // result is no translation at all, regardless of the LANGUAGE value.
    //
    // By leaving LC_MESSAGES untouched, the child inherits the system
    // locale (which is always installed) and gettext correctly applies
    // translations selected via LANGUAGE.
    //
    // Issue #158 — language change had no effect in Flatpak builds.
    let err = std::process::Command::new(exe)
        .args(&args[1..])
        .env("LANGUAGE", &lang)
        .env("_RUSTCONN_LANG_SET", "1")
        .exec();

    // exec() only returns on error
    tracing::warn!(?err, "Language re-exec failed; using setlocale fallback");
    apply_language_setlocale(&lang);
}

/// Reads just the `language` field from `~/.config/rustconn/config.toml`.
///
/// Shares the single-key scan with the renderer choice, which faces the same
/// constraint — see [`crate::startup_config`] for why neither can wait for the
/// application's own settings to load.
fn read_language_from_config() -> Option<String> {
    crate::startup_config::read_ui_string("language")
}

/// Checks whether a build-time locale directory actually contains at least
/// one compiled `.mo` file for the `rustconn` domain.
///
/// This prevents stale build-time paths (baked in via `cargo:rustc-env`) from
/// shadowing the real system locale directory in packaged installs.
fn build_locale_has_translations(dir: &str) -> bool {
    let path = std::path::Path::new(dir);
    if !path.is_dir() {
        return false;
    }
    // Expect structure: <dir>/<lang>/LC_MESSAGES/rustconn.mo
    let Ok(entries) = std::fs::read_dir(path) else {
        return false;
    };
    entries.flatten().any(|entry| {
        entry
            .path()
            .join("LC_MESSAGES")
            .join("rustconn.mo")
            .is_file()
    })
}

/// Returns the locale directory path.
///
/// Resolution order:
/// 1. `LOCALEDIR` environment variable (explicit override).
///    Set in the Flatpak manifest to `/app/share/rustconn/locale` so our
///    translations bypass `flatpak-builder`'s automatic split of
///    `/app/share/locale/<lang>/` into per-language Locale extension
///    subsets (issue #158).
/// 2. Build-time locale dir compiled by `build.rs` (`cargo run` development)
/// 3. macOS .app bundle: `Contents/Resources/locale` relative to executable
/// 4. Flatpak `/app/share/locale` (legacy fallback for older builds)
/// 5. Snap `$SNAP/share/locale`
/// 6. User-local `~/.local/share/locale` (install-desktop.sh)
/// 7. `XDG_DATA_HOME/locale`
/// 8. System `/usr/share/locale`
fn locale_dir() -> String {
    // 1. Explicit override
    if let Ok(dir) = std::env::var("LOCALEDIR") {
        return dir;
    }

    // 2. Build-time locale dir (set by build.rs via cargo:rustc-env)
    //    Only use it if the directory actually contains .mo files —
    //    packaged installs (deb/rpm/flatpak) place translations in
    //    /usr/share/locale or /app/share/locale, so the stale build-time
    //    path must not shadow the real system locale directory.
    if let Some(build_locale) = option_env!("RUSTCONN_LOCALE_DIR")
        && !build_locale.is_empty()
        && build_locale_has_translations(build_locale)
    {
        return build_locale.to_string();
    }

    // 3. macOS .app bundle detection
    //    When launched via LaunchServices, LOCALEDIR is not set but the
    //    translations live at .app/Contents/Resources/locale/
    #[cfg(target_os = "macos")]
    if let Some(bundle_locale) = macos_bundle_locale_dir() {
        return bundle_locale;
    }

    // 4. Flatpak
    if std::path::Path::new("/app/share/locale").exists() {
        return "/app/share/locale".to_string();
    }

    // 5. Snap
    if let Ok(snap) = std::env::var("SNAP") {
        let snap_locale = format!("{snap}/share/locale");
        if std::path::Path::new(&snap_locale).exists() {
            return snap_locale;
        }
    }

    // 6. User-local install (install-desktop.sh)
    if let Ok(home) = std::env::var("HOME") {
        let local_locale = format!("{home}/.local/share/locale");
        if build_locale_has_translations(&local_locale) {
            return local_locale;
        }
    }

    // 7. XDG_DATA_HOME fallback
    if let Ok(xdg_data) = std::env::var("XDG_DATA_HOME") {
        let xdg_locale = format!("{xdg_data}/locale");
        if build_locale_has_translations(&xdg_locale) {
            return xdg_locale;
        }
    }

    // 8. System default
    "/usr/share/locale".to_string()
}

/// Detects the locale directory inside a macOS .app bundle.
///
/// When the executable is at `RustConn.app/Contents/MacOS/rustconn`,
/// translations are at `RustConn.app/Contents/Resources/locale/`.
#[cfg(target_os = "macos")]
fn macos_bundle_locale_dir() -> Option<String> {
    let exe_path = std::env::current_exe().ok()?;
    // exe is at .app/Contents/MacOS/rustconn
    let macos_dir = exe_path.parent()?;
    let contents_dir = macos_dir.parent()?;
    let bundle_dir = contents_dir.parent()?;

    // Verify this looks like a .app bundle
    let bundle_ext = bundle_dir.extension().and_then(|e| e.to_str())?;
    if !bundle_ext.eq_ignore_ascii_case("app") {
        return None;
    }

    let locale_dir = contents_dir.join("Resources").join("locale");
    if build_locale_has_translations(&locale_dir.to_string_lossy()) {
        Some(locale_dir.to_string_lossy().into_owned())
    } else {
        None
    }
}

/// Translates a string using gettext.
#[inline]
pub fn i18n(msgid: &str) -> String {
    gettext(msgid)
}

/// Translates a string with format arguments.
///
/// Replaces `{}` placeholders left-to-right with the provided arguments.
///
/// # Example
///
/// ```ignore
/// let msg = i18n_f("Deleted '{}'", &[&connection_name]);
/// ```
pub fn i18n_f(msgid: &str, args: &[&str]) -> String {
    let mut result = gettext(msgid);
    for arg in args {
        if let Some(pos) = result.find("{}") {
            result.replace_range(pos..pos + 2, arg);
        }
    }
    result
}

/// Translates a string with singular/plural forms.
///
/// # Example
///
/// ```ignore
/// let msg = ni18n("{} connection", "{} connections", count);
/// ```
#[inline]
pub fn ni18n(singular: &str, plural: &str, n: u32) -> String {
    ngettext(singular, plural, n)
}

/// Translates a string with singular/plural forms and format arguments.
pub fn ni18n_f(singular: &str, plural: &str, n: u32, args: &[&str]) -> String {
    let mut result = ngettext(singular, plural, n);
    for arg in args {
        if let Some(pos) = result.find("{}") {
            result.replace_range(pos..pos + 2, arg);
        }
    }
    result
}

/// Available languages with their display names.
///
/// Returns a list of `(locale_code, display_name)` pairs.
/// The first entry is always `("system", "System")` for auto-detection.
#[must_use]
pub fn available_languages() -> Vec<(&'static str, &'static str)> {
    vec![
        ("system", "System"),
        ("be", "Беларуская"),
        ("cs", "Čeština"),
        ("da", "Dansk"),
        ("de", "Deutsch"),
        ("en", "English"),
        ("es", "Español"),
        ("fr", "Français"),
        ("it", "Italiano"),
        ("kk", "Қазақша"),
        ("nl", "Nederlands"),
        ("pl", "Polski"),
        ("pt", "Português"),
        ("sk", "Slovenčina"),
        ("sv", "Svenska"),
        ("uk", "Українська"),
        ("zh-cn", "简体中文"),
    ]
}

/// Maps a short language code to its full locale identifier.
///
/// Linux `setlocale` requires the full `ll_CC.UTF-8` form (e.g. `uk_UA.UTF-8`),
/// not just the language code (`uk`). This function provides the mapping.
fn lang_to_locale(lang: &str) -> String {
    let full = match lang {
        "be" => "be_BY",
        "cs" => "cs_CZ",
        "da" => "da_DK",
        "de" => "de_DE",
        "en" => "en_US",
        "es" => "es_ES",
        "fr" => "fr_FR",
        "it" => "it_IT",
        "kk" => "kk_KZ",
        "nl" => "nl_NL",
        "pl" => "pl_PL",
        "pt" => "pt_PT",
        "sk" => "sk_SK",
        "sv" => "sv_SE",
        "uk" => "uk_UA",
        "zh-cn" => "zh_CN",
        other => other,
    };
    format!("{full}.UTF-8")
}

/// Applies a language override using `setlocale` only (best effort).
///
/// It works when the target locale is installed on the system. For full gettext
/// support (including uninstalled locales), the `LANGUAGE` env var must be set
/// before process start — see [`apply_language_from_config`], which handles that
/// via re-exec. This function is still needed alongside `LANGUAGE`, because
/// gettext ignores `LANGUAGE` when `LC_MESSAGES` resolves to `"C"` (issue #158).
///
/// # Safety-adjacent invariant
///
/// Callable only from [`apply_configured_language`], which runs in `main()`
/// before any thread is spawned. `setlocale` writes process-global locale state
/// without synchronisation, so calling this once other threads exist is unsound
/// (RUSTSEC-2026-0244). [`rustconn_locale_sys::init_locale`] checks that and
/// panics rather than proceeding, but keep it private and keep the single call
/// site anyway: a panic at startup is still a broken build.
fn apply_language_setlocale(lang: &str) {
    use rustconn_locale_sys::{LocaleCategory, init_locale};

    /// Applies the environment's `LC_MESSAGES`, reporting a failure.
    ///
    /// This is the last resort on every path through
    /// [`apply_language_setlocale`], so unlike the startup call in [`init`] the
    /// outcome is worth a line: if even the inherited locale cannot be applied,
    /// `LC_MESSAGES` keeps whatever it had and gettext may serve untranslated
    /// strings (issue #158).
    ///
    /// Best effort, like the other `tracing` calls in this module: `main()`
    /// installs the subscriber after `apply_language_from_config()`, so the
    /// record is only seen if that order ever changes. Locale setup has to run
    /// first — it is single-threaded-only — so the alternative is not reporting
    /// at all.
    fn apply_system_messages_locale() {
        if init_locale(LocaleCategory::LcMessages, "").is_none() {
            tracing::warn!(
                "setlocale(LC_MESSAGES, \"\") failed: the locale named by the environment \
                 is not installed. Keeping the previous LC_MESSAGES; menus and messages \
                 may stay untranslated."
            );
        }
    }

    if lang == "system" || lang.is_empty() {
        apply_system_messages_locale();
    } else {
        let full_locale = lang_to_locale(lang);
        let result = init_locale(LocaleCategory::LcMessages, full_locale.as_str());
        if result.is_none() {
            tracing::info!(
                lang,
                "Locale {full_locale} not installed; \
                 falling back to system locale (LANGUAGE env var still applies)"
            );
            // Fall back to the system locale ("") rather than a hardcoded
            // en_US.UTF-8: in Flatpak sandboxes en_US.UTF-8 is itself often
            // not installed, which would leave LC_MESSAGES=C and disable
            // gettext's LANGUAGE lookup entirely (issue #158). The system
            // locale inherited from the host is guaranteed to exist.
            apply_system_messages_locale();
        }
    }

    // Re-bind domain so gettext picks up the new locale
    let locale_dir = locale_dir();
    // best-effort: bindtextdomain returns the bound directory or NULL on
    // OOM. There is nothing the app can do here, and falling back to the
    // previous binding is acceptable.
    let _ = gettextrs::bindtextdomain(GETTEXT_DOMAIN, locale_dir);
    let _ = gettextrs::bind_textdomain_codeset(GETTEXT_DOMAIN, "UTF-8");
    let _ = gettextrs::textdomain(GETTEXT_DOMAIN);
}

// A public `apply_language()` used to exist here so the GTK `activate` handler
// could re-apply the saved language after the app state was built. It was
// removed in 0.19.19: by that point the GIO worker thread is running, and
// `setlocale` is unsound once the process is multi-threaded
// (RUSTSEC-2026-0244). It was also redundant — `apply_language_from_config()`
// now applies the locale on every path from `main()`, before any thread starts,
// including the case that call was there to rescue (LANGUAGE set but
// LC_MESSAGES stuck at "C" inside a Flatpak sandbox, issue #158).
//
// Re-adding it would now panic on the first run instead of quietly corrupting
// locale state: `setlocale` lives behind `rustconn_locale_sys::init_locale`, and
// the locale is sealed at the end of `apply_language_from_config()`.
//
// Changing the language from the Settings dialog still only persists the value
// to `config.toml`; it takes effect on the next start. That was already true —
// the old runtime call could not retranslate already-rendered GTK labels.
