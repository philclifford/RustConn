# AGENTS.md — RustConn

Instructions for AI coding agents. RustConn is a GTK4/libadwaita connection
manager for SSH, RDP, VNC, SPICE, Telnet, Serial, Kubernetes and Zero Trust
brokers. Rust 2024 edition, MSRV 1.95, Wayland-first, Linux and macOS.

Communication language with the maintainer: **Ukrainian**.

## Why this file is short

The full rule set lives in `.kiro/steering/*.md` and is loaded automatically by
Kiro. This file exists because agents that do **not** read `.kiro/steering/`
(Codex, Copilot, Cursor, Zed, and Kiro CLI, which ignores inclusion modes) were
otherwise working without any of it. It carries the invariants that are
expensive to get wrong and points at the rest instead of copying it — two copies
of a rule drift, and this repo has been bitten by that before.

Source of truth, in order:

| What | Where |
|------|-------|
| Invariants, crate table, Definition of Done | `.kiro/steering/core-rules.md` |
| Code philosophy, workflow, escape hatches | `.kiro/steering/project-rules.md` |
| GNOME HIG adaptation (GUI work) | `.kiro/steering/gnome-hig.md` |
| Microsoft Pragmatic Rust adaptation | `.kiro/steering/rust-pragmatic-guidelines.md` |
| Compiler-error playbook | `.kiro/steering/error-resolution.md` |
| Credential handling | `.kiro/steering/secrets-guide.md` |
| Architecture overview | `docs/ARCHITECTURE.md` |

## Commands

```bash
cargo fmt --all                                   # format
cargo clippy --all-targets                        # lint — must be 0 warnings
cargo test --workspace                            # ~120s; argon2 is slow, not hung
cargo test -p rustconn-core --test property_tests  # property tests only
typos                                             # spell check (typos.toml)
cargo machete                                     # unused dependencies
bash po/update-pot.sh                             # after adding i18n strings
./scripts/check-potfiles.sh                       # POTFILES.in consistency (CI gate)
./scripts/check-i18n-escapes.sh                   # no \u{...} in translatable literals
./scripts/check-po-complete.sh                    # no fuzzy/missing translations
```

**Never** `cargo clippy --all-features` — it enables a gtk3 path that fails at
build time on missing `gdk-3.0.pc`. Use `--all-targets`.

**Never** pipe cargo output through `tail`/`grep`/`head`; redirect to a file and
read the file. **Never** run two cargo commands at once — check `pgrep -f cargo`
first. A repeat `cargo clippy` with nothing changed prints `Finished ... in 0.2s`
and reports zero warnings **without checking anything**; force a real re-check
before claiming it passed.

The toolchain is pinned in `rust-toolchain.toml`. MSRV is a separate, older
number in `Cargo.toml` (`rust-version`).

## Crate boundaries — the rule most often broken

| Crate | May import GTK? | Notes |
|-------|-----------------|-------|
| `rustconn-core` | **No** | Domain logic. `gtk4`, `adw`, `vte4` are forbidden. Runtime integrations stay behind features. |
| `rustconn-cli` | **No** | Depends only on `rustconn-core`. |
| `rustconn` | Yes | All GUI, dialogs, session presentation. |
| `rustconn-pty-sys` | No | Isolated FFI: macOS PTY controlling terminal. |
| `rustconn-locale-sys` | No | Isolated FFI: startup `setlocale`. |
| `rustconn-env-sys` | No | Isolated FFI: startup `GSK_RENDERER` and `LANGUAGE` writes. |

No `unsafe` outside the three `*-sys` crates: `unsafe_code = "deny"` in
`[workspace.lints.rust]`, re-opened by a crate-level
`#![expect(unsafe_code, reason = "…")]` in each helper. `deny` and not `forbid`
because `forbid` cannot be overridden, which would stop the helpers inheriting
the workspace clippy set. New FFI
gets its own `rustconn-*-sys` crate — never an exception where the caller lives,
and never a platform-gated dependency: no CI job builds macOS, so a
macOS-only `-sys` crate would hold `unsafe` that nothing compiles.
A pre-write hook blocks violations of both rules, but do not rely on it.

## Non-negotiable

- Passwords, keys, tokens → `secrecy::SecretString`, never `String`
- Intermediate `expose_secret().to_string()` → wrap in `zeroize::Zeroizing::new()`
- Secrets to external CLIs → stdin pipe, **never** `Command::arg(password)`
- Never log or format a secret into an error message
- Errors → `thiserror::Error`. No `unwrap()`/`expect()` outside tests
- Logging → `tracing`, never `println!`/`eprintln!`
- Every user-facing string → `i18n()` / `i18n_f()` with `{}` placeholders,
  then `bash po/update-pot.sh`. 16 locales: be, cs, da, de, es, fr, it, kk, nl,
  pl, pt, sk, sv, uk, uz, zh-cn
- Icon-only buttons need both a tooltip and an accessible label
- Never `std::env::set_var`/`remove_var` (unsafe in Rust 2024) — the sole
  exception is `rustconn-env-sys::set_startup_var`, callable only from `main()`
  before this program starts a thread. Its two callers are `LANGUAGE` (i18n.rs)
  and `GSK_RENDERER` (renderer.rs), in that order; the second seals the window
- Prefer `adw::` widgets; `adw::AlertDialog`, not the deprecated `gtk::MessageDialog`

## Definition of done

1. `cargo clippy --all-targets` → 0 warnings, from a run that actually re-checked
2. Relevant tests green
3. Crate boundaries intact
4. New strings wrapped in `i18n()` and POT regenerated
5. No `dbg!`/`todo!`/`println!`/`eprintln!` left behind
6. `CHANGELOG.md` updated for any user-facing change (Keep a Changelog format)

If you cannot reach this, stop and say what is blocking. Do not drop a test,
silence a lint, or skip i18n to make it look finished.

## Style

Minimum viable change. Before writing code, stop at the first rung that holds:
does it need to exist; does this repo already have it; does `std` cover it; is
there a GTK4/libadwaita feature for it; does an existing dependency do it. Prefer
deleting over adding, and boring over clever. No new dependency, abstraction, or
generic that was not asked for.

Fix root causes, not symptoms: if a bug report names one call site, check every
caller of the function you touch and fix the shared function once.

Mark a deliberate simplification with `// ponytail:` naming the ceiling and the
upgrade path, e.g. `// ponytail: O(n²) scan, fine for <100 hosts; index if the
list grows`.

Do not be lazy about input validation at trust boundaries, error handling that
prevents data loss, credential handling, accessibility, or tests.

## Commits

Conventional commits: `type(scope): description`, imperative, lowercase, no
trailing period. Types: feat, fix, docs, style, refactor, test, chore, perf, ci,
build. Scopes: rustconn-core, rustconn-cli, rustconn (gui), i18n, packaging, ci.

Releases are prepared by editing files only, then `./scripts/release.sh` performs
merge → tag → push. Never run `git tag`/`git push` by hand for a release.
