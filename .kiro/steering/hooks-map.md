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

## PostFileSave (after user or agent saves)

| Hook | Matcher | Type | Latency | Side-effects |
|------|---------|------|---------|--------------|
| **translation-sync** | `rustconn/src/.*\.rs$` | command | <100ms | Silent unless a `POTFILES.in` line must be added |
| **security-review** | `secret/.*\.rs$\|credential.*\.rs$\|password.*\.rs$` | agent | ~10s | Invokes `security-reviewer` sub-agent (read-only audit) |
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
