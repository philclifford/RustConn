---
inclusion: fileMatch
fileMatchPattern: "**/*.rs"
---

# Pragmatic Rust Guidelines (Microsoft) — RustConn Adaptation

Adaptation of [Microsoft Pragmatic Rust Guidelines](https://microsoft.github.io/rust-guidelines/) for RustConn.
Supplements `project-rules.md`, does not replace it. Only lists points missing from other steering files.

## Universal

### M-LINT-OVERRIDE-EXPECT — `#[expect]` instead of `#[allow]`

When locally overriding a clippy/compiler lint — use `#[expect(..., reason = "...")]`.
`#[expect]` emits a warning if the lint did not fire, preventing accumulation of stale overrides.

```rust
#[expect(clippy::unused_async, reason = "API stable, I/O will be added later")]
pub async fn ping_server() { }
```

`#[allow]` remains appropriate only in macros and generated code.

### M-PANIC-IS-STOP / M-PANIC-ON-BUG — panic = "the program must stop"

Panic is not an exception. `panic!()` means "stop the program now". Do not use panic for:
- communicating errors upward (that is what `Result` does),
- handling controlled conditions (timeout, unreachable host, wrong password),
- assuming the panic will be caught (if `panic = "abort"` — the program crashes).

Valid cases: `expect("must never happen")` for programming bugs, `unwrap()` on `OnceLock::get_or_init`, panic on poisoned lock.

Programming bug → `panic!` / `unreachable!` / `debug_assert!`. Recoverable state → `Result<T, ThisError>`. Do not mix.

### M-DOCUMENTED-MAGIC — document magic values

Any magic constant or default behavior must have a comment.
Especially relevant for timeouts, retry backoffs, buffer limits.

```rust
// Vault operations wait 10 seconds — Bitwarden CLI may trigger a master-pw prompt.
const VAULT_OP_TIMEOUT: Duration = Duration::from_secs(10);
```

### M-LOG-STRUCTURED — structured logging

We already use `tracing`. Additionally:
- pass data as fields, not as a formatted string: `tracing::info!(host = %h, port = p, "connecting")` instead of `tracing::info!("connecting to {}:{}", h, p)`,
- never log `SecretString` (`expose_secret()` in `tracing::*` — forbidden).

## Applications (rustconn / rustconn-cli)

### M-MIMALLOC-APP — global allocator

[Not critical, optional]. Apps can gain ~10–25% speedup on hot paths by replacing the allocator with `mimalloc`. If profiling shows allocation is a bottleneck, add:

```toml
[dependencies]
mimalloc = "0.1"
```

```rust
// rustconn/src/main.rs
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;
```

### M-APP-ERROR — `anyhow` is allowed in `rustconn` / `rustconn-cli`

Binary crates may use `anyhow` / `eyre` to reduce boilerplate.
Library functions in `rustconn-core` MUST still use `thiserror::Error`
(M-ERRORS-CANONICAL-STRUCTS — so that callers from GUI/CLI can pattern-match variants).

## Safety

### M-UNSAFE — `unsafe` is confined to the `-sys` crates

Workspace `[lints.rust] unsafe_code = "deny"`, re-opened only by a crate-level
`#![expect(unsafe_code, reason = "…")]` in the three sanctioned FFI crates:
`rustconn-pty-sys` (macOS PTY controlling terminal), `rustconn-locale-sys` (the
startup `setlocale` call) and `rustconn-env-sys` (the startup `GSK_RENDERER` and
`LANGUAGE` writes). `deny` rather than `forbid` because `forbid` cannot be
overridden at any level, which forced each helper to declare its own `[lints]`
table — and a crate-local `[lints]` table replaces the inherited one, so those
three ended up as the only crates in the workspace running without
`clippy::pedantic`, `clippy::nursery` or `clippy::unwrap_used`. Do not "tighten"
this back to `forbid` without also solving that.
If further FFI is ever needed — create another small `rustconn-*-sys` crate with
a documented `// SAFETY:` contract on every `unsafe` block, rather than relaxing
the lint where the caller lives. Miri cannot execute the syscalls/FFI used here
(`pre_exec`, `ioctl`, `setlocale`, `setenv`), so prefer a contract unit test
(asserting preconditions/behaviour where observable) over a Miri job — see
`rustconn-locale-sys` and `rustconn-env-sys`, where the precondition guard is a
testable type precisely because the FFI call itself is not reachable from a test
harness. Keep the new crate an unconditional dependency even when only one
platform reaches the call: CI has no macOS runner, so a platform-gated `-sys`
crate is `unsafe` that never gets compiled.
Do not allow unsafe to "spread" across the main crates.

## Documentation

### M-CANONICAL-DOCS — sections in doc comments

Public functions in `rustconn-core` must have:

```rust
/// Summary in one sentence, up to 15 words. (M-FIRST-DOC-SENTENCE)
///
/// Extended description.
///
/// # Errors
/// Returns `MyError::X` if ...
///
/// # Panics
/// Panics if ... (only for programming bugs, see M-PANIC-ON-BUG)
pub fn foo() -> Result<(), MyError> { ... }
```

Do not create a parameter table — describe them in the introductory sentence: `Copies a file from src to dst`.

### M-PUBLIC-DEBUG for types with secrets

If a type contains `SecretString` or credentials — `Debug` must be manual and covered by a test.
`secrecy::SecretString` already redacts itself in `Debug`, but wrappers around it need verification.

```rust
#[test]
fn debug_does_not_leak_secret() {
    let creds = Credentials::new("user", SecretString::new("hunter2".into()));
    let rendered = format!("{creds:?}");
    assert!(!rendered.contains("hunter2"));
}
```

## Naming — compromise M-CONCISE-NAMES

MS guideline recommends avoiding `Manager` / `Service` / `Factory`. We historically have
`ConnectionManager`, `SessionManager`, `SecretManager` — these names stay for compatibility.
For **new** code — choose more specific names: `ConnectionStore`, `SessionRouter`,
`CredentialResolver`, `SnippetCatalog`.

## Universal lints — recommended additions

Consider adding to `[workspace.lints.rust]` (optional, non-blocking):

```toml
missing_debug_implementations = "warn"
unsafe_op_in_unsafe_fn = "warn"  # not relevant, we have forbid
unused_lifetimes = "warn"
redundant_lifetimes = "warn"
```

And to `[workspace.lints.clippy]` from the restriction group:

```toml
allow_attributes_without_reason = "warn"  # forces reason = "..." in #[allow] / #[expect]
clone_on_ref_ptr = "warn"                 # catches .clone() on Rc/Arc — write Rc::clone()
empty_drop = "warn"
undocumented_unsafe_blocks = "warn"        # not relevant, we have forbid
```

Verify this does not break the build: `cargo clippy --all-targets`.

## References

- Checklist: <https://microsoft.github.io/rust-guidelines/guidelines/checklist/>
- Universal: <https://microsoft.github.io/rust-guidelines/guidelines/universal/>
- Apps: <https://microsoft.github.io/rust-guidelines/guidelines/apps/>
- Safety: <https://microsoft.github.io/rust-guidelines/guidelines/safety/>
- Docs: <https://microsoft.github.io/rust-guidelines/guidelines/docs/>
- Rust API Guidelines (upstream): <https://rust-lang.github.io/api-guidelines/>
