---
inclusion: always
---
# Shell Environment

> This file is `inclusion: always` on purpose. It was `inclusion: auto` until
> 2026-08-12, but `auto` matches a request against the file's `description` and
> requires `name` + `description` in the front matter — neither was present, so
> the file matched nothing and was never loaded. Its own text meanwhile asserted
> that "the non-negotiable parts live here where they are always loaded", which
> was false for as long as the mode was wrong. Terminal discipline is cheap to
> carry and expensive to omit, so it is unconditional now. If you ever switch a
> steering file to `auto`, give it both `name` and `description`.

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
- **Never wait with `sleep`.** See the next section. A sleep cannot observe
  another terminal, and if the terminal is busy the line queues behind the
  running job instead of executing.
- **Pass an explicit `timeout`** to any cargo build or test: the tool's default
  is 120 000 ms and the workspace test run is ~120 s, so the default is a
  coin flip on losing the output while the process stays alive.

The `bash-serialization-guard` hook (`.kiro/hooks/`) enforces the four rules
above that are mechanically checkable — sleep waiting, piped cargo output, a
second concurrent cargo, and a cargo run with no timeout headroom. It fails open,
so it is a faster failure, never a substitute for knowing the rules.

## Waiting Without Blocking the Terminal

The expensive failure is not a slow build, it is trying to wait for one. The
sequence that burns a session: `cargo test --workspace` starts with the default
120 s timeout → the tool returns while cargo is still running → the wait looks
necessary → `sleep 115; echo W10` is sent to the *same* terminal → bash is not
reading stdin while a foreground job runs, so the line sits in the tty buffer,
and so do the next eighteen → cargo exits, bash drains the buffer and runs every
queued sleep back to back. Nineteen queued `sleep 115` is 36 minutes of nothing,
and each one looks like a command that legitimately timed out.

Three ways out, cheapest first.

**1. Wait inside the one tool call.** Almost always the right answer.

```bash
cd /home/totoshko88/Documents/RustConn || exit 1
cargo test --workspace > /tmp/rc-test.log 2>&1
```

with `timeout=900000`, then read `/tmp/rc-test.log` with the file-reading tool
(it takes line ranges, so a 20 k-line log costs nothing).

**2. Take a handle when you want to keep working.** Poll the filesystem, never
the clock — the run is finished exactly when the `.rc` file appears:

```bash
cd /home/totoshko88/Documents/RustConn || exit 1
rm -f /tmp/rc-test.log /tmp/rc-test.rc
nohup sh -c 'cargo test --workspace > /tmp/rc-test.log 2>&1; echo $? > /tmp/rc-test.rc' >/dev/null 2>&1 &
```

Check it by reading `/tmp/rc-test.rc` with the file-reading tool between other
work. `control_bash_process` + `get_process_output` is the same idea with a
managed terminal; if you use it, stop the process when done.

**3. Delegate.** The `rust-quality-check` sub-agent owns its own terminal, which
also removes the second-terminal problem entirely.

Whatever the route: **once a terminal has a live foreground job, it is not
yours.** Do not send it another command — not a status check, not an `echo`, not
a `^C` follow-up. Read the log file instead.

## Cargo Traps in This Workspace

- **A cached clippy run hides warnings.** A second `cargo clippy` with no changes
  prints `Finished ... in 0.2s` and reports zero warnings *even when warnings
  exist* — it reports nothing at all. To make a verification meaningful, force a
  real re-check (`touch` the `.rs` files you care about, or `cargo clean -p
  <crate>`) and confirm from the output that compilation actually happened.
- **Never use `--all-features`.** It enables a gtk3-dependent path that fails at
  build time with `gdk-3.0.pc` not found via pkg-config. Use `--all-targets`.
