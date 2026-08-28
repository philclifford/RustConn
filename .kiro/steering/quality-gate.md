---
inclusion: manual
description: "Unified quality gate: quick (fmt+clippy), full (fmt+clippy+tests), or tests-only. Invoke the appropriate section."
---

# Quality Gate

Single entry point for all code quality checks. Use the section matching the request.

## Quick (fmt + clippy)

Use before committing or when the developer asks for a quick check.

Invoke the `rust-quality-check` sub-agent with prompt:
"Run fmt and clippy checks. Do NOT run tests unless explicitly requested."

Report the result. If all pass — remind to commit.

## Full (fmt + clippy + tests)

Use when finishing a feature, before release, or when explicitly asked for "full checks".

One command does all four steps, in order, with the guards already applied:

```bash
scripts/verify.sh --tests     # timeout=900000, output already goes to target/verify.log
```

It refuses to start while another cargo holds the target-dir lock, never pipes
cargo output, and flags the cached-clippy false green described below. Report its
summary block and, on a failure, the relevant part of `target/verify.log`.

The four steps it runs, for when you need to do one of them alone:

1. `cargo fmt --check` — if formatting errors, run `cargo fmt --all`, report changes.
2. `cargo clippy --all-targets -- -D warnings` — must produce 0 warnings. Fix and re-run if any.
3. Before tests: `pgrep -f 'cargo test'` — if running, report "Tests already in progress, skipping" and stop.
4. `cargo test --workspace` — run directly, NO pipes (no tail/grep). Use `timeout=900000`; the run is ~2.5 min wall (~45s tests + ~1m49s compile, ~3900 tests).

### A cached clippy run reports zero warnings even when warnings exist

This is the single easiest way to report a false green. When nothing has changed,
clippy prints `Finished \`dev\` profile ... in 0.2s` and emits no diagnostics at
all — not because the code is clean, but because it never re-checked it.

`scripts/verify.sh` handles this for you, and since 0.21.0 it handles it by
default rather than on request: it cleans the workspace crates before clippy —
not their dependencies, so it costs seconds rather than minutes — and then looks
for `Checking`/`Compiling` lines in clippy's own output. If there are none it
**fails**, because after a clean there is no innocent explanation left.

It used to clean only under an opt-in `--fresh` and, without it, record the cache
hit as

```
WARN  clippy did not compile anything — that run verified nothing.
```

which never incremented the failure count — so the script exited 0 and the run
looked done. `--cached` still buys the old fast path, and there the message stays
a warning: a caller who asked for it is entitled to be told what they gave up
rather than refused.

The CI clippy job had the same hole and got the same treatment: it caches
`target/` keyed on `Cargo.lock`, so any change leaving the lock alone restored a
tree clippy had already linted. It now cleans the workspace crates first and
fails if nothing compiled.

Doing it by hand, confirm from the output that compilation actually happened
(`Checking rustconn-core`, `Compiling rustconn`, a runtime of seconds rather than
milliseconds), and force a re-check with:

```bash
find rustconn rustconn-core rustconn-cli -name '*.rs' -exec touch {} +
cargo clippy --all-targets -- -D warnings
```

State in the report which of the two you got.

### Never use `--all-features`

It enables a gtk3-dependent path that fails at build time — `gdk-3.0.pc` is not
available via pkg-config. `--all-targets` is the flag that matters here; it covers
tests, benches and examples, which is the actual goal.

Report pass/fail for each step. If tests fail — list failing test names.

## Tests only

Use when the developer explicitly asks to run tests.

1. `pgrep -f 'cargo test'` — if running, report "Tests already in progress, skipping."
2. `cargo test --workspace` — run directly, NO pipes. Use `timeout=900000`.
3. Report final summary (e.g. "test result: ok. 42 passed; 0 failed"). If failures — list test names.

## Hygiene checks (no toolchain needed)

Wired into the CI `hygiene` job on 2026-08-12. Both are seconds-fast and need no
cargo, so run them whenever you touched prose, dependencies or Cargo.toml.

1. `typos` — spell check, config in `typos.toml`. Must exit 0.
   `typos.toml` had sat in the repo fully configured but unexecuted; when it was
   first run it produced 73 findings and **every one was a false positive**
   (HashiCorp, the `flate2`/`writeable` crate names, Asbru's own `Parrallels`
   wire format, the `bottons` field in vnc-rs, a deliberate `prodction` example
   in the CLI docs, base64 PEM fixtures). The vocabulary is now recorded with a
   reason per entry. If a new finding appears, check whether it is genuinely
   misspelled before "fixing" it — several of those corrections would have
   introduced bugs.
2. `cargo machete` — unused dependencies. Must exit 0.
   Crates whose import path differs from their package name (`md-5` → `md5`,
   `gettext-rs` → `gettextrs`, `vnc-rs` → `vnc`) are declared per-crate under
   `[package.metadata.cargo-machete] ignored`, along with `native-tls`, which is
   present only to pin a version. A genuine hit means the dependency really is
   dead — `tracing-subscriber` was a direct dependency of `rustconn-core` with
   zero references until this check found it.

Also verified in CI: the toolchain in `rust-toolchain.toml` matches the workflow's
`RUST_TOOLCHAIN`. Two copies exist because the install action cannot read the
file; the gate stops them drifting.

## GUI tests — evaluated, not wired up

There is no automated GUI check, so `cargo test --workspace` says nothing about
`rustconn/`. The numbers: 3532 test markers across 157k lines in
`rustconn-core`, 325 across 129k in `rustconn`, 23 across 14k in `rustconn-cli`.
The 0.20.0 notes carry three consecutive web-toolbar/split-view focus fixes, each
repairing the last one's incompleteness — that is what the gap costs.

[WayDriver](https://waydriver.io/) (Apache-2.0) is the candidate: it runs the app
in a headless Mutter session with a private D-Bus and drives it **through
AT-SPI** — the accessible labels `dialogs-guide.md` already requires on every
icon-only button. It also ships an MCP server, so an agent could reproduce a GUI
bug itself. Not adopted yet because the machine lacks every needed dev library
and `mutter`, so nothing could be verified; details, API notes and the intended
design are in memory under topic `decision/gui-testing-waydriver`.

To try it:

```bash
sudo apt install mutter libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev \
                 libpipewire-0.3-dev libatspi2.0-dev
```

Put it in a `gui-tests/` crate listed in the root `[workspace] exclude`, not as a
workspace member and not as a dev-dependency of `rustconn` — otherwise
waydriver's gstreamer/pipewire/zbus tree lands in the workspace lockfile, the
`cargo deny` graph and every `cargo test --workspace`, to serve tests that cannot
run without `mutter`. Run it deliberately: `cd gui-tests && cargo test -- --ignored`.
Do not make it a CI gate on the first landing.

## i18n checks

Run when translatable strings were touched (any `i18n()` call added or edited,
or any change under `po/`). Cheap — no cargo, no toolchain.

1. `./scripts/check-i18n-escapes.sh` — must print `OK`. Fails on Rust-only
   `\u{...}` escapes inside translatable literals. `xgettext --language=C`
   cannot decode them, so the extracted msgid never matches the runtime lookup
   and the string stays untranslated in every locale while the `.po` files
   still report 100% complete. Fix by putting the character in the literal
   directly (ASCII apostrophe is the project convention), then re-run
   `po/update-pot.sh`.
2. `msgfmt --check --check-format -o /dev/null po/<lang>.po` for changed
   catalogues — catches format-placeholder mismatches between msgid and msgstr.

Both also run in CI as the `i18n` job.

## Rules (all modes)

- **Never** pipe cargo output through `tail`, `grep`, or any filter.
- **Never** start cargo if another instance is already running (`pgrep -f 'cargo'`).
- A full `cargo test --workspace` is ~2.5 min wall (~45s tests + ~1m49s compile) — this is normal, do not assume timeout.
- One terminal owner at a time — do not run bash while a sub-agent is active.
