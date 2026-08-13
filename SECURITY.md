# Security Policy

## Reporting a Vulnerability

Please report security issues privately via GitHub Security Advisories
(<https://github.com/totoshko88/RustConn/security/advisories/new>) rather than
in public issues. We aim to acknowledge reports within a few days.

## Release Artifact Provenance

The `.deb`, `.rpm`, AppImage and Flatpak bundle attached to a GitHub release from
0.20.0 onwards carry a signed [SLSA build provenance](https://slsa.dev/) attestation,
issued through Sigstore to the release workflow itself:

```bash
gh attestation verify <file> --repo totoshko88/RustConn
```

**What a pass proves.** The file's digest and filename match a statement GitHub
holds, signed with a short-lived certificate bound to a specific workflow run in
this repository, naming the commit it was built from. An artifact rebuilt elsewhere,
tampered with after the run, or renamed will not verify.

**What it does not prove.** Nothing about the *contents* being free of defects or
malicious code — provenance answers "who built this, from what source", not "is this
source trustworthy". It also does not cover the snap (signed by the Snap Store) or
packages installed from Flathub, OBS, AUR, nixpkgs or Homebrew, which are signed by
whichever repository serves them.

See [docs/INSTALL.md](docs/INSTALL.md#verifying-a-file-downloaded-from-a-github-release)
for the offline variant and further detail.

## Credential Storage — Threat Model

RustConn supports several backends for storing connection credentials. They
differ in the level of protection they provide; choose the one that matches
your threat model.

### Recommended: keyring / vault backends

For real secrets, use one of the integrated secret backends:

- **System keyring** (libsecret / GNOME Keyring / KWallet via `secret-tool`)
- **Vault managers**: Bitwarden, KeePassXC, 1Password, Passbolt

These keep secrets encrypted at rest under a key that is **not** stored next to
the data, and (for the system keyring) unlocked together with your login
session. This is the appropriate choice when defending against an attacker who
can read your files.

### Machine-key encryption — obfuscation at rest, not strong protection

When no vault/keyring backend is configured, the `*_encrypted` fields in the
configuration are encrypted with AES-256-GCM using a **machine key** stored at
`~/.local/share/rustconn/.machine-key` (file mode `0600`).

**What this protects against:** casual disk inspection, shoulder-surfing the
plaintext config, and secrets leaking into backups or synced dotfiles.

**What this does NOT protect against:** an attacker who can read files as the
**same user** — the decryption key sits next to the data by design, so anyone
with read access to your home directory can decrypt these fields. Treat
machine-key encryption as **obfuscation at rest**, not as a security boundary.

> If you store sensitive credentials, use a keyring or vault backend instead of
> relying on machine-key encryption.

## Known Issues

### Passbolt: passphrase passed as a command-line argument

The Passbolt backend shells out to the community `go-passbolt-cli` tool, which
currently accepts the GPG passphrase only via the `--userPassword` argument
(no stdin or environment-variable input). While the command runs, the
passphrase is therefore visible in `/proc/<pid>/cmdline` to other processes
running under the **same user**.

- **Scope:** only during the short-lived `go-passbolt-cli` invocation, and only
  to processes of the same UID. The passphrase is never written to logs or to
  disk by RustConn.
- **Upstream limitation:** this is a constraint of `go-passbolt-cli`, not of
  RustConn. We will switch to environment-variable or stdin input (as already
  done for SSH `ASKPASS`) once upstream supports it.
- **Mitigation:** on a multi-user host, prefer the system keyring or another
  vault backend for Passbolt-stored secrets, or avoid running untrusted
  processes under the same user account while connecting.
