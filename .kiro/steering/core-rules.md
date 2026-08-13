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

## Architecture (6 crates)

| Crate | Purpose | Restrictions |
|-------|---------|-------------|
| `rustconn-core` | Domain logic: models, config, CRUD managers, import/export, protocol data, credential abstractions | **FORBIDDEN**: gtk4, adw, vte4. Default features stay headless. |
| `rustconn-cli` | Headless management over core data | Only rustconn-core. Default features minimal. |
| `rustconn` | GTK4/libadwaita GUI, dialogs, embedded/external session presentation | May import GUI crates |
| `rustconn-pty-sys` | Isolated FFI: macOS PTY controlling terminal (`setsid`+`TIOCSCTTY`) | Sanctioned `unsafe` (M-UNSAFE); `libc` only |
| `rustconn-locale-sys` | Isolated FFI: startup `setlocale`, refused once *this program* spawns a thread of its own (baseline growth, Linux only), once a call arrives from another thread, or after sealing | Sanctioned `unsafe` (M-UNSAFE); `gettext-rs` only |
| `rustconn-env-sys` | Isolated FFI: the startup `GSK_RENDERER` and `LANGUAGE` writes, guarded the same way | Sanctioned `unsafe` (M-UNSAFE); no dependencies |

Every `-sys` crate is an **unconditional** workspace member and an
unconditional dependency, with `#[cfg]` inside where the platform differs. That
is what gets the `unsafe` and its contract tests compiled by CI — no CI job
builds macOS, so a `[target.'cfg(target_os = "macos")'.dependencies]` entry
would mean an `unsafe` block nothing ever checks.

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
- After new i18n strings → `bash po/update-pot.sh` + `msgmerge --update` (16 languages)
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

## Quick Commands

```
cargo fmt --all                    # Format
cargo clippy --all-targets         # Lint (0 warnings; never --all-features)
cargo test --workspace             # Tests (~120s, argon2 is slow)
typos                              # Spell check (config: typos.toml)
bash po/update-pot.sh              # Regenerate POT after new i18n strings
```

Delegate fmt+clippy+tests to the `rust-quality-check` sub-agent rather than
running them in the main context. For quick single-file validation →
`getDiagnostics`.
