# 0.21.0 — audit prompt

Paste the "Prompt" section into a fresh session. Everything under "Known" is
already verified and should not be re-derived: it was established while preparing
0.20.11, with file references so the audit can start from evidence instead of a
search. Anything marked **unverified** is a hypothesis, not a finding.

---

## Prompt

Audit everything that landed in the 0.20.\* series and turn it into the plan for
0.21.0. The series is twelve releases in fourteen days — 0.20.0 on 2026-08-13
through 0.20.11 on 2026-08-27 — shipped one fix at a time under user pressure. I
want to know what that pace left behind before I open a minor version.

Work in this order and stop at the end of each step to show me what you found
before acting on it:

1. **Read the 0.20.\* changelog span** (`CHANGELOG.md`, `## [0.20.0]` down to
   `## [0.20.11]`) and reconstruct what the series actually changed, grouped by
   subsystem rather than by release. I am looking for the places that were
   touched three or four times over the fortnight — repeated visits to one area
   usually mean the first fix was aimed at a symptom. Item 0 below is one such
   pattern already identified; treat it as a worked example of what to look for,
   not as the complete list.
2. **Check the claims** in those entries against the code. An entry that
   describes behaviour the code no longer has is worse than no entry.
3. **Take the confirmed list below into work**, in the order you judge right,
   telling me the order and why before you start.
4. **Close the issue backlog** where it can be closed, and say what evidence is
   missing where it cannot.

Constraints:

- This is a **minor** version, so a requirement bump or an API change is allowed
  where a patch release would have refused it. Deleting behaviour still is not.
- Do not cut the release. `./scripts/release.sh --dry-run` is the finish line;
  the tag and the push are mine. Never pass `--yes`.
- The Definition of Done in `.kiro/steering/core-rules.md` applies to every step,
  and a clippy run that reports zero warnings from a cache hit does not count —
  see the first item under "Known" for why that is not a hypothetical.
- Prefer deleting over adding. Several items below are *removals*.

---

## Known — confirmed, take into work

### 0. The pattern behind three of the last four bugs — start here

Three separate reports in this series turned out to be the same shape:
`rustconn-core` grows a correct, tested resolver, and the GUI launch paths keep an
older hand-rolled copy that never learns about it. The core function is right, the
tests pass, the feature is announced, and the code that actually runs does
something else.

| Issue | Correct thing in core | What the launcher did instead |
|---|---|---|
| #303 | `which_binary` was sandbox-aware | SPICE detection had its own `which` probe, and ~25 sites each spawned `which` |
| #304 | the per-tab teardown killed the child | three exit paths each carried their own incomplete shutdown list |
| **#301** | `resolve_ssh_jump_host_id`, three tiers, seven tests | the SSH launcher reads `ssh_config.jump_host_id` raw; RDP/VNC/SPICE gate the tunnel on the same raw field |

**Do this as a sweep, not case by case.** For every resolver `rustconn-core`
exposes over a connection or settings field, find whether anything reads the
underlying field directly instead of calling it. `resolve_ssh_key_path`,
`resolve_ssh_agent_socket`, `resolve_ssh_auth_method`, the automation-inheritance
resolvers and the credential resolution chain are all candidates by construction.
The mechanical version of the question: grep the `rustconn` crate for direct field
accesses that a core resolver also covers, and for `_config.` reads inside launch
functions.

The common root is that a resolver in core cannot be enforced from core — nothing
stops a caller reading the field. Worth considering whether the fields that have
resolvers should stop being `pub`, so that bypassing one is a compile error rather
than a bug report. That would be a breaking change inside the workspace, which a
minor version can absorb.

### 0a. Issue #301 is open, confirmed, and unfixed

Diagnosed in full on 2026-08-28; the evidence and a ready reply to the reporter
are in `issue-301-reply.md` beside this file. Summary of the defect:

- `rustconn/src/window/protocols_ssh.rs:301` resolves the free-text `ProxyJump`
  through all three tiers, and `:310` reads the picker's `jump_host_id` straight
  off the connection. So **Global and Group Jump Host (picker) are ignored at
  connect time**, while the free-text equivalents work.
- `resolve_ssh_jump_host_id` has **no caller in the `rustconn` crate**. Only
  `resolve_jump_chain` calls it, and that is used by the editor subtitle, the SFTP
  browser and `mc` — never by a launcher. The editor therefore displays
  "Inherited: …" for a bastion the launcher will not use.
- RDP (`rdp_vnc.rs:273`), VNC (`:1267`) and SPICE (`protocols.rs:1224`) gate the
  entire tunnel on the connection's own `jump_host_id`, so for those protocols
  *neither* form inherits. The 0.20.9 changelog's justification for putting
  Network Mode on the Basic page — that it applies to RDP/VNC/SPICE too —
  describes an intent the launch code does not implement.

The fix is bounded and the precedence does not change: route the launchers through
`resolve_jump_chain`, which exists precisely to be the single implementation.
**Add the test that would have caught it** — nothing anywhere asserts the SSH argv
or the tunnel decision, which is exactly why a resolver with correct precedence
and seven passing tests shipped alongside launch paths that bypass it.

### 1. The CI clippy gate can pass without checking anything

`.github/workflows/ci.yml:222-233` caches `target/` keyed on
`hashFiles('**/Cargo.lock')`, with a `restore-keys` prefix fallback. With the
lock unchanged it restores a tree clippy already linted, so
`cargo clippy --all-targets -- -D warnings` prints `Finished` and exits 0 without
looking at anything. Nineteen warnings accumulated behind it and were only found
because 0.20.11 changed `Cargo.lock` and forced a real run. They are fixed;
**the gate is not.**

The 0.20.11 changelog records this deliberately without fixing the cache, because
it is a build-time trade-off and the decision is yours. Options to weigh:
dropping `target` from the cached paths in the clippy job only (keeping the
registry cache), or adding a scheduled uncached run. Whichever way, this is the
same failure class as 0.20.9 — a gate agreeing with a tree the packaging jobs
reject — and it has now happened twice.

### 2. Gates that silently skip on the maintainer's platform

`release.sh --dry-run` on macOS skips three checks and says so only in `[warn]`:
GNU `date -d` is absent (the cross-format release-date check), `typos` is not
installed, and `flatpak-cargo-generator` needs network. A fourth,
`check-i18n-escapes.sh`, was *failing* — it declared `#!/bin/bash` while using
`mapfile`, and `/bin/bash` on macOS is 3.2.57. Fixed in 0.20.11.

Audit the rest of `scripts/` for the same shape: a gate that cannot run where it
is needed is worth less than no gate, because it reports success. Note
`scripts/macos-ci.sh` and `scripts/macos-build.sh` exist and **no workflow
references either** — verified.

### 2a. One download host can hold back an entire publication

The 0.20.11 release job proved this the day it was cut. `ftp.gnu.org` went into an
outage, `flatpak-builder` timed out fetching inetutils — the third of twelve
modules — and because the GitHub release, OBS, Snap and Homebrew jobs all
`needs: build-flatpak` (`.github/workflows/release.yml:493` and 699/728), nothing
was published at all, while deb, RPM and AppImage sat there built and fine. The
tag and the merge to main existed with no release behind them: the same shape as
0.20.9, from a completely different cause.

`mirror-urls` was added for inetutils and mc afterwards, and `slang` was
deliberately left without one — see the `[Unreleased]` changelog entry. Two
things are still open and are the audit's business:

- **The dependency shape of `release.yml`.** Ask whether `create-release` should
  really be gated on every package job succeeding, or whether it should publish
  what built and let a missing artifact be added later. A transient network
  failure in one of twelve Flatpak modules currently costs the whole release.
- **The remaining single-homed hosts**: `www.jedsoft.org` (slang),
  `pub.freerdp.com`, and `ftp.midnight-commander.org`, which is also the only
  source still fetched over plain HTTP. Verify any mirror byte-for-byte before
  trusting it; an unverified mirror converts a download failure into a checksum
  failure.

### 3. macOS has no CI coverage at all, and it has already cost something

No job builds macOS. A contract test in `rustconn-pty-sys` — the one proving the
`pre_exec` hook runs in the forked child, which is what makes SSH password
prompts work (#175) — had never passed on macOS: it accepted `ENOTTY` and
`EPERM`, and macOS answers `ENODEV` for `/dev/null`. Fixed in 0.20.11, but it sat
there unnoticed, and it guards `unsafe`.

Decide whether 0.21.0 gets a macOS job. If not, say what compensates.

### 4. Credential handling: two pre-existing gaps found by review

Both were flagged by `security-reviewer` during 0.20.11 and deliberately left
alone as out of scope for a patch release. A minor version is the right place.

- **`rustconn-core/src/secret/status.rs`** — `save_password_to_kdbx` takes the
  entry password as `password: &str`, not `&SecretString`. Every current caller
  passes a borrow of a `Zeroizing` buffer or `expose_secret()` directly, so no
  unwiped copy exists today; the signature does not enforce that for the next
  caller. It is a public `rustconn-core` API, so this is a breaking change and
  belongs in a minor bump.
- **No timeouts on credential operations.** Every `keepassxc-cli` invocation in
  `status.rs` uses an unbounded `wait_with_output()` (~11 sites) against the
  project's own 30 s credential-resolution budget; only the bulk-transfer path in
  `vault_ops.rs` wraps them. The async detectors in
  `rustconn-core/src/secret/detection.rs` have the same gap at `bw status` (~285),
  `op whoami` (~430) and `passbolt list` (~541), and `detect_password_managers`
  joins all eight probes — one unresponsive CLI stalls the whole Secrets tab.
  `secret/bitwarden.rs:410` and `secret/keyring.rs:59` already establish the
  pattern to copy.

### 5. Two dependency updates deferred from 0.20.11

Both need a requirement widened, which is why they waited:

- **argon2 0.5 → 0.6** — the KDF of the encrypted credential store. Check
  whether the stored format is affected before touching it; if it is, existing
  stores need a migration path, and that decides whether this lands at all.
- **quick-xml 0.41 → 0.42** — an API break in the importers.

Everything else `scripts/dep-audit.sh` reports as held back is a pre-release pin
carried by ironrdp/picky/sspi, not our choice.

### 6. The tray drags a whole gtk3 stack into the lock file

Verified from `Cargo.lock`: `muda` → `libappindicator` → `gtk` 0.18, which brings
`atk`, `gdk`, `gtk-sys`, `gtk3-macros` and `glib` 0.18. That accounts for several
of the ten allow-listed advisories in `.cargo/audit.toml` / `deny.toml`, including
`RUSTSEC-2024-0429` (glib unsoundness) and `RUSTSEC-2024-0419` (gtk3 bindings
unmaintained).

`muda` and `tray-icon` are plain optional deps enabled by the `tray-macos`
feature (`rustconn/Cargo.toml:43-45,102`); the Linux tray uses `ksni`, which is
pure D-Bus. Verified: there is no `[target.*]` section in that manifest at all.

So the gtk3 stack is only ever *built* on macOS — but `cargo audit` scans
`Cargo.lock`, which is target-agnostic, so the advisories are reported for
everyone regardless. **Target-gating the two deps would therefore not clean this
up**, and the obvious-looking fix is the wrong one. The real levers, in order of
preference: a feature flag on `tray-icon`/`muda` that drops their
`libappindicator` backend (check their manifests — this is the part I did not
verify), a different macOS tray crate, or leaving the allow-list alone and
recording *why* in `.cargo/audit.toml` rather than only *that*. Decide
deliberately; the current entries say what is ignored without saying that the
code is never compiled on the platforms the advisories concern.

### 7. The `TerminalNotebook` god object, and the evidence it is costing money

The `// ponytail:` comment at `rustconn/src/terminal/mod.rs:14-25` says the type
is one struct with ~156 methods, that the real problem is the parallel per-tab
collections every method reaches across, and that the upgrade path is to extract
a `TerminalTab` owning its own widget, session handle and reconnect state —
explicitly "do that before splitting another file off".

0.20.11 is the argument for doing it now. Issue #304 was a session teardown that
existed in one of three exit paths, and the reason it was easy to write an
incomplete teardown is that "everything belonging to a session" is spread across
`vte_child_pids`, `pty_relays`, `pty_size_timers`, `session_widgets`,
`ssh_tunnels`, `session_info`, `child_exited_handlers` and more. I consolidated
the *exit paths* into `window::shutdown_sessions_for_exit`; the state is still
scattered. If you take this on, scope it tightly and land it behind a green test
suite in stages — it is the largest item on this list by an order of magnitude.

### 8. Documentation that contradicts the code

Found incidentally while working on 0.20.11; assume there is more.

- `.kiro/steering/rust-pragmatic-guidelines.md` (M-UNSAFE section) states the
  `-sys` crates run without `clippy::pedantic`/`nursery` because a crate-local
  `[lints]` table replaces the inherited one. **`rustconn-pty-sys/Cargo.toml`
  has `[lints] workspace = true`** and does inherit them — that is how a
  `cast_lossless` warning appeared there at all. Check the other three helpers
  and fix the steering to match reality.
- `flush_active_recordings` in `rustconn/src/terminal/recording.rs:302` documents
  itself as sending `exit` to each recording session; the body detaches the
  recorder and writes metadata, and sends nothing.
- `external_session.rs` kills owned viewers with `SIGKILL` only, while the VTE
  path now has a documented two-stage `SIGTERM`→`SIGKILL` escalation with a
  PID-reuse guard. Decide whether they should agree, and write down the answer
  either way.

### 9. Platform and packaging pins worth re-deciding in a minor version

- **VTE pinned below 0.81** in both Flatpak manifests (`x-checker-data` says
  `< 0.81.0`) while 0.84 is released and this machine has 0.84.1 installed. The
  `vte4` crate is 0.10 and the snap targets core24's VTE 0.76. The reason for
  the ceiling is not written down anywhere I found — establish it, then decide.
- **`adw-1-6` / `adw-1-7` / `adw-1-8` features** exist because the snap's
  `core24` gnome platform ships libadwaita 1.5. `rustconn/Cargo.toml` says to
  retire them "once no supported target is below 1.6". Check whether a `core26`
  gnome extension has shipped; if it has, this is a deletion.
- **musl is unsupported and undocumented as such.** `TIOCSCTTY` and `TIOCSWINSZ`
  in `rustconn-pty-sys` are cast to `libc::c_ulong`; musl types both the
  constants and `ioctl`'s parameter as `c_int`, so neither would compile. No
  current target is musl. If a musl target is ever wanted, that is where it
  breaks — recorded in the crate, not in the packaging docs.
- **snap has no `web-embedded`** (#244) and the reason is documented at length in
  `snap/snapcraft.yaml`. Nothing to fix; confirm it is still true.

---

## Known — the issue backlog

Fourteen open issues. Five are waiting on a reporter and the code is already in
(#301 was in this group until it was diagnosed — see item 0a; do not assume the
rest are fine just because a fix shipped):

| # | State |
|---|---|
| #299 | sidebar context menu, fix in 0.20.7 |
| #297 | reconnect banner, `ChildExitHook` fix in 0.20.7 |
| #295 | shortcut conflicts — `find_accel_conflict` already covers the non-rebindable list and cites #295 |
| #271 | reporter confirmed Backspace; Delete was referred to the device |
| #234 | RDM parser hardened in 0.19.13; waiting on a sample export |

Two just fixed in 0.20.11 and needing the reporters told: **#303** (SPICE viewer
detection) and **#304** (session children on exit).

One confirmed open bug with the diagnosis already done — **#301**. It is listed
above as item 0a rather than here, because the interesting part is the pattern it
belongs to rather than the issue itself. The reply to the reporter is drafted in
`issue-301-reply.md`; it has not been posted.

One genuinely open and blocked on data — **#294**, terminal size 24×80 in
Flatpak. The maintainer has already said it is sandbox-related. What would settle
it, and has not been asked for: `stty size` immediately after opening a session
*and* after resizing the window, for a Local Shell *and* for an SSH session. That
separates "the initial size is wrong" from "resizes never arrive", which are
different bugs with different fixes. The mechanism to check on our side is that
`script` copies the window size from its stdin once at startup and
`flatpak-spawn` does not forward `SIGWINCH` — both already described in the
comments at `rustconn/src/window/mod.rs:4029` onwards.

Five are feature-scale and want a decision about the 0.21.0 scope rather than an
implementation: **#262** (embedded RDP GFX delivers 0 fps against Windows 11
25H2, with server-side counter data in the thread), **#153** (RemoteApp),
**#151** (embedded Chromium), **#137** (Windows), **#129** (Android).

---

## What 0.20.11 already did, so the audit does not redo it

- #304: session children now die on all three exit paths, consolidated into
  `window::shutdown_sessions_for_exit`; the tray's Quit no longer bypasses
  everything.
- #303: executables resolved in process by `rustconn_core::which` instead of
  spawning `which`, across ~25 call sites; SPICE gained the sandbox search paths
  and a `flatpak-spawn --host` fallback.
- The nineteen clippy warnings, and the macOS-only `rustconn-pty-sys` test failure.
- FreeRDP 3.30.0 → 3.31.0 in all three Flatpak manifests (22 security advisories
  upstream, "distributors update ASAP"), waypipe 0.11.0 → 0.11.2, five Cargo
  patch/minor bumps.
- `check-i18n-escapes.sh` shebang.
