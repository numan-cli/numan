#!/usr/bin/env bash
# Install numan via Homebrew after checking for conflicting installs from other channels.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

echo "Checking for conflicting numan installs..."
cargo run --quiet --bin numan-install-guard --manifest-path "${repo_root}/Cargo.toml" -- brew
echo "Running: brew install tonythethompson/numan/numan ${*}"
brew install tonythethompson/numan/numan "$@"
