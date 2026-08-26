---
inclusion: manual
description: "Prepares a release: checks for dependency updates across ALL types (cargo, CLI downloads, flatpak, snap), bumps version in all packaging files, propagates changelog, regenerates cargo-sources.json, and verifies consistency. Does NOT run git — the merge/tag/push is done manually via scripts/release.sh. Trigger manually and provide the version number (e.g. \"0.12.6\") in your message."
---

The user wants to PREPARE a release (file edits only). Read the `rustconn` power steering file `release.md` for the full checklist.

**IMPORTANT — NO GIT:** This hook ONLY edits files. Do NOT run any git command (no `git add`, `git commit`, `git merge`, `git checkout`, `git tag`, `git push`). The actual merge → tag → push is performed manually by `scripts/release.sh` after development is complete. Your job ends at leaving a clean, consistent working tree for that script to validate.

Then perform ALL of the following steps:

1. **Read version** from user message (e.g. "0.12.6"). If not provided, ask.

2. **DEPENDENCY FRESHNESS CHECK (report, then ask before applying)** — before bumping anything, audit EVERY dependency type for available updates so the release ships current deps. Report findings grouped; do NOT silently apply. Ask the user which (if any) to update before continuing.
   - **Cargo**: `cargo update --dry-run 2>&1` (patch/minor/major) + `cargo deny check advisories` (fallback `cargo audit`). Record applied updates in CHANGELOG.md `### Dependencies`.
   - **CLI downloads**: run `scripts/check-cli-versions.sh` (or read `rustconn-core/src/cli_download.rs`). For any outdated PINNED tool, update `pinned_version`, `download_url`, `aarch64_url`, and `checksum` (Static policy → fetch the new .sha256). Record under CHANGELOG.md `### Changed` as `- CLI downloads — Tool X.Y.Z→X.Y.W`.
   - **Flatpak** (`packaging/flatpak/*.yml` + keep `packaging/flathub/*.yml` in sync): check `org.gnome.Platform`/`org.gnome.Sdk` `runtime-version` (currently '50'), the `rust-stable` SDK extension, and bundled pinned source modules with `x-checker-data` (FreeRDP `freerdp-X.Y.Z.tar.xz`, cJSON). For any bump, update the version AND its `sha256`. Ensure flathub manifest FreeRDP version matches the local manifest (they can drift independently — sync flathub to local if behind).
   - **Nix** (`flake.nix`): version string must match workspace version (checked by `release.sh`). No dependency inputs to audit — uses nixpkgs unstable.
   - **Snap** (`snap/snapcraft.yaml`): check `base` (core24) and the `gnome` extension (gnome-46-2404) — flag if a core26 gnome extension now exists (issue #174 context) — plus pinned `stage-packages`/`build-packages` drift.
   - If everything is current, note 'dependencies up to date' and proceed.

3. **Version strings — one command, not a walk through a list.**

   ```bash
   scripts/bump-version.sh X.Y.Z            # dry run: shows the diff
   scripts/bump-version.sh X.Y.Z --write    # apply
   ```

   It takes the file list from `PKG_FILES` in `scripts/release.sh` at runtime, so
   the two cannot drift, and it writes the workspace `Cargo.toml` plus all 16
   version-only files — including **every** sibling path dependency in
   `rustconn/Cargo.toml`, which is five of them and the thing release.sh's own gate
   cannot catch. Every rule is line-anchored: a global replace would corrupt
   `rustconn.spec` (it records a dependency bump `cfg-expr 0.20.8→0.20.9`) and
   `docs/USER_GUIDE.md` (it says "Changed in 0.20.9" in prose about behaviour).

   After `--write` it re-checks its work with release.sh's own patterns and exits
   non-zero while anything is still out of sync. Expect exactly three failures at
   that point — the changelog-style files from step 5, which need content, not a
   substitution. Report them; do not hand-edit around the script.

   If it says a file has no rule, add a `case` to `rules_for()`. Do not bump that
   file by hand: the by-hand list is what this replaced.

4. **CHANGELOG.md** — verify a `## [X.Y.Z] - YYYY-MM-DD` section exists. If not, ask user to write it first. Ensure any dependency updates from step 2 are recorded in `### Dependencies` / `### Changed`.

5. **Propagate changelog** to ALL of these files (convert format as needed):
   - `debian/changelog` (Debian format, prepend)
   - `packaging/obs/debian.changelog` (same Debian format, prepend)
   - `packaging/obs/rustconn.changes` (OBS format, prepend)
   - `packaging/obs/rustconn.spec` (update `Version:` field + add `%changelog` entry)
   - `rustconn/assets/io.github.totoshko88.RustConn.metainfo.xml` (add `<release>` entry)

6. **Version strings** — already done in step 3 by `scripts/bump-version.sh`. This
   list used to live here in prose, sixteen bullets long, mirroring `PKG_FILES` in
   `scripts/release.sh` and asking to be kept in sync with it by hand. It is gone
   on purpose: the release had a gate whose input was produced by copying the
   gate's own configuration.

   One file the script does not touch, because the change is not a version line:
   `docs/CI_BUILD_FLOW.md` uses example version strings inside mermaid diagrams.
   Check whether any of them still reads as the current release and update it if so.

7. **Regenerate Cargo.lock** — run `cargo generate-lockfile`

8. **Regenerate cargo-sources.json** — run:
   ```
   python3 packaging/flatpak/flatpak-cargo-generator.py Cargo.lock -o packaging/flatpak/cargo-sources.json
   cp packaging/flatpak/cargo-sources.json packaging/flathub/cargo-sources.json
   ```

9. **Verify consistency** — grep for the OLD version across the repo (excluding Cargo.lock, target/, .git/) and report any remaining references that are NOT historical changelog entries.

10. **Run quality checks** — delegate to `rust-quality-check` sub-agent for fmt + clippy.

11. **Report summary** — list all files modified, the dependency-update decisions from step 2, and any issues found. Remind the user that NO git operations were performed and the next step is to run `./scripts/release.sh` manually (it does the merge → tag → push).
