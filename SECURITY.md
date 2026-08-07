# Security policy

This repository is the Numan CLI (Rust package manager for Nushell). It
verifies signed registry indexes and artifact digests; it does not publish the
official catalog.

Companion policies:

- Registry (signed index, yanks, key incidents):
  [tonythethompson/numan-registry SECURITY.md](https://github.com/tonythethompson/numan-registry/blob/main/SECURITY.md)
- Plugin build pipeline:
  [tonythethompson/numan-plugins SECURITY.md](https://github.com/tonythethompson/numan-plugins/blob/main/SECURITY.md)

## Report a vulnerability

Do not publish exploit details, private keys, or proof-of-concept payloads that
could harm other users in a public issue.

Preferred: open a private GitHub security advisory at
<https://github.com/tonythethompson/numan/security/advisories/new>.

Fallback: open a public issue titled **Security contact request** with no
technical details. The maintainer will establish a private channel before
collecting the report.

Helpful report contents:

- Numan version (`numan --version`) and install method
- OS / architecture and Nushell version when relevant
- Exact command(s), redacted config or env only as needed
- Whether `NUMAN_ALLOW_UNSIGNED` or other override env vars were set
- Reproduction steps sufficient for independent verification

## Scope

**In scope for this repo**

- Failures or bypasses of Ed25519 index verification or SHA-256 artifact checks
- Path traversal, arbitrary write, or unsafe extraction during download/install
- Lockfile, journal, snapshot, or mutation-lock integrity bugs that could cause
  loss or silent corruption of managed state
- Plugin or module activation paths that execute untrusted code without the
  documented user-driven activate step
- Self-update verification failures (`SHA256SUMS` / signature handling)
- Secret or credential leakage introduced by this codebase or its release assets

**Out of scope here (report elsewhere)**

- Compromised or malicious packages already listed in the signed official index:
  [numan-registry](https://github.com/tonythethompson/numan-registry)
- Bad builds or release assets that never reached a signed index:
  [numan-plugins](https://github.com/tonythethompson/numan-plugins)
- Vulnerabilities inside upstream Nushell plugins/modules themselves
- Issues that only appear after deliberately disabling verification
  (`NUMAN_ALLOW_UNSIGNED=1` and similar override paths)
- Denial of service against third-party hosts or GitHub Releases

When unsure, report here. We will route it.

## Trust model (summary)

1. The `official` registry public key is built into the binary. `numan registry
   sync` verifies Ed25519 signatures over canonical `index.json` bytes.
2. Plugin artifacts require a SHA-256 digest in the index; missing digests fail
   the install transaction.
3. Install is always inert. Nu plugin registration and module autoload are owned
   by activate/deactivate, not by install.
4. Managed paths use ownership markers and mutation locks so Numan does not
   overwrite foreign files or race concurrent destructive operations.
5. Standalone `numan update --self` verifies a signed `SHA256SUMS` before
   replacing the binary. Package-manager installs (brew, winget, cargo) stay on
   those channels' update paths.

## Supported versions

Security fixes target the latest released Numan version and `master`. Older
releases may not receive backports while the project is pre-1.0. Prefer
upgrading to the latest release after an advisory.

## Related docs

- Client overview and trust features: [README.md](README.md)
- Registry incident procedures (yank, rollback, user remediation):
  [numan-registry incident-response](https://github.com/tonythethompson/numan-registry/blob/main/docs/incident-response.md)
- Active-plugin mutation gate:
  [docs/active-plugin-gate.md](docs/active-plugin-gate.md)
