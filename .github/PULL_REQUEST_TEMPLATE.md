<!--
Thanks for the patch. Keep this description short — the checklist below is the
part that saves review time.

Contribution guide: CONTRIBUTING.md
-->

## What this changes

<!-- One or two sentences. If it fixes an issue, write "Fixes #123". -->

## Why

<!--
The reasoning, not the diff. If you picked one approach over another (a new
crate, a different storage or threading model, a protocol backend), name the
alternative and why this one won — one line is enough.
-->

## How it was tested

<!--
Manual steps, the protocol and platform you exercised, or the test you added.
"CI is green" is not a test plan for a GUI or protocol change.
-->

## Checklist

- [ ] `cargo fmt --all` clean
- [ ] `cargo clippy --all-targets` reports 0 warnings, from a run that actually re-checked (a cache hit prints `Finished ... in 0.2s` and reports nothing)
- [ ] Relevant tests pass; non-trivial logic has one runnable check
- [ ] Crate boundaries intact — no `gtk4`/`adw`/`vte4` in `rustconn-core` or `rustconn-cli`, no `unsafe` outside a `rustconn-*-sys` crate
- [ ] Secrets use `SecretString`, intermediates are `Zeroizing`, nothing secret reaches a log, an error message, or a command argument
- [ ] New user-facing strings wrapped in `i18n()`/`i18n_f()`, source file listed in `po/POTFILES.in`, `bash po/update-pot.sh` run
- [ ] No `dbg!`, `todo!`, `println!`, or `eprintln!` left behind (`rustconn-cli` excepted — printing is its interface)
- [ ] `CHANGELOG.md` updated under `## [Unreleased]` if the change is user-facing
- [ ] Commits follow `type(scope): description`

<!--
If an external blocker (upstream bug, new compiler lint, flaky CI) kept you from
ticking a box, say so here rather than working around it silently.
-->
