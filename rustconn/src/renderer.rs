//! Chooses the GSK renderer before GTK starts.
//!
//! GTK exposes no API for selecting a renderer: `GSK_RENDERER` is the only
//! interface, and it is read while the first surface is realised, so it has to
//! be in the environment before `gtk_init`. Writing the environment that early
//! is what [`rustconn_env_sys`] exists for — see that crate for why the write is
//! sound only here, in `main()`, before any thread of ours is running.
//!
//! # What the automatic choice is for
//!
//! GTK's GPU renderer is the right default almost everywhere. Two environments
//! are known exceptions, and both look like a broken application rather than a
//! broken driver, so RustConn opts out of the GPU path on their behalf:
//!
//! * **X11 sessions.** On MATE, XFCE and older Mutter, popovers and menus paint
//!   blank until the pointer moves over them
//!   ([#85](https://github.com/totoshko88/RustConn/issues/85)). This was the
//!   original reason the fallback exists; it used to be applied by re-execing
//!   the process with the variable set.
//! * **macOS guests under a hypervisor.** Apple's paravirtualised GPU gives the
//!   guest Metal but no accelerated OpenGL, so GSK's GL renderer ends up on a
//!   software path that is both slower than Cairo and CPU-hungry: input lag,
//!   late frames, stuttering scroll
//!   ([#274](https://github.com/totoshko88/RustConn/issues/274)). Homebrew's
//!   `gtk4` is built with `-Dvulkan=disabled`, so Cairo is the only alternative
//!   there — there is no Vulkan-over-Metal path to fall back to.
//!
//! Both are heuristics about the environment, not facts about the user's
//! preference, so `Settings ▸ Interface ▸ Rendering` overrides them in either
//! direction and an explicit `GSK_RENDERER` in the environment outranks
//! everything.

use rustconn_core::config::RendererPreference;

/// The GSK renderer name for software rasterisation.
///
/// GTK still accepts `cairo` as a `GSK_RENDERER` value; it is the documented
/// fallback renderer, and unlike `opengl`/`ngl`/`gl` its spelling has survived
/// every renderer reshuffle since GTK 4.0.
const SOFTWARE_RENDERER: &str = "cairo";

/// Why the automatic choice landed on software rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SoftwareReason {
    /// X11 session whose compositor paints GTK4 popovers blank until hovered.
    X11Popovers,
    /// Guest OS whose virtual GPU has no accelerated OpenGL.
    HypervisorGuest,
}

impl SoftwareReason {
    /// Returns the phrase logged next to the renderer choice.
    const fn as_log_reason(self) -> &'static str {
        match self {
            Self::X11Popovers => "X11 session: GTK4 GPU renderer paints popovers blank (#85)",
            Self::HypervisorGuest => {
                "guest VM: paravirtualised GPU has no accelerated OpenGL (#274)"
            }
        }
    }
}

/// What the automatic choice gets to look at.
///
/// Gathered separately from the decision so that [`decide`] is a pure function
/// and can be tested for every combination, including the ones the host running
/// the test suite cannot produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Probe {
    /// `Some(true)` if the OS reports a hypervisor above us, `Some(false)` if it
    /// reports none, `None` if the question could not be asked on this platform.
    hypervisor: Option<bool>,
    /// A session GTK will drive through X11: `DISPLAY` set, `WAYLAND_DISPLAY` not.
    x11_session: bool,
}

/// Decides whether the automatic setting should force software rendering.
const fn decide(probe: Probe) -> Option<SoftwareReason> {
    // Checked first: inside a guest the GPU path is slow whatever the session
    // type, whereas the X11 answer is about one specific painting bug.
    if matches!(probe.hypervisor, Some(true)) {
        return Some(SoftwareReason::HypervisorGuest);
    }
    if probe.x11_session {
        return Some(SoftwareReason::X11Popovers);
    }
    None
}

/// Applies the saved renderer preference, then seals the startup environment.
///
/// Call once from `main()`, after the tracing subscriber is installed — so the
/// decision is visible in the log — and before anything spawns a thread or
/// touches GTK.
///
/// # Panics
///
/// Panics if called after the startup environment has been sealed, or from a
/// thread other than the one `main()` runs on. Both mean an environment write
/// escaped the startup window; see [`rustconn_env_sys`] for the contract.
pub fn apply_renderer_preference() {
    select_renderer();

    // The startup environment is final. Nothing in the rest of the process
    // lifetime may write it — the Settings dialog only persists the choice for
    // the next start — so close the window `rustconn-env-sys` guards.
    rustconn_env_sys::seal_env();
}

/// Selects the renderer, leaving `GSK_RENDERER` untouched when GTK's default wins.
///
/// Split from [`apply_renderer_preference`] so that each of its early returns is
/// followed by the seal without repeating the call.
fn select_renderer() {
    // An explicit value — from the user, the desktop session, a distribution
    // profile or a packaging wrapper — outranks anything decided here.
    if let Ok(existing) = std::env::var("GSK_RENDERER") {
        tracing::debug!(
            renderer = %existing,
            "GSK_RENDERER already set in the environment; leaving the renderer choice alone"
        );
        return;
    }

    let preference = read_renderer_preference();

    match preference {
        RendererPreference::Software => {
            rustconn_env_sys::set_startup_var("GSK_RENDERER", SOFTWARE_RENDERER);
            tracing::info!(
                renderer = SOFTWARE_RENDERER,
                reason = "user preference",
                "Selected GSK renderer"
            );
        }
        RendererPreference::Gpu => {
            tracing::debug!(
                reason = "user preference",
                "Keeping GTK's default GPU renderer"
            );
        }
        RendererPreference::Auto => match decide(probe_environment()) {
            Some(reason) => {
                rustconn_env_sys::set_startup_var("GSK_RENDERER", SOFTWARE_RENDERER);
                tracing::info!(
                    renderer = SOFTWARE_RENDERER,
                    reason = reason.as_log_reason(),
                    "Selected GSK renderer"
                );
            }
            None => {
                tracing::debug!("Keeping GTK's default GPU renderer");
            }
        },
    }
}

/// Reads the saved preference, falling back to [`RendererPreference::Auto`].
///
/// The value is read straight out of `config.toml` rather than from
/// `AppSettings`: this runs before the application state exists. An
/// unrecognised value is treated as unset, which is what an older RustConn
/// writing a variant this build does not know would produce.
fn read_renderer_preference() -> RendererPreference {
    let Some(raw) = crate::startup_config::read_ui_string("renderer") else {
        return RendererPreference::Auto;
    };

    // Kept in step with the `#[serde(rename_all = "snake_case")]` spelling on
    // `RendererPreference`; `settings.rs` has a round-trip test over the same
    // three strings.
    match raw.as_str() {
        "software" => RendererPreference::Software,
        "gpu" => RendererPreference::Gpu,
        "auto" => RendererPreference::Auto,
        other => {
            tracing::warn!(
                value = %other,
                "Unknown renderer preference in config; using the automatic choice"
            );
            RendererPreference::Auto
        }
    }
}

/// Gathers the facts [`decide`] works from, for this platform.
fn probe_environment() -> Probe {
    Probe {
        hypervisor: hypervisor_present(),
        // On macOS GTK uses the Quartz backend, so `DISPLAY` — which XQuartz
        // sets for the SSH askpass helper (#161) — says nothing about how GTK
        // will paint. The X11 heuristic applies to the platforms where GTK
        // really can be on X11.
        x11_session: !cfg!(target_os = "macos")
            && std::env::var_os("DISPLAY").is_some()
            && std::env::var_os("WAYLAND_DISPLAY").is_none(),
    }
}

/// Asks the OS whether a hypervisor is running above us.
///
/// Only macOS is asked. On Linux the X11 heuristic already covers the
/// virtualised case that matters (a VM without a GPU driver ends up on llvmpipe,
/// and llvmpipe under the GPU renderer is a different, better-behaved tradeoff
/// than Apple's software OpenGL), and adding a second probe there would change
/// the renderer for every Wayland VM user without a report asking for it.
#[cfg(target_os = "macos")]
fn hypervisor_present() -> Option<bool> {
    // `kern.hv_vmm_present` is Darwin's own answer to "am I a guest": 1 under a
    // hypervisor, 0 on bare metal. Absolute path because `PATH` at this point is
    // whatever launched us — a Finder launch has almost none of it.
    //
    // A process spawn costs a few milliseconds once per start. The alternative,
    // `sysctlbyname` through `libc`, would mean a second `unsafe` block and a
    // hand-written C signature in a crate whose whole purpose is to keep the
    // unsafe surface small.
    let output = std::process::Command::new("/usr/sbin/sysctl")
        .args(["-n", "kern.hv_vmm_present"])
        .output()
        .ok()?;

    if !output.status.success() {
        // An unknown key (or no sysctl at all) is "cannot tell", not "bare
        // metal": the caller must not downgrade the renderer on a guess.
        tracing::debug!(
            status = ?output.status.code(),
            "sysctl kern.hv_vmm_present unavailable; assuming no hypervisor"
        );
        return None;
    }

    match String::from_utf8_lossy(&output.stdout).trim() {
        "1" => Some(true),
        "0" => Some(false),
        other => {
            tracing::debug!(
                value = %other,
                "Unexpected kern.hv_vmm_present value; ignoring it"
            );
            None
        }
    }
}

/// Returns `None`: no non-macOS platform is probed. See the macOS variant.
#[cfg(not(target_os = "macos"))]
const fn hypervisor_present() -> Option<bool> {
    None
}

#[cfg(test)]
mod tests {
    use super::{Probe, SoftwareReason, decide, probe_environment};

    /// A guest is a guest whatever the session type — this is the #274 case.
    #[test]
    fn a_hypervisor_guest_gets_software_rendering() {
        assert_eq!(
            decide(Probe {
                hypervisor: Some(true),
                x11_session: false,
            }),
            Some(SoftwareReason::HypervisorGuest)
        );
    }

    /// The #85 case, unchanged by this module's arrival.
    #[test]
    fn an_x11_session_gets_software_rendering() {
        assert_eq!(
            decide(Probe {
                hypervisor: Some(false),
                x11_session: true,
            }),
            Some(SoftwareReason::X11Popovers)
        );
    }

    /// Bare metal on Wayland (or macOS) keeps the GPU renderer — the default
    /// this whole module must not disturb.
    #[test]
    fn bare_metal_without_x11_keeps_the_gpu_renderer() {
        assert_eq!(
            decide(Probe {
                hypervisor: Some(false),
                x11_session: false,
            }),
            None
        );
    }

    /// "Cannot tell" must read as "no hypervisor", never as "guest": a failed
    /// probe on bare metal must not cost the user their GPU renderer.
    #[test]
    fn an_unanswerable_hypervisor_probe_does_not_force_software() {
        assert_eq!(
            decide(Probe {
                hypervisor: None,
                x11_session: false,
            }),
            None
        );
    }

    /// When both apply, the guest reason is reported — it is the one that
    /// explains the whole session rather than one painting bug.
    #[test]
    fn the_guest_reason_wins_over_x11() {
        assert_eq!(
            decide(Probe {
                hypervisor: Some(true),
                x11_session: true,
            }),
            Some(SoftwareReason::HypervisorGuest)
        );
    }

    /// macOS never takes the X11 route: GTK is on Quartz there, so a `DISPLAY`
    /// left behind by XQuartz must not be read as an X11 session (#161).
    #[test]
    fn macos_is_never_treated_as_an_x11_session() {
        let probe = probe_environment();

        if cfg!(target_os = "macos") {
            assert!(!probe.x11_session);
        } else {
            // Off macOS the field is whatever this machine's session is; the
            // assertion that carries information is the one above.
            assert_eq!(probe.hypervisor, None, "only macOS is probed");
        }
    }
}
