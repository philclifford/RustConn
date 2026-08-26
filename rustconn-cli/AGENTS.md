# rustconn-cli

Headless management of RustConn data over `rustconn-core`. Root `AGENTS.md` still
applies, with one inversion you need to know before you "fix" anything here.

## `println!` is the interface, not a leftover

The root rules say logging goes through `tracing` and that `println!`/`eprintln!`
are debug leftovers to be removed. In this crate they are the product. `main.rs`
carries three crate-level allows, each with a reason:

- `clippy::print_stdout` — data output
- `clippy::print_stderr` — warnings and errors
- `unreachable_pub` — `pub` is inter-module visibility in a binary crate

So `println!` in `commands/` is correct and must not be converted to `tracing`.
What still applies: `tracing::error!` for the diagnostic record, `eprintln!` only
for what the user must see, and both in `main` gated on `--quiet`.

## The rest

- **Only `rustconn-core`.** No `gtk4`, no `adw`, no `vte4` — a pre-write hook
  rejects the edit. Any other workspace crate as a dependency needs a reason in
  the commit message.
- Default features minimal. Anything pulling a runtime integration goes behind a
  feature flag, as in core.
- Errors are `thiserror` (`src/error.rs`), not `anyhow`, even though M-APP-ERROR
  would permit `anyhow` in a binary. The reason is `exit_code()`: the process exit
  status is derived from the variant, so a new failure mode means a new variant
  with a deliberate code, not a stringly-typed context chain.
- A command that lists or shows data takes `--format` with `table` (default),
  `json` and `csv`, and implements all three. `commands/cluster.rs` is the
  reference shape; copy it rather than inventing a fourth output style. Dispatch
  on `format.effective()`, not on the raw value: `table` becomes `json` when
  stdout is not a terminal, so a piped or redirected command emits structured
  output (clig.dev). Matching the raw value drops that silently — the flag still
  says `table` and nothing fails.
- User-facing text still goes through `i18n()` / `i18n_f()`. A CLI is not exempt
  from the locales.

Surface reference: `docs/CLI_REFERENCE.md`.
