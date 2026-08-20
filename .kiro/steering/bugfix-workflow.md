---
inclusion: manual
---

# Bugfix Workflow

Use this workflow for fixing bugs.

## Steps

1. **Reproduce** — understand exact conditions triggering the bug (reproduction steps)
2. **Find root cause** — use `context-gatherer` sub-agent to locate relevant files
3. **Name the unchanged behaviour** — before writing the fix, write down what must
   keep working, as `WHEN {condition} THEN the system SHALL CONTINUE TO {behaviour}`.
   Grep every caller of the function you are about to touch and list the siblings
   here. This is the step that stops a fix from breaking an adjacent path: 0.20.0
   shipped three consecutive focus fixes, each repairing the previous one's
   incompleteness (recorded in `quality-gate.md`). Skipping this step is how that
   happens.
4. **Write failing test** — property test or integration test reproducing the bug.
   Add a second test for at least one `SHALL CONTINUE TO` line from step 3.
5. **Fix** — minimal change, no refactoring. Fix the shared function once rather
   than the one call site the report names.
6. **Verify** — new tests pass, the unchanged-behaviour tests pass, clippy clean
   from a run that actually re-checked, other tests not broken
7. **Update CHANGELOG.md** — `### Fixed` section with bug description and issue link

## Referencing existing specs

When fixing a bug related to a documented feature, use `#spec:<name>` in chat to load the relevant spec context (requirements + design + tasks). Example:

```
#spec:terminal-activity-monitor the bell trigger fires on every output line — verify against the design
```

This loads all spec files into context so the fix aligns with documented decisions.

## When to Use Bugfix Spec

- Bug in critical path (auth, credentials, protocol handshake)
- Previous fix attempts caused regressions
- Root cause not obvious
- Documentation needed for the team

## When a Quick Fix in Chat is Enough

- Typo, simple logic error
- One-line change with obvious root cause
