---
inclusion: auto
---
# Shell Environment

## Terminal Profile

The workspace uses `bash --noprofile --norc` with explicit PATH injection via the VS Code terminal profile. This means:

- No `.bashrc`, `.profile`, or `/etc/profile` is sourced in agent terminals
- `cargo`, `rustfmt`, `clippy` are available at `~/.cargo/bin/` (injected via profile `env.PATH`)
- `~/.local/bin/` is also in PATH (for `uv`, `pipx`, user scripts)
- `direnv` is NOT active in agent shells (requires hook in `.bashrc`)

## Multiline Text in Shell Commands

**Never pass multiline text inline** in bash command arguments (e.g. `--body '...'` with newlines). The bash tool cannot reliably handle unmatched quotes and heredocs across multiple lines.

Instead:
1. Write multiline content to a temp file using `fs_write`
2. Pass the file to the command (e.g. `gh issue comment --body-file /tmp/comment.md`)
3. Delete the temp file after use

## Available Tools

| Tool | Path | Notes |
|------|------|-------|
| cargo | ~/.cargo/bin/cargo | Rust toolchain via rustup |
| gh | gh (system) | GitHub CLI, authenticated |
| flatpak-builder | flatpak-builder (system) | Flatpak builds |
| kirograph | ~/.nvm/.../bin/kirograph | Code graph (when .kirograph/ exists) |

## Cargo Commands

Always use the full path if PATH issues arise: `/home/totoshko88/.cargo/bin/cargo`

Common verification sequence:
```bash
cargo fmt --check
cargo clippy --all-targets
cargo test --package rustconn-core --test property_tests
```

## Terminal Discipline

These are the rules whose violation actually costs time. The full reasoning is in
steering `quality-gate.md`, but that file is `inclusion: manual`, so the
non-negotiable parts live here where they are always loaded.

- **Never pipe cargo output** through `tail`, `grep`, `head` or any filter.
  Redirect to a file and read the file instead. Piping is the main way the shell
  tool ends up returning nothing at all.
- **One cargo at a time.** Check `pgrep -f cargo` before starting a build or test
  run. Two concurrent runs block on the same target-dir lock and both appear to
  hang.
- **One terminal owner.** Do not run bash while a sub-agent is working — the
  sub-agent needs the terminal.
- **Stop background processes when done.** A `control_bash_process` job left
  running holds its terminal. Accumulating them degrades the whole session; ~30
  stray jobs once wedged the cargo lock and the shell tool together. `list_processes`
  then stop what you started.
- **The shell tool can lose its working directory** between calls. Start any
  command that depends on the repo root with
  `cd /home/totoshko88/Documents/RustConn || exit 1`.
- **If the shell tool returns empty output twice in a row**, stop retrying it:
  delegate the run to the `rust-quality-check` sub-agent, or write to a log file
  and read it with the file-reading tool.
- Tests take ~120 s (argon2 property tests in debug). That is normal, not a hang.

## Cargo Traps in This Workspace

- **A cached clippy run hides warnings.** A second `cargo clippy` with no changes
  prints `Finished ... in 0.2s` and reports zero warnings *even when warnings
  exist* — it reports nothing at all. To make a verification meaningful, force a
  real re-check (`touch` the `.rs` files you care about, or `cargo clean -p
  <crate>`) and confirm from the output that compilation actually happened.
- **Never use `--all-features`.** It enables a gtk3-dependent path that fails at
  build time with `gdk-3.0.pc` not found via pkg-config. Use `--all-targets`.
