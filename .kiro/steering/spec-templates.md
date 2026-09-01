---
inclusion: manual
description: "Готові шаблони specs для типових задач RustConn: новий протокол, новий діалог, баг-фікс, рефакторинг. Копіювати й заповнювати. Довідник — сам нічого не запускає."
---

# Spec Templates — RustConn

Templates for quickly creating specs of various types.

## Design-First: New Protocol

Use when the architecture is already known (Protocol trait, dialog, CLI handler).
Skips requirements, goes straight to design → tasks.

### design.md template

```markdown
# Design: {Protocol} Protocol Support

## Architecture

### rustconn-core changes
- `rustconn-core/src/protocol/{protocol}.rs` — implement `Protocol` trait
- `ProtocolType::{Protocol}` variant in enum
- Capabilities: has_terminal, has_password, has_port, has_username
- Default port: {port}
- Connection logic in `connect()` method

### rustconn changes
- `rustconn/src/dialogs/connection/{protocol}_tab.rs` — connection dialog tab
- Session handling in `rustconn/src/session/`
- Sidebar icon mapping

### rustconn-cli changes
- `rustconn-cli/src/commands/{protocol}.rs` — CLI connect command

### Data model
- Fields in Connection struct (via ProtocolConfig enum or dedicated struct)
- Serialization compatibility (serde skip_serializing_if)

## Dependencies
- New crates needed: {list or "none"}
- Feature flags: {if optional}
```

### tasks.md template

```markdown
# Tasks: {Protocol} Protocol

## Dependencies

Kiro builds a dependency graph from this file and runs independent tasks
concurrently in waves, so state the dependencies instead of implying a single
sequence. For a new protocol the shape is almost always:

- Wave 1: Task 1 (core) — everything else needs `ProtocolType::{Protocol}` to exist
- Wave 2: Task 2 (tests), Task 3 (dialog), Task 5 (CLI) — all depend only on Task 1,
  and on nothing from each other; the CLI in particular does not wait on the GUI
- Wave 3: Task 4 (session handling, needs the dialog), Task 6 (i18n, needs the strings)

Listing them by crate top-to-bottom, as this template did until 2026-08-20, reads
as strictly sequential and serialises work that has no reason to be serial.

## Task 1: Core protocol implementation (rustconn-core)
- [ ] 1.1 Add `ProtocolType::{Protocol}` variant to enum
- [ ] 1.2 Create `rustconn-core/src/protocol/{protocol}.rs`
- [ ] 1.3 Implement `Protocol` trait (capabilities, default_port, connect)
- [ ] 1.4 Register module in `protocol/mod.rs`
- [ ] 1.5 Add protocol-specific fields to Connection model (if needed)

## Task 2: Property tests (rustconn-core)
- [ ] 2.1 Protocol type serialization round-trip
- [ ] 2.2 Capabilities correctness
- [ ] 2.3 Connection validation (port range, required fields)

## Task 3: Connection dialog (rustconn)
- [ ] 3.1 Create `rustconn/src/dialogs/connection/{protocol}_tab.rs`
- [ ] 3.2 Add tab to connection dialog notebook
- [ ] 3.3 Wire save/load for protocol-specific fields
- [ ] 3.4 All labels via `i18n()`

## Task 4: Session handling (rustconn)
- [ ] 4.1 Handle connection in session manager
- [ ] 4.2 Tab creation (terminal or embedded widget)
- [ ] 4.3 Disconnect/reconnect logic

## Task 5: CLI handler (rustconn-cli)
- [ ] 5.1 Add subcommand to CLI
- [ ] 5.2 Implement `cmd_{protocol}()` using core connect logic

## Task 6: i18n & accessibility
- [ ] 6.1 Wrap all strings in `i18n()`
- [ ] 6.2 Run `po/update-pot.sh`
- [ ] 6.3 Accessible labels on all interactive widgets
```

---

## Bugfix Spec

Use for critical bugs where traceability is needed.

> **Tip:** for a typical bugfix workflow see `bugfix-workflow.md` (manual inclusion: `#bugfix-workflow`).

### .config.kiro

```json
{"workflowType": "requirements-first", "specType": "bugfix"}
```

### requirements.md template

```markdown
# Requirements: Fix {Bug Title}

## Problem Statement
{Bug description, exact reproduction steps — the conditions that trigger it}

## Behaviour, in EARS

Write all three. The third one is the regression guard and is the reason this
template exists.

### Current (the defect)
WHEN {condition} THEN the system {incorrect behaviour}

### Expected (the fix)
WHEN {condition} THEN the system SHALL {correct behaviour}

### Unchanged (must keep working)
WHEN {sibling condition} THEN the system SHALL CONTINUE TO {existing behaviour}
WHEN {adjacent feature} THEN the system SHALL CONTINUE TO {existing behaviour}

## Constraints
- MUST NOT break: {list}
- MUST preserve: {API compatibility, on-disk config format, etc.}

## Acceptance Criteria

Each criterion restates one EARS line, so it maps to a test rather than to a
feeling. Phrase it as a property (a universal statement) where possible —
"for any {input class}, {invariant} holds" — because that is what turns into a
property test in `rustconn-core/tests/properties/`.

- [ ] Expected: WHEN {condition} THEN {correct behaviour}  → test `{name}`
- [ ] Unchanged: WHEN {sibling} THEN CONTINUE TO {behaviour} → test `{name}`
- [ ] Regression test added and fails against the pre-fix commit
- [ ] `cargo clippy --all-targets` → 0 warnings, from a run that re-checked
- [ ] CHANGELOG.md `### Fixed` entry
```

Why EARS and why the third clause: the format makes a requirement unambiguous,
directly testable and traceable, and the `SHALL CONTINUE TO` line is what stops a
fix from breaking a sibling caller. This repo has the scar — `quality-gate.md`
records three consecutive web-toolbar/split-view focus fixes in 0.20.0, each one
repairing the previous one's incompleteness. Naming the unchanged behaviour up
front is cheaper than the third fix.

Sources: [Kiro bugfix specs](https://kiro.dev/docs/specs/bugfix-specs.md),
[best practices](https://kiro.dev/docs/specs/best-practices.md). Note that
kiro.dev serves clean Markdown at any docs URL with a `.md` suffix; the HTML
returns only site chrome.

### tasks.md template

```markdown
# Tasks: Fix {Bug Title}

## Task 1: Reproduce
- [ ] 1.1 Write failing test that demonstrates the bug
- [ ] 1.2 Write a test for each `SHALL CONTINUE TO` line — it must pass *before*
      the fix too, otherwise it is not a regression guard

## Task 2: Fix
- [ ] 2.1 Identify root cause; grep every caller of the function being changed
- [ ] 2.2 Implement minimal fix in the shared function, not per call site
- [ ] 2.3 Verify the bug test passes and the unchanged-behaviour tests still pass

## Task 3: Verify
- [ ] 3.1 Run full test suite
- [ ] 3.2 Check related functionality not broken

## Task 4: Record (required)
- [ ] 4.1 Update CHANGELOG.md under `### Fixed`
```

`CHANGELOG.md` sat under an `(optional)` Task 3 here until 2026-08-20. It is item
6 of the Definition of Done, and a bugfix is user-facing by definition, so the
template was marking a required gate optional. Steps 3.1/3.2 are the genuinely
skippable ones for a small, well-isolated fix.

---

## Refactoring Spec (Design-First)

For refactoring where you know what you want to change.

### .config.kiro

```json
{"workflowType": "design-first", "specType": "feature"}
```

Skips requirements, starts with design describing current state → desired state → migration plan.
