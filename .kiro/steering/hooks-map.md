---
inclusion: manual
description: "Reference map of all Kiro hooks — triggers, matchers, concurrency, and side-effects."
---

# Hooks Map

Quick reference for all `.kiro/hooks/*.json` — what fires when, what it does, and what it touches.

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

The guard matches the literal string `cargo` anywhere in the command, with no
notion of whether it is being *invoked*. Three classes hit in practice, all on
2026-08-20:

1. **The mandated pre-flight check.** `pgrep -f cargo` — the very command the
   rules require before starting a build — is blocked. Write `pgrep -f '[c]argo'`:
   the bracket keeps the guard quiet and additionally stops `pgrep` from matching
   its own command line, which is the older reason for that idiom.
2. **`cargo` inside a search pattern.** `grep -rn 'cargo build' …` is blocked
   even though nothing is built. Prefer the `grepSearch` tool over shell `grep`
   here — it is the right tool anyway and sidesteps the guard entirely.
3. **A `nohup`-detached run.** It returns immediately and therefore cannot lose
   its output, but the guard cannot tell. Passing `timeout=900000` satisfies it
   and costs nothing, since the call returns either way.

None of these is worth "fixing" in the script by parsing the command line — a
guard that fails open and occasionally over-triggers is the right trade against
one that tries to be clever and misses a real case. Know the three workarounds
instead.

Unrelated shell trap in the same territory: `echo '#![allow]'` in double quotes
trips bash history expansion (`bash: ![allow]: event not found`). Single-quote
anything containing `!`.

## PostFileSave (after user or agent saves)

| Hook | Matcher | Type | Latency | Side-effects |
|------|---------|------|---------|--------------|
| **translation-sync** | `rustconn/src/.*\.rs$` | command | <100ms | Silent unless a `POTFILES.in` line must be added |
| **security-review** | `secret/.*\.rs$\|credential.*\.rs$\|password.*\.rs$` | agent | ~10s | Invokes `security-reviewer` sub-agent (read-only audit) |
| **unsafe-review** | `rustconn-(pty\|locale\|env\|dock)-sys/.*\.rs$` | agent | ~10s | Invokes `unsafe-reviewer` sub-agent (read-only audit). Added 2026-08-20 — the four crates holding the only sanctioned `unsafe` in the workspace had no hook coverage, while credential code has had it since `security-review` landed. |
| **uk-translation-review** | `po/uk\.po$` | agent | ~15s | Delegates to `uk-translation-reviewer` when available, otherwise reviews in place (the reviewer cannot invoke itself). May edit `po/uk.po` |
| **cargo-security-scan** | `Cargo\.lock$` | command | ~5s | Runs `cargo deny`/`cargo audit` (read-only) |
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

When editing a `.rs` file in `rustconn/src/secret/`:

1. `crate-boundary-guard` fires **before** write (PreToolUse)
2. After save, **three** PostFileSave hooks fire simultaneously:
   - `kirograph-mark-dirty-on-save` (~instant, command)
   - `translation-sync` (<100ms, command — checks for i18n calls)
   - `security-review` (~10s, agent — invokes sub-agent)

When editing `Cargo.lock`:
- `cargo-security-scan` + `flatpak-manifest-check` fire together

## Notes

- KiroGraph matchers are scoped to `\.(rs|toml)$` — matching the Rust-only project.
- Only one hook now runs `kirograph sync`: the Stop hook. The save/create/delete hooks just
  set the dirty marker.
- Permissions, MCP config and other files under `.kiro/settings/` cannot be edited by the
  agent (`kiro-scope` deny). Reviewed copies live in `.kiro/config-templates/` and are
  applied by hand.
