---
inclusion: always
---

# KiroGraph

A semantic graph of this codebase lives in `.kirograph/`. Prefer its MCP tools over
grep/glob/file reads for anything structural. Full parameter reference and workflows:
steering `kirograph-reference.md` (manual).

| Question | Tool |
|----------|------|
| Where do I start on this task? | `kirograph_context` |
| Find a symbol by name | `kirograph_search` |
| What is this symbol / show its code | `kirograph_node` (`detail: "full"`) |
| Who calls X? / What does X call? | `kirograph_callers` / `kirograph_callees` |
| What breaks if I rename or change X? | `kirograph_rename_preview` |
| Which tests cover my changed files? | `kirograph_affected` |
| How are X and Y connected? | `kirograph_path` |
| What implements this trait? | `kirograph_type_hierarchy` |
| What is the public API of this module? | `kirograph_module_api` |
| Which code is never called? | `kirograph_dead_code` |
| Import cycles? | `kirograph_circular_deps` |
| Most critical symbols? | `kirograph_hotspots` |
| Unexpected cross-crate coupling? | `kirograph_surprising` |
| Packages, layers, coupling? | `kirograph_architecture`, `kirograph_coupling`, `kirograph_package` |

Typical loop: `kirograph_context` to orient → `kirograph_node` to read →
`kirograph_callers`/`kirograph_callees` to trace → `kirograph_rename_preview` before editing.

## Known gotchas

- `kirograph_impact`, `kirograph_files` and `kirograph_status` do **not** exist in this
  server version. Use `kirograph_rename_preview` for blast radius, `kirograph_module_api`
  or `kirograph_exec` + `ls` for file listings, and `kirograph status` via
  `kirograph_exec` for index health.
- **"KiroGraph not initialized" is usually a lie.** It also appears when the DB is locked
  (a stale, empty `.kirograph/kirograph.db.lock` directory left by a killed sync) or when
  the MCP server resolved a different project root. Check with
  `kirograph_exec("cd <repo> && kirograph status")`; if it reports a lock and no sync is
  running, remove the empty lock directory and retry.
- A sync costs ~3-4 min even with nothing changed (full scan + global resolve) and ~20 min
  after a large branch. It runs on the `Stop` hook, so results appear after the turn ends;
  graph queries fail while it holds the lock.
- The graph can be up to one turn stale. For code you just edited, read the file.
- `kirograph_exec` runs under `/bin/sh`, not bash: no `time` builtin, and `cargo` is not on
  its PATH — use `~/.cargo/bin/cargo`.
