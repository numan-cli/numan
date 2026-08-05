#!/usr/bin/env bash
# Install numan via Homebrew after checking for conflicting installs from other channels.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
guard_manifest="${repo_root}/install-guard/Cargo.toml"

if [[ ! -f "${guard_manifest}" ]]; then
  echo "install-guard manifest not found at ${guard_manifest} (run from the numan repo)." >&2
  exit 1
fi

echo "Checking for conflicting numan installs..."
cargo run --quiet --manifest-path "${guard_manifest}" -- brew
echo "Running: brew install tonythethompson/numan/numan ${*}"
brew install tonythethompson/numan/numan "$@"
