# Kiro config templates

Kiro refuses agent writes to `~/.kiro/settings/`, `.kiro/settings/`,
`~/.kiro/workspace-roots/` and `~/.kiro/sandbox-state/` (`deny fs_write … Source: kiro-scope`).
That guard is intentional — an agent must not widen its own permissions. So the files here
are the reviewed versions; **you** copy them into place.

| Template | Copy to | Purpose |
|----------|---------|---------|
| `permissions.workspace.yaml` | `~/.kiro/workspace-roots/3324a452d6cf05d1/permissions.yaml` | Per-workspace permissions for this repo |
| `permissions.user.yaml` | `~/.kiro/settings/permissions.yaml` | Global permissions (all workspaces) |
| `mcp.json` | `.kiro/settings/mcp.json` | KiroGraph MCP server for this workspace |

`3324a452d6cf05d1` is the workspace-root hash Kiro assigned to
`/home/totoshko88/Documents/RustConn` (see `.trust-migration.json` in that directory).
On another machine the hash differs — check which directory under `~/.kiro/workspace-roots/`
holds a `.trust-migration.json` with `"root": ".../RustConn"`.

Apply with a plain copy, then reload the window:

```bash
cp .kiro/config-templates/permissions.workspace.yaml \
   ~/.kiro/workspace-roots/3324a452d6cf05d1/permissions.yaml
cp .kiro/config-templates/permissions.user.yaml ~/.kiro/settings/permissions.yaml
cp .kiro/config-templates/mcp.json .kiro/settings/mcp.json
```

## What changed versus the live files

**File reads never prompt again.** Both templates start with an unconditional
`fs_read: allow`. The workspace file already had it; the global one did not, which is why
reads still prompted outside this repo. Note that reads done through the shell (`cat`,
`grep`, `rg`) fall under `shell`, and reads through an MCP server fall under `mcp` — three
separate capabilities, all covered below.

**Deduplicated.** The live workspace file had grown two `shell` blocks and two `fs_write`
blocks (the second one allowing exactly one file, `.kiro/specs/embedded-web-browser/tasks.md`);
"Allow always" appends rather than merges. Everything is now one block per capability, sorted.

**Junk dropped from the global shell list.** Entries that "Allow always" captured from
shell fragments rather than commands: `0 *`, `-n *`, `# *`, `{ *`, `(cd *`, `if *`, `while *`,
`for *`, `declare *`, `set *`, `wait *`, `command *`, `type *`, `source *`,
`getDiagnostics_check() *`, `DOMAIN="rustconn" *`, `R=eu-central-1 *`, plus one-off
absolute paths into `~/.nvm/versions/node/v24.15.0/…` and sandbox virtualenvs.

**Destructive commands now prompt again.** Removed from the global allow list:
`rm *`, `rmdir *`, `mv *`, `chmod *`, `ln *`, `kill *`, `pkill *`, `killall *`, `fuser *`,
`fusermount *`, `ssh *`, `sshpass *`, `snapcraft *`, `snap *`, `apt *`, `dpkg *`, `brew *`,
`pip *`, `pip3 *`, `npx *`, `aws *`, `az *`, `azure-nuke *`, `lxc *`, `kubectl *`,
`gnome-extensions *`, `/snap/bin/tofu *`. A confirmation prompt on `rm -rf` or a cloud
mutation is the point, not friction to be optimised away. Read-only and build tooling
(`cargo`, `git` minus history rewrites, `grep`, `rg`, `ls`, `cat`, `find`, `msg*`,
`flatpak-builder`, `gh`, …) stays allowed.

**Secrets are excluded from writes, but reads are wide open.** `fs_read: allow` does cover
`.env` and `*.pem`. Whether a `deny` rule for `fs_read` overrides a broad `allow` in this
Kiro version is untested, so no such rule is included here — do not rely on one without
verifying it first. The `fs_write` excludes for `.env*`, `*.pem` and `secrets/**` are
carried over unchanged.

**`mcp.json`**: adds `--path` so the KiroGraph server binds to this repo explicitly.
Without it the server resolves the project from its startup cwd; when that cwd was
`rustconn/src`, every graph tool answered "KiroGraph not initialized" while the CLI in the
repo root worked fine. The `env.PATH` entry stays because the server is launched without a
login shell.
