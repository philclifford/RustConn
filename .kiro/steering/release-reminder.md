---
inclusion: auto
name: release-reminder
description: "Mandatory steps when bumping the workspace version in Cargo.toml — changelog propagation to debian/OBS/metainfo, dependency refresh, CLI version check. Use when preparing a release, bumping a version, or writing release notes."
---

# Release Process Reminder

> Was `inclusion: fileMatch` on `Cargo.toml` until 2026-08-12, which fired on
> *any* read of `Cargo.toml` — including plain dependency lookups and audits that
> have nothing to do with a release. `auto` matches the request instead of the
> file, so it now loads when the task actually is a release. Force it with
> `/release-reminder` if the matcher misses.

When the workspace version in `Cargo.toml` is being bumped:

## Mandatory Steps (in order)

1. **CHANGELOG.md** — add `## [X.Y.Z] - YYYY-MM-DD` section with `### Fixed` / `### Added` / `### Improved` etc.
2. **Changelog propagation** — after writing CHANGELOG.md, propagate to:
   - `debian/changelog` — new entry at top (Debian format)
   - `packaging/obs/debian.changelog` — same format
   - `packaging/obs/rustconn.changes` — OBS changes format
   - `packaging/obs/rustconn.spec` — add `%changelog` entry
   - `rustconn/assets/io.github.totoshko88.RustConn.metainfo.xml` — add `<release>` entry
3. **Dependency updates** — run and record results:
   ```bash
   cargo update --dry-run    # preview
   cargo update              # apply
   cargo check --all-targets # verify
   ```
   Add updated crates to CHANGELOG.md `### Dependencies` section.
4. **CLI version check** (if `scripts/check-cli-versions.sh` exists):
   ```bash
   ./scripts/check-cli-versions.sh
   ```
   If updates available — update `rustconn-core/src/cli_download.rs` and record in CHANGELOG.md.

## Important

- Version-number propagation to packaging files (flatpak/flathub tags, dsc files, AppImage, docs, spec `Version:` field) is handled by the **manual `release-version` hook** during finalize — run it when testing is done.
- YOU must handle all changelog/release-notes files manually — neither hook creates changelog entries
- For full release process details, activate the `rustconn-dev` power and read `release.md` steering
