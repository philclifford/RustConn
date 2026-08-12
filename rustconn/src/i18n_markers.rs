//! Static i18n markers for strings that are passed dynamically at runtime.
//!
//! These strings come from `rustconn-core` (predefined templates, category names)
//! and are wrapped in `i18n()` at the call site in the GUI, but xgettext cannot
//! extract them because the argument is a variable, not a string literal.
//!
//! This file is never executed — it exists solely so that `xgettext --keyword=i18n`
//! picks up these strings during POT generation.
//!
//! Keep in sync with:
//! - `rustconn-core/src/template/predefined.rs` (descriptions + category names)
//! - `rustconn-core/src/models/protocol.rs` (`display_name()` of the RDP/VNC
//!   client-mode and performance-mode enums, and of `BackspaceSends` /
//!   `DeleteSends`)

#![allow(
    dead_code,
    unreachable_code,
    reason = "module-wide override for legacy code; refactored case by case"
)]

fn _never_called() {
    return;

    // === Template category display names ===
    crate::i18n::i18n("Remote Desktop");
    crate::i18n::i18n("Containers");
    crate::i18n::i18n("Virtualization");
    crate::i18n::i18n("Hardware");
    crate::i18n::i18n("Cloud Access");
    crate::i18n::i18n("Automation");

    // === Predefined template descriptions ===
    crate::i18n::i18n("Remote desktop via RustDesk");
    crate::i18n::i18n("Remote desktop via AnyDesk");
    crate::i18n::i18n("Open Remmina connection file");
    crate::i18n::i18n("Shell into Docker container");
    crate::i18n::i18n("Shell into Podman container");
    crate::i18n::i18n("Shell into LXC instance");
    crate::i18n::i18n("Shell into Incus instance");
    crate::i18n::i18n("Enter Distrobox container");
    crate::i18n::i18n("Serial console to libvirt VM");
    crate::i18n::i18n("Terminal to Proxmox QEMU VM");
    crate::i18n::i18n("Enter Proxmox LXC container");
    crate::i18n::i18n("Serial-over-LAN via IPMI");
    crate::i18n::i18n("Serial port (ESP32, Arduino, etc.)");
    crate::i18n::i18n("BMC management via Redfish");
    crate::i18n::i18n("Bring up VPN then SSH");
    crate::i18n::i18n("Access internal app via Teleport");
    crate::i18n::i18n("Web console for Linux servers");
    crate::i18n::i18n("Ad-hoc command on remote host");
    crate::i18n::i18n("Wake server then connect");
    crate::i18n::i18n("Remote Nix build via SSH");

    // === RDP/VNC dropdown labels from `display_name()` ===
    // The call sites already wrap these in `i18n()`, but the literals live in
    // rustconn-core, which `po/update-pot.sh` does not scan — without a marker
    // here the labels stayed English in every locale.
    crate::i18n::i18n("External RDP client");
    crate::i18n::i18n("External VNC client");
    crate::i18n::i18n("Quality (RemoteFX)");
    crate::i18n::i18n("Balanced (Adaptive)");
    crate::i18n::i18n("Speed (Legacy)");
    crate::i18n::i18n("Balanced");
    crate::i18n::i18n("Speed");

    // === Erase-mode dropdown labels from `display_name()` ===
    // `BackspaceSends`/`DeleteSends` build their dropdowns with
    // `i18n(mode.display_name())`, which xgettext cannot follow, so the labels
    // are extracted here. The two Automatic labels differ because they name
    // different bytes/sequences.
    //
    // The exact spellings are pinned by `erase_mode_display_names_are_stable`
    // in `rustconn-core/src/models/protocol.rs` — change them there first.
    crate::i18n::i18n("Automatic (^?)");
    crate::i18n::i18n("Automatic (\\e[3~)");
    crate::i18n::i18n("Backspace (^H)");
    crate::i18n::i18n("Delete (^?)");
}
