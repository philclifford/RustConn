# AGENTS.md — RustConn

Instructions for AI coding agents. RustConn is a GTK4/libadwaita connection
manager for SSH, RDP, VNC, SPICE, Telnet, Serial, Kubernetes and Zero Trust
brokers. Rust 2024 edition, MSRV 1.95, Wayland-first, Linux and macOS.

Communication language with the maintainer: **Ukrainian**.

## How to use this file

This is the rule *list*. The reasoning behind each rule, and the detail needed to
apply it correctly, lives in `.kiro/steering/*.md` — plain markdown in this repo,
which Kiro loads automatically and every other tool has to be pointed at. Open the
file that matches your task before you start: a rule taken from here without its
steering file is a rule you will apply too literally.

Nothing below repeats a rationale, a measurement or a worked example that a
steering file already owns. That is deliberate. Two copies of the same fact drift,
and this file has already proved it twice — it told agents to write cargo logs to
`/tmp` for weeks after the rule became `target/`, and the locale count was out of
step with `ls po/*.po`.

| Task | Read |
|------|------|
| Anything — invariants, crate table, Definition of Done | `.kiro/steering/core-rules.md` |
| Running cargo, terminals, background jobs | `.kiro/steering/shell-environment.md` |
| Code philosophy, workflow, escape hatches | `.kiro/steering/project-rules.md` |
| GUI work — HIG, windows, dialogs | `.kiro/steering/gnome-hig.md`, `window-guide.md`, `dialogs-guide.md` |
| Credential handling | `.kiro/steering/secrets-guide.md` |
| Rust idiom | `.kiro/steering/rust-pragmatic-guidelines.md` |
| Compiler errors | `.kiro/steering/error-resolution.md` |
| CHANGELOG entries | `.kiro/steering/changelog-format.md` |
| Architecture overview | `docs/ARCHITECTURE.md` |

## Commands

```bash
cargo fmt --all                                   # format
cargo clippy --all-targets                        # lint — must be 0 warnings
cargo test --workspace                            # ~45s test time, ~2.5 min with compile
cargo test -p rustconn-core --test property_tests  # property tests only
typos                                             # spell check (typos.toml)
cargo machete                                     # unused dependencies
bash po/update-pot.sh                             # after adding i18n strings
./scripts/check-potfiles.sh                       # POTFILES.in consistency (CI gate)
./scripts/check-i18n-escapes.sh                   # no \u{...} in translatable literals
./scripts/check-po-complete.sh                    # no fuzzy/missing translations
```

## Cargo and the terminal

Every rule here has a measured reason behind it in `shell-environment.md`. Read
that file before your first cargo run; these are only the conclusions.

- **Never** `cargo clippy --all-features` — it enables a gtk3 path that fails at
  build time on missing `gdk-3.0.pc`. Use `--all-targets`.
- **Never** pipe cargo output through `tail`/`grep`/`head`. Redirect to a file
  under `target/` and read the file.
- **Never** run two cargo commands at once — check `pgrep -f cargo` first.
- **Never wait with `sleep`.** A terminal that already has a live foreground job
  is not yours: bash is not reading stdin, so anything else you send queues in the
  tty buffer and then all of it runs, one command after another.
- A workspace test run is ~2.5 min wall, well past the 120 s default tool timeout,
  so a plain `cargo test` tends to return while cargo is still alive. Give the call
  explicit headroom (~900 000 ms), or background it with a sentinel file and poll
  the file.
- A repeat `cargo clippy` with nothing changed prints `Finished ... in 0.2s` and
  reports zero warnings **without checking anything**. Force a real re-check before
  claiming it passed.

The toolchain is pinned in `rust-toolchain.toml`. MSRV is a separate, older number
in `Cargo.toml` (`rust-version`).

## Crate boundaries — the rule most often broken

| Crate | May import GTK? | Notes |
|-------|-----------------|-------|
| `rustconn-core` | **No** | Domain logic. `gtk4`, `adw`, `vte4` are forbidden. Runtime integrations stay behind features. |
| `rustconn-cli` | **No** | Depends only on `rustconn-core`. |
| `rustconn` | Yes | All GUI, dialogs, session presentation. |
| `rustconn-pty-sys` | No | Isolated FFI: macOS PTY controlling terminal. |
| `rustconn-locale-sys` | No | Isolated FFI: startup `setlocale`. |
| `rustconn-env-sys` | No | Isolated FFI: startup `GSK_RENDERER` and `LANGUAGE` writes. |
| `rustconn-dock-sys` | No | Isolated FFI: the macOS Dock tile image, for launches with no `.app` behind them. |

No `unsafe` outside the `*-sys` crates, enforced by `unsafe_code = "deny"` in
`[workspace.lints.rust]` and re-opened per helper crate. New FFI gets its own
`rustconn-*-sys` crate — never an exception where the caller lives, and never a
macOS-only crate: no CI job builds macOS, so that would hold `unsafe` nothing
compiles. Why `deny` rather than `forbid`, and why `rustconn-dock-sys` gates its
dependencies instead of itself: `core-rules.md`. A pre-write hook blocks both
violations, but do not rely on it.

## Non-negotiable

- Passwords, keys, tokens → `secrecy::SecretString`, never `String`
- Intermediate `expose_secret().to_string()` → wrap in `zeroize::Zeroizing::new()`
- Secrets to external CLIs → stdin pipe, **never** `Command::arg(password)`
- Never log or format a secret into an error message
- Errors → `thiserror::Error`. No `unwrap()`/`expect()` outside tests
- Logging → `tracing`, never `println!`/`eprintln!`
- Every user-facing string → `i18n()` / `i18n_f()` with `{}` placeholders, then
  `bash po/update-pot.sh`. `ls po/*.po` is the authoritative locale count — do not
  trust a number written in prose, including one written here
- Icon-only buttons need both a tooltip and an accessible label
- Prefer `adw::` widgets; `adw::AlertDialog`, not the deprecated `gtk::MessageDialog`
- Never `std::env::set_var`/`remove_var` (unsafe in Rust 2024). The sole exception
  is `rustconn-env-sys::set_startup_var`, callable only from `main()` before this
  program starts a thread, and its two existing callers already seal that window —
  a third will panic rather than quietly work

## Definition of done

1. `cargo clippy --all-targets` → 0 warnings, from a run that actually re-checked
2. Relevant tests green
3. Crate boundaries intact
4. New strings wrapped in `i18n()` and POT regenerated
5. No `dbg!`/`todo!`/`println!`/`eprintln!` left behind
6. `CHANGELOG.md` updated for any user-facing change

If you cannot reach this, stop and say what is blocking. Do not drop a test,
silence a lint, or skip i18n to make it look finished. The sanctioned workarounds
are for *external* blockers only and are listed in `project-rules.md`.

## Style

Minimum viable change. Before writing code, stop at the first rung that holds:
does it need to exist; does this repo already have it; does `std` cover it; is
there a GTK4/libadwaita feature for it; does an existing dependency do it. Prefer
deleting over adding, and boring over clever. No new dependency, abstraction or
generic that was not asked for.

Fix root causes, not symptoms: if a bug report names one call site, check every
caller of the function you touch and fix the shared function once.

Mark a deliberate simplification with `// ponytail:` naming the ceiling and the
upgrade path, e.g. `// ponytail: O(n²) scan, fine for <100 hosts; index if the
list grows`.

Do not be lazy about input validation at trust boundaries, error handling that
prevents data loss, credential handling, accessibility, or tests.

## Commits and releases

Conventional commits: `type(scope): description`, imperative, lowercase, no
trailing period. Types: feat, fix, docs, style, refactor, test, chore, perf, ci,
build. Scopes: rustconn-core, rustconn-cli, rustconn (gui), i18n, packaging, ci.

**An agent prepares a release; it never cuts one.** `./scripts/release.sh
--dry-run` is the agent action and is expected: it runs every gate and stops before
the plan executes. Running the script for real, passing `--yes`, or tagging and
pushing by hand is the maintainer's call — v0.20.1 was cut by an agent and reached
a pushed tag and a published release carrying a red CI job and code deletions
nobody had read. Report the dry-run gate list and the diff, then hand over. Full
account in `core-rules.md`. The `release-manual-only-guard` hook enforces all three
routes, but do not rely on it.
