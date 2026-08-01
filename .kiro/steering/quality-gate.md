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

Run sequentially in workspace root:

1. `cargo fmt --check` — if formatting errors, run `cargo fmt --all`, report changes.
2. `cargo clippy --all-targets -- -D warnings` — must produce 0 warnings. Fix and re-run if any.
3. Before tests: `pgrep -f 'cargo test'` — if running, report "Tests already in progress, skipping" and stop.
4. `cargo test --workspace` — run directly, NO pipes (no tail/grep). Allow 180s timeout (argon2 ~120s is normal).

### A cached clippy run reports zero warnings even when warnings exist

This is the single easiest way to report a false green. When nothing has changed,
clippy prints `Finished \`dev\` profile ... in 0.2s` and emits no diagnostics at
all — not because the code is clean, but because it never re-checked it.

Before claiming clippy passed, confirm from the output that compilation actually
happened (`Checking rustconn-core`, `Compiling rustconn`, a runtime of seconds
rather than milliseconds). If it was a cache hit, force a real re-check:

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
2. `cargo test --workspace` — run directly, NO pipes. Allow 180s.
3. Report final summary (e.g. "test result: ok. 42 passed; 0 failed"). If failures — list test names.

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
- Tests take ~120s (argon2 property tests in debug) — this is normal, do not assume timeout.
- One terminal owner at a time — do not run bash while a sub-agent is active.
