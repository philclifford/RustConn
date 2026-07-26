---
inclusion: manual
description: "Full KiroGraph MCP tool reference — parameters, examples, and exploration workflows. Pull in when the short always-on table in kirograph.md is not enough."
---

# KiroGraph — full tool reference

Companion to the always-on `kirograph.md`. Only the tools that exist in this server
version are listed; see the gotchas in `kirograph.md` for the ones that do not.

## Orientation

### `kirograph_context` — start here for any code task

Entry points, related symbols and code snippets for a natural-language task description.

```
kirograph_context(task: "fix SSH reconnect after suspend")
kirograph_context(task: "add dark mode", maxNodes: 30)
kirograph_context(task: "refactor secret backends", detail: "signatures")
```

### `kirograph_search` — find symbols by name

Exact match → FTS → fuzzy. Use instead of grep for symbols.

```
kirograph_search(query: "ConnectionManager")
kirograph_search(query: "SecretBackend", kind: "interface")
kirograph_search(query: "vnc", mode: "similar", limit: 20)
```

Kinds: `function`, `method`, `class`, `interface`, `type_alias`, `variable`, `route`, `component`.
Rust structs/enums/traits land in `class`/`interface`/`type_alias`.

### `kirograph_node` — inspect a symbol

```
kirograph_node(symbol: "validate_host")
kirograph_node(symbol: "ConnectionManager", detail: "full")
kirograph_node(symbol: "rustconn-core/src/connection/manager.rs::ConnectionManager", qualified: true)
```

## Call flow and blast radius

```
kirograph_callers(symbol: "spawn_session", limit: 30)
kirograph_callees(symbol: "handle_connect")
kirograph_rename_preview(symbol: "SessionId")        // every reference site
kirograph_path(from: "MainWindow", to: "SecretStore")
kirograph_type_hierarchy(symbol: "SecretBackend", direction: "down")
kirograph_module_api(path: "rustconn-core/src/secret/")
kirograph_affected(files: ["rustconn-core/src/connection/manager.rs"])
```

`kirograph_affected` walks the dependency graph to the test files that cover a change —
use it to pick which tests to run instead of the whole suite.

## Structure and health

```
kirograph_hotspots(limit: 20)          // most-connected symbols
kirograph_surprising(limit: 20)        // hidden cross-crate coupling
kirograph_dead_code(limit: 50)         // unexported, zero incoming edges
kirograph_circular_deps()
kirograph_god_class(threshold: 15)
kirograph_largest(limit: 30)           // by LOC
kirograph_rank(by: "fan-in")
kirograph_recursion()
kirograph_inheritance_depth()
kirograph_distribution(path: "rustconn/src/")
kirograph_doc_coverage(limit: 50)
kirograph_unused_imports()
kirograph_dependency_depth()
kirograph_gini(metric: "loc")
```

## Architecture (`enableArchitecture: true` — on in this project)

```
kirograph_architecture()                    // packages + layers
kirograph_architecture(level: "layers")
kirograph_coupling(sortBy: "afferent")     // most depended-on first
kirograph_package(package: "rustconn-core")
kirograph_communities(resolution: 1.0)      // clusters of related symbols
```

Ca (afferent) = depended on by; Ce (efferent) = depends on; instability = Ce/(Ca+Ce).
High Ca + low instability = load-bearing, cheap to depend on, expensive to change.

## Manifests, security, supply chain

```
kirograph_manifest(ecosystem: "cargo")
kirograph_manifest(showDrift: true)
kirograph_security()
kirograph_vulns(severity: "high")
kirograph_reachability(target: "CVE-2024-1234")
kirograph_licenses(policy: true)
kirograph_secrets(severity: "high")
kirograph_security_flows(type: "all")
kirograph_supply_chain(threshold: "high")
kirograph_staleness(threshold: 0.3)
kirograph_sbom()   /  kirograph_vex()
```

Cargo manifest parsing is line-based: a dependency spread over several lines, or a bare
version with a trailing `#` comment, is skipped with a warning (see `.kirograph/hook.log`).
Keep dependency declarations on one line if you want them in these reports.

## Snapshots and diff

```
kirograph_snapshot_save(label: "pre-refactor")
kirograph_snapshot_list()
kirograph_diff(snapshot: "pre-refactor")
```

## Memory and docs

```
kirograph_mem_search(query: "why oo7 is gated to non-macOS", kind: "decision")
kirograph_mem_store(content: "...", kind: "decision", topicKey: "architecture/secret-backends")
kirograph_mem_timeline(limit: 5)
kirograph_docs_search(query: "release process")
kirograph_docs_outline(file: "docs/ARCHITECTURE.md")
kirograph_docs_section(id: "...", context: true)
```

## Shell and I/O helpers

```
kirograph_exec(command: "~/.cargo/bin/cargo tree -p rustconn-core", level: "aggressive")
kirograph_read(path: "rustconn/src/main.rs", mode: "map")
kirograph_retrieve(path: "rustconn/src/main.rs")   // after a "[cached]" marker
kirograph_budget()  /  kirograph_gain()
```

`kirograph_exec` output is compressed; `grep`-shaped output gets restructured, which can
mangle exact formatting. Ask for plain output (or `kirograph_read`) when byte-accuracy
matters, and never pipe a progress-bar log through it — the ANSI spam is unbounded.

## Workflows

**Bug fix or feature:** `kirograph_context` → `kirograph_node` (`detail: "full"`) →
`kirograph_callers`/`kirograph_callees` → `kirograph_rename_preview` before editing →
`kirograph_affected` to choose tests.

**Refactor planning:** `kirograph_hotspots` → `kirograph_surprising` →
`kirograph_snapshot_save` → refactor → `kirograph_diff`.

**Architectural review:** `kirograph_architecture` → `kirograph_coupling` →
`kirograph_package` → `kirograph_circular_deps`.

**Cleanup:** `kirograph_dead_code` → `kirograph_unused_imports` → `kirograph_circular_deps`
→ `kirograph_god_class`.
