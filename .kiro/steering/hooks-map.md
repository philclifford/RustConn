---
inclusion: manual
description: "Reference map of all Kiro hooks — triggers, matchers, concurrency, and side-effects."
---

# Hooks Map

Quick reference for all `.kiro/hooks/*.json` — what fires when, what it does, and what it touches.

`scripts/check-ai-docs.sh` asserts that every hook file has a row somewhere in
this document. That gate exists because this table silently lost one:
`session-baseline` landed on 2026-08-26 and was still undocumented on 2026-09-02,
in a file whose first line promises to cover them all. It is the same failure the
same script already guards for the counts in `docs/AI_DEVELOPMENT.md` — a
hand-maintained inventory with no check against reality.

## SessionStart

| Hook | Matcher | Type | Latency | Side-effects |
|------|---------|------|---------|--------------|
| **session-baseline** | (none) | command | <100ms | Records a content hash per tracked file into the session baseline, so the Stop hook can tell what *this* session changed. Without it, `git diff --name-only HEAD` reports every file carrying pre-existing uncommitted work — on 2026-08-26 that made three consecutive Stop hooks run getDiagnostics over 13 `.rs` files in a session whose only edit was markdown. Hashes rather than a diff, so a commit mid-session does not invalidate it. Silent always; fails open. Logic: `bin/session-baseline.sh`. |

## PreToolUse (before write)

| Hook | Matcher | Type | Latency | Side-effects |
|------|---------|------|---------|--------------|
| **crate-boundary-guard** | `fs_write\|fs_append\|str_replace\|delete_file\|code` | command | <50ms | Blocks with exit 2. Zero model cost when clean. Fails open. |

## PreToolUse (before shell)

Both fire on every shell call, so their cost is paid constantly and their false
positives are felt immediately. Matcher for both:
`^(execute_bash|executeBash|bash|shell|control_bash_process|controlBashProcess)$`.

| Hook | Type | Latency | Side-effects |
|------|------|---------|--------------|
| **bash-serialization-guard** | command | <50ms | Blocks with exit 2. Rejects `sleep`-based waiting, cargo output piped through a filter, a second cargo while one holds the target-dir lock, and a cargo build/test issued with the default 120 s timeout. Keeps a one-shot marker in `$TMPDIR` so a differently-spelled timeout field cannot deadlock it. Fails open. Logic: `bin/bash-serialization-guard.sh`. |
| **release-manual-only-guard** | command | <50ms | Blocks with exit 2. Refuses `scripts/release.sh` without `--dry-run`, refuses `--yes` either way, and refuses the by-hand `git tag`/`git push` of a `v<semver>` tag. Tag *listing* and *deletion* stay allowed, since undoing a bad tag needs them. Fails open. Logic: `bin/release-manual-only-guard.sh`. |

### Known false positives in `bash-serialization-guard`

The guard triggers on `cargo` followed by one of
`build|test|clippy|check|run|bench|doc|nextest|machete|audit`, with no notion of
whether that pair is being *invoked* or merely appears in the command line. It
does **not** trigger on a bare `cargo` with no verb after it.

This paragraph said "matches the literal string `cargo` anywhere in the command"
until 2026-09-02, and listed `pgrep -f cargo` as the first false positive. Both
were wrong, and had been since the script grew its verb list: the claim was never
re-tested against the script, so it outlived the behaviour it described. Probed
directly by piping payloads to the guard on 2026-09-02 — `pgrep -f cargo` is
**allowed**, and `pgrep -x cargo` (which R3 itself uses) is allowed too. There is
no reason to write `pgrep -f '[c]argo'` for the guard's sake. The bracket idiom
still has its older, unrelated merit of stopping `pgrep` from matching its own
command line.

Three classes do still trigger, all verified by probe:

1. **A `cargo <verb>` pair inside a search pattern.** `grep -rn 'cargo build' …`
   is blocked even though nothing is built. Prefer the `grepSearch` tool over
   shell `grep` here — it is the right tool anyway and sidesteps the guard
   entirely.
2. **A `nohup`-detached run.** It returns immediately and therefore cannot lose
   its output, but the guard cannot tell. Passing `timeout=900000` satisfies it
   and costs nothing, since the call returns either way. The guard's own R1
   message shows this form *with* the timeout, so following the message works.
3. **A payload under test.** Feeding the guard a JSON payload to probe it puts
   the offending string on the outer command line too, so testing `sleep 115`
   trips R1 on the test call itself. Write the probe to a file and run the file.

None of these is worth "fixing" in the script by parsing the command line — a
guard that fails open and occasionally over-triggers is the right trade against
one that tries to be clever and misses a real case. Know the three workarounds
instead.

The four block messages name log paths under `target/`, matching
`shell-environment.md`. They said `/tmp` until 2026-09-02, contradicting the
always-loaded rule at the exact moment an agent was most likely to follow them.

Unrelated shell trap in the same territory: `echo '#![allow]'` in double quotes
trips bash history expansion (`bash: ![allow]: event not found`). Single-quote
anything containing `!`.

## PostFileSave (after user or agent saves)

| Hook | Matcher | Type | Latency | Side-effects |
|------|---------|------|---------|--------------|
| **translation-sync** | `rustconn/src/.*\.rs$` | command | <100ms | Silent unless a `POTFILES.in` line must be added |
| **security-review** | `rustconn-core/src/secret/.*\.rs$` **or** a filename containing `credential`/`credentials`/`password` anywhere in the tree | agent | ~10s | Invokes `security-reviewer` sub-agent (read-only audit). The `secret/` branch is *anchored to `rustconn-core/src/`* — this row abbreviated it to a bare `secret/.*\.rs$` until 2026-09-02, which reads as though any `secret/` directory qualifies. |
| **unsafe-review** | `rustconn-(pty\|locale\|env\|dock)-sys/.*\.rs$` | agent | ~10s | Invokes `unsafe-reviewer` sub-agent (read-only audit). Added 2026-08-20 — the four crates holding the only sanctioned `unsafe` in the workspace had no hook coverage, while credential code has had it since `security-review` landed. |
| **uk-translation-review** | `po/uk\.po$` | agent | ~15s | Delegates to `uk-translation-reviewer` when available, otherwise reviews in place (the reviewer cannot invoke itself). May edit `po/uk.po` |
| **cargo-security-scan** | `Cargo\.lock$` | command | ~5s | Read-only advisory check, findings to `target/cargo-advisories.log`. Skips silently when `Cargo.lock` matches HEAD. Logic: `bin/cargo-advisory-scan.sh`. Prefers the **bare** `cargo-deny` binary over `cargo deny`, so `rust-toolchain.toml` is not asked to resolve a toolchain for a check that only parses the lockfile — the same reason `ci.yml` invokes `cargo-machete` directly. Presence is probed with `command -v`, never inferred from an exit code: cargo-deny exits non-zero *because* it found an advisory, and the old inline `\|\|` chain therefore reported real findings as "neither tool installed" while `2>/dev/null` discarded the report. Fixed 2026-09-02. |
| **flatpak-manifest-check** | `Cargo\.lock$` | agent | ~2s | Warns about stale cargo-sources.json (no auto-fix) |
| **kirograph-mark-dirty-on-save** | `\.(rs\|toml)$` | command | <100ms | Writes `.kirograph/dirty`; logs to `.kirograph/hook.log` |

## PostFileCreate

| Hook | Matcher | Type | Latency | Side-effects |
|------|---------|------|---------|--------------|
| **kirograph-mark-dirty-on-create** | `\.(rs\|toml)$` | command | <100ms | Writes `.kirograph/dirty`; logs to `.kirograph/hook.log` |

## PostFileDelete

| Hook | Matcher | Type | Latency | Side-effects |
|------|---------|------|---------|--------------|
| **kirograph-sync-on-delete** | `\.(rs\|toml)$` | command | <100ms | Marks dirty only — sync is deferred to the Stop hook |

## PostTaskExec (after spec task completes)

| Hook | Matcher | Type | Latency | Side-effects |
|------|---------|------|---------|--------------|
| **post-task-diagnostics** | (none) | agent | ~5s | Runs getDiagnostics on changed .rs files. No cargo commands. |

## Stop (end of agent session)

| Hook | Matcher | Type | Latency | Side-effects |
|------|---------|------|---------|--------------|
| **post-session-diagnostics** | (none) | agent | ~10s | getDiagnostics + scans diff for debug leftovers |
| **kirograph-sync-if-dirty** | (none) | command | **~3-4 min**, up to ~20 min | Syncs KiroGraph index if dirty marker present. Runs `nice`d in the background; skipped if a sync is already running |

---

## KiroGraph sync cost and failure mode

Measured on this repo (564 files, ~36k symbols): `kirograph sync` takes **191 s even when
it reports "Nothing to sync"** — it always rescans every file and resolves ~47k symbols.
After a branch with ~130 changed `.rs` files it took **~21 min**. So the Stop hook keeps a
CPU-bound background process alive well past the end of a turn.

While that process holds `.kirograph/kirograph.db.lock`, every graph MCP call answers
**"KiroGraph not initialized. Run `kirograph init`"** — which is misleading. If such a sync
is killed (session closed, `timeout`), the empty lock directory survives and *every*
subsequent call keeps reporting "not initialized". That silently disabled KiroGraph in this
repo for a week in July 2026. Note that `kirograph unlock` does not help: it looks for a
lock *file*, while what is left behind is a *directory*.

Hardening in the four KiroGraph hooks:

- `cd "$(git rev-parse --show-toplevel …)"` first — hook cwd is not guaranteed, and a bare
  `kirograph` in a subdirectory reports "not initialized at <subdir>".
- stdout/stderr go to `.kirograph/hook.log` (gitignored) instead of `2>/dev/null`; the very
  first log line already surfaced two silently skipped `Cargo.toml` dependencies.
- the Stop hook exits early if `pgrep -f 'kirograph [s]ync'` matches, so two syncs never
  race for the lock.
- an *empty* `kirograph.db.lock` directory older than 30 min is deleted before syncing.

## Concurrency notes

When editing `rustconn/src/dialogs/password.rs`:

1. `crate-boundary-guard` fires **before** write (PreToolUse)
2. After save, **three** PostFileSave hooks fire simultaneously:
   - `kirograph-mark-dirty-on-save` (~instant, command)
   - `translation-sync` (<100ms, command — checks for i18n calls)
   - `security-review` (~10s, agent — matched on the `password` filename, not the path)

This example named `rustconn/src/secret/` until 2026-09-02. No such directory
exists, and it would not have matched `security-review` if it did — that hook's
`secret/` branch is anchored to `rustconn-core/src/`. Editing a real file under
`rustconn-core/src/secret/` fires **two** hooks, not three: `translation-sync` is
scoped to `rustconn/src/` and stays quiet for core.

When editing `Cargo.lock`:
- `cargo-security-scan` + `flatpak-manifest-check` fire together

## Notes

- KiroGraph matchers are scoped to `\.(rs|toml)$` — matching the Rust-only project.
- Only one hook now runs `kirograph sync`: the Stop hook. The save/create/delete hooks just
  set the dirty marker.
- Permissions, MCP config and other files under `.kiro/settings/` cannot be edited by the
  agent (`kiro-scope` deny). Reviewed copies live in `.kiro/config-templates/` and are
  applied by hand.
- **Pending hand-apply (found 2026-09-02):** `.kiro/config-templates/mcp.json` passes
  `--path /home/totoshko88/Documents/RustConn` to `kirograph serve`; the live
  `.kiro/settings/mcp.json` does not. Without it the server resolves the project root
  from its working directory, which is one of the documented causes of the bogus
  "KiroGraph not initialized" in `kirograph.md`. The template is the corrected copy —
  copy it over by hand, since the deny rule means no agent can.
