---
inclusion: always
---

# RustConn — Core Rules

The invariants that must be in context for *every* task. Code philosophy,
workflow and escape hatches live in `project-rules.md` (`inclusion: manual`, load
with `#project-rules`); terminal and cargo discipline lives in
`shell-environment.md`, which is `inclusion: always` and therefore already
loaded. Neither is repeated here. One source of truth per rule.

Communication language: Ukrainian.

## Architecture (7 crates)

| Crate | Purpose | Restrictions |
|-------|---------|-------------|
| `rustconn-core` | Domain logic: models, config, CRUD managers, import/export, protocol data, credential abstractions | **FORBIDDEN**: gtk4, adw, vte4. Default features stay headless. |
| `rustconn-cli` | Headless management over core data | Only rustconn-core. Default features minimal. |
| `rustconn` | GTK4/libadwaita GUI, dialogs, embedded/external session presentation | May import GUI crates |
| `rustconn-pty-sys` | Isolated FFI: macOS PTY controlling terminal (`setsid`+`TIOCSCTTY`) | Sanctioned `unsafe` (M-UNSAFE); `libc` only |
| `rustconn-locale-sys` | Isolated FFI: startup `setlocale`, refused once *this program* spawns a thread of its own (baseline growth, Linux only), once a call arrives from another thread, or after sealing | Sanctioned `unsafe` (M-UNSAFE); `gettext-rs` only |
| `rustconn-env-sys` | Isolated FFI: the startup `GSK_RENDERER` and `LANGUAGE` writes, guarded the same way | Sanctioned `unsafe` (M-UNSAFE); no dependencies |
| `rustconn-dock-sys` | Isolated FFI: the macOS Dock tile image via `-[NSApplication setApplicationIconImage:]`, for launches with no `.app` behind them. Main-thread proof via `objc2::MainThreadMarker`; a violation is an outcome, not a panic — a wrong Dock tile is cosmetic | Sanctioned `unsafe` (M-UNSAFE); `objc2` + AppKit bindings, macOS-gated |

Every `-sys` crate is an **unconditional** workspace member and an
unconditional dependency, with `#[cfg]` inside where the platform differs. That
is what gets the `unsafe` and its contract tests compiled by CI — no CI job
builds macOS, so making the *crate* macOS-only would mean an `unsafe` block
nothing ever checks.

`rustconn-dock-sys` is the one crate whose **dependencies** are target-gated
(`[target.'cfg(target_os = "macos")'.dependencies]`), because `objc2-app-kit`
does not compile off Apple platforms — there is no cross-platform stand-in for
AppKit the way `std::env::set_var` is one for `setenv`. The crate, its API and
its precondition guard are still built and tested everywhere; only the AppKit
call is gated, and its docs say so under "What CI actually checks" instead of
implying the whole crate is covered. Reach for that shape only when the bindings
genuinely cannot build elsewhere, and verify the gated path with
`cargo clippy -p <crate> --target aarch64-apple-darwin` (a `rustup target add`
away — the objc2 bindings are pure Rust, so `check`/`clippy` work without an SDK;
only linking needs one).

New FFI gets its own `rustconn-*-sys` crate — never an `unsafe` exception where
the caller lives.

## Absolute Rules

- No `unsafe` outside the `rustconn-*-sys` crates. Mechanically:
  `unsafe_code = "deny"` in `[workspace.lints.rust]`, re-opened by a crate-level
  `#![expect(unsafe_code, reason = "…")]` in each of the three helpers. It is
  `deny` and not `forbid` on purpose — `forbid` cannot be overridden, so the
  helpers had to declare their own `[lints]` table, which *replaces* the
  inherited one and left the only crates allowed to write `unsafe` as the only
  crates with no clippy lints at all. `rustconn` keeps a local `forbid`.
- Passwords/keys → `secrecy::SecretString`, never plain `String`
- Intermediate `expose_secret().to_string()` → wrap in `zeroize::Zeroizing::new()`
- Errors → `thiserror::Error`. No `unwrap()`/`expect()` in production code; both
  are fine in tests and `#[cfg(test)]` modules (`.clippy.toml` sets
  `allow-unwrap-in-tests = true`)
- Logging → `tracing`, never `println!`/`eprintln!`
- i18n → `i18n()` / `i18n_f()` with `{}` placeholders for all user-facing strings
- `display_name()` values used in UI → wrap in `i18n()` at the call site
- After new i18n strings → `bash po/update-pot.sh` + `msgmerge --update` (17 languages)
- Rust 2024 edition: let-chains instead of collapsible_if
- Never `set_var`/`remove_var` (unsafe in Rust 2024). The one exception is
  `rustconn-env-sys::set_startup_var`, which may only be called from `main()`
  before this program starts a thread and panics if it is not — reach for it only
  when a C library reads the variable later and offers no API, as GTK does for
  `GSK_RENDERER` and gettext does for `LANGUAGE`. Those two are the only callers;
  the second one seals the window, so a third added later panics rather than
  quietly working

## Definition of Done

A task is done ONLY when all hold. This is the finish line for `/goal` loops and
any self-verification:

1. `cargo clippy --all-targets` → 0 warnings, **and the run actually re-checked**
   (a cache hit prints `Finished ... in 0.2s` and reports zero warnings without
   looking at anything)
2. `cargo test --workspace` green (or the targeted tests for the change)
3. Crate boundaries intact (no gtk4/adw/vte4 in core/cli, no `unsafe` outside `*-sys`)
4. New user-facing strings wrapped in `i18n()`/`i18n_f()` + POT updated
5. No debug leftovers (`dbg!`/`todo!`/`println!`/`eprintln!`)
6. `CHANGELOG.md` updated for any user-facing change (Keep a Changelog format —
   see `changelog-format.md`)

If a goal-loop can't reach this within its iterations, STOP and report what is
blocking — never loosen the gate to "finish". Sanctioned workarounds for
*external* blockers only: see the escape-hatch table in `project-rules.md`.

## Releases: prepare, never cut

An agent prepares a release and validates it. The maintainer cuts it.

- `./scripts/release.sh --dry-run` is the agent action, and it is expected: it
  runs every gate and stops before the plan is executed.
- **Never** run `release.sh` without `--dry-run`, and never pass `--yes`. That
  flag exists so a *human* can confirm without a prompt; an agent shell has no
  TTY, so passing it means the agent has appointed itself the person who decides.
- **Never** do it by hand either — no `git tag v<x.y.z>`, no pushing a release
  tag. The tag push is what triggers the Release workflow, the artifact build and
  the Flathub/OBS/Snap updates.
- Finish by reporting: the gate list from the dry run, the diff, and what is left
  to decide. Then stop.

Why this is a rule and not a preference: v0.20.1 was cut by an agent with
`release.sh --yes`. It merged to main, pushed a tag and published a GitHub
release with five artifacts, carrying a red CI job and code deletions the
maintainer had never read. Undoing it meant deleting a published release. The
`release-manual-only-guard` hook blocks all three routes, but do not rely on it.

## Quick Commands

```
cargo fmt --all                    # Format
cargo clippy --all-targets         # Lint (0 warnings; never --all-features)
cargo test --workspace             # 3843 tests, ~45s of test time (~2.5 min with compile)
typos                              # Spell check (config: typos.toml)
bash po/update-pot.sh              # Regenerate POT after new i18n strings
```

Delegate fmt+clippy+tests to the `rust-quality-check` sub-agent rather than
running them in the main context. For quick single-file validation →
`getDiagnostics`.
