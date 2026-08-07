# Homebrew packaging

The formula **shape** (bottle platforms, install layout) is defined by
[`scripts/render_homebrew_formula.py`](../../scripts/render_homebrew_formula.py).
[`numan.rb`](numan.rb) is the last generated snapshot checked into this repo for
review; digests and bottle stanzas may lag until the next tagged release whose
`SHA256SUMS` include every required archive. Today it still matches the
pre-Linux-ARM contract (macOS ARM + Linux x86_64 only) because no published
release yet ships `aarch64-unknown-linux-gnu`. The live tap is
[`tonythethompson/homebrew-numan`](https://github.com/tonythethompson/homebrew-numan)
(tap with the HTTPS remote below), updated by the publish workflow after each
`v*.*.*` release.

## Install

Prefer the explicit HTTPS remote so `brew update` does not depend on SSH host
keys or GitHub SSH auth:

```bash
brew tap tonythethompson/numan https://github.com/tonythethompson/homebrew-numan
brew install numan
```

`brew tap tonythethompson/numan` alone also works when Homebrew clones over
HTTPS (the usual default for public taps).

The tap repository **must be public**. A private tap is the main reason earlier
`brew tap` / `brew install` attempts failed for users.

## Troubleshooting

### `Host key verification failed` while updating the tap

`brew update` fetches each tapped repo with git. If this tap was cloned (or
rewritten) to an SSH remote (`git@github.com:...`) and SSH host-key checks
fail, the tap stays stale and installs can keep an old formula.

Recover by switching the tap to HTTPS, then reinstalling:

```bash
brew untap tonythethompson/numan
brew tap tonythethompson/numan https://github.com/tonythethompson/homebrew-numan
brew install tonythethompson/numan/numan
```

Or keep the tap and only fix the remote:

```bash
git -C "$(brew --repo tonythethompson/numan)" remote set-url origin https://github.com/tonythethompson/homebrew-numan.git
brew update
brew reinstall tonythethompson/numan/numan
```

If git still rewrites HTTPS to SSH, check for `url.*.insteadOf` rules:

```bash
git config --global --get-regexp '^url\..*\.insteadof$'
```

## Updating for a release

Prefer the automated path: after a `v*.*.*` GitHub Release, the
[`Publish to Homebrew tap`](../../.github/workflows/homebrew.yml) workflow
downloads `SHA256SUMS`, regenerates the formula, and pushes
`Formula/numan.rb` to the tap.

Manual / dry-run (current formula: all three Homebrew archives, including Linux
ARM). Use a tag that ships `aarch64-unknown-linux-gnu` (the first such release
after the ARM platform expansion). Pre-ARM tags such as `v0.1.5` need
`--legacy-pre-linux-arm`:

```bash
# ARM-enabled release (preferred):
gh release download vX.Y.Z --pattern SHA256SUMS
python3 scripts/render_homebrew_formula.py \
  --version X.Y.Z \
  --sha256sums SHA256SUMS \
  --write

# Pre-Linux-ARM recovery only (e.g. v0.1.5):
gh release download v0.1.5 --pattern SHA256SUMS
python3 scripts/render_homebrew_formula.py \
  --version 0.1.5 \
  --sha256sums SHA256SUMS \
  --legacy-pre-linux-arm \
  --write
```

Required release assets (current):

- `numan-<version>-aarch64-apple-darwin.tar.gz`
- `numan-<version>-x86_64-unknown-linux-gnu.tar.gz`
- `numan-<version>-aarch64-unknown-linux-gnu.tar.gz`

Legacy (`--legacy-pre-linux-arm`) omits the Linux ARM archive and the formula
`on_linux` / `on_arm` bottle block.

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
