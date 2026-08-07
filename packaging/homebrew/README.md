# Homebrew packaging

Canonical formula source lives at [`numan.rb`](numan.rb). The published tap is
[`tonythethompson/homebrew-numan`](https://github.com/tonythethompson/homebrew-numan)
(`brew tap tonythethompson/numan`).

## Install

```bash
brew tap tonythethompson/numan
brew install numan
```

The tap repository **must be public**. A private tap is the main reason earlier
`brew tap` / `brew install` attempts failed for users.

## Updating for a release

Prefer the automated path: after a `v*.*.*` GitHub Release, the
[`Publish to Homebrew tap`](../../.github/workflows/homebrew.yml) workflow
downloads `SHA256SUMS`, regenerates the formula, and pushes
`Formula/numan.rb` to the tap.

Manual / dry-run:

```bash
gh release download v0.1.5 --pattern SHA256SUMS
python3 scripts/render_homebrew_formula.py \
  --version 0.1.5 \
  --sha256sums SHA256SUMS \
  --write
```

Required release assets:

- `numan-<version>-aarch64-apple-darwin.tar.gz`
- `numan-<version>-x86_64-unknown-linux-gnu.tar.gz`
- `numan-<version>-aarch64-unknown-linux-gnu.tar.gz`

Intel Mac (`x86_64-apple-darwin`) is not shipped; the formula fails with a clear
`odie` on Intel Hardware.

Archives must extract to `numan-<version>-<triple>/numan` (matches the Release
workflow layout). Homebrew stages into that sole top-level directory, so the
formula installs `./numan` (not `./numan-*/numan`).

## Secrets

| Secret | Where | Purpose |
|--------|--------|---------|
| `HOMEBREW_TAP_TOKEN` | `numan` repo Actions secrets | PAT with `contents:write` on `tonythethompson/homebrew-numan` so the publish workflow can push formula updates |

Without `HOMEBREW_TAP_TOKEN`, the Homebrew publish job fails closed (does not
silently skip).
