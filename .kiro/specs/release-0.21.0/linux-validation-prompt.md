# 0.21.0 — Linux validation prompt

Paste the "Prompt" section into a fresh session **on the Linux machine**, on
branch `0.21.0` after it has been pushed.

Everything under "Known" was established on macOS and should not be re-derived.
Where it says **verified**, it was measured; where it says **unverified on
Linux**, that is the point of this session.

---

## Prompt

Branch `0.21.0` carries fifteen commits of audit work done on macOS. Every gate
that can run there is green and `./scripts/release.sh --dry-run` passes with
exactly one gate skipped. Your job is the half that macOS could not do, in this
order:

1. **Regenerate both `cargo-sources.json` manifests.** This is a hard blocker,
   not a nicety — see item 1 under Known.
2. **Run the full gate set on Linux**, where the default feature set actually
   builds. macOS cannot compile `web-embedded`, so a whole feature of the
   application was never linted or tested during this work.
3. **Build the packages** and confirm the Flatpak mirror change and the
   dependency bumps survive a real build.
4. **Decide the two deferred items** — the VTE ceiling and the duplicated secret
   detectors — and either do them or write down why not.
5. **Close the issue backlog** where the evidence allows it.

Do not cut the release. `./scripts/release.sh --dry-run` is the finish line; the
merge, the tag and the push are the maintainer's. Never pass `--yes`.

---

## Known — verified on macOS

### 1. Blocker: `cargo-sources.json` is stale in both manifests

`Cargo.lock` changed three times this release — the tray feature change removed
39 crates, then quick-xml and argon2 were bumped — for a net 733 → 695. Neither
`packaging/flatpak/cargo-sources.json` nor `packaging/flathub/cargo-sources.json`
was regenerated, because `flatpak-cargo-generator` needs the tool and network and
neither is available on the macOS box. Both still list 1455 entries, 15 of them
GTK3 crates that are no longer in the graph.

A lock file ahead of those manifests makes `flatpak-builder` vendor crates the
build then cannot find. `release.sh` checks this and reported it as the single
skipped gate; on Linux it will run, and it will fail until you regenerate. The
two files must come out byte-identical to each other.

### 2. `web-embedded` has not been compiled or tested at all this release

It is a **default** feature, it pulls WebKitGTK 6.0 through `webkit6`, and its
`-sys` build scripts fail on macOS on a missing `javascriptcoregtk-6.0.pc`. So
every clippy and test run in these fifteen commits used the macOS feature set
instead:

```
--no-default-features --features tray-macos,system-keyring,vnc-embedded,rdp-embedded,gfx-h264,rdp-audio,rd-gateway,adw-1-8
```

Nothing in the diff touches the embedded browser, but "nothing should have
changed there" is not the same as having checked. A plain
`cargo clippy --all-targets -- -D warnings` and `cargo test --workspace` on Linux
is the first thing that will have exercised it.

`scripts/verify.sh` now substitutes that feature set automatically on Darwin and
leaves the default path alone elsewhere, so on Linux it does the full thing with
no flags.

### 3. What the fifteen commits contain

Read `CHANGELOG.md` under `## [0.21.0]` for the reasoning; this is the map.

| Area | Commits |
|---|---|
| POT-vs-sources gate, and `update-pot.sh` made portable | `b10ad699`, `5bebc45c` |
| clippy gates that could pass on a cache hit (CI + `verify.sh`) | `933c35e9` |
| `release.sh` skip accounting, portable release-date check | `1276e1fc` |
| GTK3 stack out of `Cargo.lock` via `default-features = false` | `41c31537` |
| Four comments that described behaviour the code lacked | `da542aee` |
| `save_password_to_kdbx(&SecretString)` — breaking | `6a25e507` |
| Deadlines on 37 credential-path subprocess waits | `648f5e52` |
| quick-xml 0.42, argon2 0.6 + a KDF fixture | `72026977`, `dd67ca82` |
| macOS CI job, VTE and `adw-1-6` pins explained | `d504d61a` |
| Version bump and changelog propagation | `99a442b0`, `90fe972c` |

New file worth knowing about: `rustconn-core/src/proc.rs` — `wait_bounded`, the
one implementation of "wait for a child, kill it if it takes too long". Three
copies of that loop were deleted in favour of it.

### 4. Measured on macOS, worth re-measuring on Linux

- `cargo audit`: **10 warnings → 1**. The nine that went are the GTK3 bindings
  (RUSTSEC-2024-0412/0413/0415/0416/0418/0419/0420), `glib` unsoundness
  (-0429) and `proc-macro-error` (-0370). The one that remains is
  RUSTSEC-2023-0089, `atomic-polyfill`, reached by another route.
  **Correction to the 0.21.0 audit prompt**: none of those nine was ever in an
  allow-list. `.cargo/audit.toml` and `deny.toml` ignore exactly one advisory
  between them, RUSTSEC-2023-0071, and still do. There was nothing to delete.
- `argon2` 0.6 derives a **byte-identical** key to 0.5. Proved with a fixed
  vector captured before the bump, `PASSPHRASE_KDF_VECTOR` in
  `rustconn-core/src/secret/local_crypto.rs`. Existing portable stores open; no
  migration. Do not regenerate that vector.
- `cargo test -p rustconn-core`: 3413 tests. `verify.sh --tests`: 14 gates green.

---

## Work items

### A. Regenerate the Flatpak sources (blocker)

Both manifests, byte-identical, from the current `Cargo.lock`. Then re-run
`./scripts/release.sh --dry-run` and confirm the skipped-gate block reports
**nothing skipped**.

### B. Full Linux gate run

```bash
./scripts/verify.sh --tests
cargo clippy --all-targets -- -D warnings     # the default set, incl. web-embedded
cargo test --workspace
cargo deny check
cargo audit
```

Expect ~2.5 min for the test run. If `web-embedded` produces warnings, they are
pre-existing rather than caused by this branch — check against `main` before
fixing, and fix them here anyway.

### C. Package builds

- **Flatpak**: build it. Two things to confirm beyond "it built": the new
  `mirror-urls` on `inetutils` and `mc` resolve, and the bundled VTE is still
  0.80.5 (the ceiling is unchanged this release — see D1).
- **deb / RPM / AppImage**: these compile `rustconn-cli` as
  `--features full`, which the local gate now covers, but the packaging jobs are
  the only place the full matrix is built.
- **snap**: unchanged this release; `core24` + `gnome-46-2404` still.

### D. Two decisions deferred to this machine

**D1. The VTE ceiling.** `packaging/flatpak/*.yml` pins `< 0.81.0`. Until 0.21.0
no comment, commit or changelog entry said why, and three release notes asserted
it was "by design". The evidence now recorded in the manifest: `vte4` 0.10 with
`v0_76` selected, so the API in use is VTE 0.76's and 0.84 provides it; and the
maintainer's macOS build runs on Homebrew's 0.84.1 daily.

To lift it: raise the version, rebuild the Flatpak, and exercise **a Local Shell,
an SSH session, `mc` inside each, and a window resize mid-session**. That last
one matters because of the `mc` SGR-mouse workaround sitting beside the pin. If
it works, delete the `versions` block in all three manifests and say so in the
changelog. If it does not, write down which of the four broke — that is the
sentence nobody wrote the first time.

**D2. Two parallel secret-detector implementations.** Found while bounding the
timeouts, and the more interesting half of that work.
`rustconn-core::secret::detection` has eight `async` detectors, public and
re-exported, with **no caller anywhere in the workspace**. The Settings → Secrets
page has its own *synchronous* copies in
`rustconn/src/dialogs/settings/secrets_tab/detection.rs`, run in a
`std::thread::scope`. Both sets are now bounded, so the user-visible stall is
fixed either way, but one of the two is dead code and the duplication is the real
defect. Deciding which to delete needs a look at whether anything outside this
repository consumes the async API. Recorded in a doc comment; a 0.22 candidate if
you would rather not touch it now.

### E. Issue backlog

`gh` is not installed on the macOS box, so none of this could be done there.

Six are waiting on a reporter with the fix already shipped: **#301**, **#299**,
**#297**, **#295**, **#271**, **#234**. Two were fixed in 0.20.11 and the
reporters have not been told: **#303**, **#304**.

**#294** (terminal 24×80 in Flatpak) is the one genuinely open and blocked on
data. What would settle it and has still not been asked for: `stty size`
immediately after opening a session *and* after resizing the window, for a Local
Shell *and* an SSH session. That separates "the initial size is wrong" from
"resizes never arrive", which are different bugs.

Five are feature-scale and want a scope decision rather than an implementation:
**#262**, **#153**, **#151**, **#137**, **#129**.

---

## Loose ends deliberately left open

Not blockers. Each is a real finding, recorded rather than fixed, so a later
session does not have to rediscover it.

1. **`keepassxc-cli show` returns the password in an unwiped `Vec<u8>`.** The
   `Zeroizing` wrapper covers the `String` copied out of `Output.stdout`, not the
   buffer itself. Pre-existing on all three read paths in
   `secret/status.rs`. `proc.rs` is now the single choke point where it could be
   closed.
2. **`wait_bounded`'s output drain is unbounded** if a grandchild inherited the
   pipe, because `wait_with_output` then never sees EOF. Reachable in principle
   through `flatpak-spawn --host`. The deadline covers the exit wait only.
3. **`spool_to_cups` writes the document to `lp`'s stdin before the wait**, so a
   PostScript page larger than the pipe buffer blocks in the write and the 2 s
   budget never starts. Marked with a `ponytail:` note.
4. **`web-embedded` is a default feature that cannot build on macOS.** That is
   why the macOS CI job is scoped to the `-sys` crates and why `verify.sh` needs a
   Darwin special case. Making it optional-by-platform would remove both.
5. **A future-incompat warning on `block v0.1.6`**, a transitive macOS objc
   dependency. Does not fail any gate; will not appear on Linux.
6. **The metainfo file has no localisation at all** — no `xml:lang` entries, and
   the `itstool` step in `po/update-pot.sh` has never run (it is `|| true` and
   the tool is absent), so the AppStream name, summary and description show in
   English in every software centre. Separate feature, not a regression.
