# rustconn-locale-sys

Sanctioned FFI crate (M-UNSAFE). It wraps exactly one call: `setlocale(3)`, the
one part of the gettext API that cannot be made safe. It replaces process-global
locale state non-atomically and reads the environment without synchronisation, so
it is sound only while the process is single-threaded — the unsoundness filed as
[RUSTSEC-2026-0244](https://rustsec.org/advisories/RUSTSEC-2026-0244).

Two functions, and that is the whole surface: `init_locale` to do it,
`seal_locale` to close the window in which doing it is allowed.

Rules for editing this crate:

- **Do not re-export the rest of gettext.** `bindtextdomain`,
  `bind_textdomain_codeset`, `textdomain`, `gettext`, `ngettext` are all safe and
  are called directly from `rustconn`. Adding them here would turn a
  one-call contract wrapper into a gettext facade, and the contract is the point.
- `gettext-rs` is the only dependency. Keep it that way.
- The contract `init_locale` enforces, and which any change must preserve: called
  from `main()` before any thread exists — before the tokio runtime, before
  GTK/GIO, before the tracing subscriber; from the same thread every time; before
  any POSIX signal handler is installed; and never after `seal_locale`.
- The guard is a **testable type** precisely because the FFI call is not reachable
  from a test harness. Miri cannot execute `setlocale`. When you change the
  precondition logic, the test that must change with it is the contract test on the
  guard, not a Miri job.
- A refusal is a returned outcome, not a panic — except where the guard documents
  a panic as the deliberate signal that a caller violated the startup ordering.
  Read the existing behaviour before changing which one a path uses.
- Every `unsafe` block: one operation, one `// SAFETY:` comment naming the
  invariant it establishes. `undocumented_unsafe_blocks` and
  `multiple_unsafe_ops_per_block` are on workspace-wide.
