# rustconn-env-sys

Sanctioned FFI crate (M-UNSAFE). It wraps exactly one call: `std::env::set_var`,
which is `unsafe` in the 2024 edition because it is `setenv(3)` — a mutation of the
process-global environment block with no synchronisation. A concurrent `getenv`
from any thread, including one inside a C library RustConn links, can read a
half-updated block or keep a string `setenv` has already freed.

Two functions, and that is the whole surface: `set_startup_var` to do it,
`seal_env` to close the window in which doing it is allowed.

Rules for editing this crate:

- **`set_startup_var` has exactly two callers, in this order:** `LANGUAGE` from
  `i18n.rs`, then `GSK_RENDERER` from `renderer.rs`. The second one seals the
  window. A third caller added later panics rather than quietly working, and that
  is the designed outcome, not a bug to route around.
- The bar for a third caller is high: the variable must be read by a C library
  later, with no API to pass it instead. GTK offers nothing but `GSK_RENDERER` for
  renderer choice; gettext offers nothing but `LANGUAGE`. "It was convenient" does
  not qualify — use an argument, a field, or a config value.
- **Reading the environment is safe and does not belong here.** Other crates call
  `std::env::var` directly. Do not add a getter for symmetry.
- No dependencies at all. Keep it that way.
- The precondition guard is a **testable type** because the FFI call is not
  reachable from a test harness, and Miri cannot execute `setenv`. Change the
  guard, change its contract test.
- Every `unsafe` block: one operation, one `// SAFETY:` comment naming the
  invariant it establishes. `undocumented_unsafe_blocks` and
  `multiple_unsafe_ops_per_block` are on workspace-wide.
